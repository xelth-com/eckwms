//! Relay-routed mesh sync queue — NAT-traversal fallback for direct P2P pull/push.
//!
//! Two NAT'd peers cannot dial each other; the public relay holds a per-target
//! request-response queue. Same dumb-pipe semantics as `xelixir.rs` (the relay
//! never inspects payloads — that's the requester/responder's job), but lives
//! in its own table `mesh_task` so xelixir control and data-sync queues stay
//! decoupled and can be GC'd / rate-limited independently.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};

use crate::db::RelayDb;

// ─── POST /E/m/dispatch/:target_uuid ───────────────────────────────────────
// Queue a mesh request for the target. The body shape is up to the WMS-side
// caller; the relay only enforces that `envelope.target_uuid` matches the path
// so an envelope can't be misrouted by tweaking the URL.

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

    let kind = payload
        .get("envelope")
        .and_then(|e| e.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender = payload
        .get("envelope")
        .and_then(|e| e.get("sender_uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Payload-relay policy gate (RELAY_PAYLOAD_MODE):
    //   open     — default; passthrough for everyone (our own internal mesh / dev).
    //   disabled — discovery-only board (the free public 9eck.com node).
    //   paid     — paid feature: allowed only when >=1 party is paid, so a paid
    //              client exchanging with a free partner still "just works".
    enforce_payload_policy(&db, &sender, &target_uuid).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let task_id = uuid::Uuid::new_v4().to_string();

    let res = db
        .query(
            "CREATE type::record('mesh_task', $tid) SET \
                target_uuid = $tu, \
                sender_uuid = $su, \
                kind = $k, \
                payload = $p, \
                created_at = $now;",
        )
        .bind(("tid", task_id.clone()))
        .bind(("tu", target_uuid.clone()))
        .bind(("su", sender.clone()))
        .bind(("k", kind.clone()))
        .bind(("p", payload))
        .bind(("now", now.clone()))
        .await;

    if let Err(e) = res {
        tracing::error!("Mesh dispatch insert failed: {e}");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    tracing::info!(
        "Mesh dispatch queued: task={} kind={} target={} sender={}",
        task_id,
        kind,
        target_uuid,
        sender
    );
    Ok(Json(json!({ "task_id": task_id, "queued_at": now })))
}

/// Enforce `RELAY_PAYLOAD_MODE` at dispatch time. Only dispatch is gated;
/// poll/ack/result inherit — if a task exists for you, it was already authorized.
async fn enforce_payload_policy(
    db: &RelayDb,
    sender: &str,
    target: &str,
) -> Result<(), (StatusCode, String)> {
    let mode = std::env::var("RELAY_PAYLOAD_MODE").unwrap_or_else(|_| "open".to_string());
    match mode.as_str() {
        "open" => Ok(()),
        "disabled" => Err((
            StatusCode::FORBIDDEN,
            "payload relay disabled on this node (discovery-only board)".into(),
        )),
        "paid" => {
            if is_paid(db, sender).await || is_paid(db, target).await {
                Ok(())
            } else {
                Err((
                    StatusCode::PAYMENT_REQUIRED,
                    "payload relay requires a paid party (paid<->paid or paid<->free)".into(),
                ))
            }
        }
        other => {
            tracing::warn!("unknown RELAY_PAYLOAD_MODE '{}', defaulting to open", other);
            Ok(())
        }
    }
}

/// Read the `paid` flag a node earned at registration time (set by the license
/// check in `register`). Tolerant: missing row / column ⇒ not paid.
async fn is_paid(db: &RelayDb, instance_id: &str) -> bool {
    let rows: Vec<Value> = match db
        .query("SELECT paid FROM registration WHERE instance_id = $iid LIMIT 1")
        .bind(("iid", instance_id.to_string()))
        .await
    {
        Ok(mut r) => r.take(0).unwrap_or_default(),
        Err(e) => {
            tracing::error!("paid lookup failed for {instance_id}: {e}");
            return false;
        }
    };
    rows.first()
        .and_then(|r| r.get("paid"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

// ─── GET /E/m/poll/:self_uuid ──────────────────────────────────────────────
// Target node pulls pending mesh tasks addressed to it. Acked tasks linger
// briefly (so the sender can read the result body via /E/m/result/...) but
// are filtered out here so they aren't re-delivered.

const POLL_INTERVAL_BUSY_SECS: u64 = 3;
const POLL_INTERVAL_IDLE_SECS: u64 = 30;

pub async fn poll(
    State(db): State<RelayDb>,
    Path(self_uuid): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, target_uuid, sender_uuid, kind, payload, \
                    type::string(created_at) AS created_at \
             FROM mesh_task \
             WHERE target_uuid = $tu AND acked = NONE \
             ORDER BY created_at ASC",
        )
        .bind(("tu", self_uuid))
        .await
        .map_err(|e| {
            tracing::error!("Mesh poll failed: {e}");
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

// ─── POST /E/m/ack/:task_id ────────────────────────────────────────────────
// Target stores the result body for the dispatcher to retrieve. Idempotent —
// the GC will drop the row later.

pub async fn ack(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, StatusCode> {
    let result = body.map(|Json(v)| v).unwrap_or(json!({}));
    let now = chrono::Utc::now().to_rfc3339();

    let res = db
        .query(
            "UPDATE type::record('mesh_task', $tid) SET \
                acked = true, \
                result = $r, \
                acked_at = $now",
        )
        .bind(("tid", task_id.clone()))
        .bind(("r", result))
        .bind(("now", now))
        .await;

    if let Err(e) = res {
        tracing::error!("Mesh ack update failed: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(json!({ "ok": true, "task_id": task_id })))
}

// ─── GET /E/m/result/:task_id ──────────────────────────────────────────────
// Dispatcher polls until the target acks. Identical contract to /E/x/result.

pub async fn result(
    State(db): State<RelayDb>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let row: Option<Value> = db
        .query(
            "SELECT acked, result, \
                    type::string(created_at) AS created_at, \
                    type::string(acked_at) AS acked_at \
             FROM type::record('mesh_task', $tid) \
             LIMIT 1",
        )
        .bind(("tid", task_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Mesh result fetch failed: {e}");
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
        Ok(Json(json!({
            "status": "completed",
            "result": row.get("result").cloned().unwrap_or(Value::Null),
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
