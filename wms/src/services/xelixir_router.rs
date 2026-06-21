//! Cross-mesh xelixir control routing.
//!
//! `dispatch()` is called by the cloud admin's `POST /X/devices/:id/start|stop`
//! handler. It resolves the target node via the relay (mesh-agnostic),
//! signs a `CommandEnvelope` with the local server identity, and tries:
//!   1. Direct HTTPS POST to `<target.base_url>/X/self/{start|stop}` (3 s timeout)
//!   2. On any failure → POST to `<relay>/E/x/dispatch/<target>`; target polls and acks.
//!
//! `start_poller()` is the inbound side: every 10 s the local WMS pulls
//! `<relay>/E/x/poll/<self_uuid>`, verifies each signed envelope against
//! `XELIXIR_ADMIN_PUBKEYS`, drives the local `AgentController`, and acks
//! the relay row. A small in-memory nonce cache prevents replay within
//! the timestamp window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use eck_core::xelixir::envelope::{
    read_admin_pubkeys_from_env, read_fleet_root_from_env, CommandEnvelope, SignedEnvelope,
    DEFAULT_MAX_AGE_SECS,
};

use crate::AppState;

const POLL_INTERVAL_SECS: u64 = 10;
const DIRECT_POST_TIMEOUT_SECS: u64 = 3;
const NONCE_TTL_SECS: u64 = (DEFAULT_MAX_AGE_SECS as u64) * 2;

#[derive(Debug, serde::Serialize)]
pub struct DispatchResult {
    pub status: &'static str,
    pub via: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xelixir_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xelixir_session_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Inline output for self-targeted (local) dispatch — read verbs return
    /// their data here directly instead of via a relay `task_id` poll.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

/// Resolve, sign, route. Returns a structured result whether the command
/// landed directly on the target or got queued via the relay.
pub async fn dispatch(
    state: &Arc<AppState>,
    target_uuid: &str,
    command: &str,
) -> Result<DispatchResult, String> {
    dispatch_envelope(state, CommandEnvelope::new(target_uuid.to_string(), command.to_string())).await
}

/// Sign + route a pre-built envelope. Used by `dispatch` (legacy
/// start/stop) and `dispatch_ops` (new ops vocabulary).
pub async fn dispatch_envelope(
    state: &Arc<AppState>,
    envelope: CommandEnvelope,
) -> Result<DispatchResult, String> {
    let target_uuid = envelope.target_uuid.clone();
    let command = envelope.command.clone();
    // Prefer fleet-admin cert signing when this node is configured as a control
    // node (`ECK_FLEET_ADMIN_*` set): sign with the operational admin key and
    // attach its root-signed cert, so targets accept it via the trusted root
    // (`ECK_FLEET_ROOT_PUBKEY`) without a per-node allow-list entry. Otherwise
    // fall back to legacy node-identity signing (static `XELIXIR_ADMIN_PUBKEYS`).
    let signed = match (
        std::env::var("ECK_FLEET_ADMIN_PRIVKEY").ok().filter(|s| !s.is_empty()),
        std::env::var("ECK_FLEET_ADMIN_PUBKEY").ok().filter(|s| !s.is_empty()),
        std::env::var("ECK_FLEET_ADMIN_CERT").ok().filter(|s| !s.is_empty()),
    ) {
        (Some(apriv), Some(apub), Some(cert)) => SignedEnvelope::sign_admin(
            envelope,
            state.instance_id.clone(),
            apub,
            &apriv,
            cert,
        )?,
        _ => SignedEnvelope::sign(
            envelope,
            state.instance_id.clone(),
            state.server_identity.public_key.clone(),
            &state.server_identity.private_key,
        )?,
    };
    dispatch_signed(state, &target_uuid, &command, signed).await
}

/// Cross-mesh dispatch of an ops verb (e.g., `deploy` with `{branch,
/// crate_name, service}`). Wraps it as an `ops.<verb>` envelope and
/// routes through the same path as agent start/stop. Returns the relay
/// task_id; callers poll `GET /E/x/result/<task_id>` until completion.
pub async fn dispatch_ops(
    state: &Arc<AppState>,
    target_uuid: &str,
    verb: &str,
    args: Value,
) -> Result<DispatchResult, String> {
    let envelope = CommandEnvelope::new_ops(target_uuid.to_string(), verb, args);
    dispatch_envelope(state, envelope).await
}

async fn dispatch_signed(
    state: &Arc<AppState>,
    target_uuid: &str,
    command: &str,
    signed: SignedEnvelope,
) -> Result<DispatchResult, String> {
    // Self-targeted dispatch: a node can't authenticate a command to itself
    // through the relay path — its own server key is not in its own
    // XELIXIR_ADMIN_PUBKEYS allow-list, so its poller rejects the queued task
    // as "signer pubkey not in allow-list" (and the relay would still report
    // it "completed" — a silent false success). The relay hop is pointless for
    // self anyway. Execute locally and return the real outcome inline.
    if target_uuid == state.instance_id {
        return match execute_envelope(state, &signed.envelope).await {
            Ok(v) => Ok(DispatchResult {
                status: "ok",
                via: "local",
                xelixir_status: None,
                xelixir_session_url: None,
                task_id: None,
                output: Some(v),
            }),
            Err(e) => Err(e),
        };
    }

    let relay_url = relay_base_url();

    // ops.* envelopes always ride the relay queue. The target's WMS
    // does not expose a /X/self/ops endpoint — it accepts ops verbs
    // only through the queue path, where ack-with-body returns the
    // verb's full output. Routing them through direct POST would
    // either 404 or (worse) land in the SvelteKit SPA fallback and
    // get misinterpreted as success.
    if command.starts_with("ops.") {
        let task_id = queue_via_relay(&relay_url, target_uuid, &signed).await?;
        return Ok(DispatchResult {
            status: "queued",
            via: "relay",
            xelixir_status: None,
            xelixir_session_url: None,
            task_id: Some(task_id),
            output: None,
        });
    }

    // Legacy start/stop: direct POST is a sub-second NAT-friendly hop
    // when the target is reachable, queue fallback otherwise.
    let maybe_target_url = resolve_base_url(&relay_url, target_uuid).await;
    if let Some(target_base) = maybe_target_url.clone() {
        match try_direct_post(&target_base, command, &signed).await {
            Ok(body) => {
                info!(
                    "[xelixir_router] direct POST to {} succeeded for target={}",
                    target_base, target_uuid
                );
                return Ok(DispatchResult {
                    status: "ok",
                    via: "direct",
                    xelixir_status: body
                        .get("xelixir_status")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    xelixir_session_url: body
                        .get("xelixir_session_url")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    task_id: None,
                    output: None,
                });
            }
            Err(e) => {
                debug!(
                    "[xelixir_router] direct POST to {} failed: {} — falling back to relay queue",
                    target_base, e
                );
            }
        }
    }

    // 4. Fallback: queue on the relay.
    let task_id = queue_via_relay(&relay_url, target_uuid, &signed).await?;
    Ok(DispatchResult {
        status: "queued",
        via: "relay",
        xelixir_status: None,
        xelixir_session_url: None,
        task_id: Some(task_id),
        output: None,
    })
}

/// Long-running poller — pulls xelixir commands queued for this node.
///
/// Cadence is adaptive: relay returns `next_poll_in_seconds` in each poll
/// response (fast when work is pending, slow when the queue is empty).
/// Each envelope is processed in a spawned task so that a long-running
/// ops verb (e.g., `ops.deploy`) doesn't stall the next poll cycle.
pub async fn start_poller(state: Arc<AppState>) {
    info!(
        "[xelixir_router] poller starting (adaptive cadence) for instance {}",
        state.instance_id
    );
    // Replay-guard nonce store is persisted in SurrealDB table `xelixir_nonce`
    // (rows are deleted on TTL expiry). Surviving WMS restarts means a captured
    // envelope can't be re-injected after a bounce within its 60 s validity
    // window — the in-memory variant lost state on every launch and reopened
    // exactly that replay slot.
    // Task IDs we have already spawned a worker for in this process
    // lifetime. The relay continues to return un-acked rows on every poll
    // cycle, so without this guard a long-running verb (cargo_build ~2 min)
    // gets re-dispatched on the next 3 s poll and the duplicate is then
    // rejected on the nonce cache — but only AFTER overwriting the first
    // worker's eventual success ack with "duplicate nonce". The set is
    // memory-only and reset on WMS restart, which is fine: on restart the
    // nonce cache also clears, so any re-issued task processes cleanly.
    let in_flight: Arc<Mutex<std::collections::HashSet<String>>> =
        Arc::new(Mutex::new(std::collections::HashSet::new()));

    let mut next_poll_in_secs: u64 = POLL_INTERVAL_SECS;

    loop {
        tokio::time::sleep(Duration::from_secs(next_poll_in_secs.max(1))).await;
        match poll_once(&state, &in_flight).await {
            Ok(hint) => {
                next_poll_in_secs = hint.unwrap_or(POLL_INTERVAL_SECS).clamp(1, 60);
            }
            Err(e) => {
                debug!("[xelixir_router] poll cycle: {}", e);
                // Stay at default cadence after errors so we don't tight-loop.
                next_poll_in_secs = POLL_INTERVAL_SECS;
            }
        }
    }
}

/// Returns the relay's `next_poll_in_seconds` hint when present.
async fn poll_once(
    state: &Arc<AppState>,
    in_flight: &Arc<Mutex<std::collections::HashSet<String>>>,
) -> Result<Option<u64>, String> {
    let relay_url = relay_base_url();
    let url = format!("{}/E/x/poll/{}", relay_url, state.instance_id);
    let client = http_client(15);

    let resp = client
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
    let next_hint = body
        .get("next_poll_in_seconds")
        .and_then(|v| v.as_u64());
    let tasks = body
        .get("tasks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if tasks.is_empty() {
        return Ok(next_hint);
    }

    let allowed = read_admin_pubkeys_from_env();
    let fleet_root = read_fleet_root_from_env();
    prune_nonces_db(&state.db).await;

    for task in tasks {
        let task_id = task
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Skip tasks already being processed by a previous poll cycle.
        // Without this guard a long-running verb gets re-spawned each
        // poll tick and the dupe-nonce reject overwrites the real result.
        {
            let mut set = in_flight.lock().await;
            if set.contains(&task_id) {
                debug!(
                    "[xelixir_router] task {} already in flight — skipping",
                    task_id
                );
                continue;
            }
            set.insert(task_id.clone());
        }
        let signed: SignedEnvelope = match task
            .get("payload")
            .cloned()
            .map(serde_json::from_value::<SignedEnvelope>)
        {
            Some(Ok(s)) => s,
            _ => {
                warn!(
                    "[xelixir_router] task {} has invalid payload — acking and dropping",
                    task_id
                );
                let _ = ack_relay(&relay_url, &task_id, json!({"success": false, "error":"invalid payload"})).await;
                in_flight.lock().await.remove(&task_id);
                continue;
            }
        };

        if let Err(e) = signed.verify(&state.instance_id, &allowed, fleet_root.as_deref(), DEFAULT_MAX_AGE_SECS) {
            warn!(
                "[xelixir_router] reject task {} from signer {}: {}",
                task_id, signed.signer_uuid, e
            );
            let _ = ack_relay(&relay_url, &task_id, json!({"success": false, "error": e})).await;
            in_flight.lock().await.remove(&task_id);
            continue;
        }

        // Replay guard — persistent across WMS restart.
        match nonce_seen_and_record(&state.db, &signed.envelope.nonce, NONCE_TTL_SECS).await {
            Ok(true) => {
                warn!(
                    "[xelixir_router] duplicate nonce {} from {} — ignoring",
                    signed.envelope.nonce, signed.signer_uuid
                );
                let _ = ack_relay(&relay_url, &task_id, json!({"success": false, "error":"duplicate nonce"})).await;
                in_flight.lock().await.remove(&task_id);
                continue;
            }
            Ok(false) => {} // newly recorded — proceed
            Err(e) => {
                // DB hiccup. Fail safe: reject (better duplicate than admitted replay).
                warn!(
                    "[xelixir_router] nonce store error for {}: {} — rejecting",
                    signed.envelope.nonce, e
                );
                let _ = ack_relay(&relay_url, &task_id, json!({"success": false, "error":"nonce store unavailable"})).await;
                in_flight.lock().await.remove(&task_id);
                continue;
            }
        }

        // Hand off each verified task to a worker so the poll loop is
        // never blocked by a long-running ops verb.
        let state_for_worker = Arc::clone(state);
        let relay_for_worker = relay_url.clone();
        let in_flight_for_worker = Arc::clone(in_flight);
        let task_id_for_worker = task_id.clone();
        tokio::spawn(async move {
            let result = execute_envelope(&state_for_worker, &signed.envelope).await;
            match &result {
                Ok(_) => info!(
                    "[xelixir_router] executed '{}' from {} (task={}) ok",
                    signed.envelope.command, signed.signer_uuid, task_id_for_worker
                ),
                Err(e) => warn!(
                    "[xelixir_router] local exec failed for task {}: {}",
                    task_id_for_worker, e
                ),
            };
            let ack_body = match result {
                Ok(v) => json!({"success": true, "output": v}),
                Err(e) => json!({"success": false, "error": e}),
            };
            let _ = ack_relay(&relay_for_worker, &task_id_for_worker, ack_body).await;
            in_flight_for_worker.lock().await.remove(&task_id_for_worker);
        });
    }
    Ok(next_hint)
}

/// Dispatch a verified envelope locally. Handles legacy `start`/`stop`
/// (AgentController) and the new `ops.<verb>` family (HTTP-proxy to the
/// local `/X/ops/*` endpoint).
async fn execute_envelope(
    state: &Arc<AppState>,
    env: &eck_core::xelixir::envelope::CommandEnvelope,
) -> Result<Value, String> {
    if env.command.starts_with("ops.") {
        let verb = env.command.trim_start_matches("ops.").to_string();
        execute_ops_locally(&verb, env.args.clone().unwrap_or(json!({}))).await
    } else {
        execute_local(state, &env.command).await
    }
}

/// Forward an `ops.<verb>` to the local `/X/ops/<verb>` HTTP endpoint
/// using the in-process service token. For verbs that return a `task_id`
/// (async), poll `/X/ops/task/<task_id>` until completion. The final
/// payload is what we send back in the relay ack.
async fn execute_ops_locally(verb: &str, args: Value) -> Result<Value, String> {
    let svc_token = std::env::var("XELIXIR_SERVICE_TOKEN")
        .map_err(|_| "XELIXIR_SERVICE_TOKEN not set on this node".to_string())?;
    let local_base = std::env::var("WMS_LOCAL_BASE")
        .unwrap_or_else(|_| "http://127.0.0.1:3210".to_string());
    let client = http_client(15);

    // Choose method by verb. Read-only verbs are GET with query string;
    // mutating / structured verbs are POST with JSON body. We honour the
    // arg shape: object → POST JSON, object with `_method:"GET"` → GET.
    let (method_is_post, query) = decide_method(verb, &args);
    let url = format!("{}/X/ops/{}", local_base, verb);

    let mut req = if method_is_post {
        client.post(&url).json(&args)
    } else {
        // Lower-case key path: assemble query string from args object.
        let mut url_with_qs = url.clone();
        if let Some(map) = args.as_object() {
            let qs: String = map
                .iter()
                .filter(|(k, _)| *k != "_method")
                .filter_map(|(k, v)| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => b.to_string(),
                        Value::Number(n) => n.to_string(),
                        Value::Null => return None,
                        _ => v.to_string(),
                    };
                    Some(format!(
                        "{}={}",
                        urlencoding_encode(k),
                        urlencoding_encode(&s)
                    ))
                })
                .collect::<Vec<_>>()
                .join("&");
            if !qs.is_empty() {
                url_with_qs.push('?');
                url_with_qs.push_str(&qs);
            }
        }
        let _ = query;
        client.get(&url_with_qs)
    };
    req = req.header("X-Xelixir-Service-Token", svc_token.as_str());

    let resp = req
        .send()
        .await
        .map_err(|e| format!("local /X/ops dispatch: {}", e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("local /X/ops body parse: {}", e))?;
    if !status.is_success() {
        return Err(format!("local /X/ops HTTP {}: {}", status, body));
    }

    // Async verbs return a task_id — poll until completion.
    if let Some(task_id) = body.get("task_id").and_then(|v| v.as_str()) {
        let task_id = task_id.to_string();
        let deadline = Instant::now() + Duration::from_secs(900); // 15 min cap
        let url = format!("{}/X/ops/task/{}", local_base, task_id);
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if Instant::now() > deadline {
                return Err(format!("local task {} timed out after 15 min", task_id));
            }
            let r = client
                .get(&url)
                .header("X-Xelixir-Service-Token", svc_token.as_str())
                .send()
                .await
                .map_err(|e| format!("poll local task: {}", e))?;
            if !r.status().is_success() {
                return Err(format!("poll local task HTTP {}", r.status()));
            }
            let task_body: Value = r
                .json()
                .await
                .map_err(|e| format!("poll task body parse: {}", e))?;
            let state_s = task_body
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            if state_s != "running" {
                return Ok(task_body);
            }
        }
    } else {
        Ok(body)
    }
}

fn decide_method(verb: &str, args: &Value) -> (bool, ()) {
    // POST verbs (mutating or structured args).
    const POST_VERBS: &[&str] = &[
        "surrealql_read",
        "surrealql_write",
        "restart_service",
        "git_pull",
        "cargo_build",
        "deploy",
        "nginx_test_reload",
        "package_install",
        "file_write",
    ];
    if POST_VERBS.contains(&verb) {
        return (true, ());
    }
    // Allow caller override.
    if let Some(m) = args.get("_method").and_then(|v| v.as_str()) {
        return (m.eq_ignore_ascii_case("POST"), ());
    }
    (false, ())
}

fn urlencoding_encode(s: &str) -> String {
    // Minimal percent-encoding for query strings.
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Run the verified command against the local AgentController and update
/// the local self-row state. Used by both the poller and `/X/self/*`.
pub async fn execute_local(state: &Arc<AppState>, command: &str) -> Result<Value, String> {
    match command {
        "start" => {
            let token = state.agent_controller.approve().await?;
            let url = format!(
                "{}?token={}",
                std::env::var("XELTH_SESSION_BASE")
                    .unwrap_or_else(|_| "".into()),
                token
            );
            Ok(json!({
                "xelixir_status": "running",
                "xelixir_session_url": url,
            }))
        }
        "stop" => {
            state.agent_controller.stop_agent().await;
            Ok(json!({ "xelixir_status": "stopped" }))
        }
        other => Err(format!("unknown command '{}'", other)),
    }
}

// ─── transport helpers ────────────────────────────────────────────────────

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

async fn resolve_base_url(relay_url: &str, target_uuid: &str) -> Option<String> {
    let url = format!("{}/E/resolve/{}", relay_url, target_uuid);
    let client = http_client(DIRECT_POST_TIMEOUT_SECS);
    let mut req = client.get(&url);
    // When relay's /E/resolve enforces RELAY_RESOLVE_TOKEN, send our local
    // copy as bearer. Choosing XELIXIR_SERVICE_TOKEN because this caller
    // exists strictly for xelixir cross-mesh dispatch — if we have that
    // token, we're trusted to look up UUIDs. SYNC_SECRET would work too
    // but it's mesh-specific and this is a mesh-agnostic call.
    if let Ok(tok) = std::env::var("XELIXIR_SERVICE_TOKEN") {
        if !tok.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {}", tok));
        }
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body.get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_end_matches('/').to_string())
}

async fn try_direct_post(
    target_base: &str,
    command: &str,
    signed: &SignedEnvelope,
) -> Result<Value, String> {
    let url = format!("{}/X/self/{}", target_base.trim_end_matches('/'), command);
    let client = http_client(DIRECT_POST_TIMEOUT_SECS);
    let resp = client
        .post(&url)
        .json(signed)
        .send()
        .await
        .map_err(|e| format!("send: {}", e))?;
    let status = resp.status();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    // Guard against SPA-fallback HTML responses sneaking through as
    // "success". The receiving endpoint must explicitly return JSON.
    if !ct.starts_with("application/json") {
        return Err(format!(
            "non-JSON content-type '{}' from {} (likely SPA fallback — endpoint not mounted)",
            ct, url
        ));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("parse JSON from {}: {}", url, e))?;
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status, body));
    }
    Ok(body)
}

async fn queue_via_relay(
    relay_url: &str,
    target_uuid: &str,
    signed: &SignedEnvelope,
) -> Result<String, String> {
    let url = format!("{}/E/x/dispatch/{}", relay_url, target_uuid);
    let client = http_client(DIRECT_POST_TIMEOUT_SECS);
    let resp = client
        .post(&url)
        .json(signed)
        .send()
        .await
        .map_err(|e| format!("relay dispatch: {}", e))?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(format!("relay dispatch failed: {}", txt));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("relay dispatch body parse: {}", e))?;
    let task_id = body
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or("relay dispatch response missing task_id")?
        .to_string();
    info!("[xelixir_router] queued via relay: task={}", task_id);
    Ok(task_id)
}

async fn ack_relay(relay_url: &str, task_id: &str, body: Value) -> Result<(), String> {
    let url = format!("{}/E/x/ack/{}", relay_url, task_id);
    let client = http_client(DIRECT_POST_TIMEOUT_SECS);
    client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ack send: {}", e))?;
    Ok(())
}

async fn prune_nonces_db(db: &eck_core::db::SurrealDb) {
    let _ = db
        .query("DELETE xelixir_nonce WHERE expires_at < time::now()")
        .await;
}

/// Returns `Ok(true)` when the nonce already exists and has not yet expired
/// (replay); `Ok(false)` when it was newly inserted; `Err` on DB error.
///
/// Two-step (SELECT then INSERT) — there's a tiny race window if the same
/// nonce arrives concurrently on two pollers, but the xelixir poller is
/// single-task per WMS instance so this can only happen across the network
/// (two WMS instances on the same mesh receiving the same envelope). For
/// that case the second writer hits a unique-id collision and the worst
/// outcome is one duplicate task — acceptable.
async fn nonce_seen_and_record(
    db: &eck_core::db::SurrealDb,
    nonce: &str,
    ttl_secs: u64,
) -> Result<bool, String> {
    let existing: Vec<Value> = db
        .query(
            "SELECT nonce FROM xelixir_nonce \
             WHERE nonce = $n AND expires_at > time::now() LIMIT 1",
        )
        .bind(("n", nonce.to_string()))
        .await
        .map_err(|e| e.to_string())?
        .take(0)
        .map_err(|e| e.to_string())?;

    if !existing.is_empty() {
        return Ok(true);
    }

    db.query(
        "INSERT INTO xelixir_nonce { \
            nonce: $n, \
            expires_at: time::now() + type::duration($ttl) \
         }",
    )
    .bind(("n", nonce.to_string()))
    .bind(("ttl", format!("{}s", ttl_secs)))
    .await
    .map_err(|e| e.to_string())?;
    Ok(false)
}
