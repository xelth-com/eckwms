use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stock move line (individual pick/put line) (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveLine {
    pub id: Uuid,
    pub picking_id: Uuid,
    pub product_id: Uuid,
    pub location_id: Uuid,
    pub location_dest_id: Uuid,
    pub quantity: f64,
    pub quantity_done: f64,
    pub lot_name: Option<String>,
    pub state: String,
    pub last_synced_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
