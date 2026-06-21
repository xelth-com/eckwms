use chrono::{DateTime, Utc};
use futures_util::future::join_all;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Error, Debug)]
pub enum RelayError {
    #[error("HTTP request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
    #[error("Relay returned error status: {0}")]
    StatusError(StatusCode),
    #[error("Serialization failed: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("All relays unreachable")]
    AllRelaysDown,
}

#[derive(Debug, Serialize)]
struct RelayRegisterRequest {
    pub instance_id: String,
    pub mesh_id: String,
    pub external_ip: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `"full"` (default) or `"cache"`. Peers use this to skip the periodic
    /// merkle sync against cache nodes — they own only their pulled subset,
    /// so symmetric drill-down would falsely report drift on every cycle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_role: Option<String>,
    /// 9eck product license token (see `crate::licensing`). Read from
    /// `ECK_LICENSE_TOKEN`; lets a paid relay tag us as `paid` so we can use the
    /// payload queue. Omitted when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Private/LAN address (from `LAN_BASE_URL`) advertised alongside the public
    /// `base_url`. Co-located peers prefer this fast path (B3). Omitted when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lan_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RelayRegisterResponse {
    pub ok: bool,
    pub instance_id: String,
    pub mesh_id: String,
    pub status: String,
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
    #[serde(default)]
    pub base_url: Option<String>,
    /// `"full"` (default if missing) or `"cache"`. Mesh sync uses this to
    /// decide whether to push to / drill into a peer.
    #[serde(default)]
    pub node_role: Option<String>,
    /// Peer's private/LAN address, if it advertised one. `discover_peers`
    /// prefers it over `base_url` when reachable (B3 fast-path).
    #[serde(default)]
    pub lan_url: Option<String>,
}

/// Client for one or more relay "bulletin boards".
///
/// Discovery (heartbeat + `get_mesh_status` + `resolve_node`) **fans out across
/// ALL** configured relays so that the failure or overload of one or two does
/// not isolate the node — the HA property the paid (`eck1/eck2/eck3`) tier
/// needs (see B2 in `.eck/ENTERPRISE_CLUSTER.md`). The board is "duplicated" via
/// this client-side fan-out: every node heartbeats to every relay, so each relay
/// independently holds the full picture — no server-to-server replication needed.
///
/// The NAT-traversal **payload queue** (`/E/m/*`) is a separate concern and stays
/// pinned to the **primary** relay (`relay_urls[0]`): a queued task lives on one
/// relay, and dispatch/ack/result must agree on which.
#[derive(Clone)]
pub struct RelayClient {
    client: Client,
    relay_urls: Vec<String>,
    instance_id: String,
    mesh_id: String,
}

impl RelayClient {
    /// Single-relay constructor (back-compat). Prefer [`new_multi`].
    pub fn new(relay_url: &str, instance_id: &str, mesh_id: &str) -> Self {
        Self::new_multi(&[relay_url.to_string()], instance_id, mesh_id)
    }

    /// Build a client that fans discovery out across several relays. URLs are
    /// trimmed, de-duplicated (order preserved), and the first becomes the
    /// primary (payload queue). An empty list falls back to `https://9eck.com`.
    pub fn new_multi(relay_urls: &[String], instance_id: &str, mesh_id: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        let mut seen = std::collections::HashSet::new();
        let mut urls: Vec<String> = relay_urls
            .iter()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .filter(|u| seen.insert(u.clone()))
            .collect();
        if urls.is_empty() {
            urls.push("https://9eck.com".to_string());
        }

        Self {
            client,
            relay_urls: urls,
            instance_id: instance_id.to_string(),
            mesh_id: mesh_id.to_string(),
        }
    }

    /// Relay URL list from env: `RELAY_URLS` (comma-separated) takes precedence,
    /// else single `RELAY_URL`, else the public default `https://9eck.com`.
    pub fn relay_urls_from_env() -> Vec<String> {
        if let Ok(multi) = std::env::var("RELAY_URLS") {
            let v: Vec<String> = multi
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
        if let Ok(single) = std::env::var("RELAY_URL") {
            let s = single.trim().to_string();
            if !s.is_empty() {
                return vec![s];
            }
        }
        vec!["https://9eck.com".to_string()]
    }

    /// The primary relay — hosts this node's payload queue (`/E/m/*`).
    fn primary(&self) -> &str {
        &self.relay_urls[0]
    }

    /// Send one register/heartbeat to a single relay base URL.
    async fn heartbeat_one(
        &self,
        base: &str,
        payload: &RelayRegisterRequest,
    ) -> Result<RelayRegisterResponse, RelayError> {
        let url = format!("{}/E/register", base);
        let response = self.client.post(&url).json(payload).send().await?;
        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }
        Ok(response.json().await?)
    }

    /// Sends a heartbeat (register) to **every** configured relay so other nodes
    /// can discover us on any of them. Succeeds if at least one relay accepts;
    /// failures against the others are logged, not fatal.
    pub async fn send_heartbeat(
        &self,
        external_ip: &str,
        port: u16,
        status: Option<&str>,
        base_url: Option<&str>,
        node_role: Option<&str>,
    ) -> Result<RelayRegisterResponse, RelayError> {
        let payload = RelayRegisterRequest {
            instance_id: self.instance_id.clone(),
            mesh_id: self.mesh_id.clone(),
            external_ip: external_ip.to_string(),
            port,
            status: status.map(|s| s.to_string()),
            base_url: base_url.filter(|s| !s.is_empty()).map(|s| s.to_string()),
            node_role: node_role.filter(|s| !s.is_empty()).map(|s| s.to_string()),
            license: std::env::var("ECK_LICENSE_TOKEN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            lan_url: std::env::var("LAN_BASE_URL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        };

        let futs = self
            .relay_urls
            .iter()
            .map(|base| async { (base.as_str(), self.heartbeat_one(base, &payload).await) });
        let results = join_all(futs).await;

        let mut first_ok: Option<RelayRegisterResponse> = None;
        let mut last_err: Option<RelayError> = None;
        for (base, res) in results {
            match res {
                Ok(r) => {
                    if first_ok.is_none() {
                        first_ok = Some(r);
                    }
                }
                Err(e) => {
                    warn!("Heartbeat to {} failed: {}", base, e);
                    last_err = Some(e);
                }
            }
        }
        match first_ok {
            Some(r) => {
                info!(
                    "Heartbeat OK: [{}] {} -> {} ({} relay(s))",
                    self.mesh_id,
                    self.instance_id,
                    r.status,
                    self.relay_urls.len()
                );
                Ok(r)
            }
            None => Err(last_err.unwrap_or(RelayError::AllRelaysDown)),
        }
    }

    /// Gets the union of nodes registered in our mesh across **all** relays.
    /// Deduped by `instance_id`, keeping the freshest sighting (and preferring an
    /// `online` status). Succeeds if at least one relay responds.
    pub async fn get_mesh_status(&self) -> Result<Vec<RelayNodeInfo>, RelayError> {
        let futs = self.relay_urls.iter().map(|base| {
            let url = format!("{}/E/mesh/{}/status", base, self.mesh_id);
            async move {
                let resp = self.client.get(&url).send().await?;
                if !resp.status().is_success() {
                    return Err(RelayError::StatusError(resp.status()));
                }
                let parsed: MeshStatusResponse = resp.json().await?;
                Ok::<_, RelayError>(parsed.nodes)
            }
        });
        let results = join_all(futs).await;

        let mut merged: HashMap<String, RelayNodeInfo> = HashMap::new();
        let mut any_ok = false;
        let mut last_err: Option<RelayError> = None;
        for res in results {
            match res {
                Ok(nodes) => {
                    any_ok = true;
                    for n in nodes {
                        match merged.entry(n.instance_id.clone()) {
                            Entry::Occupied(mut e) => {
                                let cur = e.get();
                                let newer = n.last_seen > cur.last_seen;
                                let upgrade = cur.status != "online" && n.status == "online";
                                if newer || upgrade {
                                    e.insert(n);
                                }
                            }
                            Entry::Vacant(e) => {
                                e.insert(n);
                            }
                        }
                    }
                }
                Err(e) => last_err = Some(e),
            }
        }

        if !any_ok {
            return Err(last_err.unwrap_or(RelayError::AllRelaysDown));
        }

        let nodes: Vec<RelayNodeInfo> = merged.into_values().collect();
        info!(
            "Mesh status: [{}] {} nodes (union of {} relay(s))",
            self.mesh_id,
            nodes.len(),
            self.relay_urls.len()
        );
        Ok(nodes)
    }

    /// Fetch the cross-mesh node registry from the primary relay's `/E/registry`
    /// (gated there by `RELAY_ADMIN_TOKEN`, which we present as a bearer token).
    /// Used by the WMS admin proxy so the cloud UI can list nodes across meshes.
    pub async fn fetch_registry(
        &self,
        admin_token: &str,
    ) -> Result<Vec<serde_json::Value>, RelayError> {
        let url = format!("{}/E/registry", self.primary());
        let resp = self.client.get(&url).bearer_auth(admin_token).send().await?;
        if !resp.status().is_success() {
            return Err(RelayError::StatusError(resp.status()));
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(body
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Resolves a specific node's address, trying each relay until one knows it.
    /// Returns `Ok(None)` if any relay definitively answered NOT_FOUND and none
    /// resolved it; surfaces a transport error only if every relay failed.
    pub async fn resolve_node(
        &self,
        instance_id: &str,
    ) -> Result<Option<RelayNodeInfo>, RelayError> {
        let mut saw_not_found = false;
        let mut last_err: Option<RelayError> = None;
        for base in &self.relay_urls {
            let url = format!("{}/E/mesh/{}/resolve/{}", base, self.mesh_id, instance_id);
            match self.client.get(&url).send().await {
                Ok(resp) if resp.status() == StatusCode::NOT_FOUND => {
                    saw_not_found = true;
                }
                Ok(resp) if resp.status().is_success() => match resp.json::<RelayNodeInfo>().await {
                    Ok(node) => return Ok(Some(node)),
                    Err(e) => last_err = Some(RelayError::from(e)),
                },
                Ok(resp) => last_err = Some(RelayError::StatusError(resp.status())),
                Err(e) => last_err = Some(RelayError::from(e)),
            }
        }
        if saw_not_found {
            Ok(None)
        } else {
            match last_err {
                Some(e) => Err(e),
                None => Ok(None),
            }
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn relay_url(&self) -> &str {
        self.primary()
    }

    pub fn mesh_id(&self) -> &str {
        &self.mesh_id
    }

    // ─── Mesh-sync queue (NAT-traversal fallback) ─────────────────────────
    // These wrap /E/m/dispatch + /E/m/poll + /E/m/ack + /E/m/result on the
    // PRIMARY relay only — a queued task lives on one relay, so dispatch/poll/
    // ack/result must all target the same one. The relay is a dumb queue;
    // semantics are interpreted by the receiving WMS poller (`mesh_relay_poller`).

    /// Queue a mesh task for `target_uuid`. Returns the relay-assigned task_id.
    /// `kind` is one of: "pull_request", "push". `payload` shape:
    ///   pull_request: { entity_type, ids: [String] }
    ///   push:         { entity_type, entities: [Value] }
    pub async fn mesh_dispatch(
        &self,
        target_uuid: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<String, RelayError> {
        let url = format!("{}/E/m/dispatch/{}", self.primary(), target_uuid);
        let envelope = serde_json::json!({
            "envelope": {
                "target_uuid": target_uuid,
                "sender_uuid": self.instance_id,
                "kind": kind,
                "payload": payload,
            }
        });
        let response = self.client.post(&url).json(&envelope).send().await?;
        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }
        let body: serde_json::Value = response.json().await?;
        let task_id = body
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RelayError::StatusError(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
            })?
            .to_string();
        Ok(task_id)
    }

    /// Fetch pending mesh tasks for `self_uuid` (this node) from the primary relay.
    /// Returns the JSON `tasks` array straight from the relay.
    pub async fn mesh_poll(&self) -> Result<Vec<serde_json::Value>, RelayError> {
        let url = format!("{}/E/m/poll/{}", self.primary(), self.instance_id);
        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }
        let body: serde_json::Value = response.json().await?;
        let tasks = body
            .get("tasks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tasks)
    }

    /// Acknowledge a mesh task with a result body.
    pub async fn mesh_ack(
        &self,
        task_id: &str,
        result: serde_json::Value,
    ) -> Result<(), RelayError> {
        let url = format!("{}/E/m/ack/{}", self.primary(), task_id);
        let response = self.client.post(&url).json(&result).send().await?;
        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }
        Ok(())
    }

    /// Read the result of a mesh task (used by the dispatcher polling for a reply).
    /// Returns `Some(result_body)` on completion, `None` while still pending.
    pub async fn mesh_result(
        &self,
        task_id: &str,
    ) -> Result<Option<serde_json::Value>, RelayError> {
        let url = format!("{}/E/m/result/{}", self.primary(), task_id);
        let response = self.client.get(&url).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(RelayError::StatusError(response.status()));
        }
        let body: serde_json::Value = response.json().await?;
        let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "completed" {
            Ok(Some(body.get("result").cloned().unwrap_or(serde_json::Value::Null)))
        } else {
            Ok(None)
        }
    }

    /// High-level: dispatch a pull_request, wait for the target's ack, return the
    /// entities the target served. Used as the cross-NAT fallback when direct
    /// HTTP to the peer fails.
    pub async fn pull_entities_via_relay(
        &self,
        target_uuid: &str,
        entity_type: &str,
        ids: &[String],
        timeout_secs: u64,
    ) -> Result<Vec<serde_json::Value>, RelayError> {
        let task_id = self
            .mesh_dispatch(
                target_uuid,
                "pull_request",
                serde_json::json!({
                    "entity_type": entity_type,
                    "ids": ids,
                }),
            )
            .await?;

        let start = std::time::Instant::now();
        let deadline = start + Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Some(result) = self.mesh_result(&task_id).await? {
                let entities = result
                    .get("entities")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                // Blind-cache: the fulfiller encrypted each entity. A node that
                // holds MESH_DATA_KEY decrypts here; a cache node (no key) keeps
                // the ciphertext envelopes and stores/serves them as-is.
                let entities = match crate::utils::crypto::data_key() {
                    Some(key) => entities
                        .into_iter()
                        .map(|e| {
                            if crate::utils::crypto::is_encrypted(&e) {
                                crate::utils::crypto::decrypt_json(&key, &e).unwrap_or(e)
                            } else {
                                e
                            }
                        })
                        .collect(),
                    None => entities,
                };
                return Ok(entities);
            }
        }
        Err(RelayError::StatusError(StatusCode::REQUEST_TIMEOUT))
    }

    /// High-level: dispatch a push, wait briefly for ack, return applied count.
    /// On timeout, returns 0 (best-effort — the target may still apply later).
    pub async fn push_entities_via_relay(
        &self,
        target_uuid: &str,
        entity_type: &str,
        entities: &[serde_json::Value],
        timeout_secs: u64,
    ) -> Result<usize, RelayError> {
        let task_id = self
            .mesh_dispatch(
                target_uuid,
                "push",
                serde_json::json!({
                    "entity_type": entity_type,
                    "entities": entities,
                    "source_instance": self.instance_id,
                }),
            )
            .await?;

        let start = std::time::Instant::now();
        let deadline = start + Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Some(result) = self.mesh_result(&task_id).await? {
                let applied = result
                    .get("applied")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                return Ok(applied);
            }
        }
        Ok(0)
    }
}
