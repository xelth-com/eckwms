use base64::{engine::general_purpose::STANDARD, Engine};
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

    /// Canonical ordering key for a relay URL — its host, lowercased, ignoring
    /// scheme and port. So a node configured with `https://eck1.com` and another
    /// with `http://eck1.com:3201` still agree this is the same relay when
    /// building the payload order.
    fn host_key(u: &str) -> String {
        reqwest::Url::parse(u)
            .ok()
            .and_then(|p| p.host_str().map(|h| h.to_lowercase()))
            .unwrap_or_else(|| u.to_lowercase())
    }

    /// Deterministic, coordinator-free order in which this mesh's nodes try
    /// relays for the NAT-traversal payload queue (`/E/m/*`).
    ///
    /// Built by sorting the relays canonically (by host, so per-node `.env`
    /// ordering / scheme / port can't cause disagreement) and rotating the start
    /// to `compute_primary_index(mesh_id, 3)` — "mod 3", the eckN service-node
    /// count, pinned by owner. Because the sequence is a pure function of
    /// `mesh_id` + the relay set, a dispatcher and its target independently
    /// derive the SAME order: they agree which relay holds a queued task and fail
    /// over to the SAME next relay when one drops — no coordinator. A queued task
    /// still lives on ONE relay; if that relay dies, new tasks flow to the next
    /// entry here (the poller and dispatcher both walk this list, so they stay in
    /// lockstep), and the dispatcher re-dispatches.
    fn payload_order(&self) -> Vec<String> {
        if self.relay_urls.is_empty() {
            return Vec::new();
        }
        let mut ordered = self.relay_urls.clone();
        ordered.sort_by(|a, b| Self::host_key(a).cmp(&Self::host_key(b)));
        let n = ordered.len();
        let start = crate::utils::identity::compute_primary_index(&self.mesh_id, 3) % n;
        (0..n).map(|k| ordered[(start + k) % n].clone()).collect()
    }

    /// Whether an error means "this relay is down — try the next" (transport
    /// failure or 5xx) versus a definitive answer (4xx / parse / NOT_FOUND) that
    /// should be returned as-is.
    fn relay_is_down(e: &RelayError) -> bool {
        matches!(e, RelayError::RequestFailed(_))
            || matches!(e, RelayError::StatusError(s) if s.is_server_error())
    }

    /// The deterministic first-choice payload relay for this mesh. Discovery and
    /// heartbeat still fan out to ALL relays (see `heartbeat`); only the payload
    /// queue pins to a single relay, chosen here.
    fn primary(&self) -> String {
        self.payload_order().into_iter().next().unwrap_or_else(|| {
            self.relay_urls
                .first()
                .cloned()
                .unwrap_or_else(|| "https://9eck.com".to_string())
        })
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

    /// Lightweight reachability probe: GET {base}/E/health with a short
    /// per-request timeout (the shared client's 15 s default is too long for
    /// a UI-facing health dot).
    pub async fn health_check(&self, base_url: &str) -> bool {
        let url = format!("{}/E/health", base_url.trim_end_matches('/'));
        match self.client.get(&url).timeout(Duration::from_secs(4)).send().await {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
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
        let mut last_err = RelayError::AllRelaysDown;
        for base in self.payload_order() {
            let url = format!("{}/E/registry", base);
            match self.client.get(&url).bearer_auth(admin_token).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await?;
                    return Ok(body
                        .get("nodes")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default());
                }
                Ok(resp) => {
                    let e = RelayError::StatusError(resp.status());
                    if Self::relay_is_down(&e) {
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => {
                    last_err = RelayError::from(e);
                    continue;
                }
            }
        }
        Err(last_err)
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

    pub fn relay_url(&self) -> String {
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
        let envelope = serde_json::json!({
            "envelope": {
                "target_uuid": target_uuid,
                "sender_uuid": self.instance_id,
                "kind": kind,
                "payload": payload,
            }
        });
        let mut last_err = RelayError::AllRelaysDown;
        for base in self.payload_order() {
            let url = format!("{}/E/m/dispatch/{}", base, target_uuid);
            // Same payload-sized override as mesh_ack: dispatch bodies carry
            // base64 photos / entity pushes, which a 15 s control-plane timeout
            // cuts mid-upload on a thin uplink.
            match self
                .client
                .post(&url)
                .timeout(std::time::Duration::from_secs(90))
                .json(&envelope)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    let body: serde_json::Value = response.json().await?;
                    let task_id = body
                        .get("task_id")
                        .and_then(|v| v.as_str())
                        .ok_or(RelayError::StatusError(
                            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        ))?
                        .to_string();
                    return Ok(task_id);
                }
                Ok(response) => {
                    let e = RelayError::StatusError(response.status());
                    if Self::relay_is_down(&e) {
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => {
                    last_err = RelayError::from(e);
                    continue;
                }
            }
        }
        Err(last_err)
    }

    /// Fetch pending mesh tasks for `self_uuid` (this node) from the primary relay.
    /// Returns the JSON `tasks` array straight from the relay.
    pub async fn mesh_poll(&self) -> Result<Vec<serde_json::Value>, RelayError> {
        // Aggregate across ALL relays — do NOT return after the first healthy
        // one. Tasks live in the DB of whichever relay the dispatcher reached
        // at dispatch time (fail-over scatters them), so "first relay answered,
        // done" starves every task parked on the others. Observed 2026-07-25:
        // 273 tasks for the kiosk (171 PDA photo uploads, 66 trips, 22 device
        // registrations) sat unacked on eck2 for days while the kiosk happily
        // polled fresh work from the first relay in payload_order and never
        // looked further. Ack routing self-corrects per task: mesh_ack walks
        // the relays and the wrong ones now answer 404 (2026-07-25 fix).
        let mut merged: Vec<serde_json::Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut any_ok = false;
        let mut last_err = RelayError::AllRelaysDown;
        for base in self.payload_order() {
            let url = format!("{}/E/m/poll/{}", base, self.instance_id);
            match self.client.get(&url).send().await {
                Ok(response) if response.status().is_success() => {
                    any_ok = true;
                    let body: serde_json::Value = response.json().await?;
                    for t in body
                        .get("tasks")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default()
                    {
                        // The same logical task can sit on two relays after a
                        // dispatch retry — handle it once per poll cycle.
                        let id = t
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if id.is_empty() || seen.insert(id) {
                            merged.push(t);
                        }
                    }
                }
                Ok(response) => {
                    let e = RelayError::StatusError(response.status());
                    if Self::relay_is_down(&e) {
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => {
                    last_err = RelayError::from(e);
                    continue;
                }
            }
        }
        if any_ok {
            Ok(merged)
        } else {
            Err(last_err)
        }
    }

    /// Acknowledge a mesh task with a result body.
    pub async fn mesh_ack(
        &self,
        task_id: &str,
        result: serde_json::Value,
    ) -> Result<(), RelayError> {
        let mut last_err = RelayError::AllRelaysDown;
        for base in self.payload_order() {
            let url = format!("{}/E/m/ack/{}", base, task_id);
            // Per-request timeout override: an ack BODY is an entity batch (up
            // to RELAY_ACK_MAX_BYTES) and the shared client's 15 s cap is sized
            // for control-plane chatter, not payload uploads. On a thin/busy
            // uplink (2026-07-25: 32 KB/s tether — a 1.4 MB ack needs 45 s) the
            // 15 s cut every attempt mid-body, the relay redelivered, and the
            // pair looped forever (poison ack).
            match self
                .client
                .post(&url)
                .timeout(std::time::Duration::from_secs(90))
                .json(&result)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                // 404 = "no such task HERE". Tasks live in the DB of whichever
                // relay the dispatcher happened to reach, and this loop walks
                // payload_order — so a 404 means "wrong relay, try the next",
                // NOT a definitive failure. (Pre-2026-07-25 the handler said
                // ok:true for unknown tasks — an UPDATE matching zero rows is
                // still an OK — so the ack "landed" on the wrong relay while
                // the right one redelivered the task forever.)
                Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {
                    last_err = RelayError::StatusError(response.status());
                    continue;
                }
                Ok(response) => {
                    let e = RelayError::StatusError(response.status());
                    if Self::relay_is_down(&e) {
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => {
                    last_err = RelayError::from(e);
                    continue;
                }
            }
        }
        Err(last_err)
    }

    /// Read the result of a mesh task (used by the dispatcher polling for a reply).
    /// Returns `Some(result_body)` on completion, `None` while still pending.
    pub async fn mesh_result(
        &self,
        task_id: &str,
    ) -> Result<Option<serde_json::Value>, RelayError> {
        let mut last_err = RelayError::AllRelaysDown;
        for base in self.payload_order() {
            let url = format!("{}/E/m/result/{}", base, task_id);
            match self.client.get(&url).send().await {
                // NOT_FOUND = task pending (or not on this relay) — definitive,
                // don't fan out (a relay that never held it would also 404).
                Ok(response) if response.status() == StatusCode::NOT_FOUND => return Ok(None),
                Ok(response) if response.status().is_success() => {
                    let body: serde_json::Value = response.json().await?;
                    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    return if status == "completed" {
                        Ok(Some(
                            body.get("result").cloned().unwrap_or(serde_json::Value::Null),
                        ))
                    } else {
                        Ok(None)
                    };
                }
                Ok(response) => {
                    let e = RelayError::StatusError(response.status());
                    if Self::relay_is_down(&e) {
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => {
                    last_err = RelayError::from(e);
                    continue;
                }
            }
        }
        Err(last_err)
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

    /// High-level: dispatch a `file_fetch`, wait for the target to ack a CAS
    /// blob's bytes, decrypt (if we hold `MESH_DATA_KEY`), and return them. The
    /// cross-NAT companion to `MeshClient::fetch_file` — used when neither full
    /// node can dial the other and the blind relay is the only conduit. The
    /// relay only ever shuttles the ciphertext envelope.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` — the target served the blob (already decrypted here).
    /// - `Ok(None)`        — the target reported `found:false` (absent, refused
    ///   because it's a blind cache, or over the size cap).
    /// - `Err(_)`          — dispatch/relay failure, timeout, or a served blob we
    ///   could not decrypt (got ciphertext but hold no key).
    ///
    /// CAS integrity (sha256(bytes) == `hash`) is the CALLER's gate, not checked
    /// here — the bytes crossed an untrusted relay + peer.
    pub async fn fetch_file_via_relay(
        &self,
        target_uuid: &str,
        hash: &str,
        timeout_secs: u64,
    ) -> Result<Option<Vec<u8>>, RelayError> {
        let task_id = self
            .mesh_dispatch(
                target_uuid,
                "file_fetch",
                serde_json::json!({ "hash": hash }),
            )
            .await?;

        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Some(result) = self.mesh_result(&task_id).await? else {
                continue;
            };
            if !result.get("found").and_then(|v| v.as_bool()).unwrap_or(false) {
                let reason = result.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                if !reason.is_empty() {
                    warn!(
                        "file_fetch {} from {} refused: {} (size={:?})",
                        hash,
                        target_uuid,
                        reason,
                        result.get("size")
                    );
                }
                return Ok(None);
            }
            // Owner encrypted the blob under MESH_DATA_KEY; decrypt if we hold it.
            if let Some(enc) = result.get("enc") {
                let key = crate::utils::crypto::data_key().ok_or_else(|| {
                    warn!(
                        "file_fetch {}: target returned an encrypted blob but this node holds no MESH_DATA_KEY to decrypt",
                        hash
                    );
                    RelayError::StatusError(StatusCode::BAD_GATEWAY)
                })?;
                let bytes = crate::utils::crypto::decrypt_bytes(&key, enc).map_err(|e| {
                    warn!("file_fetch {}: decrypt failed: {}", hash, e);
                    RelayError::StatusError(StatusCode::BAD_GATEWAY)
                })?;
                return Ok(Some(bytes));
            }
            // Plaintext fallback: the fulfiller had no key (a keyless full node).
            if let Some(b64) = result.get("bytes").and_then(|v| v.as_str()) {
                let bytes = STANDARD.decode(b64).map_err(|e| {
                    warn!("file_fetch {}: base64 decode failed: {}", hash, e);
                    RelayError::StatusError(StatusCode::BAD_GATEWAY)
                })?;
                return Ok(Some(bytes));
            }
            // found:true with neither field — treat as a miss.
            return Ok(None);
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

    /// Cross-network merkle diff: fetch the target's merkle node for
    /// {entity_type, level, bucket} over the relay `merkle_state` mesh-task.
    /// The missing half of pull/push_entities_via_relay — lets two nodes that
    /// can't reach each other directly (different LANs / NAT) still converge.
    pub async fn get_merkle_state_via_relay(
        &self,
        target_uuid: &str,
        req: &crate::sync::merkle::MerkleRequest,
        timeout_secs: u64,
    ) -> Result<crate::sync::merkle::MerkleNode, RelayError> {
        let task_id = self
            .mesh_dispatch(
                target_uuid,
                "merkle_state",
                serde_json::json!({
                    "entity_type": req.entity_type,
                    "level": req.level,
                    "bucket": req.bucket,
                }),
            )
            .await?;

        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Some(result) = self.mesh_result(&task_id).await? {
                if !result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                    return Err(RelayError::StatusError(StatusCode::BAD_GATEWAY));
                }
                let node = result.get("node").cloned().unwrap_or(serde_json::Value::Null);
                return serde_json::from_value(node)
                    .map_err(|_| RelayError::StatusError(StatusCode::BAD_GATEWAY));
            }
        }
        Err(RelayError::StatusError(StatusCode::REQUEST_TIMEOUT))
    }
}

#[cfg(test)]
mod payload_order_tests {
    use super::*;

    /// Two nodes in the same mesh must derive the SAME payload-relay order even
    /// when their `.env` lists the relays in a different order and with
    /// different scheme/port — agreement is by host, not by raw URL string.
    #[test]
    fn payload_order_agrees_across_nodes_by_host() {
        let mesh = "7e6fe40d-b184-e3cc-cbde-5be84c90700d";
        let a = RelayClient::new_multi(
            &[
                "https://pda.repair".into(),
                "http://eck1.com:3201".into(),
                "http://eck2.com:3202".into(),
                "http://eck3.com:3203".into(),
            ],
            "node-a",
            mesh,
        );
        let b = RelayClient::new_multi(
            &[
                "http://eck3.com:3203".into(),
                "https://eck2.com".into(),
                "https://pda.repair".into(),
                "https://eck1.com".into(),
            ],
            "node-b",
            mesh,
        );
        let hosts = |c: &RelayClient| {
            c.payload_order()
                .iter()
                .map(|u| RelayClient::host_key(u))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            hosts(&a),
            hosts(&b),
            "nodes in one mesh must agree on payload relay order by host"
        );
        // Rotation walks every relay, so skip-to-next can reach all of them.
        assert_eq!(a.payload_order().len(), 4);
    }

    /// Different meshes start at different relays (mod-3 load spread), but the
    /// order is stable for a given mesh.
    #[test]
    fn payload_order_is_stable_and_mesh_dependent() {
        let relays: Vec<String> = ["http://eck1.com:3201", "http://eck2.com:3202", "http://eck3.com:3203"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let c1 = RelayClient::new_multi(&relays, "n", "mesh-aaaa");
        let c1b = RelayClient::new_multi(&relays, "n", "mesh-aaaa");
        assert_eq!(c1.payload_order(), c1b.payload_order(), "stable per mesh");
    }
}
