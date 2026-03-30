use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::odoo_types::OdooString;

/// ProductProduct — UUID-native product entity (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Product {
    pub id: Uuid,
    pub default_code: OdooString,
    pub barcode: OdooString,
    pub name: String,
    pub active: bool,
    #[serde(rename = "type")]
    pub r#type: String,
    pub list_price: f64,
    pub standard_price: f64,
    pub weight: f64,
    pub volume: f64,
    pub write_date: DateTime<Utc>,
    #[serde(rename = "lastSyncedAt")]
    pub last_synced_at: DateTime<Utc>,
}
