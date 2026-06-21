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
    /// `"full"` (default) or `"cache"`. Set from the relay's registration row
    /// during discovery so the sync engine can skip cache peers in the
    /// periodic loop — they own no canonical data.
    node_role: String,
    /// Peer's instance_id (UUID). Needed for relay-routed fallback when direct
    /// HTTP fails — the relay's mesh queue is addressed by UUID, not URL.
    target_instance_id: String,
}

impl MeshClient {
    pub fn new(peer_base_url: &str, sync_secret: Option<&str>) -> Self {
        Self::new_full(peer_base_url, sync_secret, "full", "")
    }

    pub fn new_with_role(peer_base_url: &str, sync_secret: Option<&str>, role: &str) -> Self {
        Self::new_full(peer_base_url, sync_secret, role, "")
    }

    pub fn new_full(
        peer_base_url: &str,
        sync_secret: Option<&str>,
        role: &str,
        target_instance_id: &str,
    ) -> Self {
        // 30s was inherited but is far too long when N entity_types × 60s
        // cycle means a single unreachable peer stalls every cycle for 19×30s
        // = ~10 min on this codebase. Cross-NAT setups expect some peers to
        // be unreachable at any given time — fail fast.
        //
        // 5s connect + 8s overall is enough for healthy local LAN (<50ms) and
        // healthy public HTTPS (typically 200–800ms across continents) while
        // still cutting noise from dead peers by ~3.75×.
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap_or_default();

        Self {
            client,
            peer_base_url: peer_base_url.trim_end_matches('/').to_string(),
            sync_secret: sync_secret.map(|s| s.to_string()),
            node_role: role.to_string(),
            target_instance_id: target_instance_id.to_string(),
        }
    }

    pub fn peer_url(&self) -> &str {
        &self.peer_base_url
    }

    pub fn target_instance_id(&self) -> &str {
        &self.target_instance_id
    }

    pub fn is_cache(&self) -> bool {
        self.node_role == "cache"
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
            Some(Value::Array(arr)) => {
                // Blind-cache: the fulfiller encrypted each entity if it holds
                // MESH_DATA_KEY. A node with the key decrypts here; a cache node
                // (no key) keeps the ciphertext envelopes and stores them as-is.
                // Mirrors relay_client::pull_entities_via_relay.
                let entities = match crate::utils::crypto::data_key() {
                    Some(key) => arr
                        .iter()
                        .map(|e| {
                            if crate::utils::crypto::is_encrypted(e) {
                                crate::utils::crypto::decrypt_json(&key, e).unwrap_or_else(|_| e.clone())
                            } else {
                                e.clone()
                            }
                        })
                        .collect(),
                    None => arr.clone(),
                };
                Ok(entities)
            }
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

    // ─── Task Queue (Reverse-Fetch) ────────────────────────────────────────

    /// Fetch pending tasks assigned to us from a peer node.
    pub async fn fetch_tasks(&self, my_instance_id: &str) -> Result<Vec<Value>, String> {
        let url = format!(
            "{}/api/mesh/tasks?instance_id={}",
            self.peer_base_url, my_instance_id
        );

        let mut http_req = self.client.get(&url);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("Tasks GET failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Tasks GET {} returned {}", url, resp.status()));
        }

        resp.json::<Vec<Value>>()
            .await
            .map_err(|e| format!("Tasks response parse failed: {}", e))
    }

    /// Delete a completed task from a peer node.
    pub async fn complete_task(&self, task_id: &str) -> Result<(), String> {
        let url = format!("{}/api/mesh/tasks/{}", self.peer_base_url, task_id);

        let mut http_req = self.client.delete(&url);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("Task DELETE failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Task DELETE {} returned {}", url, resp.status()));
        }

        Ok(())
    }

    // ─── Raw Document Fetch ─────────────────────────────────────────────────

    /// Fetch raw document payloads for a ticket from a peer (fat node).
    /// Returns the ticket + all thread document_raw records.
    pub async fn fetch_raw_docs(&self, ticket_id: &str) -> Result<Value, String> {
        let url = format!("{}/api/mesh/raw-docs/{}", self.peer_base_url, ticket_id);

        let mut http_req = self.client.get(&url);
        if let Some(ref auth) = self.auth_header() {
            http_req = http_req.header("authorization", auth);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| format!("Raw docs GET failed ({}): {}", url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Raw docs GET {} returned {}", url, resp.status()));
        }

        resp.json::<Value>()
            .await
            .map_err(|e| format!("Raw docs response parse failed: {}", e))
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

    // Short, dedicated probe client for the LAN fast-path (B3): a peer that
    // advertised a `lan_url` is preferred over its public `base_url` IF we can
    // reach the private address quickly. Re-evaluated every discovery cycle, so
    // it self-heals when the LAN path appears/disappears. Probes run concurrently.
    let probe = Client::builder()
        .connect_timeout(Duration::from_millis(800))
        .timeout(Duration::from_millis(1200))
        .build()
        .unwrap_or_default();

    let online = nodes
        .into_iter()
        .filter(|n| n.instance_id != own_instance_id && n.status == "online");

    let futs = online.map(|n| {
        let probe = probe.clone();
        async move {
            let public = match &n.base_url {
                Some(url) if !url.is_empty() => url.clone(),
                _ => format!("http://{}:{}", n.external_ip, n.port),
            };
            let base = match n.lan_url.as_deref() {
                Some(lan) if !lan.is_empty() && lan.trim_end_matches('/') != public => {
                    let lan = lan.trim_end_matches('/');
                    let healthy = probe
                        .get(format!("{}/E/health", lan))
                        .send()
                        .await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false);
                    if healthy {
                        debug!("peer {} reachable via LAN fast-path {}", n.instance_id, lan);
                        lan.to_string()
                    } else {
                        public
                    }
                }
                _ => public,
            };
            let role = n.node_role.as_deref().unwrap_or("full");
            debug!("Discovered peer {} at {} (role={})", n.instance_id, base, role);
            MeshClient::new_full(&base, sync_secret, role, &n.instance_id)
        }
    });

    futures_util::future::join_all(futs).await
}
