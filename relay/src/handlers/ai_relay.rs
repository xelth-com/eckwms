//! Relay-carried AI-distribution channel (`/E/ai/*`) — NATIVE paid-mesh AI.
//!
//! Owner decision (2026-08-30): "asking glm" is a native paid-mesh service that
//! rides the relay infrastructure, not a separate HTTP port. A keyless managed
//! client posts an `ai.or.request` envelope for the serving node; the serving
//! node's `services::or_relay_poller` pulls it, runs `handlers::or_proxy::
//! serve_or_request` (the SAME logic the `POST /ai/or` HTTP path uses), and acks
//! the OpenRouter/Z.ai response back here for the client to read.
//!
//! DUMB PIPE — the relay never inspects content or credentials. Unlike the paid
//! `/E/c/*` MCP channel (which gates on an authority-signed `SubscriptionCert`),
//! this channel carries the client's paid **license** INSIDE the opaque payload
//! and it is verified END-TO-END on the serving node (exactly as the HTTP
//! `/ai/or` handler verifies `Authorization: Bearer`). The relay only enforces
//! that `envelope.target_uuid` matches the path so an envelope can't be
//! misrouted by tweaking the URL. This is why it needs its OWN table/routes and
//! could not simply reuse `/E/x/*` (see below).
//!
//! Why a NEW channel rather than an existing one (design record):
//!   * `/E/x/*` (ops) re-delivers every un-acked row on EVERY poll (no
//!     `delivered_at`) and its ack has no first-ack-wins — for a MINUTES-long AI
//!     call that means constant re-delivery and a lost-ack/redelivery can clobber
//!     the good result with a duplicate-nonce error. It also already has a
//!     consumer (the xelixir poller) that would ack-and-drop a foreign payload.
//!   * `/E/c/*` has exactly the robust lifecycle we want but HARD-gates on a
//!     `SubscriptionCert`; the managed AI credential is a paid license, a
//!     different credential — riding it would force every AI node to also hold a
//!     subscription cert and entangle the MCP serve path.
//! So this channel mirrors `/E/c/*`'s robust semantics (delivered_at stamping,
//! crash re-delivery window, first-ack-wins, long-poll) minus the cert gate.
//!
//! Lifecycle guarantees the AI use requires (all satisfied here, none on `/E/x`):
//!   * validity window is the ENVELOPE's own `expires_at` (client sets >= 10 min),
//!     checked on the serving node — NOT a 60 s signature age;
//!   * result PERSISTS across a WMS restart on either side: the acked row lingers
//!     (GC'd after 1 h), and `delivered_at` + `REDELIVER_AFTER_SECS` means a
//!     serving node that restarts mid-call re-picks the task instead of losing it;
//!   * NO duplicate-nonce failures on redelivery: first-ack-wins here + the
//!     serving poller's namespaced nonce guard with a >= window TTL.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::db::RelayDb;

/// With long-polling the hold does the pacing; the node may re-poll immediately.
const POLL_INTERVAL_SECS: u64 = 1;
/// How long an empty poll is HELD waiting for a task before returning empty.
/// Stays under the serving poller's HTTP timeout.
const LONG_POLL_HOLD_SECS: u64 = 8;
/// Fallback re-check cadence inside the hold (the dispatch Notify normally wakes
/// it instantly; this covers a notify lost to a race).
const LONG_POLL_CHECK_SECS: u64 = 2;
/// A delivered-but-unacked task is re-delivered after this long (node crashed /
/// restarted mid-call). Before it elapses the task is INVISIBLE to poll — that,
/// not the node's transient in-flight set, is what prevents double execution.
/// AI calls run minutes, so this is longer than the MCP channel's 60 s: a slow
/// glm generation must not look like a crash and get re-dispatched under it.
const REDELIVER_AFTER_SECS: i64 = 240;

/// Per-target wakeups: dispatch() rings the bell, a held poll() re-checks at
/// once. Process-local by design — the client dispatches to the SAME relay the
/// serving node polls.
fn task_notify(target: &str) -> Arc<Notify> {
    static NOTIFIES: OnceLock<std::sync::Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    let map = NOTIFIES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|p| p.into_inner());
    guard.entry(target.to_string()).or_default().clone()
}

// ─── POST /E/ai/dispatch/:target_uuid ──────────────────────────────────────
// Queue an AI request for the serving node. The relay only checks that
// `envelope.target_uuid` matches the path; the license inside is verified by
// the serving node, not here.

pub async fn dispatch(
    State(db): State<RelayDb>,
    Path(target_uuid): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let envelope_target = payload
        .get("envelope")
        .and_then(|e| e.get("target_uuid"))
        .and_then(|v| v.as_str())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "missing envelope.target_uuid".into(),
        ))?;
    if envelope_target != target_uuid {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "path target_uuid '{}' != envelope.target_uuid '{}'",
                target_uuid, envelope_target
            ),
        ));
    }

    let now_rfc = chrono::Utc::now().to_rfc3339();
    let task_id = uuid::Uuid::new_v4().to_string();

    let res = db
        .query(
            "CREATE type::record('ai_task', $tid) SET \
                target_uuid = $tu, \
                payload = $p, \
                created_at = $now;",
        )
        .bind(("tid", task_id.clone()))
        .bind(("tu", target_uuid.clone()))
        .bind(("p", payload))
        .bind(("now", now_rfc.clone()))
        .await;

    if let Err(e) = res {
        tracing::error!("ai-relay dispatch insert failed: {e}");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    tracing::info!("ai-relay queued: task={} target={}", task_id, target_uuid);
    task_notify(&target_uuid).notify_waiters();
    Ok(Json(json!({ "task_id": task_id, "queued_at": now_rfc })))
}

// ─── GET /E/ai/poll/:self_uuid ─────────────────────────────────────────────
// The serving node pulls pending AI requests addressed to it.

/// Claim the node's pending tasks: every un-acked task that is either
/// never-delivered or stale-delivered (crash re-delivery), stamping
/// `delivered_at` so subsequent polls don't re-hand it while the node runs the
/// (minutes-long) generation.
async fn claim_pending(db: &RelayDb, self_uuid: &str) -> Result<Vec<Value>, StatusCode> {
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(REDELIVER_AFTER_SECS)).to_rfc3339();
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, target_uuid, payload, \
                    type::string(created_at) AS created_at \
             FROM ai_task \
             WHERE target_uuid = $tu AND acked = NONE \
               AND (delivered_at = NONE OR delivered_at < $stale) \
             ORDER BY created_at ASC",
        )
        .bind(("tu", self_uuid.to_string()))
        .bind(("stale", stale))
        .await
        .map_err(|e| {
            tracing::error!("ai-relay poll failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    if !rows.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        for row in &rows {
            if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                if let Err(e) = db
                    .query("UPDATE type::record('ai_task', $tid) SET delivered_at = $now")
                    .bind(("tid", id.to_string()))
                    .bind(("now", now.clone()))
                    .await
                {
                    tracing::warn!("ai-relay delivered_at stamp failed for {id}: {e}");
                }
            }
        }
    }
    Ok(rows)
}

pub async fn poll(
    State(db): State<RelayDb>,
    Path(self_uuid): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let notify = task_notify(&self_uuid);
    let deadline = Instant::now() + Duration::from_secs(LONG_POLL_HOLD_SECS);
    let rows = loop {
        let rows = claim_pending(&db, &self_uuid).await?;
        if !rows.is_empty() {
            break rows;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break rows; // hold expired — return empty
        };
        let wait = remaining.min(Duration::from_secs(LONG_POLL_CHECK_SECS));
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    };

    Ok(Json(json!({
        "tasks": rows,
        "next_poll_in_seconds": POLL_INTERVAL_SECS,
    })))
}

// ─── POST /E/ai/ack/:task_id ───────────────────────────────────────────────
// The serving node posts the AI result body `{status, body}`.

pub async fn ack(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let result = body.map(|Json(v)| v).unwrap_or(json!({}));
    let now = chrono::Utc::now().to_rfc3339();

    // FIRST ACK WINS: a re-delivered task's second run would ack a duplicate —
    // that must never overwrite the real result the first run already stored.
    let res = db
        .query(
            "UPDATE type::record('ai_task', $tid) SET \
                acked = true, \
                result = $r, \
                acked_at = $now \
             WHERE acked = NONE \
             RETURN AFTER",
        )
        .bind(("tid", task_id.clone()))
        .bind(("r", result))
        .bind(("now", now))
        .await;

    match res {
        Err(e) => {
            tracing::error!("ai-relay ack update failed: {e}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        // Zero rows matched = this relay never held the task (wrong relay in a
        // multi-relay walk) OR it was already acked. Either way the caller has
        // nothing to do; report ok so a duplicate ack isn't treated as failure.
        Ok(_) => Ok(Json(json!({ "ok": true, "task_id": task_id }))),
    }
}

// ─── GET /E/ai/result/:task_id ─────────────────────────────────────────────
// The client polls this until the serving node acks. Transport-neutral: the
// same envelope/result shape would ride a future long-poll or WS burst.

pub async fn result(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let row: Option<Value> = db
        .query(
            "SELECT acked, result, \
                    (delivered_at != NONE) AS delivered, \
                    type::string(created_at) AS created_at, \
                    type::string(acked_at) AS acked_at \
             FROM type::record('ai_task', $tid) \
             LIMIT 1",
        )
        .bind(("tid", task_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("ai-relay result fetch failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    let row = match row {
        Some(r) => r,
        None => return Err(StatusCode::NOT_FOUND),
    };
    let acked = row.get("acked").and_then(|v| v.as_bool()).unwrap_or(false);
    if acked {
        let result = row.get("result").cloned().unwrap_or(Value::Null);
        // The serving node acks `{status, body}`. Success = a 2xx status and no
        // transport-level `error` marker; anything else is a failure the client
        // can escalate on (fallback ladder, §10).
        let http_ok = result
            .get("status")
            .and_then(|v| v.as_u64())
            .map(|s| (200..300).contains(&s))
            .unwrap_or(false);
        let succeeded = http_ok && result.get("error").is_none();
        Ok(Json(json!({
            "status": if succeeded { "completed" } else { "failed" },
            "success": succeeded,
            "result": result,
            "created_at": row.get("created_at"),
            "acked_at": row.get("acked_at"),
        })))
    } else {
        Ok(Json(json!({
            "status": "pending",
            "delivered": row.get("delivered").and_then(|v| v.as_bool()).unwrap_or(false),
            "created_at": row.get("created_at"),
        })))
    }
}
