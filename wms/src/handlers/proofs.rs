use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use chrono::Utc;
use eck_core::models::action_proof::ActionProof;
use eck_core::sync::hedera;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

/// POST /api/proofs — Receive an ActionProof from an edge device, hash it, and seal it in Hedera HCS
pub async fn submit_proof(
    State(state): State<Arc<AppState>>,
    Json(mut payload): Json<ActionProof>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    payload.created_at = Utc::now();

    // 1. Generate JSON representation for hashing
    let proof_json = serde_json::to_string(&json!({
        "entity_type": payload.entity_type,
        "entity_id": payload.entity_id,
        "proof_type": payload.proof_type,
        "verified_by": payload.verified_by,
        "location": payload.location,
        "device_id": payload.device_id,
        "signature_image": payload.signature_image,
        "created_at": payload.created_at.to_rfc3339(),
    }))
    .unwrap();

    // 2. Hash the proof
    let content_hash = hex::encode(Sha256::digest(proof_json.as_bytes()));
    payload.content_hash = Some(content_hash.clone());

    // 3. Submit to Hedera HCS
    let hcs_receipt =
        hedera::submit_hash_if_configured(state.hedera.as_ref(), &content_hash).await;

    if let Some(receipt) = hcs_receipt {
        payload.hedera_sequence = Some(receipt.sequence_number);
        payload.hedera_timestamp = Some(receipt.consensus_timestamp);
    }

    // Capture audit fields before `payload` is moved into the DB write.
    let audit_action = format!("proof.{}", payload.proof_type);
    let audit_summary = format!(
        "Proof {} on {}:{} by device {}",
        payload.proof_type, payload.entity_type, payload.entity_id, payload.device_id
    );
    let audit_payload = json!({
        "entity_type": payload.entity_type,
        "entity_id": payload.entity_id,
        "proof_type": payload.proof_type,
        "device_id": payload.device_id,
        "verified_by": payload.verified_by,
        "content_hash": content_hash,
    });
    let audit_actor = payload.device_id.clone();

    // 4. Save to SurrealDB
    let doc_id = Uuid::new_v4().to_string();
    let created: Option<Value> = state
        .db
        .create(("action_proof", doc_id.as_str()))
        .content(payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 5. Tamper-evident audit chain (best-effort; never fails the proof write).
    eck_core::audit::append_soft(
        &state.db,
        &state.server_identity,
        &eck_core::audit::wms_chain(&state.instance_id),
        &audit_actor,
        &audit_action,
        eck_core::audit::class::MUTATE,
        &audit_summary,
        audit_payload,
    )
    .await;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to store action proof".into(),
        )),
    }
}
