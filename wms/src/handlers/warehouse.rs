use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ─── Warehouses / Locations ──────────────────────────────────────────────────

/// GET /api/warehouse — list all warehouses/locations, ordered by complete_name
pub async fn list(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let locs: Vec<Value> = state
        .db
        .query("SELECT * FROM location ORDER BY complete_name ASC")
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(locs))
}

/// POST /api/warehouse — create a warehouse/location
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let created: Option<Value> = state
        .db
        .create("location")
        .content(payload)
        .await
        .map_err(db_err)?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// GET /api/warehouse/:id — get a single warehouse with its racks
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let loc = state
        .get_synced_entity("location", &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Warehouse '{id}' not found")))?;

    // Fetch racks belonging to this warehouse
    let racks: Vec<Value> = state
        .db
        .query("SELECT * FROM rack WHERE warehouse_id = $wid ORDER BY name ASC")
        .bind(("wid", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let mut result = loc;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("racks".to_string(), json!(racks));
    }

    Ok(Json(result))
}

// ─── Racks ───────────────────────────────────────────────────────────────────

/// GET /api/warehouse/racks — list all racks
pub async fn list_racks(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let racks: Vec<Value> = state
        .db
        .query("SELECT * FROM rack ORDER BY name ASC")
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(racks))
}

/// POST /api/warehouse/racks — create or upsert a rack
pub async fn create_rack(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let created: Option<Value> = state
        .db
        .create("rack")
        .content(payload)
        .await
        .map_err(db_err)?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/warehouse/racks/:id — update a rack
pub async fn update_rack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    let updated: Option<Value> = state
        .db
        .update(("rack", &*id))
        .content(payload)
        .await
        .map_err(db_err)?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Rack '{id}' not found"))),
    }
}

/// DELETE /api/warehouse/racks/:id — delete a rack
pub async fn delete_rack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let deleted: Option<Value> = state
        .db
        .delete(("rack", &*id))
        .await
        .map_err(db_err)?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Rack '{id}' not found"))),
    }
}
