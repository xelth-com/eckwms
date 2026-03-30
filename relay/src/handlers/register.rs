use axum::{extract::State, http::StatusCode, Json};

use eck_core::models::relay::{RegisterRequest, RegisterResponse};
use crate::db::RelayDb;

pub async fn register(
    State(db): State<RelayDb>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, StatusCode> {
    let status = req.status.unwrap_or_else(|| "online".to_string());

    let instance_id = req.instance_id.clone();
    let mesh_id = req.mesh_id.clone();
    let external_ip = req.external_ip.clone();
    let port = req.port;
    let status_clone = status.clone();

    // IMPORTANT: SurrealDB embedded mode doesn't share transaction state across
    // multi-statement queries. Each .query() call must be separate.

    // Step 1: Delete old registration
    db.query("DELETE FROM registration WHERE instance_id = $iid")
        .bind(("iid", instance_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Register DELETE failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Step 2: Insert new registration (separate query for tx isolation)
    db.query("INSERT INTO registration {
            instance_id: $iid,
            mesh_id: $mid,
            external_ip: $eip,
            port: $pt,
            status: $st,
            last_seen: time::now()
        }")
    .bind(("iid", instance_id.clone()))
    .bind(("mid", mesh_id.clone()))
    .bind(("eip", external_ip.clone()))
    .bind(("pt", port as i64))
    .bind(("st", status_clone))
    .await
    .map_err(|e| {
        tracing::error!("Register INSERT failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tracing::info!(
        "Heartbeat: {} ({}) at {}:{} [{}]",
        instance_id, mesh_id, external_ip, port, status
    );

    Ok(Json(RegisterResponse {
        ok: true,
        instance_id,
        mesh_id,
        status,
    }))
}
