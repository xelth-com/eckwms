use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::sync::vector_clock::VectorClock;

/// EncryptedSyncPacket represents a data packet for Blind Relay servers.
/// The relay server can read metadata for routing, but cannot read the encrypted payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedSyncPacket {
    #[serde(rename = "entityType")]
    pub entity_type: String,
    #[serde(rename = "entityId")]
    pub entity_id: String,
    pub version: i64,
    #[serde(rename = "sourceInstance")]
    pub source_instance: String,
    pub vector_clock: serde_json::Value,

    // Security metadata
    pub key_id: String,
    pub algorithm: String,

    // The encrypted payload (opaque to relay)
    pub encrypted_payload: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// Metadata for entities undergoing synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMetadata {
    pub entity_id: String,
    pub entity_type: String,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub source: String,
    pub source_priority: i32,
    pub instance_id: String,
    pub device_id: Option<String>,
    pub vector_clock: VectorClock,
}
