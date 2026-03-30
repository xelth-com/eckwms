use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

use super::merkle::{MerkleNode, MerkleRequest};

/// HTTP client for direct P2P communication with mesh peers.
///
/// Unlike the RelayClient (which routes encrypted packets through a blind relay),
/// MeshClient talks directly to a peer's WMS API endpoints. The relay is only
/// used for service discovery (heartbeat + mesh status) — actual data flows P2P.
///
/// Payloads travel as plaintext JSON over the wire. For production, secure with
/// mTLS or VPN at the network layer. The `SYNC_SECRET` shared key can optionally
/// be sent as a bearer token for basic authentication between trusted nodes.
#[derive(Clone)]
pub struct MeshClient {
    client: Client,
    peer_base_url: String,
    sync_secret: Option<String>,
}

impl MeshClient {
    pub fn new(peer_base_url: &str, sync_secret: Option<&str>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            client,
            peer_base_url: peer_base_url.trim_end_matches('/').to_string(),
            sync_secret: sync_secret.map(|s| s.to_string()),
        }
    }

    pub fn peer_url(&self) -> &str {
        &self.peer_base_url
    }

    fn auth_header(&self) -> Option<String> {
        self.sync_secret.as_ref().map(|s| format!("Bearer {}", s))
    }

    // ─── Merkle Tree ─────────────────────────────────────────────────────────

    /// Fetch Merkle tree state from a peer (level 0 = root, level 1 = bucket).
    pub async fn get_merkle_state(&self, req: &MerkleRequest) -> Result<MerkleNode, String> {
        let mut url = format!(
            "{}/api/mesh/merkle/state?entity_type={}&level={}",
            self.peer_base_url, req.entity_type, req.level
        );
        if let Some(ref bucket) = req.bucket {
            url.push_str(&format!("&bucket={}", bucket));
        }

        let mut http_req = self.client.get(&url);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("Merkle GET failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Merkle GET {} returned {}",
                url,
                resp.status()
            ));
        }

        resp.json::<MerkleNode>()
            .await
            .map_err(|e| format!("Merkle response parse failed: {}", e))
    }

    // ─── Pull Entities ───────────────────────────────────────────────────────

    /// Pull specific entities from a peer by type and IDs.
    /// The peer responds with a JSON array of entity documents.
    pub async fn pull_entities(
        &self,
        entity_type: &str,
        ids: &[String],
    ) -> Result<Vec<Value>, String> {
        let url = format!("{}/api/mesh/sync/pull", self.peer_base_url);

        let body = serde_json::json!({
            "entity_type": entity_type,
            "ids": ids,
        });

        let mut http_req = self.client.post(&url).json(&body);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("Pull POST failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Pull POST {} returned {}", url, resp.status()));
        }

        let result: Value = resp
            .json()
            .await
            .map_err(|e| format!("Pull response parse failed: {}", e))?;

        match result.get("entities") {
            Some(Value::Array(arr)) => Ok(arr.clone()),
            _ => Ok(vec![]),
        }
    }

    // ─── Push Entities ───────────────────────────────────────────────────────

    /// Push entities to a peer. The peer upserts them into its local DB.
    /// Returns the number of entities applied by the peer.
    pub async fn push_entities(
        &self,
        entity_type: &str,
        entities: &[Value],
        source_instance: &str,
    ) -> Result<usize, String> {
        let url = format!("{}/api/mesh/sync/push", self.peer_base_url);

        let body = serde_json::json!({
            "entity_type": entity_type,
            "entities": entities,
            "source_instance": source_instance,
        });

        let mut http_req = self.client.post(&url).json(&body);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("Push POST failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Push POST {} returned {}", url, resp.status()));
        }

        let result: Value = resp
            .json()
            .await
            .map_err(|e| format!("Push response parse failed: {}", e))?;

        Ok(result["applied"].as_u64().unwrap_or(0) as usize)
    }

    // ─── File Fetch ──────────────────────────────────────────────────────────

    /// Fetch a file's bytes from a peer by its SHA-256 hash.
    /// Used to hydrate FileStore after pulling file_resource metadata.
    pub async fn fetch_file(&self, hash: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/api/mesh/file/{}", self.peer_base_url, hash);

        let mut http_req = self.client.get(&url);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("File GET failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("File GET {} returned {}", url, resp.status()));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("File response read failed: {}", e))
    }
}

/// Discover online peers from the relay and build MeshClients for each.
///
/// Filters out ourselves (by instance_id) and offline nodes.
pub async fn discover_peers(
    relay: &super::relay_client::RelayClient,
    own_instance_id: &str,
    sync_secret: Option<&str>,
) -> Vec<MeshClient> {
    let nodes = match relay.get_mesh_status().await {
        Ok(n) => n,
        Err(e) => {
            warn!("Peer discovery failed (relay unreachable): {}", e);
            return vec![];
        }
    };

    nodes
        .into_iter()
        .filter(|n| n.instance_id != own_instance_id && n.status == "online")
        .map(|n| {
            let base = format!("http://{}:{}", n.external_ip, n.port);
            debug!("Discovered peer {} at {}", n.instance_id, base);
            MeshClient::new(&base, sync_secret)
        })
        .collect()
}
