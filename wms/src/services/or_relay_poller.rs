//! Serving side of the native paid-mesh AI channel (`/E/ai/*`).
//!
//! Owner decision (2026-08-30): "asking glm" is a native paid-mesh service that
//! rides the relay, not a separate HTTP port. This poller is the relay-transport
//! twin of the `POST /ai/or` HTTP handler: it pulls `ai.or.request` envelopes
//! addressed to THIS node out of the relay's `/E/ai/*` queue and runs each
//! through the SHARED [`crate::handlers::or_proxy::serve_or_request`] — the very
//! same license verify + allowlist/policy/cap gate + verbatim upstream forward +
//! metering the HTTP path uses. The client's paid license travels INSIDE the
//! envelope and is verified there, so admission is identical on both transports.
//!
//! Gated by the SAME `ECK_OR_SERVE=1` as the HTTP route: a node that doesn't
//! serve AI distribution never polls a channel it wouldn't answer.
//!
//! Lifecycle constraints this satisfies (recorded because they are exactly what
//! disqualified the `/E/x/*` ops channel for AI, verified live 2026-08-30):
//!   * validity window is the ENVELOPE's own `expires_at` (client sets >= 10 min)
//!     — NOT a 60-90 s signature age. AI calls run MINUTES; a short age window
//!     would expire a request mid-generation.
//!   * result persists across a WMS restart on either side: the relay keeps the
//!     acked row (GC'd after 1 h) and re-delivers an un-acked task only after its
//!     `REDELIVER_AFTER_SECS` window, so a restart re-picks rather than loses it.
//!   * NO duplicate-nonce failure on redelivery: the `completed` set skips a
//!     re-delivered finished task without re-executing (so it never trips the
//!     nonce guard), and the relay ack is first-ack-wins.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, info, warn};

use crate::handlers::or_proxy::serve_or_request;
use crate::services::client_mcp::{nonce_seen_and_record, NONCE_TTL_SECS};
use crate::AppState;

const POLL_INTERVAL_SECS: u64 = 10;
/// Never let a relay hint slow this below a 10 s cadence — a client's AI call is
/// waiting. (The relay long-poll hold does the real pacing when work is due.)
const MAX_POLL_INTERVAL_SECS: u64 = 10;
/// Poll HTTP timeout — must exceed the relay's long-poll hold (8 s) plus DB
/// latency on a slow relay box.
const POLL_TIMEOUT_SECS: u64 = 35;
/// Default parallel in-flight generations. Small: each is a minutes-long,
/// metered upstream call. Overridable via `ECK_OR_SERVE_CONCURRENCY` (clamped 1..=8).
const DEFAULT_CONCURRENCY: usize = 3;
/// Recently-completed task ids kept for dedup — a re-delivered finished task must
/// not re-execute (double upstream charge) nor re-ack (first-ack-wins already
/// protects the stored result, but skipping avoids the wasted call).
const COMPLETED_CAP: usize = 4096;
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

fn concurrency() -> usize {
    std::env::var("ECK_OR_SERVE_CONCURRENCY")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_CONCURRENCY)
        .clamp(1, 8)
}

/// `true` when the envelope's own validity window has already passed — the
/// client has given up, so we must not spend an upstream call on it. Pure so the
/// window logic is unit-tested without a relay. A missing/zero `expires_at` is
/// treated as NOT expired (fail-open to serving; the client-side deadline still
/// bounds the wait) so an older client that omits the field still works.
pub(crate) fn envelope_expired(expires_at: i64, now: i64) -> bool {
    expires_at > 0 && now > expires_at
}

async fn ack_relay(relay_url: &str, task_id: &str, body: Value) {
    let url = format!("{}/E/ai/ack/{}", relay_url, task_id);
    let client = http_client(15);
    for attempt in 1..=ACK_MAX_ATTEMPTS {
        match client.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => return,
            Ok(r) => warn!(
                "[or-relay] ack task={} attempt {}/{} non-success: {}",
                task_id, attempt, ACK_MAX_ATTEMPTS, r.status()
            ),
            Err(e) => warn!(
                "[or-relay] ack task={} attempt {}/{} failed: {}",
                task_id, attempt, ACK_MAX_ATTEMPTS, e
            ),
        }
        if attempt < ACK_MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(ACK_RETRY_BACKOFF_MS * attempt as u64)).await;
        }
    }
    warn!(
        "[or-relay] ack task={} FAILED after {} attempts — result produced but not delivered; \
         the client will time out",
        task_id, ACK_MAX_ATTEMPTS
    );
}

pub async fn start_poller(state: Arc<AppState>) {
    if !std::env::var("ECK_OR_SERVE").map(|v| v == "1").unwrap_or(false) {
        info!("[or-relay] ECK_OR_SERVE != 1 — native AI-distribution channel disabled on this node");
        return;
    }
    let permits = concurrency();
    info!(
        "[or-relay] AI-distribution poller starting for instance {} (concurrency={})",
        state.instance_id, permits
    );
    let sem = Arc::new(Semaphore::new(permits));
    let in_flight: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));
    let completed: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));
    let mut next_poll = POLL_INTERVAL_SECS;
    loop {
        tokio::time::sleep(Duration::from_secs(next_poll.max(1))).await;
        match poll_once(&state, &sem, &in_flight, &completed).await {
            Ok(hint) => {
                next_poll = hint
                    .unwrap_or(POLL_INTERVAL_SECS)
                    .clamp(1, MAX_POLL_INTERVAL_SECS)
            }
            Err(e) => {
                debug!("[or-relay] poll cycle: {}", e);
                next_poll = POLL_INTERVAL_SECS;
            }
        }
    }
}

async fn poll_once(
    state: &Arc<AppState>,
    sem: &Arc<Semaphore>,
    in_flight: &Arc<Mutex<std::collections::HashSet<String>>>,
    completed: &Arc<Mutex<std::collections::HashSet<String>>>,
) -> Result<Option<u64>, String> {
    let relay_url = relay_base_url();
    let url = format!("{}/E/ai/poll/{}", relay_url, state.instance_id);

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
        {
            let done = completed.lock().await;
            if done.contains(&task_id) {
                debug!("[or-relay] skipping re-delivered completed task {}", task_id);
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
            warn!("[or-relay] task {} has no payload — dropping", task_id);
            ack_relay(&relay_url, &task_id, json!({"error": "invalid payload"})).await;
            in_flight.lock().await.remove(&task_id);
            continue;
        };

        // Envelope validity window (the client's own >= 10 min budget). An
        // expired envelope means the client already gave up — ack an error and
        // do NOT burn an upstream call.
        let now = chrono::Utc::now().timestamp();
        let env = payload.get("envelope");
        let expires_at = env.and_then(|e| e.get("expires_at")).and_then(|v| v.as_i64()).unwrap_or(0);
        if envelope_expired(expires_at, now) {
            warn!("[or-relay] task {} envelope expired (expires_at={expires_at}, now={now}) — dropping", task_id);
            ack_relay(&relay_url, &task_id, json!({"status": 408, "error": "request envelope expired before pickup"})).await;
            in_flight.lock().await.remove(&task_id);
            continue;
        }

        // Replay guard — namespaced (`orai:`) so it can't collide with ops/cmcp
        // nonces in the shared `xelixir_nonce` table. TTL is the same 2× window
        // policy; the envelope's own validity window is <= that in practice.
        let nonce = env
            .and_then(|e| e.get("nonce"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !nonce.is_empty() {
            let nonce_key = format!("orai:{}", nonce);
            match nonce_seen_and_record(&state.db, &nonce_key, NONCE_TTL_SECS).await {
                Ok(true) => {
                    warn!("[or-relay] duplicate nonce {} for task {} — ignoring", nonce, task_id);
                    ack_relay(&relay_url, &task_id, json!({"error": "duplicate nonce"})).await;
                    in_flight.lock().await.remove(&task_id);
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    warn!("[or-relay] nonce store error for {}: {} — rejecting", nonce, e);
                    ack_relay(&relay_url, &task_id, json!({"error": format!("nonce store unavailable: {e}")})).await;
                    in_flight.lock().await.remove(&task_id);
                    continue;
                }
            }
        }

        let license = payload
            .get("license")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let request = payload.get("request").cloned().unwrap_or(json!({}));

        // Bound parallel generations. Acquire BEFORE spawning so the poll loop
        // naturally backpressures when all permits are busy.
        let permit = match Arc::clone(sem).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                // Semaphore closed (never in practice) — skip this cycle.
                in_flight.lock().await.remove(&task_id);
                continue;
            }
        };

        let state_w = Arc::clone(state);
        let relay_w = relay_url.clone();
        let in_flight_w = Arc::clone(in_flight);
        let completed_w = Arc::clone(completed);
        let task_w = task_id.clone();
        tokio::spawn(async move {
            let _permit = permit; // held for the whole generation
            let (status, body) = serve_or_request(&state_w, &license, request).await;
            info!("[or-relay] served task={} status={}", task_w, status.as_u16());
            // Transport-neutral result: the client re-derives its HTTP-shaped
            // (status, body) from this, then runs the SAME response shim.
            let ack_body = json!({ "status": status.as_u16(), "body": body });
            ack_relay(&relay_w, &task_w, ack_body).await;
            {
                let mut done = completed_w.lock().await;
                if done.len() >= COMPLETED_CAP {
                    done.clear();
                }
                done.insert(task_w.clone());
            }
            in_flight_w.lock().await.remove(&task_w);
        });
    }
    Ok(next_hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_window_expiry() {
        let now = 1_800_000_000i64;
        // Live window (10 min out) → not expired.
        assert!(!envelope_expired(now + 600, now));
        // Exactly at the deadline → not yet expired (strict >).
        assert!(!envelope_expired(now, now));
        // Past the deadline → expired.
        assert!(envelope_expired(now - 1, now));
        // Missing/zero expires_at → fail-open (older client omits it).
        assert!(!envelope_expired(0, now));
    }

    #[test]
    fn concurrency_clamps() {
        std::env::set_var("ECK_OR_SERVE_CONCURRENCY", "0");
        assert_eq!(concurrency(), 1, "0 clamps up to 1");
        std::env::set_var("ECK_OR_SERVE_CONCURRENCY", "99");
        assert_eq!(concurrency(), 8, "99 clamps down to 8");
        std::env::set_var("ECK_OR_SERVE_CONCURRENCY", "4");
        assert_eq!(concurrency(), 4);
        std::env::remove_var("ECK_OR_SERVE_CONCURRENCY");
        assert_eq!(concurrency(), DEFAULT_CONCURRENCY, "unset → default");
    }
}
