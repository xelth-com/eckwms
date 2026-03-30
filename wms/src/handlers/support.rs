use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use crate::services::support as svc;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct ImportTicketRequest {
    /// Zoho ticket ID (used as SurrealDB record key)
    pub ticket_id: String,
    /// Full Zoho ticket payload
    pub ticket: Value,
}

/// POST /api/support/import-ticket — import or update a Zoho Desk ticket.
/// Computes MurmurHash3 of the payload for change detection.
pub async fn import_ticket(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ImportTicketRequest>,
) -> ApiResult<Json<Value>> {
    let result = svc::import_ticket(&state.db, &payload.ticket_id, &payload.ticket)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "changed": result.changed, "ticket_id": result.id })))
}

#[derive(Deserialize)]
pub struct ImportThreadRequest {
    /// Zoho thread ID (used as SurrealDB record key)
    pub thread_id: String,
    /// Parent Zoho ticket ID
    pub ticket_id: String,
    /// Full Zoho thread payload
    pub thread: Value,
}

/// POST /api/support/import-thread — import or update a Zoho Desk thread.
/// On change, marks the PARENT TICKET for re-summarization.
pub async fn import_thread(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ImportThreadRequest>,
) -> ApiResult<Json<Value>> {
    let result = svc::import_thread(&state.db, &payload.thread_id, &payload.ticket_id, &payload.thread)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "changed": result.changed, "thread_id": result.id })))
}
