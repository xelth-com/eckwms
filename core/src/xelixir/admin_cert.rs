//! Fleet-admin authorization certificates — a CA-style trust chain for the
//! privileged cross-node ops channel, **separate from product licensing**.
//!
//! ## Why
//! The legacy ops channel verifies an envelope signer against a per-node static
//! allow-list (`XELIXIR_ADMIN_PUBKEYS`). That doesn't scale: every new fleet
//! node must be hand-provisioned with the operational admin pubkey, and rotating
//! that key means editing every node. A leaked operational key is also live until
//! manually purged everywhere.
//!
//! ## Model (mirrors `crate::licensing`, but a DIFFERENT root)
//! - A long-lived **fleet root** keypair lives OFFLINE and only ever *signs*.
//!   Its public key (`ECK_FLEET_ROOT_PUBKEY`) is baked into every node at
//!   provisioning — it is the single, ~never-changing trust anchor (bound to no
//!   IP/domain, just a key).
//! - The root issues short-lived **AdminCerts**: each binds an *operational* admin
//!   pubkey to the root's trust, with `exp` + optional scopes.
//! - The operational admin key (e.g. on the 222.64 control node) signs the actual
//!   ops envelopes and attaches its cert. A node accepts the envelope iff the cert
//!   chains to the trusted root, hasn't expired, and the envelope signer matches
//!   the cert's `pubkey`.
//! - Rotating the operational key = re-issue a cert from the offline root. **Zero
//!   fleet-wide changes.** Key leak = it expires on its own; the root stays safe.
//!
//! Deliberately distinct from `licensing` (product entitlement / "who paid") so a
//! compromise of one root never grants the other authority.
//!
//! ## Wire format (JWS-lite, same as licensing)
//! `base64url(cert_json) "." base64std(ed25519_sig_over_payload_b64)`

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

use crate::utils::identity::{sign_message, verify_signature};

/// Grace window (seconds) past `exp` before a cert is treated as expired — a
/// short bounce while re-issuing doesn't lock the operator out. 1 day.
pub const DEFAULT_GRACE_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminCert {
    /// The operational admin Ed25519 public key (STANDARD base64) this cert
    /// authorizes — must equal the envelope's `signer_pubkey`.
    pub pubkey: String,
    /// Human label, e.g. `"222.64-fleet-control"`. Informational.
    #[serde(default)]
    pub label: String,
    /// Granted ops scopes (e.g. `"ops.restart_service"`). Empty = all verbs.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds).
    pub exp: i64,
}

impl AdminCert {
    /// True if this cert authorizes `verb` (full command string, e.g.
    /// `"ops.restart_service"` or legacy `"start"`). Empty scopes = all.
    pub fn allows(&self, verb: &str) -> bool {
        self.scopes.is_empty() || self.scopes.iter().any(|s| s == verb)
    }
}

#[derive(Debug)]
pub enum CertError {
    Malformed(String),
    BadSignature,
    Expired { exp: i64, now: i64 },
}

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CertError::Malformed(m) => write!(f, "malformed admin cert: {m}"),
            CertError::BadSignature => write!(f, "bad admin-cert signature (not signed by fleet root)"),
            CertError::Expired { exp, now } => write!(f, "admin cert expired (exp={exp}, now={now})"),
        }
    }
}
impl std::error::Error for CertError {}

/// Issue (sign) an AdminCert with the fleet **root** private key (STANDARD
/// base64 32-byte seed). Run on the offline root only.
pub fn issue(root_priv_key_b64: &str, cert: &AdminCert) -> Result<String, String> {
    let payload = serde_json::to_vec(cert).map_err(|e| e.to_string())?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
    let sig_b64 = sign_message(root_priv_key_b64, &payload_b64)?;
    Ok(format!("{payload_b64}.{sig_b64}"))
}

/// Verify a cert offline against the fleet **root** public key. Checks the
/// signature and `now <= exp + grace_secs`. Returns the cert.
pub fn verify(
    root_pub_key_b64: &str,
    token: &str,
    now: i64,
    grace_secs: i64,
) -> Result<AdminCert, CertError> {
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| CertError::Malformed("expected payload.sig".into()))?;

    let ok = verify_signature(root_pub_key_b64, payload_b64, sig_b64)
        .map_err(CertError::Malformed)?;
    if !ok {
        return Err(CertError::BadSignature);
    }

    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| CertError::Malformed(format!("payload base64: {e}")))?;
    let cert: AdminCert = serde_json::from_slice(&payload)
        .map_err(|e| CertError::Malformed(format!("cert json: {e}")))?;

    if now > cert.exp + grace_secs {
        return Err(CertError::Expired { exp: cert.exp, now });
    }
    Ok(cert)
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

    fn cert(pubkey: &str, exp: i64) -> AdminCert {
        AdminCert {
            pubkey: pubkey.into(),
            label: "test".into(),
            scopes: vec![],
            iat: 1000,
            exp,
        }
    }

    #[test]
    fn roundtrip_and_chain() {
        let (root_priv, root_pub) = keypair(1);
        let (_admin_priv, admin_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&admin_pub, 10_000)).unwrap();

        let c = verify(&root_pub, &token, 5_000, DEFAULT_GRACE_SECS).unwrap();
        assert_eq!(c.pubkey, admin_pub);
        assert!(c.allows("ops.restart_service")); // empty scopes = all
    }

    #[test]
    fn wrong_root_rejected() {
        let (root_priv, _) = keypair(1);
        let (_, other_root_pub) = keypair(9);
        let (_, admin_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&admin_pub, 10_000)).unwrap();
        assert!(matches!(
            verify(&other_root_pub, &token, 5_000, 0),
            Err(CertError::BadSignature)
        ));
    }

    #[test]
    fn expiry_enforced() {
        let (root_priv, root_pub) = keypair(1);
        let (_, admin_pub) = keypair(2);
        let token = issue(&root_priv, &cert(&admin_pub, 10_000)).unwrap();
        assert!(verify(&root_pub, &token, 10_500, 1_000).is_ok()); // within grace
        assert!(matches!(
            verify(&root_pub, &token, 12_000, 1_000),
            Err(CertError::Expired { .. })
        ));
    }

    #[test]
    fn scopes() {
        let mut c = cert("x", 10_000);
        c.scopes = vec!["ops.restart_service".into()];
        assert!(c.allows("ops.restart_service"));
        assert!(!c.allows("ops.deploy"));
    }
}
