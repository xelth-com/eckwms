use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stock picking (warehouse operation) (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Picking {
    pub id: Uuid,
    pub name: String,
    pub origin: Option<String>,
    pub picking_type: String,
    pub state: String,
    pub location_id: Uuid,
    pub location_dest_id: Uuid,
    pub partner_id: Option<Uuid>,
    pub scheduled_date: Option<DateTime<Utc>>,
    pub date_done: Option<DateTime<Utc>>,
    pub last_synced_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
