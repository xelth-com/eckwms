//! 9eck **product** licensing — the business/entitlement layer.
//!
//! Deliberately separate from the xelth/xelixir *technical* license (the
//! agent/C2 access claim in `wms::services::agent_manager`, env `LICENSE_TOKEN`
//! + `XELTH_CLAIM_URL`). That one is infrastructure: "this device may connect to
//! the xelth C2 mesh". THIS one is the 9eck commercial product saying "this
//! customer/mesh paid for 9eck and its paid-tier features" — e.g. relay payload
//! passthrough. Kept self-contained so the 9eck product (WMS/POS/relay +
//! licensing/billing) can one day be sold and operated independently of the
//! xelth tech stack.
//!
//! ## Token format (JWS-lite)
//! `base64url(claims_json) "." base64(ed25519_sig)` — signed by the 9eck
//! licensing authority (9eck.com) with its Ed25519 private key. Relays verify
//! **offline** with the issuer's public key, so a paid customer keeps working
//! even when the billing service is down (no phone-home on the hot path). Short
//! `exp` + a generous grace window is the revocation mechanism.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

/// Scope granting use of the relay's NAT-traversal payload queue (`/E/m/*`).
pub const SCOPE_RELAY_PAYLOAD: &str = "relay:payload";

/// Default grace window (seconds) the verifier allows past `exp` before a
/// license is treated as expired — so a hiccup refreshing never breaks a paying
/// customer mid-work. 7 days.
pub const DEFAULT_GRACE_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseClaims {
    /// Customer / firm identifier (the billing subject).
    pub tenant: String,
    /// Entitlement tier, e.g. `"paid"` or `"free"`.
    pub tier: String,
    /// Bound subject: the `mesh_id` this license is valid for. Anti-replay — a
    /// leaked token can't be presented by an unrelated mesh.
    pub sub: String,
    /// Granted feature scopes. Empty = all features of the tier.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds).
    pub exp: i64,
}

impl LicenseClaims {
    pub fn is_paid(&self) -> bool {
        self.tier.eq_ignore_ascii_case("paid")
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.is_empty() || self.scopes.iter().any(|s| s == scope)
    }
}

#[derive(Debug)]
pub enum LicenseError {
    Malformed(String),
    BadSignature,
    Expired { exp: i64, now: i64 },
}

impl std::fmt::Display for LicenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LicenseError::Malformed(m) => write!(f, "malformed license: {m}"),
            LicenseError::BadSignature => write!(f, "bad license signature"),
            LicenseError::Expired { exp, now } => {
                write!(f, "license expired (exp={exp}, now={now})")
            }
        }
    }
}
impl std::error::Error for LicenseError {}

/// Issue (sign) a license token. Used by the 9eck.com licensing authority / ops
/// minting tooling. `issuer_priv_key_b64` is the Ed25519 private key (32-byte
/// seed, STANDARD base64).
pub fn issue(issuer_priv_key_b64: &str, claims: &LicenseClaims) -> Result<String, String> {
    let payload = serde_json::to_vec(claims).map_err(|e| e.to_string())?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
    let sig_b64 = crate::utils::identity::sign_message(issuer_priv_key_b64, &payload_b64)?;
    Ok(format!("{payload_b64}.{sig_b64}"))
}

/// Verify a license token **offline** against the issuer's Ed25519 public key
/// (STANDARD base64). Checks the signature and that `now <= exp + grace_secs`.
/// Returns the claims; the caller decides on `tier` / `scope` / `sub`.
pub fn verify(
    issuer_pub_key_b64: &str,
    token: &str,
    now: i64,
    grace_secs: i64,
) -> Result<LicenseClaims, LicenseError> {
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| LicenseError::Malformed("expected payload.sig".into()))?;

    let ok = crate::utils::identity::verify_signature(issuer_pub_key_b64, payload_b64, sig_b64)
        .map_err(LicenseError::Malformed)?;
    if !ok {
        return Err(LicenseError::BadSignature);
    }

    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|e| LicenseError::Malformed(format!("payload base64: {e}")))?;
    let claims: LicenseClaims = serde_json::from_slice(&payload)
        .map_err(|e| LicenseError::Malformed(format!("claims json: {e}")))?;

    if now > claims.exp + grace_secs {
        return Err(LicenseError::Expired {
            exp: claims.exp,
            now,
        });
    }
    Ok(claims)
}

/// Relay convenience: is `token` a currently-valid **paid** license for
/// `mesh_id` that grants `scope`? Verifies offline; returns `false` on any
/// failure (malformed, bad sig, expired, wrong mesh, missing scope).
pub fn is_paid_for(
    issuer_pub_key_b64: &str,
    token: &str,
    mesh_id: &str,
    scope: &str,
    now: i64,
    grace_secs: i64,
) -> bool {
    match verify(issuer_pub_key_b64, token, now, grace_secs) {
        Ok(c) => c.is_paid() && c.sub == mesh_id && c.has_scope(scope),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::SigningKey;

    fn keypair(seed: u8) -> (String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (STANDARD.encode([seed; 32]), STANDARD.encode(vk.to_bytes()))
    }

    fn claims(exp: i64) -> LicenseClaims {
        LicenseClaims {
            tenant: "acme-gmbh".into(),
            tier: "paid".into(),
            sub: "mesh-123".into(),
            scopes: vec![SCOPE_RELAY_PAYLOAD.into()],
            iat: 1000,
            exp,
        }
    }

    #[test]
    fn roundtrip_and_gate() {
        let (priv_b64, pub_b64) = keypair(7);
        let token = issue(&priv_b64, &claims(10_000)).unwrap();

        // Verifies and is paid for the bound mesh + scope.
        let c = verify(&pub_b64, &token, 5_000, DEFAULT_GRACE_SECS).unwrap();
        assert!(c.is_paid());
        assert!(is_paid_for(&pub_b64, &token, "mesh-123", SCOPE_RELAY_PAYLOAD, 5_000, 0));

        // Wrong mesh (anti-replay) and missing scope are rejected.
        assert!(!is_paid_for(&pub_b64, &token, "other-mesh", SCOPE_RELAY_PAYLOAD, 5_000, 0));
        assert!(!is_paid_for(&pub_b64, &token, "mesh-123", "relay:other", 5_000, 0));
    }

    #[test]
    fn wrong_key_rejected() {
        let (priv_b64, _) = keypair(7);
        let (_, other_pub) = keypair(9);
        let token = issue(&priv_b64, &claims(10_000)).unwrap();
        assert!(matches!(
            verify(&other_pub, &token, 5_000, 0),
            Err(LicenseError::BadSignature)
        ));
    }

    #[test]
    fn expiry_with_grace() {
        let (priv_b64, pub_b64) = keypair(7);
        let token = issue(&priv_b64, &claims(10_000)).unwrap();
        // Past exp but within grace → still valid.
        assert!(verify(&pub_b64, &token, 10_500, 1_000).is_ok());
        // Past exp + grace → expired.
        assert!(matches!(
            verify(&pub_b64, &token, 12_000, 1_000),
            Err(LicenseError::Expired { .. })
        ));
    }

    #[test]
    fn malformed_rejected() {
        let (_, pub_b64) = keypair(7);
        assert!(matches!(
            verify(&pub_b64, "not-a-token", 0, 0),
            Err(LicenseError::Malformed(_))
        ));
    }
}
