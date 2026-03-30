use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::json;

use crate::AppState;

/// GET /api/admin/db/backups — list all backups
pub async fn list_backups(
    State(_state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    match crate::services::backup::list_backups().await {
        Ok(backups) => (
            StatusCode::OK,
            Json(json!({ "success": true, "backups": backups })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

/// POST /api/admin/db/backup — create a new backup
pub async fn create_backup(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    match crate::services::backup::create_backup(&state.db).await {
        Ok(filename) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "message": "Backup created",
                "filename": filename,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

/// POST /api/admin/db/restore/:filename — restore from a backup
pub async fn restore_backup(
    State(state): State<Arc<AppState>>,
    Path(filename): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    match crate::services::backup::restore_backup(&state.db, &filename).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "message": "Restored successfully",
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}
