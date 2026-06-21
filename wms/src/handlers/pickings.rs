use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

use crate::AppState;

#[derive(Deserialize)]
pub struct PickingQuery {
    pub state: Option<String>,
    pub picking_type: Option<String>,
}

/// GET /api/pickings — list pickings, optionally filtered by state or type
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PickingQuery>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let mut query = "SELECT * FROM picking".to_string();
    let mut conditions = Vec::new();

    if q.state.is_some() { conditions.push("state = $state"); }
    if q.picking_type.is_some() { conditions.push("picking_type = $ptype"); }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    query.push_str(" ORDER BY created_at DESC");

    let mut stmt = state.db.query(&query);

    if let Some(s) = q.state { stmt = stmt.bind(("state", s)); }
    if let Some(p) = q.picking_type { stmt = stmt.bind(("ptype", p)); }

    let pickings: Vec<Value> = stmt
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(pickings))
}

/// GET /api/pickings/:id
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match state.get_synced_entity("picking", &id).await {
        Ok(Some(v)) => Ok(Json(v)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("Picking '{id}' not found"))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/pickings
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(mut payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    if let Some(obj) = payload.as_object_mut() {
        let now = chrono::Utc::now().to_rfc3339();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), serde_json::json!(now));
        }
        obj.insert("updated_at".to_string(), serde_json::json!(now));
    }

    let created: Option<Value> = state
        .db
        .create("picking")
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/pickings/:id
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("updated_at".to_string(), serde_json::json!(chrono::Utc::now().to_rfc3339()));
    }

    let updated: Option<Value> = state
        .db
        .update(("picking", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Picking '{id}' not found"))),
    }
}

/// DELETE /api/pickings/:id
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let deleted: Option<Value> = state
        .db
        .delete(("picking", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Picking '{id}' not found"))),
    }
}

// ─── Move Lines ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MoveLineQuery {
    pub picking_id: Option<String>,
}

/// GET /api/move-lines — list move lines, optionally filtered by picking
pub async fn list_lines(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MoveLineQuery>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let mut query = "SELECT * FROM move_line".to_string();

    if q.picking_id.is_some() {
        query.push_str(" WHERE picking_id = $pick_id");
    }

    query.push_str(" ORDER BY created_at DESC");

    let mut stmt = state.db.query(&query);

    if let Some(p) = q.picking_id { stmt = stmt.bind(("pick_id", p)); }

    let lines: Vec<Value> = stmt
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(lines))
}

/// POST /api/move-lines
pub async fn create_line(
    State(state): State<Arc<AppState>>,
    Json(mut payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    if let Some(obj) = payload.as_object_mut() {
        let now = chrono::Utc::now().to_rfc3339();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), serde_json::json!(now));
        }
        obj.insert("updated_at".to_string(), serde_json::json!(now));
    }

    let created: Option<Value> = state
        .db
        .create("move_line")
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/move-lines/:id
pub async fn update_line(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("updated_at".to_string(), serde_json::json!(chrono::Utc::now().to_rfc3339()));
    }

    let updated: Option<Value> = state
        .db
        .update(("move_line", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Move line '{id}' not found"))),
    }
}
