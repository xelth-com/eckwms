use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Sync history log entry (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncHistory {
    pub id: String,
    pub instance_id: String,
    pub provider: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration: i64,
    pub created: i64,
    pub updated: i64,
    pub skipped: i64,
    pub errors: i64,
    pub error_detail: String,
    pub debug_info: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
