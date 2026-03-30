use axum::{extract::{Path, State}, http::StatusCode, Json};
use serde_json::Value;

use crate::db::RelayDb;

// IMPORTANT: SurrealDB always includes the `id` field as a Thing (enum) in results.
// serde_json::Value can't deserialize Thing. Use record::id(id) AS id to convert it.

pub async fn mesh_status(
    State(db): State<RelayDb>,
    Path(mesh_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let rows: Vec<Value> = db
        .query("SELECT record::id(id) AS id, instance_id, external_ip, port, status, last_seen FROM registration WHERE mesh_id = $mesh_id ORDER BY last_seen DESC")
        .bind(("mesh_id", mesh_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Mesh status failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    tracing::info!("Mesh status: [{}] {} nodes", mesh_id, rows.len());

    Ok(Json(serde_json::json!({
        "mesh_id": mesh_id,
        "nodes": rows,
    })))
}

pub async fn resolve_node(
    State(db): State<RelayDb>,
    Path((mesh_id, instance_id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let rows: Vec<Value> = db
        .query("SELECT record::id(id) AS id, instance_id, external_ip, port, status, last_seen FROM registration WHERE mesh_id = $mesh_id AND instance_id = $instance_id LIMIT 1")
        .bind(("mesh_id", mesh_id))
        .bind(("instance_id", instance_id))
        .await
        .map_err(|e| {
            tracing::error!("Resolve failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    match rows.into_iter().next() {
        Some(node) => Ok(Json(node)),
        None => Err(StatusCode::NOT_FOUND),
    }
}
