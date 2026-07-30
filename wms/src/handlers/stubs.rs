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

/// GET /api/delivery/shipments — reads the meshed `stock_picking_delivery` model
/// (works on every node, not just the scraper node). The dashboard map parses a
/// few keys out of `raw_response`, so we reconstruct that object from the
/// distilled fields — the SPA can't tell it from the original scraper blob.
pub async fn list_shipments(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let rows: Vec<Value> = state.db
        .query(
            "SELECT record::id(id) AS id, tracking_number, status, provider, \
                    recipient_name, recipient_street, recipient_city, recipient_zip, \
                    recipient_country, pickup_name, pickup_street, pickup_city, \
                    pickup_zip, pickup_country, description, reference, weight, \
                    dimensions, status_date, created_ext, delivery_date, updated_at \
             FROM stock_picking_delivery ORDER BY updated_at DESC LIMIT 200",
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .unwrap_or_default();

    let get = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let city = get(r, "recipient_city");
            // The PDA receiving flow reads recipient_*/delivery_* (with fallbacks),
            // plus product/description for what the parcel is. This is the operator
            // surface — recipient PII is intentionally in clear here, unlike the
            // ops/brain plane where stock_picking_delivery is Zone-1-denied.
            let raw = json!({
                "recipient_name": get(r, "recipient_name"),
                "recipient_street": get(r, "recipient_street"),
                "recipient_zip": get(r, "recipient_zip"),
                "recipient_country": get(r, "recipient_country"),
                "recipient_city": city,
                "delivery_city": city,
                "pickup_name": get(r, "pickup_name"),
                "pickup_street": get(r, "pickup_street"),
                "pickup_zip": get(r, "pickup_zip"),
                "pickup_city": get(r, "pickup_city"),
                "pickup_country": get(r, "pickup_country"),
                "product": get(r, "description"),
                "description": get(r, "description"),
                "reference": get(r, "reference"),
                "weight": get(r, "weight"),
                "dimensions": get(r, "dimensions"),
                "status_date": get(r, "status_date"),
                "created_at": get(r, "created_ext"),
                "delivery_date": get(r, "delivery_date"),
            });
            json!({
                "id": r.get("id").cloned().unwrap_or(Value::Null),
                "tracking_number": r.get("tracking_number").cloned().unwrap_or(Value::Null),
                "status": r.get("status").cloned().unwrap_or(Value::Null),
                "provider": r.get("provider").cloned().unwrap_or(Value::Null),
                "updated_at": r.get("updated_at").cloned().unwrap_or(Value::Null),
                "raw_response": serde_json::to_string(&raw).unwrap_or_default(),
            })
        })
        .collect();
    Ok(Json(out))
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
