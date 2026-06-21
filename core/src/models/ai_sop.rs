use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use surrealdb::types::SurrealValue;

#[derive(Clone, Debug, Serialize, Deserialize, SurrealValue)]
pub struct AiSop {
    pub id: Option<Value>,
    pub title: String,
    pub trigger_context: String,
    pub embedding: Option<Vec<f32>>,
    pub rule: String,
    pub success_count: i64,
    pub failure_count: i64,
    pub usage_count: i64,
    pub deprecated: bool,
    pub _vclock: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
