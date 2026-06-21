//! WMS audit-chain endpoints — verify, export, and on-demand anchor.
//!
//! Chain id for this WMS node is `9eck:wms:<instance_id>`. Inventory-side
//! mutations (action proofs, and any move/picking call that adopts the same
//! one-line `eck_core::audit::append_soft(...)` hook) link into it. Anchoring
//! shares the same Merkle→Hedera path as the Kasse.

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

pub fn chain_id(state: &AppState) -> String {
    eck_core::audit::wms_chain(&state.instance_id)
}

/// GET /api/audit/verify — recompute the hash-chain in process.
pub async fn verify(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let cid = chain_id(&state);
    match eck_core::audit::verify_chain(&state.db, &cid).await {
        Ok(report) => (StatusCode::OK, Json(json!({ "success": true, "report": report }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

/// GET /api/audit/chain — full ordered chain + signer public key (hex) for
/// the offline `verify_audit.py`.
pub async fn chain(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let cid = chain_id(&state);
    match eck_core::audit::chain_events(&state.db, &cid).await {
        Ok(events) => {
            let signer_pub = events.first().map(|e| e.signer_pub.clone()).unwrap_or_default();
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "chain_id": cid,
                    "signer_pub": signer_pub,
                    "events": events,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

/// POST /api/audit/anchor — force a Merkle batch → Hedera anchor now.
pub async fn anchor(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    match eck_core::audit::anchor_pending(&state.db).await {
        Ok(Some(a)) => (StatusCode::OK, Json(json!({ "success": true, "anchor": a }))),
        Ok(None) => (StatusCode::OK, Json(json!({ "success": true, "anchor": null, "note": "nothing pending" }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}
