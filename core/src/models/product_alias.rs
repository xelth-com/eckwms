use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Product alias / alternative barcode (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProductAlias {
    pub id: Uuid,
    pub product_id: Uuid,
    pub alias_type: String,
    pub alias_value: String,
    pub created_at: DateTime<Utc>,
}
