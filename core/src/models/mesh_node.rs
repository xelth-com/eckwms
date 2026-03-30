use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Mesh node registration (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshNode {
    pub instance_id: String,
    pub name: String,
    pub base_url: String,
    pub role: String,
    pub status: String,
    pub last_seen: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
