use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stock picking delivery / shipment (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StockPickingDelivery {
    pub id: Uuid,
    pub picking_id: Option<Uuid>,
    pub carrier_id: Option<Uuid>,
    pub tracking_number: Option<String>,
    pub label_url: Option<String>,
    pub status: String,
    pub weight: Option<f64>,
    pub length: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub recipient_name: Option<String>,
    pub recipient_street: Option<String>,
    pub recipient_city: Option<String>,
    pub recipient_zip: Option<String>,
    pub recipient_country: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
