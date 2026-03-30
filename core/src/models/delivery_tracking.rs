use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Delivery tracking event (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeliveryTracking {
    pub id: Uuid,
    pub delivery_id: Uuid,
    pub status: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub event_time: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
