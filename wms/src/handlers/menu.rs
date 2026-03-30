use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::Value;
use std::sync::Arc;

use crate::AppState;

// ============================================================
// Categories
// ============================================================

/// GET /api/menu/categories
pub async fn list_categories(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let categories: Vec<Value> = state
        .db
        .select("category")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(categories))
}

/// POST /api/menu/categories
pub async fn create_category(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    let created: Option<Value> = state
        .db
        .create("category")
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/menu/categories/:id
pub async fn update_category(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let updated: Option<Value> = state
        .db
        .update(("category", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Category '{id}' not found"))),
    }
}

/// DELETE /api/menu/categories/:id
pub async fn delete_category(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let deleted: Option<Value> = state
        .db
        .delete(("category", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Category '{id}' not found"))),
    }
}

// ============================================================
// Items
// ============================================================

/// GET /api/menu/items
pub async fn list_items(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let items: Vec<Value> = state
        .db
        .select("item")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(items))
}

/// POST /api/menu/items
pub async fn create_item(
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
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/menu/items/:id
pub async fn update_item(
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

/// DELETE /api/menu/items/:id
pub async fn delete_item(
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
