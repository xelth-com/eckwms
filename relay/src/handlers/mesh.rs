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
        .query("SELECT record::id(id) AS id, instance_id, external_ip, port, status, last_seen, base_url, lan_url, node_role, paid, tier FROM registration WHERE mesh_id = $mesh_id ORDER BY last_seen DESC")
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

/// GET /E/registry — ALL registrations across ALL meshes. This is cross-tenant
/// data, so it's CLOSED by default: requires `Authorization: Bearer
/// <RELAY_ADMIN_TOKEN>` and is disabled (403) when that env var is unset. Used by
/// the WMS admin proxy (`/api/admin/known-nodes`) so the cloud admin UI can list
/// kiosks regardless of which mesh they're in.
pub async fn registry(
    State(db): State<RelayDb>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let expected = std::env::var("RELAY_ADMIN_TOKEN").unwrap_or_default();
    if expected.trim().is_empty() {
        return Err(StatusCode::FORBIDDEN);
    }
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if !ct_eq(presented.as_bytes(), expected.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, instance_id, mesh_id, external_ip, port, status, \
                    last_seen, base_url, lan_url, node_role, paid, tier \
             FROM registration ORDER BY mesh_id, last_seen DESC",
        )
        .await
        .map_err(|e| {
            tracing::error!("Registry query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    Ok(Json(serde_json::json!({ "nodes": rows })))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub async fn resolve_node(
    State(db): State<RelayDb>,
    Path((mesh_id, instance_id)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let rows: Vec<Value> = db
        .query("SELECT record::id(id) AS id, instance_id, external_ip, port, status, last_seen, base_url, lan_url, node_role, paid, tier FROM registration WHERE mesh_id = $mesh_id AND instance_id = $instance_id LIMIT 1")
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
