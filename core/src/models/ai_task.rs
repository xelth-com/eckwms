use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use surrealdb::types::SurrealValue;

#[derive(Clone, Debug, Serialize, Deserialize, SurrealValue)]
pub struct AiTask {
    pub id: Option<Value>,
    pub state: String,
    pub owner_instance_id: String,
    pub worker_id: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub parent_task_id: Option<String>,
    pub context: Value,
    pub embedding: Option<Vec<f32>>,
    pub awaiting_input_schema: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
