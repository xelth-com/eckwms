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
pub struct PartnerQuery {
    pub source_system: Option<String>,
    pub external_id: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
}

/// GET /api/partners — list partners, optionally filtered by external IDs or contact info.
/// Supports `source_system` + `external_id` for Odoo/Twenty CRM mapping,
/// plus `email` exact match and `name` case-insensitive substring search.
pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PartnerQuery>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let mut query = "SELECT * FROM partner".to_string();
    let mut conditions = Vec::new();

    if q.source_system.is_some() { conditions.push("source_system = $source"); }
    if q.external_id.is_some() { conditions.push("external_id = $ext_id"); }
    if q.email.is_some() { conditions.push("email = $email"); }
    if q.name.is_some() { conditions.push("string::lowercase(name) CONTAINS string::lowercase($name)"); }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    query.push_str(" ORDER BY created_at DESC");

    let mut stmt = state.db.query(&query);

    if let Some(s) = q.source_system { stmt = stmt.bind(("source", s)); }
    if let Some(e) = q.external_id { stmt = stmt.bind(("ext_id", e)); }
    if let Some(m) = q.email { stmt = stmt.bind(("email", m)); }
    if let Some(n) = q.name { stmt = stmt.bind(("name", n)); }

    let partners: Vec<Value> = stmt
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(partners))
}

/// GET /api/partners/:id
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match state.get_synced_entity("partner", &id).await {
        Ok(Some(v)) => Ok(Json(v)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("Partner '{id}' not found"))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

/// POST /api/partners
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
        .create("partner")
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

/// PUT /api/partners/:id
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
        .update(("partner", &*id))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Partner '{id}' not found"))),
    }
}

/// DELETE /api/partners/:id
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let deleted: Option<Value> = state
        .db
        .delete(("partner", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Partner '{id}' not found"))),
    }
}
