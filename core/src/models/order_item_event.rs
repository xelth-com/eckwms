use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Order-Item lifecycle event (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderItemEvent {
    pub id: Uuid,
    pub order_id: Uuid,
    pub item_id: Option<Uuid>,
    pub event_type: String,
    pub description: String,
    pub actor_id: Option<Uuid>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
