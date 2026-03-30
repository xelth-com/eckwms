use base64::{engine::general_purpose::STANDARD as BASE64_STD, Engine as _};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tracing::info;
use uuid::Uuid;

use crate::models::sync_packet::EncryptedSyncPacket;

#[derive(Error, Debug)]
pub enum RelayError {
    #[error("HTTP request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
    #[error("Relay returned error status: {0}")]
    StatusError(StatusCode),
    #[error("Serialization failed: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("Base64 decode failed: {0}")]
    Base64Error(#[from] base64::DecodeError),
}

#[derive(Debug, Serialize)]
struct RelayRegisterRequest {
    pub instance_id: String,
    pub mesh_id: String,
    pub external_ip: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RelayRegisterResponse {
    pub ok: bool,
    pub instance_id: String,
    pub mesh_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
struct RelayPushRequest {
    pub mesh_id: String,
    pub target_instance_id: String,
    pub sender_instance_id: String,
    pub payload_cipher: String,
    pub nonce: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RelayPushResponse {
    pub ok: bool,
    pub packet_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct RelayPullPacket {
    pub id: Uuid,
    pub mesh_id: String,
    pub target_instance_id: String,
    pub sender_instance_id: String,
    #[serde(with = "serde_base64")]
    pub payload_cipher: Vec<u8>,
    #[serde(with = "serde_base64")]
    pub nonce: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub ttl: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct RelayPullResponse {
    pub mesh_id: String,
    pub packets: Vec<RelayPullPacket>,
}

#[derive(Debug, Deserialize)]
pub struct MeshStatusResponse {
    pub mesh_id: String,
    pub nodes: Vec<RelayNodeInfo>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RelayNodeInfo {
    pub instance_id: String,
    pub external_ip: String,
    pub port: u16,
    pub status: String,
    pub last_seen: DateTime<Utc>,
}

#[derive(Clone)]
pub struct RelayClient {
    client: Client,
    relay_url: String,
    instance_id: String,
    mesh_id: String,
}

impl RelayClient {
    pub fn new(relay_url: &str, instance_id: &str, mesh_id: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        Self {
            client,
            relay_url: relay_url.trim_end_matches('/').to_string(),
            instance_id: instance_id.to_string(),
            mesh_id: mesh_id.to_string(),
        }
    }

    /// Sends a heartbeat (register) to the relay so other nodes can discover us.
    pub async fn send_heartbeat(
        &self,
        external_ip: &str,
        port: u16,
        status: Option<&str>,
    ) -> Result<RelayRegisterResponse, RelayError> {
        let url = format!("{}/E/register", self.relay_url);

        let payload = RelayRegisterRequest {
            instance_id: self.instance_id.clone(),
            mesh_id: self.mesh_id.clone(),
            external_ip: external_ip.to_string(),
            port,
            status: status.map(|s| s.to_string()),
        };

        let response = self.client.post(&url).json(&payload).send().await?;

        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }

        let result: RelayRegisterResponse = response.json().await?;
        info!(
            "Heartbeat OK: [{}] {} -> {}",
            self.mesh_id, self.instance_id, result.status
        );
        Ok(result)
    }

    /// Pushes an EncryptedSyncPacket to the relay for a specific target.
    pub async fn push_packet(
        &self,
        target_instance: &str,
        packet: &EncryptedSyncPacket,
        ttl_seconds: Option<u64>,
    ) -> Result<Uuid, RelayError> {
        let url = format!("{}/E/push", self.relay_url);

        let packet_bytes = serde_json::to_vec(packet)?;

        let req_body = RelayPushRequest {
            mesh_id: self.mesh_id.clone(),
            target_instance_id: target_instance.to_string(),
            sender_instance_id: self.instance_id.clone(),
            payload_cipher: BASE64_STD.encode(&packet_bytes),
            nonce: BASE64_STD.encode(&packet.nonce),
            ttl_seconds,
        };

        let response = self.client.post(&url).json(&req_body).send().await?;

        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }

        let push_resp: RelayPushResponse = response.json().await?;
        Ok(push_resp.packet_id)
    }

    /// Pulls pending packets from the relay for this instance.
    pub async fn pull_packets(&self) -> Result<Vec<EncryptedSyncPacket>, RelayError> {
        self.pull_packets_for_target(&self.mesh_id, &self.instance_id)
            .await
    }

    /// Pulls packets from the relay for an arbitrary target_id (used by pairing).
    pub async fn pull_packets_for(
        &self,
        target_id: &str,
    ) -> Result<Vec<EncryptedSyncPacket>, RelayError> {
        self.pull_packets_for_target(target_id, target_id).await
    }

    async fn pull_packets_for_target(
        &self,
        mesh_id: &str,
        target_id: &str,
    ) -> Result<Vec<EncryptedSyncPacket>, RelayError> {
        let url = format!("{}/E/pull/{}/{}", self.relay_url, mesh_id, target_id);

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }

        let pull_resp: RelayPullResponse = response.json().await?;

        let mut packets = Vec::new();
        for rp in pull_resp.packets {
            let packet: EncryptedSyncPacket = serde_json::from_slice(&rp.payload_cipher)?;
            packets.push(packet);
        }

        Ok(packets)
    }

    /// Gets the list of nodes registered in our mesh from the relay.
    pub async fn get_mesh_status(&self) -> Result<Vec<RelayNodeInfo>, RelayError> {
        let url = format!("{}/E/mesh/{}/status", self.relay_url, self.mesh_id);

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }

        let result: MeshStatusResponse = response.json().await?;
        info!(
            "Mesh status: [{}] {} nodes",
            self.mesh_id,
            result.nodes.len()
        );
        Ok(result.nodes)
    }

    /// Resolves a specific node's address from the relay.
    pub async fn resolve_node(
        &self,
        instance_id: &str,
    ) -> Result<Option<RelayNodeInfo>, RelayError> {
        let url = format!(
            "{}/E/mesh/{}/resolve/{}",
            self.relay_url, self.mesh_id, instance_id
        );

        let response = self.client.get(&url).send().await?;

        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }

        let node: RelayNodeInfo = response.json().await?;
        Ok(Some(node))
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    pub fn mesh_id(&self) -> &str {
        &self.mesh_id
    }
}

/// Base64 serde helper for Vec<u8> fields
mod serde_base64 {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    #[allow(dead_code)]
    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}
