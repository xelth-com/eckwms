//! Cross-mesh xelixir control-plane routing on the eck relay.
//!
//! The relay is intentionally a dumb pipe: it stores `xelixir_task` rows
//! (signed envelopes) for offline / NAT'd targets, and serves a global
//! UUID resolver so any caller can find any node regardless of its
//! `mesh_id`. Signature verification happens **only on the target**,
//! never here.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};

use crate::db::RelayDb;

// ─── GET /E/resolve/:instance_id ───────────────────────────────────────────
// Mesh-agnostic UUID → location resolver. Returns the most recently-seen
// `online` row for this instance_id across all meshes.
//
// Optional bearer auth: when `RELAY_RESOLVE_TOKEN` is set in the relay's env,
// callers must supply `Authorization: Bearer <token>` matching it. When the
// env var is unset, access is open (back-compat default). The token defends
// against UUID→IP recon when a UUID accidentally leaks (logs, screenshots).

pub async fn resolve(
    State(db): State<RelayDb>,
    headers: axum::http::HeaderMap,
    Path(instance_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    if let Ok(expected) = std::env::var("RELAY_RESOLVE_TOKEN") {
        if !expected.trim().is_empty() {
            let presented = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .unwrap_or("");
            // Constant-time compare to avoid timing leaks on token length.
            if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    }

    let row: Option<Value> = db
        .query(
            "SELECT record::id(id) AS id, instance_id, external_ip, port, status, \
                    type::string(last_seen) AS last_seen, base_url, mesh_id \
             FROM registration \
             WHERE instance_id = $iid \
             ORDER BY last_seen DESC \
             LIMIT 1",
        )
        .bind(("iid", instance_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Resolve failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    row.map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// Length-checked constant-time byte compare. Returns true only when both
/// slices are non-empty and identical in length + content.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

// ─── POST /E/x/dispatch/:target_uuid ───────────────────────────────────────
// Store a signed envelope for the target. Target polls and pulls.
// We do NOT verify signatures here; the relay is just a queue.

pub async fn dispatch(
    State(db): State<RelayDb>,
    Path(target_uuid): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Sanity: payload must look like a SignedEnvelope (envelope.target_uuid == path).
    let envelope_target = payload
        .get("envelope")
        .and_then(|e| e.get("target_uuid"))
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_REQUEST, "missing envelope.target_uuid".into()))?;
    if envelope_target != target_uuid {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "path target_uuid '{}' != envelope.target_uuid '{}'",
                target_uuid, envelope_target
            ),
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let task_id = uuid::Uuid::new_v4().to_string();

    let res = db
        .query(
            "CREATE type::record('xelixir_task', $tid) SET \
                target_uuid = $tu, \
                payload = $p, \
                created_at = $now;",
        )
        .bind(("tid", task_id.clone()))
        .bind(("tu", target_uuid.clone()))
        .bind(("p", payload))
        .bind(("now", now.clone()))
        .await;

    if let Err(e) = res {
        tracing::error!("Dispatch insert failed: {e}");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    tracing::info!("Xelixir dispatch queued: task={} target={}", task_id, target_uuid);
    Ok(Json(json!({ "task_id": task_id, "queued_at": now })))
}

// ─── GET /E/x/poll/:self_uuid ──────────────────────────────────────────────
// Target node pulls pending envelopes addressed to it. Response carries an
// adaptive `next_poll_in_seconds` hint so the poller speeds up when there
// is work and slows down when idle. Only pending (un-acked) tasks are
// returned — acked tasks live on with their result body for a short window
// so the dispatcher can read it.

const POLL_INTERVAL_BUSY_SECS: u64 = 3;
const POLL_INTERVAL_IDLE_SECS: u64 = 30;

pub async fn poll(
    State(db): State<RelayDb>,
    Path(self_uuid): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, target_uuid, payload, \
                    type::string(created_at) AS created_at \
             FROM xelixir_task \
             WHERE target_uuid = $tu AND acked = NONE \
             ORDER BY created_at ASC",
        )
        .bind(("tu", self_uuid))
        .await
        .map_err(|e| {
            tracing::error!("Xelixir poll failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    let n = rows.len();
    let next_poll = if n == 0 {
        POLL_INTERVAL_IDLE_SECS
    } else {
        POLL_INTERVAL_BUSY_SECS
    };

    Ok(Json(json!({
        "tasks": rows,
        "next_poll_in_seconds": next_poll,
    })))
}

// ─── POST /E/x/ack/:task_id ────────────────────────────────────────────────
// Target acks with an optional JSON result body. The relay keeps the row
// (acked=true, result=<body>) so the cloud-side dispatcher can read the
// outcome via `GET /E/x/result/:task_id`. Garbage-collected on a schedule.

pub async fn ack(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let result = body.map(|Json(v)| v).unwrap_or(json!({}));
    let now = chrono::Utc::now().to_rfc3339();

    let res = db
        .query(
            "UPDATE type::record('xelixir_task', $tid) SET \
                acked = true, \
                result = $r, \
                acked_at = $now",
        )
        .bind(("tid", task_id.clone()))
        .bind(("r", result))
        .bind(("now", now))
        .await;

    if let Err(e) = res {
        tracing::error!("Xelixir ack update failed: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(json!({ "ok": true, "task_id": task_id })))
}

// ─── GET /E/x/result/:task_id ──────────────────────────────────────────────
// Cloud-side dispatcher polls this until the target acks. Returns
// `{ status: "pending" }` while the task is queued, `{ status:
// "completed", result: <body> }` once the target has ack'd.

pub async fn result(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let row: Option<Value> = db
        .query(
            "SELECT acked, result, \
                    type::string(created_at) AS created_at, \
                    type::string(acked_at) AS acked_at \
             FROM type::record('xelixir_task', $tid) \
             LIMIT 1",
        )
        .bind(("tid", task_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Xelixir result fetch failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    let row = match row {
        Some(r) => r,
        None => return Err(StatusCode::NOT_FOUND),
    };
    let acked = row
        .get("acked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if acked {
        let result = row.get("result").cloned().unwrap_or(Value::Null);
        // "acked" is not "succeeded": the target acks both successful runs and
        // rejections (bad signer, expired envelope, duplicate nonce). Surface
        // the distinction so a poller can't mistake a rejected/errored task for
        // success. Fall back to "did the ack carry an error?" for legacy ack
        // bodies that predate the explicit `success` field.
        let succeeded = result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| result.get("error").is_none());
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
            "created_at": row.get("created_at"),
        })))
    }
}
