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

/// Scope granting the embedded POS register (ecKasse, the `/K/` tree).
pub const SCOPE_POS_REGISTER: &str = "pos:register";

/// Scope granting the WASM plugin runtime (install/enable + hook execution).
pub const SCOPE_PLUGIN_RUNTIME: &str = "plugin:runtime";

/// Default grace window (seconds) the verifier allows past `exp` before a
/// license is treated as expired — so a hiccup refreshing never breaks a paying
/// customer mid-work. 7 days.
pub const DEFAULT_GRACE_SECS: i64 = 7 * 24 * 3600;

/// Grace window for NODE-side feature gates (POS / plugin runtime): 30 days.
/// Deliberately long — a Kasse must never refuse to start because a renewal
/// slipped during a holiday; the operator gets a month of loud warnings first.
/// A running process is NEVER killed by expiry; the gate only applies at boot.
pub const NODE_GRACE_SECS: i64 = 30 * 24 * 3600;

/// The 9eck licensing authority's Ed25519 public key, BAKED into the binary.
/// In RELEASE builds this is the only trust anchor for node-side feature gates
/// (`node_license_for_scope`) — an `ECK_LICENSE_PUBKEY` env var cannot retarget
/// the check at a self-minted key. Debug builds may override via that env var
/// so tests/dev can mint with throwaway keys.
pub const BAKED_ISSUER_PUBKEY_B64: &str = "ZaA560m5NU/d29tLf81a4Xve9cbLzViaV+28JPNc3jU=";

/// Issuer pubkey used by node-side feature gates. Release: baked constant,
/// env ignored. Debug: `ECK_LICENSE_PUBKEY` env override allowed for dev keys.
pub fn node_issuer_pubkey() -> String {
    #[cfg(debug_assertions)]
    {
        if let Ok(k) = std::env::var("ECK_LICENSE_PUBKEY") {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return k;
            }
        }
    }
    BAKED_ISSUER_PUBKEY_B64.to_string()
}

/// Node-side verdict for one feature scope. `Grace` still unlocks the feature
/// (with warnings surfaced by the caller); `Unlicensed` never does.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeLicense {
    Licensed(LicenseClaims),
    /// Past `exp` but within [`NODE_GRACE_SECS`]; `days_left` until hard stop.
    Grace { claims: LicenseClaims, days_left: i64 },
    Unlicensed { reason: String },
}

impl NodeLicense {
    pub fn allows(&self) -> bool {
        !matches!(self, NodeLicense::Unlicensed { .. })
    }
}

/// Evaluate this node's `ECK_LICENSE_TOKEN` for one feature `scope`, bound to
/// the node's own `mesh_id`. Offline, pure; the trust anchor comes from
/// [`node_issuer_pubkey`]. Any failure (missing/malformed token, bad signature,
/// wrong mesh, missing scope, free tier, expired past grace) is `Unlicensed`.
pub fn node_license_for_scope(
    token: Option<&str>,
    mesh_id: &str,
    scope: &str,
    now: i64,
) -> NodeLicense {
    let token = match token.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => {
            return NodeLicense::Unlicensed {
                reason: "no ECK_LICENSE_TOKEN configured".into(),
            }
        }
    };
    let pubkey = node_issuer_pubkey();
    // Verify with the node grace window, then classify fresh-vs-grace below.
    let claims = match verify(&pubkey, token, now, NODE_GRACE_SECS) {
        Ok(c) => c,
        Err(e) => {
            return NodeLicense::Unlicensed {
                reason: e.to_string(),
            }
        }
    };
    if !claims.is_paid() {
        return NodeLicense::Unlicensed {
            reason: format!("tier '{}' is not paid", claims.tier),
        };
    }
    if claims.sub != mesh_id {
        return NodeLicense::Unlicensed {
            reason: format!("license is bound to mesh {}, this node is {}", claims.sub, mesh_id),
        };
    }
    if !claims.has_scope(scope) {
        return NodeLicense::Unlicensed {
            reason: format!("license lacks scope '{scope}' (has {:?})", claims.scopes),
        };
    }
    if now > claims.exp {
        let days_left = (claims.exp + NODE_GRACE_SECS - now) / 86400;
        return NodeLicense::Grace { claims, days_left };
    }
    NodeLicense::Licensed(claims)
}

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

// ─── Revocation list (CRL) ───────────────────────────────────────────────────
//
// The mid-period kill switch for leaked licenses and MCP SubscriptionCerts.
// One signed document, authored by the 9eck licensing authority (same issuer
// key relays already trust via `ECK_LICENSE_PUBKEY` — the subscription root
// stays a signing-only key, revocation authority is deliberately centralized).
// Distribution: a one-line token file on each relay (`ECK_REVOCATION_FILE`),
// pushed over the existing ops channel; relays re-read it on every gate check
// (Ed25519 verify is ~µs, and a file read beats a cache-invalidation bug).
// Latest `updated` wins operationally — there is no delta protocol, the file
// IS the list.

/// Discriminator baked into every CRL payload — domain separation so a CRL
/// can never be confused with (or replayed as) a `LicenseClaims` token.
pub const CRL_KIND: &str = "eck-crl-v1";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RevocationList {
    /// Must equal [`CRL_KIND`].
    pub kind: String,
    /// Unix seconds when this list was authored (bump on every re-issue).
    pub updated: i64,
    /// Product-license subjects (`LicenseClaims.sub` = mesh_id) revoked.
    #[serde(default)]
    pub license_subs: Vec<String>,
    /// MCP `SubscriptionCert`s revoked — matched against BOTH the cert's
    /// `subject` label and its `client_pubkey` (ops may only have one of them).
    #[serde(default)]
    pub cert_subjects: Vec<String>,
}

impl RevocationList {
    pub fn revokes_license_sub(&self, sub: &str) -> bool {
        self.license_subs.iter().any(|s| s == sub)
    }
    pub fn revokes_cert(&self, subject: &str, client_pubkey: &str) -> bool {
        self.cert_subjects
            .iter()
            .any(|s| s == subject || s == client_pubkey)
    }
}

/// Sign a revocation list with the license issuer key. Same `payload.sig`
/// JWS-lite shape as license tokens; the `kind` field is forced.
pub fn issue_crl(issuer_priv_key_b64: &str, crl: &RevocationList) -> Result<String, String> {
    let mut crl = crl.clone();
    crl.kind = CRL_KIND.to_string();
    let payload = serde_json::to_vec(&crl).map_err(|e| e.to_string())?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
    let sig_b64 = crate::utils::identity::sign_message(issuer_priv_key_b64, &payload_b64)?;
    Ok(format!("{payload_b64}.{sig_b64}"))
}

/// Verify a CRL token offline. Rejects a wrong `kind` (a license token can
/// never pass as a CRL and vice versa). CRLs do not expire — the newest file
/// on the relay simply replaces the previous one.
pub fn verify_crl(issuer_pub_key_b64: &str, token: &str) -> Result<RevocationList, LicenseError> {
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
    let crl: RevocationList = serde_json::from_slice(&payload)
        .map_err(|e| LicenseError::Malformed(format!("crl json: {e}")))?;
    if crl.kind != CRL_KIND {
        return Err(LicenseError::Malformed(format!(
            "wrong kind '{}' (expected {CRL_KIND})",
            crl.kind
        )));
    }
    Ok(crl)
}

/// Load + verify the CRL named by the `ECK_REVOCATION_FILE` env var, if any.
/// `None` when unset, unreadable, or invalid (each invalid load WARNs — a
/// present-but-broken CRL should be loud, not silently ignored). Verified
/// against `issuer_pub_key_b64` (relays pass their `ECK_LICENSE_PUBKEY`).
pub fn load_revocations(issuer_pub_key_b64: &str) -> Option<RevocationList> {
    let path = std::env::var("ECK_REVOCATION_FILE").ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("ECK_REVOCATION_FILE '{path}' unreadable: {e}");
            return None;
        }
    };
    match verify_crl(issuer_pub_key_b64, raw.trim()) {
        Ok(crl) => Some(crl),
        Err(e) => {
            tracing::warn!("ECK_REVOCATION_FILE '{path}' invalid: {e}");
            None
        }
    }
}

// ─── Plugin artifact signing ─────────────────────────────────────────────────
//
// Marketplace layer 1 (WASM_ARCHITECTURE.md §9): the 9eck authority signs a
// plugin artifact's sha256; nodes verify at install. The signed message is
// domain-prefixed so an artifact signature can never double as any other
// Ed25519 message in this codebase (licenses, envelopes, admin certs).

const PLUGIN_SIG_DOMAIN: &str = "eck-plugin-v1:";

/// Sign a plugin artifact (identified by its lowercase sha256 hex).
pub fn sign_plugin_artifact(issuer_priv_key_b64: &str, sha256_hex: &str) -> Result<String, String> {
    let msg = format!("{PLUGIN_SIG_DOMAIN}{}", sha256_hex.to_ascii_lowercase());
    crate::utils::identity::sign_message(issuer_priv_key_b64, &msg)
}

/// Verify an authority signature over a plugin artifact's sha256. `false` on
/// any failure.
pub fn verify_plugin_artifact(
    issuer_pub_key_b64: &str,
    sha256_hex: &str,
    sig_b64: &str,
) -> bool {
    let msg = format!("{PLUGIN_SIG_DOMAIN}{}", sha256_hex.to_ascii_lowercase());
    crate::utils::identity::verify_signature(issuer_pub_key_b64, &msg, sig_b64).unwrap_or(false)
}

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

    /// Mint with a throwaway key and point the debug-only env override at its
    /// pubkey. All node-gate tests share the SAME seed so the concurrent
    /// `set_var` calls are idempotent (tests run multi-threaded).
    fn node_token(scopes: Vec<String>, exp: i64) -> String {
        let (priv_b64, pub_b64) = keypair(42);
        std::env::set_var("ECK_LICENSE_PUBKEY", pub_b64);
        let c = LicenseClaims {
            scopes,
            exp,
            ..claims(exp)
        };
        issue(&priv_b64, &c).unwrap()
    }

    #[test]
    fn node_gate_licensed_and_scope_bound() {
        let token = node_token(
            vec![SCOPE_RELAY_PAYLOAD.into(), SCOPE_POS_REGISTER.into()],
            10_000,
        );
        assert!(matches!(
            node_license_for_scope(Some(&token), "mesh-123", SCOPE_POS_REGISTER, 5_000),
            NodeLicense::Licensed(_)
        ));
        // Missing scope, wrong mesh, absent token — all locked.
        assert!(!node_license_for_scope(Some(&token), "mesh-123", SCOPE_PLUGIN_RUNTIME, 5_000).allows());
        assert!(!node_license_for_scope(Some(&token), "other-mesh", SCOPE_POS_REGISTER, 5_000).allows());
        assert!(!node_license_for_scope(None, "mesh-123", SCOPE_POS_REGISTER, 5_000).allows());
        assert!(!node_license_for_scope(Some("  "), "mesh-123", SCOPE_POS_REGISTER, 5_000).allows());
    }

    #[test]
    fn node_gate_grace_then_hard_stop() {
        let token = node_token(vec![SCOPE_POS_REGISTER.into()], 10_000);
        // 10 days past exp: inside the 30-day node grace, still allowed.
        let ten_days = 10 * 86_400;
        match node_license_for_scope(Some(&token), "mesh-123", SCOPE_POS_REGISTER, 10_000 + ten_days) {
            NodeLicense::Grace { days_left, .. } => assert_eq!(days_left, 20),
            other => panic!("expected Grace, got {other:?}"),
        }
        // Past exp + 30d: locked.
        assert!(!node_license_for_scope(
            Some(&token),
            "mesh-123",
            SCOPE_POS_REGISTER,
            10_000 + NODE_GRACE_SECS + 1
        )
        .allows());
    }

    #[test]
    fn crl_roundtrip_and_domain_separation() {
        let (priv_b64, pub_b64) = keypair(7);
        let crl = RevocationList {
            kind: String::new(), // issue_crl forces it
            updated: 1_000,
            license_subs: vec!["mesh-123".into()],
            cert_subjects: vec!["pda-demo-owner".into(), "PUBKEYB64".into()],
        };
        let token = issue_crl(&priv_b64, &crl).unwrap();
        let parsed = verify_crl(&pub_b64, &token).unwrap();
        assert_eq!(parsed.kind, CRL_KIND);
        assert!(parsed.revokes_license_sub("mesh-123"));
        assert!(!parsed.revokes_license_sub("mesh-999"));
        // cert match by subject OR client_pubkey
        assert!(parsed.revokes_cert("pda-demo-owner", "other"));
        assert!(parsed.revokes_cert("other", "PUBKEYB64"));
        assert!(!parsed.revokes_cert("other", "other"));

        // A LICENSE token must NOT parse as a CRL (kind/domain separation).
        let lic = issue(&priv_b64, &claims(10_000)).unwrap();
        assert!(verify_crl(&pub_b64, &lic).is_err());
        // And a CRL signed by the wrong key is rejected.
        let (evil_priv, _) = keypair(9);
        let forged = issue_crl(&evil_priv, &crl).unwrap();
        assert!(matches!(verify_crl(&pub_b64, &forged), Err(LicenseError::BadSignature)));
    }

    #[test]
    fn plugin_artifact_signature() {
        let (priv_b64, pub_b64) = keypair(7);
        let sha = "ab".repeat(32);
        let sig = sign_plugin_artifact(&priv_b64, &sha).unwrap();
        assert!(verify_plugin_artifact(&pub_b64, &sha, &sig));
        // Case-insensitive on the sha, bound to the exact digest, wrong key fails.
        assert!(verify_plugin_artifact(&pub_b64, &sha.to_ascii_uppercase(), &sig));
        assert!(!verify_plugin_artifact(&pub_b64, &"cd".repeat(32), &sig));
        let (_, other_pub) = keypair(9);
        assert!(!verify_plugin_artifact(&other_pub, &sha, &sig));
        // The domain prefix keeps it from validating as a bare-message sig.
        assert!(!crate::utils::identity::verify_signature(&pub_b64, &sha, &sig).unwrap_or(false));
    }

    #[test]
    fn node_gate_free_tier_locked() {
        let (priv_b64, pub_b64) = keypair(42);
        std::env::set_var("ECK_LICENSE_PUBKEY", pub_b64);
        let c = LicenseClaims {
            tier: "free".into(),
            scopes: vec![SCOPE_POS_REGISTER.into()],
            ..claims(10_000)
        };
        let token = issue(&priv_b64, &c).unwrap();
        assert!(!node_license_for_scope(Some(&token), "mesh-123", SCOPE_POS_REGISTER, 5_000).allows());
    }
}
