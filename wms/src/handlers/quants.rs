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
pub struct QuantQuery {
    pub location_id: Option<String>,
    pub product_id: Option<String>,
}

/// GET /api/quants — list inventory quantities, optionally filtered by location or product
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<QuantQuery>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let mut query = "SELECT * FROM quant".to_string();
    let mut conditions = Vec::new();

    if q.location_id.is_some() { conditions.push("location_id = $loc_id"); }
    if q.product_id.is_some() { conditions.push("product_id = $prod_id"); }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    query.push_str(" ORDER BY created_at DESC");

    let mut stmt = state.db.query(&query);

    if let Some(l) = q.location_id { stmt = stmt.bind(("loc_id", l)); }
    if let Some(p) = q.product_id { stmt = stmt.bind(("prod_id", p)); }

    let quants: Vec<Value> = stmt
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(quants))
}

/// GET /api/quants/:id
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match state.get_synced_entity("quant", &id).await {
        Ok(Some(v)) => Ok(Json(v)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("Quant '{id}' not found"))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/quants
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
        .create("quant")
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/quants/:id
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
        .update(("quant", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Quant '{id}' not found"))),
    }
}

/// DELETE /api/quants/:id
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let deleted: Option<Value> = state
        .db
        .delete(("quant", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Quant '{id}' not found"))),
    }
}
