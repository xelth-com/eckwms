use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use surrealdb::types::SurrealValue;

#[derive(Clone, Debug, Serialize, Deserialize, SurrealValue)]
pub struct AiThought {
    pub id: Option<Value>,
    pub task_id: String,
    pub iteration: i32,
    pub phase: String,
    pub payload: Value,
    pub content_hash: String,
    pub hedera_sequence: Option<u64>,
    pub hedera_timestamp: Option<String>,
    pub created_at: DateTime<Utc>,
}
