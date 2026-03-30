use axum::{extract::State, http::StatusCode, Json};
use base64::Engine;
use uuid::Uuid;

use eck_core::models::relay::{PushRequest, PushResponse};
use crate::db::RelayDb;

/// Maximum payload size: 1 MB.
const MAX_PAYLOAD_SIZE: usize = 1_048_576;

pub async fn push(
    State(db): State<RelayDb>,
    Json(req): Json<PushRequest>,
) -> Result<Json<PushResponse>, StatusCode> {
    if req.payload_cipher.len() > MAX_PAYLOAD_SIZE {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let id = Uuid::new_v4();
    let ttl_seconds = req.ttl_seconds.unwrap_or(3600);

    let mesh_id = req.mesh_id;
    let sender = req.sender_instance_id;

    // Fan-out: find all registered instances in this mesh (except sender)
    // and create a packet for each one
    let targets: Vec<String> = db
        .query(
            "SELECT record::id(id) AS id, instance_id FROM registration \
             WHERE mesh_id = $mesh_id AND instance_id != $sender AND status = 'online'",
        )
        .bind(("mesh_id", mesh_id.clone()))
        .bind(("sender", sender.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Fan-out query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take::<Vec<serde_json::Value>>(0)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.get("instance_id")?.as_str().map(String::from))
        .collect();

    if targets.is_empty() {
        tracing::warn!("Push: no targets in mesh {} (sender: {})", mesh_id, sender);
        return Ok(Json(PushResponse { ok: true, packet_id: id }));
    }

    // Store binary data as base64 strings (SurrealDB bytes can't deserialize to serde_json::Value)
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(&req.payload_cipher);
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(&req.nonce);

    let mut created = 0usize;
    for target in &targets {
        let result = db
            .query(
                "INSERT INTO packet {
                    mesh_id: $mesh_id,
                    target_instance_id: $target_instance_id,
                    sender_instance_id: $sender_instance_id,
                    payload_b64: $payload_b64,
                    nonce_b64: $nonce_b64,
                    created_at: time::now(),
                    ttl: time::now() + type::duration($ttl_dur)
                }",
            )
            .bind(("mesh_id", mesh_id.clone()))
            .bind(("target_instance_id", target.clone()))
            .bind(("sender_instance_id", sender.clone()))
            .bind(("payload_b64", payload_b64.clone()))
            .bind(("nonce_b64", nonce_b64.clone()))
            .bind(("ttl_dur", format!("{}s", ttl_seconds)))
            .await;

        match result {
            Ok(mut resp) => {
                if let Err(e) = resp.check() {
                    tracing::error!("Packet CREATE check failed for {}: {}", target, e);
                } else {
                    created += 1;
                }
            },
            Err(e) => tracing::error!("Failed to create packet for {}: {}", target, e),
        }
    }

    tracing::info!(
        "Packet {} fan-out: [{}] {} -> {} target(s) ({} created, ttl {}s)",
        id, mesh_id, sender, targets.len(), created, ttl_seconds
    );

    Ok(Json(PushResponse { ok: true, packet_id: id }))
}
