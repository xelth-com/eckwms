use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use surrealdb::types::SurrealValue;

#[derive(Clone, Debug, Serialize, Deserialize, SurrealValue)]
pub struct AiInbox {
    pub id: Option<Value>,
    pub task_id: String,
    pub source: String,
    pub content: Value,
    pub created_at: DateTime<Utc>,
}
