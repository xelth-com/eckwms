use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stock quant — quantity of a product at a location (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quant {
    pub id: Uuid,
    pub product_id: Uuid,
    pub location_id: Uuid,
    pub quantity: f64,
    pub reserved_quantity: f64,
    pub lot_id: Option<String>,
    pub package_id: Option<String>,
    pub last_synced_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
