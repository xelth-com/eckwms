//! Warehouse receiving — the PDA-facing HTTP glue.
//!
//! All the logic lives in `services::receiving` (testable against a `mem://`
//! DB without HTTP); this module only does extraction, device auth and the
//! JSON envelope. Mounted in the same `/api` group as `/scan`, so it inherits
//! the JWT layer, and each handler additionally enforces `require_active_device`
//! exactly like `/scan` and `/repair/event` do.
//!
//!   POST /api/receiving/receipts            — idempotent receipt + units upsert
//!   GET  /api/receiving/units?zone=&limit=  — what is lying in a zone
//!   POST /api/receiving/units/:unit_id/move — move a unit onto another place
//!   GET  /api/receiving/tickets/lookup?q=   — find the Zoho ticket behind a
//!                                             walk-in, from OCR garbage
//!
//! The ACTIVE receipt (Beleg) lane — the receiver activates one receipt on the
//! phone and everything the camera produces is attached to it, so these are
//! small point operations on a receipt that already exists:
//!
//!   GET  /api/receiving/receipts/:id        — full context (header, units with
//!                                             their live repair order, photos)
//!   POST /api/receiving/receipts/:id/units  — add one unit to it
//!   POST /api/receiving/receipts/:id/note   — append a note / OCR page
//!   POST /api/receiving/receipts/:id/repair — put a unit into repair / take out

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use eck_core::auth::Claims;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::services::{plan, receiving, ticket_lookup};
use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn bad(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

/// The receipt body plus the PDA's own `device_id` (same field name `/scan`
/// uses). The device the receipt is stamped with is the authenticated JWT
/// subject when the body omits one.
#[derive(Deserialize)]
pub struct CreateReceiptRequest {
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(flatten)]
    pub receipt: receiving::ReceiptInput,
}

/// `POST /api/receiving/receipts`
pub async fn create_receipt(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateReceiptRequest>,
) -> ApiResult<Json<Value>> {
    let device_id = body.device_id.clone().unwrap_or_else(|| claims.sub.clone());
    crate::handlers::pda::require_active_device(&state, &device_id).await?;

    let stored = receiving::upsert_receipt(
        &state.db,
        &state.instance_id,
        &device_id,
        &body.receipt,
    )
    .await
    .map_err(bad)?;

    Ok(Json(json!({
        "success": true,
        "receipt_id": stored.receipt_id,
        "intake": stored.intake,
        "zoho_ticket": stored.zoho_ticket,
        "ticket_id": stored.ticket_id,
        "units": stored.units.iter().map(|u| json!({
            "unit_id": u.unit_id,
            "code": u.code,
            "location_barcode": u.location_barcode,
        })).collect::<Vec<_>>(),
    })))
}

/// `GET /api/receiving/tickets/lookup?q=<text>&limit=20`
///
/// The walk-in lane: the receiver either scanned the device serial or pointed
/// the camera at the paper "Reparaturauftrag" and got OCR garbage. Rank the
/// support tickets against it so the receipt can carry customer + ticket.
///
/// Read-only, but in the same device-gated group as `/receiving/units` — the
/// payload carries the customer's clear contact data, which is exactly what
/// the receiver needs and exactly what an unauthenticated caller must not get.
pub async fn lookup_tickets(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let q = params.get("q").cloned().unwrap_or_default();
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20);

    let res = ticket_lookup::lookup_tickets(&state.db, &q, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(res))
}

/// `GET /api/receiving/plan?receipt=<id>` (also `?ticket=`, `?shipment=`,
/// `?serial=` for the pre-receipt moment) — the DERIVED, stateless plan the PDA
/// renders (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §8). Exactly one source is
/// honoured, in the order receipt → ticket → shipment → serial. No source
/// matched → HTTP 200 with empty `steps` and an empty subject title, NEVER 404.
///
/// Read-only, in the same device-gated group as the other `/receiving/*` reads
/// (no `require_active_device` — matches `get_receipt` / `list_units`).
pub async fn get_plan(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let pick = |k: &str| params.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let query = if let Some(r) = pick("receipt") {
        plan::PlanQuery::Receipt(r)
    } else if let Some(t) = pick("ticket") {
        plan::PlanQuery::Ticket(t)
    } else if let Some(sh) = pick("shipment") {
        plan::PlanQuery::Shipment(sh)
    } else if let Some(sn) = pick("serial") {
        plan::PlanQuery::Serial(sn)
    } else {
        return Err(bad("one of receipt|ticket|shipment|serial is required"));
    };

    let plan = plan::load_receiving_plan(&state.db, &query).await;
    Ok(Json(serde_json::to_value(plan).unwrap_or(Value::Null)))
}

/// `GET /api/receiving/units?zone=inbound|outbound|all&limit=200`
pub async fn list_units(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let zone = params.get("zone").cloned().unwrap_or_else(|| "inbound".into());
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(200);

    let units = receiving::list_units(&state.db, &zone, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!(units)))
}

#[derive(Deserialize)]
pub struct MoveUnitRequest {
    pub location_barcode: String,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// `POST /api/receiving/units/:unit_id/move`
pub async fn move_unit(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(unit_id): Path<String>,
    Json(body): Json<MoveUnitRequest>,
) -> ApiResult<Json<Value>> {
    let device_id = body.device_id.clone().unwrap_or_else(|| claims.sub.clone());
    crate::handlers::pda::require_active_device(&state, &device_id).await?;

    let res = receiving::move_unit(
        &state.db,
        &state.instance_id,
        &unit_id,
        &body.location_barcode,
    )
    .await
    .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok(Json(res))
}

// ─── The active receipt (Beleg) ──────────────────────────────────────────────
//
// Same device-gated group as `/receiving/units`: the payload carries the
// customer's clear contact data and lets the receiver change the receipt's
// properties, so a device JWT is the floor, not the ceiling.

/// The optional `device_id` every write in this lane may carry (the PDA sends
/// its own; absent = the authenticated JWT subject).
fn actor(claims: &Claims, device_id: &Option<String>) -> String {
    device_id.clone().unwrap_or_else(|| claims.sub.clone())
}

/// `GET /api/receiving/receipts/:receipt_id`
///
/// Everything the phone needs to render the active receipt in one round-trip:
/// the header as stored, its units (each with the live repair order folded in)
/// and the photos hanging off it.
pub async fn get_receipt(
    State(state): State<Arc<AppState>>,
    Path(receipt_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let ctx = receiving::get_receipt_context(&state.db, &receipt_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok(Json(ctx))
}

#[derive(Deserialize)]
pub struct AddUnitRequest {
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(flatten)]
    pub unit: receiving::UnitInput,
}

/// `POST /api/receiving/receipts/:receipt_id/units`
pub async fn add_unit(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(receipt_id): Path<String>,
    Json(body): Json<AddUnitRequest>,
) -> ApiResult<Json<Value>> {
    let device_id = actor(&claims, &body.device_id);
    crate::handlers::pda::require_active_device(&state, &device_id).await?;

    let res = receiving::add_unit(&state.db, &state.instance_id, &receipt_id, &body.unit)
        .await
        .map_err(bad)?;
    Ok(Json(res))
}

#[derive(Deserialize)]
pub struct NoteRequest {
    pub text: String,
    /// `ocr` (also appended to `ocr_text`) | `manual`. Anything else = manual.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// `POST /api/receiving/receipts/:receipt_id/note`
pub async fn append_note(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(receipt_id): Path<String>,
    Json(body): Json<NoteRequest>,
) -> ApiResult<Json<Value>> {
    let device_id = actor(&claims, &body.device_id);
    crate::handlers::pda::require_active_device(&state, &device_id).await?;

    let res = receiving::append_note(
        &state.db,
        &state.instance_id,
        &receipt_id,
        &body.text,
        body.source.as_deref().unwrap_or("manual"),
    )
    .await
    .map_err(bad)?;
    Ok(Json(res))
}

#[derive(Deserialize)]
pub struct RepairRequest {
    pub unit_id: String,
    /// `add` — put the device into repair; `remove` — take it back out.
    pub action: String,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// `POST /api/receiving/receipts/:receipt_id/repair`
pub async fn set_repair(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(receipt_id): Path<String>,
    Json(body): Json<RepairRequest>,
) -> ApiResult<Json<Value>> {
    let device_id = actor(&claims, &body.device_id);
    crate::handlers::pda::require_active_device(&state, &device_id).await?;

    let res = receiving::set_repair(
        &state.db,
        &state.instance_id,
        &receipt_id,
        &body.unit_id,
        &body.action,
    )
    .await
    .map_err(bad)?;
    Ok(Json(res))
}
