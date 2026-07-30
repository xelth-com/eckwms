use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Registered PDA / device (SurrealDB document)
/// Record key (`device_id`) = server-minted UUID. The device's stable identity
/// anchor is its Ed25519 `public_key`; `android_id` (Settings.Secure.ANDROID_ID)
/// is kept only as a secondary lookup hint so pairing still works before the app
/// knows its UUID. Legacy rows keyed by the 16-hex ANDROID_ID are migrated to a
/// UUID key with the old id moved into `android_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisteredDevice {
    pub device_id: String,
    #[serde(default)]
    pub android_id: Option<String>,
    pub device_name: Option<String>,
    pub public_key: String,
    pub status: String,
    pub home_instance_id: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
