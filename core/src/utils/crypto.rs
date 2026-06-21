//! Blind-cache symmetric encryption.
//!
//! Full (data) nodes hold `MESH_DATA_KEY` and encrypt entity payloads before
//! they reach a cache node over the reverse-fetch path. A cache node has NO
//! key, so it stores and forwards only ciphertext — it cannot read the data
//! (zero-knowledge). Consumers that hold the key decrypt on read.
//!
//! Envelope shape: `{ "__enc": "<base64( 24-byte XNonce || ciphertext )>" }`.
//! XChaCha20-Poly1305 — 24-byte nonce is safe to generate randomly.

use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use serde_json::Value;

const ENC_FIELD: &str = "__enc";

/// 32-byte data key from `MESH_DATA_KEY` (64 hex chars). `None` on nodes that
/// lack it — i.e. cache nodes, which can therefore neither encrypt nor decrypt
/// and only ever shuttle ciphertext.
pub fn data_key() -> Option<[u8; 32]> {
    let raw = std::env::var("MESH_DATA_KEY").ok()?;
    let bytes = hex::decode(raw.trim()).ok()?;
    bytes.try_into().ok()
}

/// True if `v` is an encryption envelope produced by [`encrypt_json`].
pub fn is_encrypted(v: &Value) -> bool {
    v.get(ENC_FIELD).and_then(|x| x.as_str()).is_some()
}

/// Encrypt a JSON value into `{ "__enc": "<base64>" }`.
pub fn encrypt_json(key: &[u8; 32], v: &Value) -> Result<Value, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let pt = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), pt.as_ref())
        .map_err(|e| format!("encrypt failed: {e}"))?;
    let mut blob = Vec::with_capacity(24 + ct.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    Ok(serde_json::json!({ ENC_FIELD: STANDARD.encode(&blob) }))
}

/// Decrypt an envelope produced by [`encrypt_json`] back to the original value.
pub fn decrypt_json(key: &[u8; 32], v: &Value) -> Result<Value, String> {
    let b64 = v
        .get(ENC_FIELD)
        .and_then(|x| x.as_str())
        .ok_or_else(|| "not an __enc envelope".to_string())?;
    let blob = STANDARD.decode(b64).map_err(|e| format!("base64: {e}"))?;
    if blob.len() < 24 {
        return Err("ciphertext too short".into());
    }
    let (nonce, ct) = blob.split_at(24);
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let pt = cipher
        .decrypt(XNonce::from_slice(nonce), ct)
        .map_err(|e| format!("decrypt failed (wrong key?): {e}"))?;
    serde_json::from_slice(&pt).map_err(|e| e.to_string())
}

/// Outbound transform for mesh entity serving — enforces the blind-cache
/// invariant in ONE place, shared by every serve path (`sync_pull`, relay
/// reverse-fetch `handle_pull_request`).
///
/// - **Owner** (`key = Some`): encrypt every entity before it leaves the wire.
/// - **Blind cache** (`key = None` AND `is_cache`): serve ONLY ciphertext
///   envelopes — WITHHOLD any plaintext row. A cache is never supposed to hold
///   cleartext; if it does (e.g. legacy data synced while it was `role=full`,
///   before blind-cache encryption existed), leaking it would defeat the whole
///   zero-knowledge property. Dropping it is the safe default.
/// - **Plain full node** (`key = None`, not a cache): serve as-is — a normal
///   unencrypted mesh member is allowed to share its own plaintext.
///
/// Returns the entities to actually send; compare its len to the input to know
/// how many were withheld (for logging).
pub fn prepare_outbound(entities: Vec<Value>, key: Option<[u8; 32]>, is_cache: bool) -> Vec<Value> {
    match key {
        Some(k) => entities
            .iter()
            .map(|e| encrypt_json(&k, e).unwrap_or_else(|_| e.clone()))
            .collect(),
        None if is_cache => entities.into_iter().filter(is_encrypted).collect(),
        None => entities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_cache_withholds_plaintext_on_serve() {
        let key = [7u8; 32];
        let plain = serde_json::json!({"id":"trip:1","vehicle_plate":"B AB 123"});
        let enc = encrypt_json(&key, &plain).unwrap();

        // Owner (has key): everything goes out encrypted.
        let out = prepare_outbound(vec![plain.clone()], Some(key), false);
        assert_eq!(out.len(), 1);
        assert!(out.iter().all(is_encrypted));

        // Blind cache (no key, is_cache): plaintext is WITHHELD, ciphertext passes.
        let out = prepare_outbound(vec![plain.clone(), enc.clone()], None, true);
        assert_eq!(out.len(), 1, "the plaintext row must be dropped");
        assert!(is_encrypted(&out[0]));

        // Plain full node (no key, not a cache): serves its own plaintext as-is.
        let out = prepare_outbound(vec![plain.clone()], None, false);
        assert_eq!(out.len(), 1);
        assert!(!is_encrypted(&out[0]));
    }

    #[test]
    fn roundtrip_and_no_plaintext_leak() {
        let key = [7u8; 32];
        let v = serde_json::json!({"id":"ACC001","name":"Transporttasche","qty":42});
        let env = encrypt_json(&key, &v).unwrap();
        assert!(is_encrypted(&env));
        // The envelope exposes no plaintext fields.
        assert!(env.get("name").is_none());
        assert!(!env.to_string().contains("Transporttasche"));
        // Round-trips with the right key.
        assert_eq!(decrypt_json(&key, &env).unwrap(), v);
        // Fails with the wrong key.
        assert!(decrypt_json(&[9u8; 32], &env).is_err());
    }
}
