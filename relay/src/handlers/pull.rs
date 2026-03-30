use axum::{extract::{Path, State}, http::StatusCode, Json};
use base64::Engine;
use serde_json::Value;

use eck_core::models::relay::PullResponse;
use crate::db::RelayDb;

pub async fn pull(
    State(db): State<RelayDb>,
    Path((mesh_id, instance_id)): Path<(String, String)>,
) -> Result<Json<PullResponse>, StatusCode> {
    // SELECT all fields including base64-encoded binary data (stored as strings, not bytes)
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS rid, mesh_id, target_instance_id, sender_instance_id, \
             payload_b64, nonce_b64, created_at, ttl \
             FROM packet \
             WHERE mesh_id = $mesh_id AND target_instance_id = $instance_id AND ttl > time::now()"
        )
        .bind(("mesh_id", mesh_id.clone()))
        .bind(("instance_id", instance_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Pull SELECT failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();

    if rows.is_empty() {
        tracing::info!("Pull: [{}] {} got 0 packets", mesh_id, instance_id);
        return Ok(Json(PullResponse { mesh_id, packets: vec![] }));
    }

    // Convert Value rows to EncryptedPacket
    let packets: Vec<eck_core::models::relay::EncryptedPacket> = rows
        .iter()
        .filter_map(|r| {
            let payload_b64 = r.get("payload_b64")?.as_str()?;
            let nonce_b64 = r.get("nonce_b64")?.as_str()?;

            let payload = base64::engine::general_purpose::STANDARD.decode(payload_b64).ok()?;
            let nonce = base64::engine::general_purpose::STANDARD.decode(nonce_b64).ok()?;

            Some(eck_core::models::relay::EncryptedPacket {
                id: uuid::Uuid::new_v4(),
                mesh_id: r.get("mesh_id")?.as_str()?.to_string(),
                target_instance_id: r.get("target_instance_id")?.as_str()?.to_string(),
                sender_instance_id: r.get("sender_instance_id")?.as_str()?.to_string(),
                payload_cipher: payload,
                nonce,
                created_at: r.get("created_at").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default(),
                ttl: r.get("ttl").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or_default(),
            })
        })
        .collect();

    // Delete consumed packets
    if !packets.is_empty() {
        let _ = db.query(
            "DELETE FROM packet WHERE mesh_id = $mesh_id AND target_instance_id = $instance_id AND ttl > time::now()"
        )
        .bind(("mesh_id", mesh_id.clone()))
        .bind(("instance_id", instance_id.clone()))
        .await;
    }

    tracing::info!("Pull: [{}] {} delivered {} packets", mesh_id, instance_id, packets.len());

    Ok(Json(PullResponse {
        mesh_id,
        packets,
    }))
}
