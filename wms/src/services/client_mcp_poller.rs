//! Inbound side of the paid relay-carried MCP channel (`/E/c/*`).
//!
//! A subscriber's LLM agent can't reach this NAT'd node directly, so it posts
//! signed MCP requests to `<relay>/E/c/dispatch/<our_uuid>`; the relay gates
//! them on a valid `SubscriptionCert` and queues them. This poller pulls the
//! queue and hands each payload to the SHARED [`client_mcp::serve_signed`], which
//! **re-verifies each request against our own `ECK_SUB_ROOT_PUBKEY`** (defense
//! in depth — we don't trust the relay's admission), runs the JSON-RPC through
//! the same `mcp::handle_jsonrpc` the direct `/mcp` uses (with `over_relay =
//! true`), and acks the response back to the relay. The direct `POST /mcp/signed`
//! ingress calls the very same `serve_signed`, so both transports share one gate.
//!
//! The poller only starts when `ECK_SUB_ROOT_PUBKEY` is configured: a node not
//! participating in paid relay access never polls a channel it couldn't verify.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use eck_core::xelixir::subscription::read_sub_root_from_env;

use crate::services::client_mcp::serve_signed;
use crate::AppState;

const POLL_INTERVAL_SECS: u64 = 10;
/// Never let a relay `next_poll_in_seconds` hint slow this channel below a
/// 10 s cadence — a subscriber's MCP call is waiting on the other side.
/// (Long-poll relays hint 1: the relay-side hold does the pacing.)
const MAX_POLL_INTERVAL_SECS: u64 = 10;
/// Poll HTTP timeout. Must exceed the relay's long-poll hold (8 s) plus its
/// per-query DB latency; old relays answer instantly and are unaffected.
const POLL_TIMEOUT_SECS: u64 = 35;
/// Recently-completed task ids kept for dedup: a task the relay re-delivers
/// (stale delivered_at, relay restart) must NOT be re-executed — the nonce
/// guard would 403 it and the ack could shadow the real result on old relays.
const COMPLETED_CAP: usize = 4096;
/// The relay task row stays `acked = NONE` until our ack POST lands, so a LOST
/// ack strands the task: the relay re-delivers it after `REDELIVER_AFTER_SECS`,
/// but our `completed` set then SKIPS it (double-execution guard), so it is
/// never re-acked and the waiting subscriber burns its whole poll window. Under
/// many parallel callers a single ack POST can transiently fail/time out, so
/// retry it (bounded, with backoff) and verify the relay accepted it.
const ACK_MAX_ATTEMPTS: u32 = 4;
const ACK_RETRY_BACKOFF_MS: u64 = 250;

fn relay_base_url() -> String {
    std::env::var("RELAY_URL")
        .unwrap_or_else(|_| "http://localhost:3200".into())
        .trim_end_matches('/')
        .to_string()
}

fn http_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

async fn ack_relay(relay_url: &str, task_id: &str, body: Value) {
    let url = format!("{}/E/c/ack/{}", relay_url, task_id);
    let client = http_client(15);
    for attempt in 1..=ACK_MAX_ATTEMPTS {
        match client.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => warn!(
                "[client_mcp] ack task={} attempt {}/{} non-success: {}",
                task_id,
                attempt,
                ACK_MAX_ATTEMPTS,
                r.status()
            ),
            Err(e) => warn!(
                "[client_mcp] ack task={} attempt {}/{} failed: {}",
                task_id, attempt, ACK_MAX_ATTEMPTS, e
            ),
        }
        if attempt < ACK_MAX_ATTEMPTS {
            // Linear backoff — the relay's embedded DB just needs a beat to drain
            // a concurrent write burst; this is not a long outage.
            tokio::time::sleep(Duration::from_millis(ACK_RETRY_BACKOFF_MS * attempt as u64)).await;
        }
    }
    warn!(
        "[client_mcp] ack task={} FAILED after {} attempts — result was produced but could \
         not be delivered to the relay; the subscriber will time out",
        task_id, ACK_MAX_ATTEMPTS
    );
}

pub async fn start_poller(state: Arc<AppState>) {
    if read_sub_root_from_env().is_none() {
        info!("[client_mcp] ECK_SUB_ROOT_PUBKEY unset — relay MCP channel disabled on this node");
        return;
    }
    info!(
        "[client_mcp] relay-MCP poller starting for instance {}",
        state.instance_id
    );
    let in_flight: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));
    let completed: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));
    let mut next_poll = POLL_INTERVAL_SECS;
    loop {
        tokio::time::sleep(Duration::from_secs(next_poll.max(1))).await;
        match poll_once(&state, &in_flight, &completed).await {
            Ok(hint) => {
                next_poll = hint
                    .unwrap_or(POLL_INTERVAL_SECS)
                    .clamp(1, MAX_POLL_INTERVAL_SECS)
            }
            Err(e) => {
                debug!("[client_mcp] poll cycle: {}", e);
                next_poll = POLL_INTERVAL_SECS;
            }
        }
    }
}

async fn poll_once(
    state: &Arc<AppState>,
    in_flight: &Arc<Mutex<std::collections::HashSet<String>>>,
    completed: &Arc<Mutex<std::collections::HashSet<String>>>,
) -> Result<Option<u64>, String> {
    let relay_url = relay_base_url();
    let url = format!("{}/E/c/poll/{}", relay_url, state.instance_id);

    let resp = http_client(POLL_TIMEOUT_SECS)
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("poll request failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("poll non-success status: {}", resp.status()));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("poll body parse: {}", e))?;
    let next_hint = body.get("next_poll_in_seconds").and_then(|v| v.as_u64());
    let tasks = body
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if tasks.is_empty() {
        return Ok(next_hint);
    }

    // GC expired nonces once per cycle (the shared `xelixir_nonce` table).
    let _ = state
        .db
        .query("DELETE xelixir_nonce WHERE expires_at < time::now()")
        .await;

    for task in tasks {
        let task_id = task
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // A task we already finished must never run again (re-delivery after a
        // relay restart / stale delivered_at): the nonce guard would 403 it and
        // on an old relay that ack could shadow the real result.
        {
            let done = completed.lock().await;
            if done.contains(&task_id) {
                debug!("[client_mcp] skipping re-delivered completed task {}", task_id);
                continue;
            }
        }
        {
            let mut set = in_flight.lock().await;
            if set.contains(&task_id) {
                continue;
            }
            set.insert(task_id.clone());
        }

        let Some(payload) = task.get("payload").cloned() else {
            warn!("[client_mcp] task {} has no payload — dropping", task_id);
            ack_relay(&relay_url, &task_id, json!({"error": "invalid payload"})).await;
            in_flight.lock().await.remove(&task_id);
            continue;
        };

        // Run each request on a worker so a slow tool (e.g. ask_brain) doesn't
        // stall the poll loop. Verification + execution is the SHARED gate the
        // direct `/mcp/signed` ingress also uses.
        let state_w = Arc::clone(state);
        let relay_w = relay_url.clone();
        let in_flight_w = Arc::clone(in_flight);
        let task_w = task_id.clone();
        let completed_w = Arc::clone(completed);
        tokio::spawn(async move {
            let outcome = serve_signed(&state_w, payload).await;
            info!("[client_mcp] served task={} status={}", task_w, outcome.http_status());
            ack_relay(&relay_w, &task_w, outcome.into_ack_body()).await;
            // Mark completed BEFORE leaving in_flight so no poll can slip
            // between the two sets and re-execute the task.
            {
                let mut done = completed_w.lock().await;
                if done.len() >= COMPLETED_CAP {
                    done.clear(); // relay GC'd these tasks long ago anyway
                }
                done.insert(task_w.clone());
            }
            in_flight_w.lock().await.remove(&task_w);
        });
    }
    Ok(next_hint)
}
