//! Relay-specific models (stripped from SQLx)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Encrypted packet stored by the relay. The relay never sees the plaintext.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPacket {
    pub id: Uuid,
    pub mesh_id: String,
    pub target_instance_id: String,
    pub sender_instance_id: String,
    #[serde(with = "base64_bytes")]
    pub payload_cipher: Vec<u8>,
    #[serde(with = "base64_bytes")]
    pub nonce: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub ttl: DateTime<Utc>,
}

/// Account with rate-limit plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub api_key: String,
    pub plan: Plan,
    pub allowance: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Plan {
    Free,
    Pro,
}

/// Heartbeat registration payload.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub instance_id: String,
    pub mesh_id: String,
    pub external_ip: String,
    pub port: u16,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// `"full"` (default) or `"cache"`. The relay stores and re-emits this
    /// string; the sync engine on each peer decides what to do with it.
    #[serde(default)]
    pub node_role: Option<String>,
    /// 9eck product license token (JWS-lite, see `crate::licensing`). Optional —
    /// the relay verifies it offline and tags the registration `paid`/`tier`.
    /// Absent / unverifiable ⇒ treated as free.
    #[serde(default)]
    pub license: Option<String>,
    /// Private/LAN address advertised alongside the public `base_url` (B3).
    #[serde(default)]
    pub lan_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub ok: bool,
    pub instance_id: String,
    pub mesh_id: String,
    pub status: String,
}

/// Push packet request.
#[derive(Debug, Deserialize)]
pub struct PushRequest {
    pub mesh_id: String,
    pub target_instance_id: String,
    pub sender_instance_id: String,
    #[serde(with = "base64_bytes")]
    pub payload_cipher: Vec<u8>,
    #[serde(with = "base64_bytes")]
    pub nonce: Vec<u8>,
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    pub ok: bool,
    pub packet_id: Uuid,
}

/// Pull response — list of pending packets.
#[derive(Debug, Serialize)]
pub struct PullResponse {
    pub mesh_id: String,
    pub packets: Vec<EncryptedPacket>,
}

/// Mesh status response — list of nodes in a mesh.
#[derive(Debug, Serialize)]
pub struct MeshStatusResponse {
    pub mesh_id: String,
    pub nodes: Vec<NodeStatus>,
}

#[derive(Debug, Serialize)]
pub struct NodeStatus {
    pub instance_id: String,
    pub external_ip: String,
    pub port: u16,
    pub status: String,
    pub last_seen: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// Base64 serde helper for Vec<u8> fields.
pub mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        use base64::Engine;
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(&s)
            .map_err(serde::de::Error::custom)
    }
}
