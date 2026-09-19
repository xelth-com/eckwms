//! Blind-cache symmetric encryption.
//!
//! Full (data) nodes hold `MESH_DATA_KEY` and encrypt entity payloads before
//! they reach a cache node over the reverse-fetch path. A cache node has NO
//! key, so it stores and forwards only ciphertext — it cannot read the data
//! (zero-knowledge). Consumers that hold the key decrypt on read.
//!
//! Envelope shape: `{ "__enc": "<base64( 24-byte XNonce || ciphertext )>" }`,
//! plus `"__e": <mdk epoch>` once the fleet has rotated past epoch 0
//! (`.eck/PII_VAULT.md` §2 "Rotation", P0.7). XChaCha20-Poly1305 — the 24-byte
//! nonce is safe to generate randomly.
//!
//! **Epoch tag compatibility.** An envelope written before P0.7 carries no
//! `__e`; it is epoch 0 by definition and [`decrypt_json_any`] reads it exactly
//! as before. An envelope sealed under epoch 0 is still written WITHOUT the tag,
//! so a node running an older build keeps reading everything a rotated fleet has
//! not yet touched. Decryption never trusts the tag blindly: it only orders the
//! attempts (tagged epoch first, then current, then the retired epochs) — the
//! AEAD tag is what actually decides.

use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use serde_json::Value;

const ENC_FIELD: &str = "__enc";
/// Key-epoch tag on a JSON envelope. Omitted for epoch 0 (wire-identical to
/// pre-rotation builds); present from epoch 1 on.
const EPOCH_FIELD: &str = "__e";

/// 32-byte mesh data key. `None` on nodes that lack it — i.e. cache nodes,
/// which can therefore neither encrypt nor decrypt and only ever shuttle
/// ciphertext. Sourced from the process-global [`crate::keyring`] (authority /
/// keyfile / env `MESH_DATA_KEY`, 64 hex); env-only config is byte-identical to
/// the historical direct read.
pub fn data_key() -> Option<[u8; 32]> {
    crate::keyring::mesh_data_key()
}

/// Epoch of the key [`data_key`] returns — stamped into every new envelope.
pub fn data_key_epoch() -> u32 {
    crate::keyring::mesh_data_key_epoch()
}

/// True if `v` is an encryption envelope produced by [`encrypt_json`].
pub fn is_encrypted(v: &Value) -> bool {
    v.get(ENC_FIELD).and_then(|x| x.as_str()).is_some()
}

/// The key epoch an envelope claims. `None` for an untagged (pre-P0.7, epoch-0)
/// envelope or a non-envelope value.
pub fn envelope_epoch(v: &Value) -> Option<u32> {
    v.get(EPOCH_FIELD)
        .and_then(|x| x.as_u64())
        .map(|e| e as u32)
}

/// Encrypt a JSON value under the LIVE mesh data key epoch.
pub fn encrypt_json(key: &[u8; 32], v: &Value) -> Result<Value, String> {
    encrypt_json_epoch(key, data_key_epoch(), v)
}

/// Encrypt a JSON value into `{ "__enc": "<base64>" [, "__e": epoch] }`.
/// `epoch` must be the epoch of `key` — it is a routing hint for the reader,
/// never a substitute for the AEAD tag.
pub fn encrypt_json_epoch(key: &[u8; 32], epoch: u32, v: &Value) -> Result<Value, String> {
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
    let mut env = serde_json::json!({ ENC_FIELD: STANDARD.encode(&blob) });
    if epoch > 0 {
        env[EPOCH_FIELD] = serde_json::json!(epoch);
    }
    Ok(env)
}

/// Decrypt an envelope with whatever epoch of the mesh data key opens it —
/// current first (or the tagged epoch, if this node still holds it), then the
/// retired epochs the ring kept during a rotation window.
///
/// When nothing opens it, this nudges a safe re-fetch
/// ([`crate::keyring::request_refresh`], throttled): "an envelope I cannot open"
/// is precisely the signal that a peer rotated and this node is still on the old
/// ring — waiting out the full TTL would stall sync for an hour.
pub fn decrypt_json_any(v: &Value) -> Result<Value, String> {
    let keys = ordered_keys(envelope_epoch(v));
    if keys.is_empty() {
        return Err("no mesh data key on this node".into());
    }
    let mut last = String::new();
    for (_, k) in &keys {
        match decrypt_json(k, v) {
            Ok(plain) => return Ok(plain),
            Err(e) => last = e,
        }
    }
    crate::keyring::request_refresh();
    Err(format!(
        "no known mesh data key epoch opened this envelope (tag={:?}, tried {}): {last}",
        envelope_epoch(v),
        keys.len()
    ))
}

/// Candidate decrypt keys, best guess first: the envelope's tagged epoch (when
/// this node holds it), then the ring order (current, then retired newest-first).
fn ordered_keys(tag: Option<u32>) -> Vec<(u32, [u8; 32])> {
    let mut keys = crate::keyring::mesh_data_keys_for_decrypt();
    if let Some(want) = tag {
        if let Some(pos) = keys.iter().position(|(e, _)| *e == want) {
            keys.swap(0, pos);
        }
    }
    keys
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
/// Carries `"e": <mdk epoch>` from epoch 1 on (omitted at epoch 0, so the wire
/// stays byte-compatible with pre-rotation readers).
pub fn encrypt_bytes(key: &[u8; 32], data: &[u8]) -> Result<Value, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), data)
        .map_err(|e| format!("encrypt failed: {e}"))?;
    let mut env = serde_json::json!({
        "v": BYTES_ENC_VERSION,
        "n": STANDARD.encode(nonce),
        "ct": STANDARD.encode(&ct),
    });
    let epoch = data_key_epoch();
    if epoch > 0 {
        env["e"] = serde_json::json!(epoch);
    }
    Ok(env)
}

/// Decrypt a [`encrypt_bytes`] envelope with whatever mesh-data-key epoch opens
/// it (same fallback + refresh-nudge policy as [`decrypt_json_any`]).
pub fn decrypt_bytes_any(env: &Value) -> Result<Vec<u8>, String> {
    let tag = env.get("e").and_then(|x| x.as_u64()).map(|e| e as u32);
    let keys = ordered_keys(tag);
    if keys.is_empty() {
        return Err("no mesh data key on this node".into());
    }
    let mut last = String::new();
    for (_, k) in &keys {
        match decrypt_bytes(k, env) {
            Ok(b) => return Ok(b),
            Err(e) => last = e,
        }
    }
    crate::keyring::request_refresh();
    Err(format!(
        "no known mesh data key epoch opened this blob envelope (tag={tag:?}, tried {}): {last}",
        keys.len()
    ))
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

/// Encrypt the `payload` field of each `document_raw`-shaped row with the mesh
/// data key, leaving the other columns (id/type/ticket_id/updated_at) as plain
/// routing metadata. Shared by the raw-doc pull-serve endpoint
/// (`GET /api/mesh/raw-docs/:ticket_id`) and the push side of the
/// `request_raw_docs` reverse-fetch (`sync::engine`) — PII_VAULT.md P0#4: a
/// `document_raw.payload` is a whole customer ticket/thread (name/email/phone
/// in clear) and must never cross the mesh unenveloped.
pub fn encrypt_raw_doc_payloads(rows: Vec<Value>, key: &[u8; 32]) -> Vec<Value> {
    rows.into_iter()
        .map(|mut row| {
            if let Some(payload) = row.get("payload").cloned() {
                match encrypt_json(key, &payload) {
                    Ok(enc) => {
                        row["payload"] = enc;
                    }
                    Err(e) => {
                        tracing::warn!("raw doc payload encrypt failed, row withheld: {}", e);
                    }
                }
            }
            row
        })
        .collect()
}

/// Accept EITHER shape for a `document_raw.payload` that just arrived over the
/// mesh (pull response or push): the new `{"__enc": ...}` envelope, or legacy
/// plaintext from a peer that hasn't rolled the encryption forward yet. Mixed
/// fleets are normal for ~15 min during a deploy wave (PII_VAULT.md P0#4).
///
/// Returns `Some(row)` with `payload` decrypted back to plaintext for storage
/// (raw docs are not yet encrypted at rest — see PII_VAULT.md P1), or `None`
/// if the row is an envelope this node cannot open (no data key) — callers
/// must skip such a row rather than store the ciphertext as if it were data.
pub fn decrypt_raw_doc_payload(mut row: Value) -> Option<Value> {
    let Some(payload) = row.get("payload").cloned() else {
        return Some(row);
    };
    if !is_encrypted(&payload) {
        return Some(row); // legacy plaintext — store as today.
    }
    // Any epoch the ring still knows — during a rotation window a peer may send
    // epoch 0 or epoch 1 depending on whether it has refreshed yet.
    match decrypt_json_any(&payload) {
        Ok(plain) => {
            row["payload"] = plain;
            Some(row)
        }
        Err(e) => {
            tracing::warn!("raw doc payload decrypt failed, row skipped: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serializes tests that mutate the process-wide MESH_DATA_KEY env var
    // (data_key()/keyring falls back to it when the authority keyring is
    // never initialized, which is the case in this test binary).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn set_mesh_data_key(key: &[u8; 32]) {
        std::env::set_var("MESH_DATA_KEY", hex::encode(key));
    }

    #[test]
    fn raw_doc_payload_roundtrips_through_encrypt_decrypt() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = [3u8; 32];
        set_mesh_data_key(&key);

        let rows = vec![serde_json::json!({
            "id": "document_raw:abc",
            "type": "support_ticket",
            "ticket_id": "abc",
            "payload": {"customer": "Max Mustermann", "email": "max@example.com"},
            "updated_at": "2026-09-06T00:00:00Z",
        })];

        let enveloped = encrypt_raw_doc_payloads(rows.clone(), &key);
        assert_eq!(enveloped.len(), 1);
        // The payload leaves as ciphertext; no clear PII in the outbound row.
        assert!(is_encrypted(&enveloped[0]["payload"]));
        assert!(!enveloped[0].to_string().contains("Mustermann"));
        // Other columns are untouched.
        assert_eq!(enveloped[0]["ticket_id"], rows[0]["ticket_id"]);

        // Round-trips back to plaintext on the receiving side.
        let decrypted = decrypt_raw_doc_payload(enveloped[0].clone()).expect("decryptable");
        assert_eq!(decrypted["payload"], rows[0]["payload"]);

        std::env::remove_var("MESH_DATA_KEY");
    }

    #[test]
    fn decrypt_raw_doc_payload_accepts_legacy_plaintext() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("MESH_DATA_KEY");
        // A row from a peer that hasn't rolled the encryption forward yet —
        // must pass through unchanged (mixed-fleet rollout window).
        let row = serde_json::json!({
            "id": "document_raw:legacy",
            "payload": {"customer": "Legacy Peer"},
        });
        let out = decrypt_raw_doc_payload(row.clone()).expect("legacy plaintext kept");
        assert_eq!(out, row);
    }

    #[test]
    fn decrypt_raw_doc_payload_skips_undecryptable_row_without_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = [9u8; 32];
        let enveloped = encrypt_raw_doc_payloads(
            vec![serde_json::json!({"id": "document_raw:x", "payload": {"customer": "X"}})],
            &key,
        );
        // Receiving node holds NO mesh data key — must skip the row, never
        // store the ciphertext as if it were the plaintext payload.
        std::env::remove_var("MESH_DATA_KEY");
        assert!(decrypt_raw_doc_payload(enveloped[0].clone()).is_none());
    }

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

    // ── P0.7 epoch tag / prev-key fallback ───────────────────────────────────

    #[test]
    fn epoch0_envelope_is_byte_compatible_with_pre_rotation_builds() {
        // An epoch-0 envelope carries NO tag — an older build (which only knows
        // `__enc`) keeps reading everything a rotated fleet hasn't touched yet.
        let key = [1u8; 32];
        let v = serde_json::json!({"id": "document:1", "x": 1});
        let env = encrypt_json_epoch(&key, 0, &v).unwrap();
        assert!(env.get(EPOCH_FIELD).is_none(), "epoch 0 must stay untagged");
        assert_eq!(envelope_epoch(&env), None);
        assert!(is_encrypted(&env));
        assert_eq!(decrypt_json(&key, &env).unwrap(), v);
    }

    #[test]
    fn rotated_envelope_carries_its_epoch_and_still_decrypts_by_key() {
        let key = [2u8; 32];
        let v = serde_json::json!({"id": "document:2"});
        let env = encrypt_json_epoch(&key, 1, &v).unwrap();
        assert_eq!(envelope_epoch(&env), Some(1));
        // The tag is a hint; the key is what decides.
        assert_eq!(decrypt_json(&key, &env).unwrap(), v);
        assert!(decrypt_json(&[9u8; 32], &env).is_err());
        // A forged/wrong tag cannot make the wrong key work.
        let mut lying = env.clone();
        lying[EPOCH_FIELD] = serde_json::json!(7);
        assert_eq!(decrypt_json(&key, &lying).unwrap(), v);
    }

    #[test]
    fn ordered_keys_puts_the_tagged_epoch_first() {
        // Pure ordering check against a synthetic ring (no keyring needed): the
        // helper only reorders what the ring hands it.
        let mut keys = vec![(1u32, [1u8; 32]), (0u32, [0u8; 32])];
        if let Some(pos) = keys.iter().position(|(e, _)| *e == 0) {
            keys.swap(0, pos);
        }
        assert_eq!(keys[0].0, 0);
    }

    #[test]
    fn decrypt_json_any_falls_back_to_the_retired_epoch() {
        let _guard = ENV_LOCK.lock().unwrap();
        // The ring is env-sourced in this test binary, so it knows exactly ONE
        // key. Simulate the rotation window by checking both directions:
        // sealed-under-the-env-key opens, sealed-under-another-key does not.
        let env_key = [3u8; 32];
        set_mesh_data_key(&env_key);
        let v = serde_json::json!({"id": "document:3"});

        let current = encrypt_json_epoch(&env_key, 0, &v).unwrap();
        assert_eq!(decrypt_json_any(&current).unwrap(), v);

        // Tagged with an epoch this node does not hold, sealed with a key it
        // does not hold → refused (and never mis-decrypted).
        let foreign = encrypt_json_epoch(&[4u8; 32], 1, &v).unwrap();
        let err = decrypt_json_any(&foreign).unwrap_err();
        assert!(err.contains("no known mesh data key epoch"), "{err}");

        // …but a foreign TAG over OUR key still opens (tag is only a hint).
        let mistagged = encrypt_json_epoch(&env_key, 1, &v).unwrap();
        assert_eq!(decrypt_json_any(&mistagged).unwrap(), v);

        std::env::remove_var("MESH_DATA_KEY");
    }

    #[test]
    fn decrypt_json_any_without_any_key_is_an_error_not_a_panic() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("MESH_DATA_KEY");
        let env = encrypt_json_epoch(&[5u8; 32], 0, &serde_json::json!({"a": 1})).unwrap();
        assert!(decrypt_json_any(&env).is_err());
    }

    #[test]
    fn decrypt_bytes_any_reads_tagged_and_untagged_blobs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = [6u8; 32];
        set_mesh_data_key(&key);
        let data = b"odometer.jpg bytes".to_vec();

        // Untagged (epoch 0, what encrypt_bytes writes with an env ring).
        let env = encrypt_bytes(&key, &data).unwrap();
        assert!(env.get("e").is_none(), "epoch 0 blob envelope stays untagged");
        assert_eq!(decrypt_bytes_any(&env).unwrap(), data);

        // Tagged by a rotated peer, same key → still opens.
        let mut tagged = env.clone();
        tagged["e"] = serde_json::json!(1);
        assert_eq!(decrypt_bytes_any(&tagged).unwrap(), data);

        // Sealed under a key we don't have → refused.
        let foreign = encrypt_bytes(&[8u8; 32], &data).unwrap();
        assert!(decrypt_bytes_any(&foreign).is_err());

        std::env::remove_var("MESH_DATA_KEY");
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
