//! Dashboard alert HTTP surface.
//!
//! `GET /api/alerts/unread` lets a freshly-loaded dashboard restore the red
//! banner (WS only delivers alerts that fire while a tab is open). `POST
//! /api/alerts/:id/read` is the operator dismiss control.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use eck_core::auth::Claims;
use serde_json::{json, Value};

use crate::AppState;

/// GET /api/alerts/unread — up to 20 newest unread alerts (any authenticated
/// role; the kiosk observer must be able to see the banner too).
pub async fn list_unread(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, key, title, message, severity, created_at, last_seen_at \
             FROM system_alert WHERE status = 'unread' \
             ORDER BY last_seen_at DESC, created_at DESC LIMIT 20",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": e.to_string() })),
            )
        })?;
    Ok(Json(json!({ "ok": true, "alerts": rows })))
}

/// POST /api/alerts/:id/read — dismiss one alert (admin or operator; observers
/// may view but not dismiss). Route is JWT-gated; this is the role check on top.
pub async fn mark_read(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if claims.role != "admin" && claims.role != "operator" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "admin or operator role required" })),
        ));
    }

    let note = format!("dismissed by {}", claims.sub);
    let changed: Vec<Value> = state
        .db
        .query(
            "UPDATE system_alert SET status = 'read', resolved_note = $note, resolved_at = time::now() \
             WHERE record::id(id) = $id AND status = 'unread' RETURN AFTER",
        )
        .bind(("id", id))
        .bind(("note", note))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": e.to_string() })),
            )
        })?;

    Ok(Json(json!({ "ok": true, "dismissed": !changed.is_empty() })))
}
