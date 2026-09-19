//! Client side of the "safe" protocol — a full node talking to the eck1/2/3
//! vault servers (`.eck/PII_VAULT.md` §2/§5). The node splits/rebuilds the blob
//! locally (see [`super::shamir`]); the servers only ever hold opaque shares.
//!
//! Wire shapes here MUST match the server side (`wms/src/handlers/vault_safe.rs`,
//! contract §2.1). The request/response structs and [`signing_message`] are the
//! single source of truth both sides share — `PutSharesRequest`,
//! `NodeActionRequest`, `VaultNodeRow`, and the `ACTION_*` constants are `pub` so
//! the wms handler can reuse them.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{blob, kek, shamir};
use crate::utils::identity;

/// Re-fetch cadence for the safe (honours revocation within an hour).
pub const SAFE_TTL_SECS: u64 = 3600;

/// Per-request timeout to a single safe server.
const REQ_TIMEOUT: Duration = Duration::from_secs(6);

/// Canonical `{action}` field values for [`signing_message`] (contract §2).
pub const ACTION_PUT_SHARE: &str = "put_share";
pub const ACTION_REGISTER: &str = "register";
pub const ACTION_REVOKE: &str = "revoke";
/// Arms an already-ACTIVE node to adopt a re-sealed blob after a key rotation
/// (P0.7, `.eck/PII_VAULT.md` §2 "Rotation"): it sets the node's `enroll_proof`
/// so it may fetch the new enroll share, and changes nothing else about the row.
/// Only an active `home` may sign it, and only for an active node.
pub const ACTION_REKEY: &str = "rekey";

// ─── wire bodies (shared with the server) ─────────────────────────────────────

/// `PUT /api/vault/share` body (contract §2.1). One request per server; the
/// `share_index`/`share`/`sig`/`ts` fields are per-node (see [`SafeClient::put_shares`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutSharesRequest {
    pub node_id: String,
    /// `"keys" | "recovery" | "enroll"`.
    pub kind: String,
    /// Shamir x-coordinate (`1..=n`). Filled per server by `put_shares`.
    #[serde(default)]
    pub share_index: u8,
    /// Base64 share bytes. Filled per server by `put_shares`.
    #[serde(default)]
    pub share: String,
    pub blob_len: u32,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
    pub signer_node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_pubkey: Option<String>,
    pub ts: i64,
    /// Base64 Ed25519 signature over [`signing_message`]. Filled per server.
    #[serde(default)]
    pub sig: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_proof: Option<String>,
    /// Enroll-completion only: the enroll proof preimage
    /// (`hex(sha256("9eck-vault-enroll-v1" || code))`) the newcomer presents to
    /// prove code possession before its pubkey is bound. Not in the signed message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enroll_proof: Option<String>,
}

/// `POST /api/vault/node` body (contract §2.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeActionRequest {
    pub node_id: String,
    /// `"register" | "revoke"`.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub signer_node_id: String,
    pub ts: i64,
    pub sig: String,
    /// `register` only: `hex(sha256("9eck-vault-enroll-v1" || code))` — the enroll
    /// proof preimage the newcomer must later present. The server stores its
    /// SHA-256 hash on the pending row (not in the signed message; see contract).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enroll_proof: Option<String>,
}

/// One entry of `GET /api/vault/nodes` (contract §2.1) — never shares, never proofs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultNodeRow {
    pub node_id: String,
    pub role: String,
    pub status: String,
    pub has_pubkey: bool,
    #[serde(default)]
    pub created_at: Option<serde_json::Value>,
    #[serde(default)]
    pub revoked_at: Option<serde_json::Value>,
}

/// `POST /api/vault/share` read-response body (contract §2.1).
#[derive(Debug, Clone, Deserialize)]
struct ShareResponse {
    share_index: u8,
    share: String,
    #[allow(dead_code)]
    blob_len: u32,
    version: u32,
}

// ─── canonical signed message ─────────────────────────────────────────────────

/// The canonical Ed25519 message for a share PUT or a node action (contract §2):
/// `"9eck-vault-v1\n{tenant}\n{action}\n{node_id}\n{kind}\n{share_index}\n{version}\n{ts}"`.
/// Fields not applicable to an action are the empty string.
pub fn signing_message(
    tenant: &str,
    action: &str,
    node_id: &str,
    kind: &str,
    share_index: &str,
    version: &str,
    ts: i64,
) -> String {
    format!("9eck-vault-v1\n{tenant}\n{action}\n{node_id}\n{kind}\n{share_index}\n{version}\n{ts}")
}

/// Lenient tenant (`sub`) extraction from a license token — decodes the JWS-lite
/// payload without verifying (the safe server verifies the license itself). Used
/// to build the AAD / signing message. `None` if the token is malformed.
pub fn tenant_from_license(license: &str) -> Option<String> {
    let payload_b64 = license.split('.').next()?;
    let bytes = URL_SAFE_NO_PAD.decode(payload_b64.trim()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("sub").and_then(|s| s.as_str()).map(|s| s.to_string())
}

// ─── safe client ──────────────────────────────────────────────────────────────

/// The signing identity a full node uses to authenticate writes to the safe.
struct Signer {
    node_id: String,
    priv_key_b64: String,
    tenant: String,
}

/// A client over the tenant's safe servers.
pub struct SafeClient {
    http: reqwest::Client,
    nodes: Vec<String>,
    license: String,
    signer: Option<Signer>,
}

/// Parse `ECK_VAULT_NODES` (comma-separated base URLs) into a trimmed, non-empty list.
pub fn safe_nodes_from_env() -> Vec<String> {
    std::env::var("ECK_VAULT_NODES")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

impl SafeClient {
    /// Build from the environment. `None` if `ECK_VAULT_NODES` is empty or
    /// `ECK_LICENSE_TOKEN` is unset. No signer is attached — reads work without
    /// one; call [`SafeClient::with_signer`] before any write.
    pub fn from_env(http: reqwest::Client) -> Option<Self> {
        let nodes = safe_nodes_from_env();
        if nodes.is_empty() {
            return None;
        }
        let license = std::env::var("ECK_LICENSE_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())?;
        Some(SafeClient {
            http,
            nodes,
            license,
            signer: None,
        })
    }

    /// Attach the signing identity (node's Ed25519 private key, base64) used to
    /// authenticate writes. `tenant` is `LicenseClaims.sub`.
    pub fn with_signer(mut self, node_id: &str, priv_key_b64: &str, tenant: &str) -> Self {
        self.signer = Some(Signer {
            node_id: node_id.to_string(),
            priv_key_b64: priv_key_b64.to_string(),
            tenant: tenant.to_string(),
        });
        self
    }

    fn share_url(node: &str) -> String {
        format!("{node}/api/vault/share")
    }

    /// Read one `kind` share from each server in parallel; returns the shares
    /// that answered 200 (as `(node_url, Share, version)`).
    pub async fn fetch_shares(
        &self,
        node_id: &str,
        kind: &str,
        proof: Option<&str>,
    ) -> Vec<(String, shamir::Share, u32)> {
        let mut nonce = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let boot_nonce = hex::encode(nonce);
        let body = serde_json::json!({
            "node_id": node_id,
            "kind": kind,
            "boot_nonce": boot_nonce,
            "proof": proof,
        });

        let futures = self.nodes.iter().map(|node| {
            let http = self.http.clone();
            let license = self.license.clone();
            let url = Self::share_url(node);
            let node = node.clone();
            let body = body.clone();
            async move {
                let res = http
                    .post(&url)
                    .timeout(REQ_TIMEOUT)
                    .bearer_auth(&license)
                    .json(&body)
                    .send()
                    .await
                    .ok()?;
                if !res.status().is_success() {
                    return None;
                }
                let sr: ShareResponse = res.json().await.ok()?;
                let data = STANDARD.decode(sr.share.trim()).ok()?;
                Some((
                    node,
                    shamir::Share {
                        index: sr.share_index,
                        data,
                    },
                    sr.version,
                ))
            }
        });
        futures_util::future::join_all(futures)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    /// PUT one share per server (`shares[i]` → `nodes[i]`), each signed for its
    /// own `share_index`. `Ok(n)` where `n >= 2` servers accepted; `Err` otherwise.
    /// Requires a signer (attach via [`SafeClient::with_signer`]).
    pub async fn put_shares(
        &self,
        req: &PutSharesRequest,
        shares: &[shamir::Share],
    ) -> Result<usize, String> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| "put_shares requires a signer (call with_signer)".to_string())?;
        if shares.len() > self.nodes.len() {
            return Err(format!(
                "have {} shares but only {} safe nodes",
                shares.len(),
                self.nodes.len()
            ));
        }
        let ts = chrono::Utc::now().timestamp();

        let mut ok = 0usize;
        let mut errs = Vec::new();
        for (node, share) in self.nodes.iter().zip(shares.iter()) {
            let mut body = req.clone();
            body.share_index = share.index;
            body.share = STANDARD.encode(&share.data);
            body.ts = ts;
            body.signer_node_id = signer.node_id.clone();
            let msg = signing_message(
                &signer.tenant,
                ACTION_PUT_SHARE,
                &req.node_id,
                &req.kind,
                &share.index.to_string(),
                &req.version.to_string(),
                ts,
            );
            body.sig = match identity::sign_message(&signer.priv_key_b64, &msg) {
                Ok(s) => s,
                Err(e) => return Err(format!("sign: {e}")),
            };
            match self.send_put(node, &body).await {
                Ok(()) => ok += 1,
                Err(e) => {
                    warn!("vault put_shares to {node} failed: {e}");
                    errs.push(format!("{node}: {e}"));
                }
            }
        }
        if ok >= 2 {
            Ok(ok)
        } else {
            Err(format!("only {ok} server(s) accepted the shares: {}", errs.join("; ")))
        }
    }

    async fn send_put(&self, node: &str, body: &PutSharesRequest) -> Result<(), String> {
        let res = self
            .http
            .put(Self::share_url(node))
            .timeout(REQ_TIMEOUT)
            .bearer_auth(&self.license)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("transport: {e}"))?;
        if res.status().is_success() {
            Ok(())
        } else {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            Err(format!("{status}: {text}"))
        }
    }

    /// POST a node action (register/revoke) to every server; the request is
    /// already signed by the caller (the message has no per-node variation).
    /// `Ok(n)` if `n >= 2` accepted.
    pub async fn node_action(&self, req: &NodeActionRequest) -> Result<usize, String> {
        let futures = self.nodes.iter().map(|node| {
            let http = self.http.clone();
            let license = self.license.clone();
            let url = format!("{node}/api/vault/node");
            let node = node.clone();
            async move {
                match http
                    .post(&url)
                    .timeout(REQ_TIMEOUT)
                    .bearer_auth(&license)
                    .json(req)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => Ok(()),
                    Ok(r) => Err(format!("{node}: {}", r.status())),
                    Err(e) => Err(format!("{node}: {e}")),
                }
            }
        });
        let results = futures_util::future::join_all(futures).await;
        let ok = results.iter().filter(|r| r.is_ok()).count();
        if ok >= 2 {
            Ok(ok)
        } else {
            let errs: Vec<String> = results.into_iter().filter_map(|r| r.err()).collect();
            Err(format!("only {ok} server(s) accepted the action: {}", errs.join("; ")))
        }
    }

    /// List the tenant's registered nodes from the first server that answers.
    pub async fn list_nodes(&self) -> Result<Vec<VaultNodeRow>, String> {
        let mut last_err = String::from("no safe nodes configured");
        for node in &self.nodes {
            let url = format!("{node}/api/vault/nodes");
            match self
                .http
                .get(&url)
                .timeout(REQ_TIMEOUT)
                .bearer_auth(&self.license)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => match r.json::<Vec<VaultNodeRow>>().await {
                    Ok(rows) => return Ok(rows),
                    Err(e) => last_err = format!("{node}: decode {e}"),
                },
                Ok(r) => last_err = format!("{node}: {}", r.status()),
                Err(e) => last_err = format!("{node}: {e}"),
            }
        }
        Err(last_err)
    }
}

// ─── boot path ────────────────────────────────────────────────────────────────

/// Fetch >= 2 `keys` shares, rebuild + open the blob with this node's KEK, and
/// return the sealed payload plus its version. Used by the keyring boot path.
pub(crate) async fn load_payload_from_safe(
    http: &reqwest::Client,
    node_id: &str,
    tenant_hint: Option<&str>,
) -> Result<(blob::VaultPayload, u32), String> {
    let client = SafeClient::from_env(http.clone())
        .ok_or_else(|| "ECK_VAULT_NODES or ECK_LICENSE_TOKEN not set".to_string())?;
    let tenant = match tenant_hint {
        Some(t) => t.to_string(),
        None => tenant_from_license(&client.license)
            .ok_or_else(|| "could not parse tenant (sub) from ECK_LICENSE_TOKEN".to_string())?,
    };

    let fetched = client.fetch_shares(node_id, "keys", None).await;
    // Keep only shares of the newest version — mixing shares from two different
    // blob versions (a partially-applied PUT) would combine to garbage.
    let version = fetched.iter().map(|(_, _, v)| *v).max().unwrap_or(0);
    let shares: Vec<shamir::Share> = fetched
        .into_iter()
        .filter(|(_, _, v)| *v == version)
        .map(|(_, s, _)| s)
        .collect();
    if shares.len() < 2 {
        return Err(format!(
            "safe returned {} keys-share(s) at version {version}; need >= 2",
            shares.len()
        ));
    }
    // A Shamir share's byte length equals the secret (blob) length.
    let secret_len = shares.iter().map(|s| s.data.len()).max().unwrap_or(0);
    let blob_bytes = shamir::combine(&shares, secret_len)?;

    let kek = kek::load_or_create_kek()?;
    let aad = blob::aad(&tenant, node_id, "keys");
    let payload = blob::open(&blob_bytes, &kek, &aad)?;
    debug!(
        "vault blob opened (pp_epoch={}, mdk_epoch={}, vk_epoch={}, version={version})",
        payload.pp_epoch, payload.mdk_epoch, payload.vk_epoch
    );
    Ok((payload, version))
}

/// Full boot path used by the keyring: fetch → combine → open → derive.
/// `Err` on any failure (the keyring then falls back to env with an ERROR log).
pub async fn load_from_safe(
    http: &reqwest::Client,
    node_id: &str,
    tenant_hint: Option<&str>,
) -> Result<(blob::Derived, u32), String> {
    let (payload, version) = load_payload_from_safe(http, node_id, tenant_hint).await?;
    Ok((blob::derive(&payload), version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_nodes_parse_trims_and_drops_empty() {
        std::env::set_var(
            "ECK_VAULT_NODES",
            " https://eck1.9eck.com:3201/ , ,https://eck2.9eck.com:3201 ",
        );
        let nodes = safe_nodes_from_env();
        assert_eq!(
            nodes,
            vec![
                "https://eck1.9eck.com:3201".to_string(),
                "https://eck2.9eck.com:3201".to_string(),
            ]
        );
        std::env::remove_var("ECK_VAULT_NODES");
    }

    #[test]
    fn signing_message_is_stable() {
        let m = signing_message("tenant-1", ACTION_PUT_SHARE, "node-1", "keys", "2", "5", 1000);
        assert_eq!(
            m,
            "9eck-vault-v1\ntenant-1\nput_share\nnode-1\nkeys\n2\n5\n1000"
        );
    }

    #[test]
    fn tenant_from_license_reads_sub() {
        // JWS-lite: base64url(payload).sig — matches eck_core::licensing::issue.
        let claims = serde_json::json!({
            "tenant": "acme", "tier": "paid", "sub": "mesh-abc",
            "scopes": [], "iat": 0, "exp": 9999999999i64
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{payload_b64}.deadbeef");
        assert_eq!(tenant_from_license(&token).as_deref(), Some("mesh-abc"));
        assert!(tenant_from_license("garbage").is_none());
    }

    #[test]
    fn put_request_serializes_expected_fields() {
        let req = PutSharesRequest {
            node_id: "n1".into(),
            kind: "keys".into(),
            share_index: 0,
            share: String::new(),
            blob_len: 128,
            version: 1,
            expires_in_secs: None,
            signer_node_id: "n1".into(),
            signer_pubkey: Some("PUB".into()),
            ts: 0,
            sig: String::new(),
            role: Some("home".into()),
            recovery_proof: None,
            enroll_proof: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["kind"], "keys");
        assert_eq!(v["blob_len"], 128);
        assert_eq!(v["signer_pubkey"], "PUB");
        assert_eq!(v["role"], "home");
        // Optional None fields are omitted from the wire body.
        assert!(v.get("expires_in_secs").is_none());
        assert!(v.get("recovery_proof").is_none());
    }
}
