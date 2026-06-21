//! Signed command envelope for cross-node xelixir control.
//!
//! Wire format (`SignedEnvelope`):
//! ```json
//! {
//!   "envelope":      { "target_uuid":..., "command":..., "args":?, "timestamp":..., "nonce":... },
//!   "signer_uuid":   "<sender instance_id>",
//!   "signer_pubkey": "<base64 Ed25519 pubkey>",
//!   "signature":     "<base64 Ed25519 signature over canonical(envelope)>"
//! }
//! ```
//!
//! Canonical string signed/verified:
//!
//! * **Legacy (no args, e.g. `start`/`stop`):**
//!   `"{target_uuid}|{command}|{timestamp}|{nonce}"`
//! * **With args (ops verbs):**
//!   `"{target_uuid}|{command}|{args_json}|{timestamp}|{nonce}"`
//!   where `args_json` is the JSON-serialised args (deterministic
//!   `serde_json` output for a plain `Value`).
//!
//! The two canonical formats are explicitly distinguished by the
//! presence of `args` on the envelope, so an attacker cannot replay a
//! legacy signature against an envelope they augmented with args.
//!
//! Replay protection: timestamp must be within ±60 s of "now" on the
//! verifier. Combined with a short-TTL nonce cache (kept by the
//! caller), this gives us a tight replay window.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::utils::identity::{sign_message, verify_signature};

pub const DEFAULT_MAX_AGE_SECS: i64 = 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub target_uuid: String,
    /// `"start"` / `"stop"` for agent lifecycle, or `"ops.<verb>"` for
    /// cross-mesh ops dispatch (e.g., `"ops.deploy"`, `"ops.journal"`).
    pub command: String,
    /// Structured args for ops verbs. `None` for legacy start/stop.
    /// Presence/absence is part of the canonical signing string —
    /// adding args to a legacy-signed envelope invalidates the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    pub timestamp: i64,
    pub nonce: String,
}

impl CommandEnvelope {
    pub fn new(target_uuid: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            target_uuid: target_uuid.into(),
            command: command.into(),
            args: None,
            timestamp: chrono::Utc::now().timestamp(),
            nonce: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Build an ops envelope (`command = "ops.<verb>"`, `args = ...`).
    pub fn new_ops(
        target_uuid: impl Into<String>,
        verb: impl AsRef<str>,
        args: Value,
    ) -> Self {
        Self {
            target_uuid: target_uuid.into(),
            command: format!("ops.{}", verb.as_ref()),
            args: Some(args),
            timestamp: chrono::Utc::now().timestamp(),
            nonce: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn canonical(&self) -> String {
        match &self.args {
            Some(a) => format!(
                "{}|{}|{}|{}|{}",
                self.target_uuid,
                self.command,
                serde_json::to_string(a).unwrap_or_default(),
                self.timestamp,
                self.nonce
            ),
            None => format!(
                "{}|{}|{}|{}",
                self.target_uuid, self.command, self.timestamp, self.nonce
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub envelope: CommandEnvelope,
    pub signer_uuid: String,
    pub signer_pubkey: String,
    pub signature: String,
    /// Optional fleet-admin authorization cert (root-signed token binding
    /// `signer_pubkey` to the offline fleet root). When present and a trusted
    /// root pubkey is configured on the verifier, the cert chain replaces the
    /// static `XELIXIR_ADMIN_PUBKEYS` allow-list — so a node trusts the root,
    /// not each operational key. See `crate::xelixir::admin_cert`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_cert: Option<String>,
}

impl SignedEnvelope {
    /// Build a signed envelope using the caller's node identity (legacy path —
    /// the target must allow-list `signer_pubkey` in `XELIXIR_ADMIN_PUBKEYS`).
    pub fn sign(
        envelope: CommandEnvelope,
        signer_uuid: impl Into<String>,
        signer_pubkey_b64: impl Into<String>,
        signer_private_key_b64: &str,
    ) -> Result<Self, String> {
        let signature = sign_message(signer_private_key_b64, &envelope.canonical())?;
        Ok(Self {
            envelope,
            signer_uuid: signer_uuid.into(),
            signer_pubkey: signer_pubkey_b64.into(),
            signature,
            admin_cert: None,
        })
    }

    /// Build a signed envelope using a fleet **operational admin** keypair and
    /// attach its root-signed cert. The target accepts it by chaining the cert
    /// to its trusted `ECK_FLEET_ROOT_PUBKEY` — no per-node allow-list entry
    /// needed. The operational key may rotate freely (re-issue the cert).
    pub fn sign_admin(
        envelope: CommandEnvelope,
        signer_uuid: impl Into<String>,
        admin_pubkey_b64: impl Into<String>,
        admin_private_key_b64: &str,
        admin_cert: impl Into<String>,
    ) -> Result<Self, String> {
        let signature = sign_message(admin_private_key_b64, &envelope.canonical())?;
        Ok(Self {
            envelope,
            signer_uuid: signer_uuid.into(),
            signer_pubkey: admin_pubkey_b64.into(),
            signature,
            admin_cert: Some(admin_cert.into()),
        })
    }

    /// Verify the envelope:
    ///   1. **Authorization** — the signer is trusted either via a root-signed
    ///      `admin_cert` chaining to `root_pubkey` (preferred), or by membership
    ///      in the static `allowed_pubkeys` allow-list (legacy fallback).
    ///   2. `envelope.target_uuid` equals `expected_target`.
    ///   3. timestamp within `±max_age_secs` of now.
    ///   4. Ed25519 signature is valid against `signer_pubkey`.
    ///
    /// `root_pubkey` is the node's trusted fleet-admin root (`ECK_FLEET_ROOT_PUBKEY`),
    /// or `None` to disable the cert path. The caller handles nonce-replay tracking.
    pub fn verify(
        &self,
        expected_target: &str,
        allowed_pubkeys: &[String],
        root_pubkey: Option<&str>,
        max_age_secs: i64,
    ) -> Result<(), String> {
        // ── 1. Authorization ──────────────────────────────────────────────
        let now = chrono::Utc::now().timestamp();
        let authorized_via_cert = match (self.admin_cert.as_deref(), root_pubkey) {
            (Some(cert_token), Some(root)) if !root.trim().is_empty() => {
                let cert = crate::xelixir::admin_cert::verify(
                    root,
                    cert_token,
                    now,
                    crate::xelixir::admin_cert::DEFAULT_GRACE_SECS,
                )
                .map_err(|e| e.to_string())?;
                // The cert must authorize exactly this signing key, and the verb.
                if cert.pubkey.trim() != self.signer_pubkey.trim() {
                    return Err("admin cert pubkey != envelope signer_pubkey".into());
                }
                if !cert.allows(&self.envelope.command) {
                    return Err(format!(
                        "admin cert does not grant '{}'",
                        self.envelope.command
                    ));
                }
                true
            }
            _ => false,
        };
        if !authorized_via_cert
            && !allowed_pubkeys
                .iter()
                .any(|k| k.trim() == self.signer_pubkey.trim())
        {
            return Err("signer pubkey not in allow-list".into());
        }

        if self.envelope.target_uuid != expected_target {
            return Err(format!(
                "target_uuid mismatch: envelope={} self={}",
                self.envelope.target_uuid, expected_target
            ));
        }
        if (now - self.envelope.timestamp).abs() > max_age_secs {
            return Err(format!(
                "envelope timestamp out of window (delta={}s)",
                now - self.envelope.timestamp
            ));
        }
        let ok = verify_signature(&self.signer_pubkey, &self.envelope.canonical(), &self.signature)
            .map_err(|e| format!("signature verify error: {}", e))?;
        if !ok {
            return Err("signature invalid".into());
        }
        Ok(())
    }
}

/// Parse `XELIXIR_ADMIN_PUBKEYS=key1,key2,...` from env. Empty/unset → empty Vec.
pub fn read_admin_pubkeys_from_env() -> Vec<String> {
    std::env::var("XELIXIR_ADMIN_PUBKEYS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Read the node's trusted fleet-admin root pubkey (`ECK_FLEET_ROOT_PUBKEY`).
/// `None`/empty disables the cert-chain path (legacy allow-list only).
pub fn read_fleet_root_from_env() -> Option<String> {
    std::env::var("ECK_FLEET_ROOT_PUBKEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
