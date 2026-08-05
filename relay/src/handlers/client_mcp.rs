//! Relay-carried MCP channel (`/E/c/*`) — the PAID-subscription gate.
//!
//! Unlike the blind `/E/x/*` xelixir channel, `c_dispatch` is NOT a dumb pipe:
//! it admits a request only if it carries a valid, authority-signed
//! `SubscriptionCert` (`ECK_SUB_ROOT_PUBKEY`). Free clients run open-source
//! code, so this signature — produced only by the offline subscription root —
//! is the one thing they cannot forge. The relay is the enforcement point we
//! control; the check lives here, never in shipped client code.
//!
//! The relay stays blind to CONTENT: it validates the cert + the client's
//! signature over the request, then stores the opaque `mcp` body for the node
//! to pull. poll/ack/result mirror the xelixir channel on a separate
//! `client_mcp_task` table so a gated MCP request can never be confused with a
//! free ops envelope.

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

use eck_core::xelixir::envelope::DEFAULT_MAX_AGE_SECS;
use eck_core::xelixir::subscription::{
    read_sub_root_from_env, SignedClientMcp, DEFAULT_GRACE_SECS,
};

use crate::db::RelayDb;

/// With long-polling the hold does the pacing — the node may re-poll
/// immediately (old pollers clamp this to their [1,60] window anyway).
const POLL_INTERVAL_SECS: u64 = 1;
/// How long an empty poll is HELD waiting for a task before returning empty.
/// MUST stay comfortably under the fleet's poll client timeout (old WMS
/// pollers use 15 s), including the DB query latency on a slow relay box.
const LONG_POLL_HOLD_SECS: u64 = 8;
/// Fallback re-check cadence inside the hold (the dispatch Notify normally
/// wakes the hold instantly; this covers a notify lost to a race).
const LONG_POLL_CHECK_SECS: u64 = 2;
/// A delivered-but-unacked task is re-delivered after this long (node crashed
/// mid-execution). Before it elapses the task is INVISIBLE to poll — that, not
/// the node's transient in-flight set, is what prevents double execution.
const REDELIVER_AFTER_SECS: i64 = 60;

/// Per-target wakeups: dispatch() rings the bell, a held poll() re-checks
/// immediately instead of on the 2 s cadence. Process-local by design — the
/// client dispatches to the SAME relay the node polls.
fn task_notify(target: &str) -> Arc<Notify> {
    static NOTIFIES: OnceLock<std::sync::Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    let map = NOTIFIES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|p| p.into_inner());
    guard.entry(target.to_string()).or_default().clone()
}

// ─── POST /E/c/dispatch/:target_uuid ───────────────────────────────────────
// Admit a subscriber's MCP request for a NAT'd node, or 403. The gate.

pub async fn dispatch(
    State(db): State<RelayDb>,
    Path(target_uuid): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Fail closed: no configured subscription root ⇒ the channel is off, and no
    // cert could verify anyway. Never silently downgrade to an open pipe.
    let root = read_sub_root_from_env().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "relay MCP channel disabled (ECK_SUB_ROOT_PUBKEY unset)".into(),
    ))?;

    let signed: SignedClientMcp = serde_json::from_value(payload).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("malformed SignedClientMcp: {e}"),
        )
    })?;

    let now = chrono::Utc::now().timestamp();
    let cert = signed
        .admit(
            &root,
            &target_uuid,
            now,
            DEFAULT_MAX_AGE_SECS,
            DEFAULT_GRACE_SECS,
        )
        .map_err(|e| {
            // One line so a probing free client shows up in the relay log, but
            // don't leak which check failed to the caller.
            tracing::warn!(
                "client-mcp dispatch refused: target={} signer={} err={}",
                target_uuid,
                signed.signer_pubkey,
                e
            );
            (StatusCode::FORBIDDEN, "subscription not authorized".into())
        })?;

    // Mid-period revocation (ROADMAP "Revocation list distributed to relays"):
    // the CRL is signed by the LICENSE issuer (ECK_LICENSE_PUBKEY — the
    // relay's product-license trust anchor), deliberately not the sub root —
    // one revocation authority for the whole fleet. Matched on the cert's
    // `subject` label OR its `client_pubkey`. Same opaque 403 as a bad cert.
    if let Ok(lic_pub) = std::env::var("ECK_LICENSE_PUBKEY") {
        if let Some(crl) = eck_core::licensing::load_revocations(&lic_pub) {
            if crl.revokes_cert(&cert.subject, &cert.client_pubkey) {
                tracing::warn!(
                    "client-mcp dispatch refused: cert subject='{}' REVOKED (crl updated {})",
                    cert.subject,
                    crl.updated
                );
                return Err((StatusCode::FORBIDDEN, "subscription not authorized".into()));
            }
        }
    }

    // Store the whole signed request (cert included) so the NODE re-verifies
    // and derives its own tier — a compromised relay can't forge access or
    // escalate to master by lying about the tier.
    let now_rfc = chrono::Utc::now().to_rfc3339();
    let task_id = uuid::Uuid::new_v4().to_string();
    let stored = serde_json::to_value(&signed).unwrap_or(Value::Null);

    let res = db
        .query(
            "CREATE type::record('client_mcp_task', $tid) SET \
                target_uuid = $tu, \
                payload = $p, \
                created_at = $now;",
        )
        .bind(("tid", task_id.clone()))
        .bind(("tu", target_uuid.clone()))
        .bind(("p", stored))
        .bind(("now", now_rfc.clone()))
        .await;

    if let Err(e) = res {
        tracing::error!("client-mcp dispatch insert failed: {e}");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    tracing::info!(
        "client-mcp queued: task={} target={} plan={} tier={}",
        task_id,
        target_uuid,
        cert.plan,
        if cert.grants_master() { "master" } else { "agent" }
    );
    // Wake a long-poll held for this node so pickup is ~instant.
    task_notify(&target_uuid).notify_waiters();
    Ok(Json(json!({ "task_id": task_id, "queued_at": now_rfc })))
}

// ─── GET /E/c/poll/:self_uuid ──────────────────────────────────────────────
// The node pulls pending MCP requests addressed to it. Same shape as the
// xelixir poll; the node re-verifies each `payload` before executing.

/// Claim the node's pending tasks: return every un-acked task that is either
/// never-delivered or stale-delivered (crash re-delivery), and stamp
/// `delivered_at` on each so subsequent polls DON'T see it again while the
/// node executes. This closes the observed double-serve → the replayed run's
/// 403 ack clobbering the good result.
async fn claim_pending(db: &RelayDb, self_uuid: &str) -> Result<Vec<Value>, StatusCode> {
    let stale = (chrono::Utc::now() - chrono::Duration::seconds(REDELIVER_AFTER_SECS)).to_rfc3339();
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, target_uuid, payload, \
                    type::string(created_at) AS created_at \
             FROM client_mcp_task \
             WHERE target_uuid = $tu AND acked = NONE \
               AND (delivered_at = NONE OR delivered_at < $stale) \
             ORDER BY created_at ASC",
        )
        .bind(("tu", self_uuid.to_string()))
        .bind(("stale", stale))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp poll failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    if !rows.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        for row in &rows {
            if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                if let Err(e) = db
                    .query("UPDATE type::record('client_mcp_task', $tid) SET delivered_at = $now")
                    .bind(("tid", id.to_string()))
                    .bind(("now", now.clone()))
                    .await
                {
                    tracing::warn!("client-mcp delivered_at stamp failed for {id}: {e}");
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
    // LONG-POLL: hold an empty poll so a task dispatched mid-hold is handed
    // over ~instantly (dispatch rings `task_notify`). Total hold stays under
    // the fleet's 15 s poll client timeout.
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

// ─── POST /E/c/ack/:task_id ────────────────────────────────────────────────
// The node posts the MCP result body. Kept so the client can read it.

pub async fn ack(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let result = body.map(|Json(v)| v).unwrap_or(json!({}));
    let now = chrono::Utc::now().to_rfc3339();

    // FIRST ACK WINS: a re-delivered task's second run fails the node's nonce
    // guard and acks a 403 — that must never overwrite the real result the
    // first run already stored.
    let res = db
        .query(
            "UPDATE type::record('client_mcp_task', $tid) SET \
                acked = true, \
                result = $r, \
                acked_at = $now \
             WHERE acked = NONE",
        )
        .bind(("tid", task_id.clone()))
        .bind(("r", result))
        .bind(("now", now))
        .await;

    if let Err(e) = res {
        tracing::error!("client-mcp ack update failed: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(json!({ "ok": true, "task_id": task_id })))
}

// ─── GET /E/c/result/:task_id ──────────────────────────────────────────────
// The subscriber's client polls this until the node acks.

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
             FROM type::record('client_mcp_task', $tid) \
             LIMIT 1",
        )
        .bind(("tid", task_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("client-mcp result fetch failed: {e}");
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
        // The node acks both successful MCP results and rejections (re-verify
        // failure, unknown tool). MCP tool errors carry `isError`; transport
        // rejections carry `error`. Treat either as not-success.
        let succeeded = !result.get("error").is_some()
            && !result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        Ok(Json(json!({
            "status": if succeeded { "completed" } else { "failed" },
            "success": succeeded,
            "result": result,
            "created_at": row.get("created_at"),
            "acked_at": row.get("acked_at"),
        })))
    } else {
        // `delivered` lets the waiting client tell a queued-but-busy node (the
        // node polled the task) from an absent one (never picked up) when its
        // ack window elapses — a busy node earns a structured retry, not a
        // generic timeout.
        Ok(Json(json!({
            "status": "pending",
            "delivered": row.get("delivered").and_then(|v| v.as_bool()).unwrap_or(false),
            "created_at": row.get("created_at"),
        })))
    }
}
