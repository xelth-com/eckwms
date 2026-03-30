use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::odoo_types::OdooString;

/// StockLocation — UUID-native location entity (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub id: Uuid,
    pub name: String,
    pub complete_name: String,
    pub barcode: OdooString,
    pub usage: String,
    pub location_id: Option<Uuid>,
    pub active: bool,
    #[serde(rename = "lastSyncedAt")]
    pub last_synced_at: DateTime<Utc>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime<Utc>,
}
