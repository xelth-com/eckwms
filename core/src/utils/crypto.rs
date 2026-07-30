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

/// Envelope version for [`encrypt_bytes`]/[`decrypt_bytes`].
const BYTES_ENC_VERSION: u64 = 1;

/// Encrypt RAW BYTES (not a JSON value) into a compact envelope safe for JSON
/// transport over the relay:
/// `{ "v": 1, "n": "<base64 24-byte XNonce>", "ct": "<base64 ciphertext>" }`.
///
/// Unlike [`encrypt_json`] (which serializes a `Value` and hides the whole blob
/// in one `__enc` string), this keeps nonce and ciphertext as separate compact
/// fields — used by the cross-NAT CAS blob fetch (`file_fetch`) so the relay
/// only ever shuttles ciphertext, same zero-knowledge policy as the entity path.
/// XChaCha20-Poly1305; the 24-byte nonce is safe to generate randomly.
pub fn encrypt_bytes(key: &[u8; 32], data: &[u8]) -> Result<Value, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), data)
        .map_err(|e| format!("encrypt failed: {e}"))?;
    Ok(serde_json::json!({
        "v": BYTES_ENC_VERSION,
        "n": STANDARD.encode(nonce),
        "ct": STANDARD.encode(&ct),
    }))
}

/// Decrypt an envelope produced by [`encrypt_bytes`] back to raw bytes. Any
/// tamper (bad version, malformed base64, flipped ciphertext/nonce, wrong key)
/// surfaces as `Err` — XChaCha20-Poly1305 authenticates the ciphertext.
pub fn decrypt_bytes(key: &[u8; 32], env: &Value) -> Result<Vec<u8>, String> {
    let version = env
        .get("v")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| "missing envelope version".to_string())?;
    if version != BYTES_ENC_VERSION {
        return Err(format!("unsupported envelope version {version}"));
    }
    let nonce_b64 = env
        .get("n")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing nonce".to_string())?;
    let ct_b64 = env
        .get("ct")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing ciphertext".to_string())?;
    let nonce = STANDARD
        .decode(nonce_b64)
        .map_err(|e| format!("base64 nonce: {e}"))?;
    if nonce.len() != 24 {
        return Err("bad nonce length".into());
    }
    let ct = STANDARD
        .decode(ct_b64)
        .map_err(|e| format!("base64 ciphertext: {e}"))?;
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    cipher
        .decrypt(XNonce::from_slice(&nonce), ct.as_ref())
        .map_err(|e| format!("decrypt failed (wrong key or tampered?): {e}"))
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

    #[test]
    fn encrypt_bytes_roundtrip_and_tamper() {
        let key = [7u8; 32];
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let env = encrypt_bytes(&key, &data).unwrap();

        // Compact envelope shape; no plaintext bytes leak into the JSON.
        assert_eq!(env.get("v").and_then(|v| v.as_u64()), Some(BYTES_ENC_VERSION));
        assert!(env.get("n").and_then(|v| v.as_str()).is_some());
        assert!(env.get("ct").and_then(|v| v.as_str()).is_some());

        // Round-trips with the right key.
        assert_eq!(decrypt_bytes(&key, &env).unwrap(), data);
        // Wrong key is rejected (AEAD tag mismatch).
        assert!(decrypt_bytes(&[9u8; 32], &env).is_err());

        // Tamper with the ciphertext → decrypt must fail (authenticated).
        let mut tampered = env.clone();
        let ct = tampered["ct"].as_str().unwrap();
        let mut raw = STANDARD.decode(ct).unwrap();
        raw[0] ^= 0xFF;
        tampered["ct"] = serde_json::json!(STANDARD.encode(&raw));
        assert!(decrypt_bytes(&key, &tampered).is_err());

        // A bumped version is refused rather than mis-decrypted.
        let mut wrong_ver = env.clone();
        wrong_ver["v"] = serde_json::json!(BYTES_ENC_VERSION + 1);
        assert!(decrypt_bytes(&key, &wrong_ver).is_err());

        // Empty payload round-trips too.
        let empty = encrypt_bytes(&key, &[]).unwrap();
        assert_eq!(decrypt_bytes(&key, &empty).unwrap(), Vec::<u8>::new());
    }
}
