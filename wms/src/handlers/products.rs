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
pub struct ProductQuery {
    pub source_system: Option<String>,
    pub external_id: Option<String>,
    pub barcode: Option<String>,
    pub default_code: Option<String>,
}

/// GET /api/products — list products, optionally filtered by external identifiers.
/// Supports `source_system` + `external_id` for Odoo/Twenty CRM mapping.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ProductQuery>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let mut query = "SELECT * FROM product".to_string();
    let mut conditions = Vec::new();

    if q.source_system.is_some() { conditions.push("source_system = $source"); }
    if q.external_id.is_some() { conditions.push("external_id = $ext_id"); }
    if q.barcode.is_some() { conditions.push("barcode = $barcode"); }
    if q.default_code.is_some() { conditions.push("default_code = $default_code"); }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    query.push_str(" ORDER BY created_at DESC");

    let mut stmt = state.db.query(&query);

    if let Some(s) = q.source_system { stmt = stmt.bind(("source", s)); }
    if let Some(e) = q.external_id { stmt = stmt.bind(("ext_id", e)); }
    if let Some(b) = q.barcode { stmt = stmt.bind(("barcode", b)); }
    if let Some(d) = q.default_code { stmt = stmt.bind(("default_code", d)); }

    let products: Vec<Value> = stmt
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(products))
}

/// GET /api/products/:id
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let product: Option<Value> = state
        .db
        .select(("product", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match product {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Product '{id}' not found"))),
    }
}

/// POST /api/products
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
        .create("product")
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

/// PUT /api/products/:id
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "updated_at".to_string(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
    }

    let updated: Option<Value> = state
        .db
        .update(("product", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Product '{id}' not found"))),
    }
}

/// DELETE /api/products/:id
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let deleted: Option<Value> = state
        .db
        .delete(("product", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Product '{id}' not found"))),
    }
}
