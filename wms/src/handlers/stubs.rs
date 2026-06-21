//! Stub and recently-wired handlers for frontend endpoints not yet fully ported.
//! Returns valid responses so the UI doesn't crash.

use axum::{extract::{Path, State}, http::StatusCode, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

/// GET /api/odoo/pickings — stub
pub async fn odoo_pickings() -> Json<Vec<Value>> {
    Json(vec![])
}

/// GET /api/delivery/shipments — Real implementation (fetches from shipment table)
pub async fn list_shipments(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let shipments: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, tracking_number, status, raw_response, provider, updated_at FROM shipment ORDER BY updated_at DESC LIMIT 100")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();
    Ok(Json(shipments))
}

/// GET /api/delivery/config — stub
pub async fn delivery_config() -> Json<Value> {
    Json(json!({ "opal": true, "dhl": true, "carriers": [], "defaults": {} }))
}

/// POST /api/delivery/shipments — stub
pub async fn create_shipment(Json(_body): Json<Value>) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, Json(json!({ "error": "Manual shipment creation not yet ported" })))
}

/// POST /api/delivery/shipments/:id/cancel — stub
pub async fn cancel_shipment() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_IMPLEMENTED, Json(json!({ "error": "Shipment cancellation not yet ported" })))
}

/// POST /api/delivery/shipments/:id/resolve — force-mark a stuck shipment as delivered.
/// Used by the operator when the carrier's status feed is broken but the parcel
/// actually arrived. Bumps updated_at so SLA timers reset.
pub async fn resolve_shipment(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let now = chrono::Utc::now().to_rfc3339();
    let rid = format!("shipment:{}", id);
    let _: Vec<Value> = state.db
        .query("UPDATE type::record($rid) SET status = 'delivered', resolved_manually = true, updated_at = $now")
        .bind(("rid", rid))
        .bind(("now", now))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();
    Ok(Json(json!({ "success": true, "id": id })))
}

/// POST /api/delivery/import/opal — Triggers OPAL scraper
pub async fn import_opal(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_default();
    crate::services::scheduler::sync_opal(&state.db, &client, &state.instance_id).await;
    Ok(Json(json!({ "success": true, "message": "OPAL sync completed" })))
}

/// POST /api/delivery/import/dhl — Triggers DHL scraper
pub async fn import_dhl(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap_or_default();
    crate::services::scheduler::sync_dhl(&state.db, &client, &state.instance_id).await;
    Ok(Json(json!({ "success": true, "message": "DHL sync completed" })))
}

/// GET /api/delivery/shipments/:id/ai-match — stub
pub async fn ai_match_shipment() -> Json<Value> {
    Json(json!({ "matches": [] }))
}

/// GET /api/delivery/sync/history — Real implementation (fetches from sync_history table)
pub async fn delivery_sync_history(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let history: Vec<Value> = state.db
        .query("SELECT * FROM sync_history ORDER BY started_at DESC LIMIT 100")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(history))
}

/// GET /api/delivery/carriers — stub
pub async fn delivery_carriers() -> Json<Vec<Value>> {
    Json(vec![])
}

/// GET /api/analysis/support-dump — Real implementation
pub async fn analysis_support_dump(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let tickets: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, status, summary_status, payload.subject AS subject FROM document WHERE type = 'support_ticket' ORDER BY updated_at DESC LIMIT 100")
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    Ok(Json(json!({ "tickets": tickets })))
}
