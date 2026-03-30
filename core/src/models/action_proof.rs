use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct GeoLocation {
    pub lat: f64,
    pub lng: f64,
    pub accuracy: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct ActionProof {
    pub id: Option<String>,
    pub entity_type: String,
    pub entity_id: String,
    pub proof_type: String,
    pub verified_by: Option<String>,
    pub location: Option<GeoLocation>,
    pub device_id: String,
    pub signature_image: Option<String>,
    pub content_hash: Option<String>,
    pub hedera_sequence: Option<u64>,
    pub hedera_timestamp: Option<String>,
    pub created_at: DateTime<Utc>,
}
