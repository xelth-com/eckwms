//! Subscription entitlement certs — the unforgeable gate for **relay-carried
//! MCP access**. A paid client reaches its own NAT'd WMS node's `/mcp` through
//! our relay; the relay refuses to carry the request unless it presents a valid
//! subscription cert. Free clients run open-source code, so the gate CANNOT
//! live in anything we ship them — it lives in the authority's signature, which
//! only we can produce.
//!
//! ## Model (mirrors [`crate::xelixir::admin_cert`], DIFFERENT root)
//! - A long-lived **subscription root** keypair lives OFFLINE on the token
//!   authority and only ever *signs*. Its public key (`ECK_SUB_ROOT_PUBKEY`) is
//!   configured on the **relay** (the enforcement point we control) — that is
//!   the single trust anchor.
//! - The authority issues short-lived **SubscriptionCerts** to subscribers,
//!   each binding the subscriber's own Ed25519 pubkey to a set of node UUIDs
//!   they own, a feature list, and an MCP tier — with `exp` = the billing
//!   period end. Lapse a subscription → stop re-issuing → access dies at `exp`.
//! - The subscriber signs each relay-channel request with their key and attaches
//!   the cert. The relay accepts iff the cert chains to the trusted root, hasn't
//!   expired, covers the target node, and grants the feature.
//!
//! Deliberately a SEPARATE root from both `licensing` (product entitlement) and
//! `admin_cert` (fleet control): compromising one root never grants the others.
//! There is intentionally NO metering here — relay MCP is a flat subscription
//! benefit, not pay-per-use; `ask_brain` still bills the subscriber's own AI
//! balance downstream, so cost can't be amplified by a leaked cert.
//!
//! ## Wire format (JWS-lite, same as licensing / admin_cert)
//! `base64url(cert_json) "." base64std(ed25519_sig_over_payload_b64)`

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::utils::identity::{sign_message, verify_signature};

/// The canonical feature string for relay-carried MCP access.
pub const FEATURE_MCP_RELAY: &str = "mcp.relay";

/// Grace window (seconds) past `exp` before a cert is refused. SHORT on purpose
/// (unlike admin certs): a lapsed subscription must stop working promptly, not a
/// day later. This only absorbs clock skew / a renewal in flight. 5 minutes.
pub const DEFAULT_GRACE_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionCert {
    /// The subscriber's Ed25519 public key (STANDARD base64) that signs the
    /// relay-channel envelopes — must equal the envelope's `signer_pubkey`.
    pub client_pubkey: String,
    /// Account / license id the subscription belongs to. Informational here;
    /// the load-bearing binding is `client_pubkey`. Also the key a future
    /// revocation list would name.
    #[serde(default)]
    pub subject: String,
    /// Plan label, e.g. `"pro-monthly"`. Informational.
    #[serde(default)]
    pub plan: String,
    /// Node UUIDs this subscription may reach over the relay. **Explicit list —
    /// empty grants nothing** (entitlement, not admin: no "empty = all").
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Granted features, e.g. `["mcp.relay"]`. **Explicit — empty grants
    /// nothing.**
    #[serde(default)]
    pub features: Vec<String>,
    /// MCP privilege tier this cert grants over the relay: `"agent"` (masked
    /// PII, default) or `"master"` (clear PII). Anything else → agent.
    #[serde(default)]
    pub tier: String,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds) — the billing-period end.
    pub exp: i64,
}

impl SubscriptionCert {
    /// True iff this cert may reach `target_uuid`. Explicit membership only.
    pub fn covers_node(&self, target_uuid: &str) -> bool {
        let t = target_uuid.trim();
        !t.is_empty() && self.nodes.iter().any(|n| n.trim() == t)
    }

    /// True iff this cert grants `feature` (exact match). Explicit only.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f.trim() == feature)
    }

    /// The MCP tier this cert grants: only an explicit `"master"` unlocks clear
    /// PII; everything else (incl. empty / typo) is the safe `agent` default.
    pub fn grants_master(&self) -> bool {
        self.tier.trim().eq_ignore_ascii_case("master")
    }
}

#[derive(Debug, PartialEq)]
pub enum SubCertError {
    Malformed(String),
    BadSignature,
    Expired { exp: i64, now: i64 },
    NodeNotCovered { target: String },
    FeatureNotGranted { feature: String },
    PubkeyMismatch,
}

impl std::fmt::Display for SubCertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubCertError::Malformed(m) => write!(f, "malformed subscription cert: {m}"),
            SubCertError::BadSignature => {
                write!(f, "bad subscription-cert signature (not signed by the subscription root)")
            }
            SubCertError::Expired { exp, now } => {
                write!(f, "subscription expired (exp={exp}, now={now})")
            }
            SubCertError::NodeNotCovered { target } => {
                write!(f, "subscription does not cover node '{target}'")
            }
            SubCertError::FeatureNotGranted { feature } => {
                write!(f, "subscription does not grant feature '{feature}'")
            }
            SubCertError::PubkeyMismatch => {
                write!(f, "subscription cert pubkey != channel signer pubkey")
            }
        }
    }
}
impl std::error::Error for SubCertError {}

/// Issue (sign) a SubscriptionCert with the subscription **root** private key
/// (STANDARD base64 32-byte seed). Authority-side only — the relay and nodes
/// never hold the root key, so this path is inert there even though it links.
pub fn issue(root_priv_key_b64: &str, cert: &SubscriptionCert) -> Result<String, String> {
    let payload = serde_json::to_vec(cert).map_err(|e| e.to_string())?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
    let sig_b64 = sign_message(root_priv_key_b64, &payload_b64)?;
    Ok(format!("{payload_b64}.{sig_b64}"))
}

/// Verify a cert against the subscription **root** public key: signature +
/// `now <= exp + grace`. Returns the cert. Does NOT check node/feature — that
/// is [`authorize`], so a caller can inspect the cert first.
pub fn verify(
    root_pub_key_b64: &str,
    token: &str,
    now: i64,
    grace_secs: i64,
) -> Result<SubscriptionCert, SubCertError> {
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| SubCertError::Malformed("expected payload.sig".into()))?;

    let ok = verify_signature(root_pub_key_b64, payload_b64, sig_b64)
        .map_err(SubCertError::Malformed)?;
    if !ok {
        return Err(SubCertError::BadSignature);
    }

    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| SubCertError::Malformed(format!("payload base64: {e}")))?;
    let cert: SubscriptionCert = serde_json::from_slice(&payload)
        .map_err(|e| SubCertError::Malformed(format!("cert json: {e}")))?;

    if now > cert.exp + grace_secs {
        return Err(SubCertError::Expired { exp: cert.exp, now });
    }
    Ok(cert)
}

/// One-call admission check for the relay: verify the cert chains to the root
/// and is unexpired, that it was issued to `channel_signer_pubkey` (so a stolen
/// cert can't be replayed under a different key), that it covers `target_uuid`,
/// and that it grants `feature`. Returns the cert (for the tier) on success.
pub fn authorize(
    root_pub_key_b64: &str,
    token: &str,
    channel_signer_pubkey: &str,
    target_uuid: &str,
    feature: &str,
    now: i64,
    grace_secs: i64,
) -> Result<SubscriptionCert, SubCertError> {
    let cert = verify(root_pub_key_b64, token, now, grace_secs)?;
    if cert.client_pubkey.trim() != channel_signer_pubkey.trim() {
        return Err(SubCertError::PubkeyMismatch);
    }
    if !cert.covers_node(target_uuid) {
        return Err(SubCertError::NodeNotCovered { target: target_uuid.to_string() });
    }
    if !cert.has_feature(feature) {
        return Err(SubCertError::FeatureNotGranted { feature: feature.to_string() });
    }
    Ok(cert)
}

// ─── The relay-carried MCP request envelope ────────────────────────────────
//
// The subscriber signs this with their own key; the relay admits it (or 403s)
// and the node re-verifies it (defense in depth). The `mcp` body is opaque to
// the relay — it is bound into the signed canonical so it can't be swapped, but
// the relay never parses it (blind to content).

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientMcpRequest {
    pub target_uuid: String,
    /// The MCP JSON-RPC body (`initialize` / `tools/list` / `tools/call` …),
    /// forwarded verbatim to the node's `mcp_handler`. Opaque to the relay.
    pub mcp: Value,
    pub timestamp: i64,
    pub nonce: String,
}

impl ClientMcpRequest {
    /// Signed/verified string. The whole `mcp` body is bound in, so a valid
    /// signature can't be lifted onto a different request or a swapped body.
    pub fn canonical(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.target_uuid,
            serde_json::to_string(&self.mcp).unwrap_or_default(),
            self.timestamp,
            self.nonce
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedClientMcp {
    pub request: ClientMcpRequest,
    /// The subscriber's Ed25519 pubkey (STANDARD base64) — must equal the
    /// cert's `client_pubkey`.
    pub signer_pubkey: String,
    /// Ed25519 signature over `request.canonical()`.
    pub signature: String,
    /// The `SubscriptionCert` token (`payload.sig`).
    pub cert: String,
}

impl SignedClientMcp {
    /// Full admission check — used by BOTH the relay (the gate) and the node
    /// (defense in depth, so a compromised relay can't forge access or escalate
    /// tier). Verifies: target match, timestamp freshness, the client's
    /// signature over the request, and the subscription cert
    /// (root chain / exp / pubkey binding / node coverage / `mcp.relay`).
    /// Returns the authorized cert (whose `tier` the node maps to `McpTier`).
    /// The caller still owns nonce-replay tracking.
    pub fn admit(
        &self,
        sub_root_pubkey: &str,
        expected_target: &str,
        now: i64,
        max_age_secs: i64,
        grace_secs: i64,
    ) -> Result<SubscriptionCert, SubCertError> {
        if self.request.target_uuid.trim() != expected_target.trim() {
            return Err(SubCertError::Malformed(format!(
                "target mismatch: request={} expected={}",
                self.request.target_uuid, expected_target
            )));
        }
        if (now - self.request.timestamp).abs() > max_age_secs {
            return Err(SubCertError::Malformed(format!(
                "request timestamp out of window (delta={}s)",
                now - self.request.timestamp
            )));
        }
        let ok = verify_signature(&self.signer_pubkey, &self.request.canonical(), &self.signature)
            .map_err(SubCertError::Malformed)?;
        if !ok {
            return Err(SubCertError::BadSignature);
        }
        authorize(
            sub_root_pubkey,
            &self.cert,
            &self.signer_pubkey,
            expected_target,
            FEATURE_MCP_RELAY,
            now,
            grace_secs,
        )
    }
}

/// The relay's trusted subscription root pubkey (`ECK_SUB_ROOT_PUBKEY`).
/// `None`/empty ⇒ the relay MCP channel is disabled (fail closed: no cert can
/// ever verify, so no free client slips through a mis-provisioned relay).
pub fn read_sub_root_from_env() -> Option<String> {
    std::env::var("ECK_SUB_ROOT_PUBKEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::SigningKey;

    fn keypair(seed: u8) -> (String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        (
            STANDARD.encode(sk.to_bytes()),
            STANDARD.encode(sk.verifying_key().to_bytes()),
        )
    }

    fn cert(client_pub: &str, exp: i64) -> SubscriptionCert {
        SubscriptionCert {
            client_pubkey: client_pub.into(),
            subject: "acct-42".into(),
            plan: "pro-monthly".into(),
            nodes: vec!["node-A".into(), "node-B".into()],
            features: vec![FEATURE_MCP_RELAY.into()],
            tier: "agent".into(),
            iat: 1000,
            exp,
        }
    }

    #[test]
    fn happy_path_authorizes() {
        let (root_priv, root_pub) = keypair(1);
        let (_client_priv, client_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&client_pub, 10_000)).unwrap();

        let c = authorize(
            &root_pub, &token, &client_pub, "node-A", FEATURE_MCP_RELAY, 5_000, DEFAULT_GRACE_SECS,
        )
        .unwrap();
        assert_eq!(c.subject, "acct-42");
        assert!(!c.grants_master()); // "agent" tier
    }

    #[test]
    fn forged_root_rejected() {
        // A free client with the OPEN SOURCE can build a cert and sign it with
        // ITS OWN key — but not the authority's. This is the whole gate.
        let (attacker_priv, _attacker_pub) = keypair(9);
        let (_real_root_priv, real_root_pub) = keypair(1);
        let (_client_priv, client_pub) = keypair(2);
        let forged = issue(&attacker_priv, &cert(&client_pub, 10_000)).unwrap();

        let err = authorize(
            &real_root_pub, &forged, &client_pub, "node-A", FEATURE_MCP_RELAY, 5_000, DEFAULT_GRACE_SECS,
        )
        .unwrap_err();
        assert_eq!(err, SubCertError::BadSignature);
    }

    #[test]
    fn expired_rejected_after_grace() {
        let (root_priv, root_pub) = keypair(1);
        let (_client_priv, client_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&client_pub, 10_000)).unwrap();
        let now = 10_000 + DEFAULT_GRACE_SECS + 1;
        let err = authorize(
            &root_pub, &token, &client_pub, "node-A", FEATURE_MCP_RELAY, now, DEFAULT_GRACE_SECS,
        )
        .unwrap_err();
        assert!(matches!(err, SubCertError::Expired { .. }));
    }

    #[test]
    fn stolen_cert_under_other_key_rejected() {
        // A cert leaked to a third party is useless without the client privkey:
        // the relay checks the cert's client_pubkey == the envelope signer.
        let (root_priv, root_pub) = keypair(1);
        let (_client_priv, client_pub) = keypair(2);
        let (_thief_priv, thief_pub) = keypair(7);
        let token = issue(&root_priv, &cert(&client_pub, 10_000)).unwrap();
        let err = authorize(
            &root_pub, &token, &thief_pub, "node-A", FEATURE_MCP_RELAY, 5_000, DEFAULT_GRACE_SECS,
        )
        .unwrap_err();
        assert_eq!(err, SubCertError::PubkeyMismatch);
    }

    #[test]
    fn node_and_feature_scoping() {
        let (root_priv, root_pub) = keypair(1);
        let (_client_priv, client_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&client_pub, 10_000)).unwrap();
        // Node not in the cert's list.
        assert!(matches!(
            authorize(&root_pub, &token, &client_pub, "node-Z", FEATURE_MCP_RELAY, 5_000, DEFAULT_GRACE_SECS),
            Err(SubCertError::NodeNotCovered { .. })
        ));
        // Feature not granted.
        assert!(matches!(
            authorize(&root_pub, &token, &client_pub, "node-A", "some.other", 5_000, DEFAULT_GRACE_SECS),
            Err(SubCertError::FeatureNotGranted { .. })
        ));
    }

    fn signed_request(client_priv: &str, client_pub: &str, cert_token: &str, target: &str, ts: i64) -> SignedClientMcp {
        let request = ClientMcpRequest {
            target_uuid: target.into(),
            mcp: serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            timestamp: ts,
            nonce: "n-1".into(),
        };
        let signature = sign_message(client_priv, &request.canonical()).unwrap();
        SignedClientMcp {
            request,
            signer_pubkey: client_pub.into(),
            signature,
            cert: cert_token.into(),
        }
    }

    #[test]
    fn admit_happy_path_returns_cert() {
        let (root_priv, root_pub) = keypair(1);
        let (client_priv, client_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&client_pub, 10_000)).unwrap();
        let signed = signed_request(&client_priv, &client_pub, &token, "node-A", 5_000);
        let c = signed.admit(&root_pub, "node-A", 5_000, 60, DEFAULT_GRACE_SECS).unwrap();
        assert_eq!(c.subject, "acct-42");
    }

    #[test]
    fn admit_rejects_swapped_body() {
        // A valid signature can't be lifted onto a different MCP body.
        let (root_priv, root_pub) = keypair(1);
        let (client_priv, client_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&client_pub, 10_000)).unwrap();
        let mut signed = signed_request(&client_priv, &client_pub, &token, "node-A", 5_000);
        signed.request.mcp = serde_json::json!({"method":"tools/call","params":{"name":"surrealql_read"}});
        assert_eq!(
            signed.admit(&root_pub, "node-A", 5_000, 60, DEFAULT_GRACE_SECS).unwrap_err(),
            SubCertError::BadSignature
        );
    }

    #[test]
    fn admit_rejects_stale_timestamp() {
        let (root_priv, root_pub) = keypair(1);
        let (client_priv, client_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&client_pub, 100_000)).unwrap();
        let signed = signed_request(&client_priv, &client_pub, &token, "node-A", 5_000);
        // now is 200s past the request timestamp, window is 60s.
        let err = signed.admit(&root_pub, "node-A", 5_200, 60, DEFAULT_GRACE_SECS).unwrap_err();
        assert!(matches!(err, SubCertError::Malformed(_)));
    }

    #[test]
    fn empty_scopes_grant_nothing() {
        // Unlike admin certs (empty scopes = all), entitlement is explicit.
        let mut c = cert("x", 10_000);
        c.nodes.clear();
        c.features.clear();
        assert!(!c.covers_node("node-A"));
        assert!(!c.has_feature(FEATURE_MCP_RELAY));
    }
}
