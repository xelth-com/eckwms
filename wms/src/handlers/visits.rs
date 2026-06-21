//! Visit tasks ("надо съездить к клиенту") — check-in/check-out model.
//!
//! Legally deliberate design (VG Lüneburg / DSGVO, see .eck/PRIVACY_BY_DESIGN.md):
//! the server NEVER derives visits from movement tracks. The PDA geofences
//! locally and the WORKER confirms arrival/departure explicitly; only those
//! confirmed point-in-time events reach the server. Work time between
//! check-in and check-out doubles as ArbZG-grade time evidence.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct CreateVisitRequest {
    pub title: String,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lng: Option<f64>,
    /// ISO date (YYYY-MM-DD) the visit is due
    pub due_date: String,
    #[serde(default)]
    pub assigned_user_id: Option<String>,
    #[serde(default)]
    pub assigned_device_id: Option<String>,
    /// Optional link to an order/ticket: {entity_type, entity_id}
    #[serde(default)]
    pub target_entity_type: Option<String>,
    #[serde(default)]
    pub target_entity_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// POST /api/visits — create a visit task (dashboard or PDA)
pub async fn create_visit(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateVisitRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if body.title.is_empty() || body.due_date.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "title and due_date are required".into()));
    }

    let now = Utc::now().to_rfc3339();
    let created: Option<Value> = state
        .db
        .create("visit_task")
        .content(json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "title": body.title,
            "address": body.address,
            "lat": body.lat,
            "lng": body.lng,
            "due_date": body.due_date,
            "assigned_user_id": body.assigned_user_id,
            "assigned_device_id": body.assigned_device_id,
            "target_entity_type": body.target_entity_type,
            "target_entity_id": body.target_entity_id,
            "note": body.note,
            "status": "open",
            "created_at": now, "updated_at": now,
        }))
        .await
        .map_err(db_err)?;

    Ok((StatusCode::CREATED, Json(created.unwrap_or(Value::Null))))
}

#[derive(Deserialize)]
pub struct VisitListQuery {
    /// Visits due on/before this date (default: today)
    pub due: Option<String>,
    pub device_id: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/visits — visit tasks for the PDA's daily plan.
/// Default: everything still open with due_date <= today.
pub async fn list_visits(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VisitListQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let due = q.due.unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let limit = q.limit.unwrap_or(100).clamp(1, 500);

    let mut conditions = vec!["due_date <= $due"];
    if q.status.is_some() {
        conditions.push("status = $status");
    } else {
        conditions.push("status IN ['open', 'checked_in']");
    }
    if q.device_id.is_some() {
        conditions.push("(assigned_device_id = $dev OR assigned_device_id IS NONE)");
    }

    let query = format!(
        "SELECT record::id(id) AS id, title, address, lat, lng, due_date, status, \
         assigned_user_id, assigned_device_id, target_entity_type, target_entity_id, \
         note, checked_in_at, checked_out_at, created_at \
         FROM visit_task WHERE {} ORDER BY due_date ASC, created_at ASC LIMIT $limit",
        conditions.join(" AND ")
    );

    let mut stmt = state.db.query(&query).bind(("due", due)).bind(("limit", limit));
    if let Some(s) = q.status {
        stmt = stmt.bind(("status", s));
    }
    if let Some(d) = q.device_id {
        stmt = stmt.bind(("dev", d));
    }

    let visits: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();
    Ok(Json(visits))
}

#[derive(Deserialize)]
pub struct CheckEventRequest {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    /// Client timestamp (RFC3339) — kept distinct from server receive time
    #[serde(default)]
    pub ts: Option<String>,
    /// One-shot position at the moment of confirmation (user-initiated, optional)
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lng: Option<f64>,
    #[serde(default)]
    pub accuracy_m: Option<f64>,
    #[serde(default)]
    pub note: Option<String>,
}

async fn apply_check_event(
    state: &AppState,
    id: &str,
    body: CheckEventRequest,
    kind: &str, // "checkin" | "checkout"
) -> ApiResult<Json<Value>> {
    super::pda::require_active_device(state, &body.device_id).await?;

    let now = Utc::now().to_rfc3339();
    let event_ts = body.ts.clone().unwrap_or_else(|| now.clone());

    let (new_status, ts_field) = match kind {
        "checkin" => ("checked_in", "checked_in_at"),
        _ => ("done", "checked_out_at"),
    };

    let query = format!(
        "UPDATE type::record('visit_task', $id) SET status = $status, {} = $ts, \
         updated_at = $now, events = array::concat(events ?? [], [$event])",
        ts_field
    );

    let event = json!({
        "kind": kind,
        "ts": event_ts,
        "device_id": body.device_id,
        "user_id": body.user_id,
        "lat": body.lat,
        "lng": body.lng,
        "accuracy_m": body.accuracy_m,
        "note": body.note,
        "received_at": now,
    });

    let updated: Vec<Value> = state
        .db
        .query(&query)
        .bind(("id", id.to_string()))
        .bind(("status", new_status.to_string()))
        .bind(("ts", event_ts))
        .bind(("now", Utc::now().to_rfc3339()))
        .bind(("event", event))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    if updated.is_empty() {
        return Err((StatusCode::NOT_FOUND, format!("Visit '{id}' not found")));
    }

    info!("Visit {}: {} by device {}", id, kind, body.device_id);
    Ok(Json(json!({ "success": true, "visit": updated.first() })))
}

/// POST /api/visits/:id/checkin — worker confirmed arrival
pub async fn checkin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<CheckEventRequest>,
) -> ApiResult<Json<Value>> {
    apply_check_event(&state, &id, body, "checkin").await
}

/// POST /api/visits/:id/checkout — worker confirmed departure
pub async fn checkout(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<CheckEventRequest>,
) -> ApiResult<Json<Value>> {
    apply_check_event(&state, &id, body, "checkout").await
}
