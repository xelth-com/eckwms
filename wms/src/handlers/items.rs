use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::Value;
use std::sync::Arc;

use crate::AppState;

/// GET /api/items — list all items
pub async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let items: Vec<Value> = state
        .db
        .select("item")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(items))
}

/// GET /api/items/:id — get a single item
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let item: Option<Value> = state
        .db
        .select(("item", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match item {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Item '{id}' not found"))),
    }
}

/// POST /api/items — create a new item
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let created: Option<Value> = state
        .db
        .create("item")
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Create returned no record".into(),
        )),
    }
}

/// PUT /api/items/:id — replace an item
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let updated: Option<Value> = state
        .db
        .update(("item", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Item '{id}' not found"))),
    }
}

/// DELETE /api/items/:id — delete an item
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let deleted: Option<Value> = state
        .db
        .delete(("item", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Item '{id}' not found"))),
    }
}
