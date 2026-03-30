use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Entity checksum for Merkle tree sync (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityChecksum {
    pub entity_type: String,
    pub entity_id: String,
    pub content_hash: String,
    pub children_hash: Option<String>,
    pub full_hash: String,
    pub child_count: i32,
    pub last_updated: DateTime<Utc>,
    pub source_instance: String,
    pub source_device: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
