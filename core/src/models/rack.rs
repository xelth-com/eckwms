use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Warehouse rack / storage position (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rack {
    pub id: Uuid,
    pub name: String,
    pub barcode: String,
    pub location_id: Uuid,
    pub row: Option<i32>,
    pub column: Option<i32>,
    pub level: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
