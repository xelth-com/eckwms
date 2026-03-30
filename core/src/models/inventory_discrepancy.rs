use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Inventory discrepancy report (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InventoryDiscrepancy {
    pub id: Uuid,
    pub product_id: Uuid,
    pub location_id: Uuid,
    pub expected_qty: f64,
    pub actual_qty: f64,
    pub discrepancy_type: String,
    pub status: String,
    pub reviewed_by: Option<Uuid>,
    pub review_notes: Option<String>,
    pub detected_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
