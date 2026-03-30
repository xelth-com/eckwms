use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Registered PDA / device (SurrealDB document)
/// Record key = Android device ID (Settings.Secure.ANDROID_ID)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisteredDevice {
    pub device_id: String,
    pub device_name: Option<String>,
    pub public_key: String,
    pub status: String,
    pub home_instance_id: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
