use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Order represents a unified order/request for RMA and repairs (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    pub id: Uuid,
    pub order_number: String,
    pub order_type: String,
    pub customer_name: String,
    pub customer_email: String,
    pub customer_phone: String,
    pub item_id: Option<i32>,
    pub product_sku: String,
    pub product_name: String,
    pub serial_number: String,
    pub purchase_date: Option<DateTime<Utc>>,
    pub issue_description: String,
    pub diagnosis_notes: String,
    pub assigned_to: Option<String>,
    pub status: String,
    pub priority: String,
    pub repair_notes: String,
    pub parts_used: serde_json::Value,
    pub labor_hours: f64,
    pub total_cost: f64,
    pub resolution: String,
    pub notes: String,
    pub metadata: serde_json::Value,
    pub rma_reason: String,
    pub is_refund_requested: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub deleted_at: Option<DateTime<Utc>>,
}
