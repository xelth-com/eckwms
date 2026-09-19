//! The safe payload — the KEK-sealed blob and the key-derivation it feeds.
//!
//! The blob is `AEAD_KEK(VaultPayload-JSON)` (`.eck/PII_VAULT.md` §2). It carries
//! the tenant master key (TMK) plus the epoch-0 *imported* legacy secrets, so a
//! fleet that switches on the vault keeps byte-identical PPRL tokens and mesh
//! envelopes until an operator deliberately rotates. From the payload we derive
//! the live ring keys (pepper / mesh data key / vault key) — see [`derive`].

use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

/// Domain tag for every vault AAD and HKDF salt.
const DOMAIN: &str = "9eck-vault-v1";

/// The plaintext inside the safe blob. All secret fields are wiped on drop.
#[derive(Clone, Serialize, Deserialize, Zeroize, zeroize::ZeroizeOnDrop)]
pub struct VaultPayload {
    /// Schema version (= 1).
    pub v: u32,
    /// Tenant master key — the root of the whole key hierarchy.
    #[serde(with = "b64_32")]
    pub tmk: [u8; 32],
    /// Epoch-0 imported pepper (today's `SYNC_SECRET` bytes). `None` once rotated
    /// to a derived pepper (pp_epoch >= 1).
    #[serde(default, with = "b64_vec_opt")]
    pub legacy_pp: Option<Vec<u8>>,
    /// Epoch-0 imported `MESH_DATA_KEY`. `None` once rotated (mdk_epoch >= 1).
    #[serde(default, with = "b64_32_opt")]
    pub legacy_mdk: Option<[u8; 32]>,
    pub pp_epoch: u32,
    pub mdk_epoch: u32,
    pub vk_epoch: u32,
    /// Epoch of the data-at-rest volume key (`DFK`, `.eck/DATA_AT_REST.md`).
    /// `#[serde(default)]` so blobs sealed before the datafs feature existed
    /// open unchanged and derive DFK[0].
    ///
    /// **Bumping this re-keys the gocryptfs volume's passphrase** — the volume
    /// must be re-`-init`ed and copied, it is NOT a live rewrap. Leave at 0
    /// unless you are deliberately migrating the volume.
    #[serde(default)]
    pub datafs_epoch: u32,
    /// Retired mesh-data-key epochs that must STILL open stored ciphertext
    /// (`.eck/PII_VAULT.md` §2 "Rotation", P0.7). After `POST /admin/vault/rotate
    /// {"mdk":true}` this holds `[0]`: cache nodes keep epoch-0 envelopes until
    /// the re-envelope sweep has rewritten them, and other full nodes keep
    /// reading them until their keyring TTL refresh lands. `{"finalize":true}`
    /// clears it (and drops `legacy_mdk`) once the sweep reports 100 %.
    #[serde(default)]
    pub prev_mdk_epochs: Vec<u32>,
    /// Retired vault-key epochs still needed to unwrap not-yet-rewrapped DEKs.
    /// Empty until P1 exists; the plumbing is here so a VK bump is a no-op today
    /// and correct once DEKs do exist.
    #[serde(default)]
    pub prev_vk_epochs: Vec<u32>,
    pub created_at: i64,
}

impl std::fmt::Debug for VaultPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultPayload")
            .field("v", &self.v)
            .field("tmk", &"[redacted]")
            .field("legacy_pp", &self.legacy_pp.as_ref().map(|_| "[redacted]"))
            .field("legacy_mdk", &self.legacy_mdk.as_ref().map(|_| "[redacted]"))
            .field("pp_epoch", &self.pp_epoch)
            .field("mdk_epoch", &self.mdk_epoch)
            .field("vk_epoch", &self.vk_epoch)
            .field("datafs_epoch", &self.datafs_epoch)
            .field("prev_mdk_epochs", &self.prev_mdk_epochs)
            .field("prev_vk_epochs", &self.prev_vk_epochs)
            .field("created_at", &self.created_at)
            .finish()
    }
}

// ─── AEAD seal / open ────────────────────────────────────────────────────────

/// AAD binding the blob to `tenant`, `node_id` and `kind`
/// (`"keys"|"recovery"|"enroll"`): `"9eck-vault-v1|{tenant}|{node_id}|{kind}"`.
pub fn aad(tenant: &str, node_id: &str, kind: &str) -> String {
    format!("{DOMAIN}|{tenant}|{node_id}|{kind}")
}

/// Seal a payload under `key` (a KEK or a code key): `24-byte XNonce || ct`.
pub fn seal(payload: &VaultPayload, key: &[u8; 32], aad: &str) -> Result<Vec<u8>, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut pt = Zeroizing::new(serde_json::to_vec(payload).map_err(|e| e.to_string())?);
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &pt,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|e| format!("seal failed: {e}"))?;
    pt.zeroize();
    let mut blob = Vec::with_capacity(24 + ct.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    Ok(blob)
}

/// Open a blob produced by [`seal`]. Any tamper (wrong key, wrong AAD, flipped
/// byte) surfaces as `Err` — XChaCha20-Poly1305 authenticates the ciphertext.
pub fn open(blob: &[u8], key: &[u8; 32], aad: &str) -> Result<VaultPayload, String> {
    if blob.len() < 24 {
        return Err("blob too short".into());
    }
    let (nonce, ct) = blob.split_at(24);
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "bad key length".to_string())?;
    let pt = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| format!("open failed (wrong key/aad or tampered?): {e}"))?,
    );
    serde_json::from_slice(&pt).map_err(|e| format!("payload json: {e}"))
}

// ─── key derivation ──────────────────────────────────────────────────────────

/// The live ring keys derived from a payload.
pub struct Derived {
    pub pepper: Zeroizing<Vec<u8>>,
    pub pp_epoch: u32,
    pub mdk: Zeroizing<[u8; 32]>,
    pub mdk_epoch: u32,
    pub vk: Zeroizing<[u8; 32]>,
    pub vk_epoch: u32,
    /// Data-at-rest volume key — HKDF(TMK, "datafs", datafs_epoch). Feeds the
    /// gocryptfs passphrase for the encrypted `data/` volume
    /// (`crate::vault::datafs`). Always derived, never legacy-imported.
    pub dfk: Zeroizing<[u8; 32]>,
    pub dfk_epoch: u32,
    /// Retired mesh data keys, newest epoch first — decrypt-only. Never used to
    /// encrypt: [`Derived::mdk`] is always the one that seals new envelopes.
    pub prev_mdk: Vec<(u32, Zeroizing<[u8; 32]>)>,
    /// Retired vault keys, newest epoch first — unwrap-only (P1 DEK rewrap).
    pub prev_vk: Vec<(u32, Zeroizing<[u8; 32]>)>,
}

/// HKDF-SHA256(ikm = tmk, salt = "9eck-vault-v1", info = "{label}|{epoch}") → 32B.
fn hkdf32(tmk: &[u8; 32], label: &str, epoch: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(DOMAIN.as_bytes()), tmk);
    let info = format!("{label}|{epoch}");
    let mut okm = [0u8; 32];
    hk.expand(info.as_bytes(), &mut okm)
        .expect("32 is a valid HKDF-SHA256 output length");
    okm
}

/// Derive the ring keys from a payload. At epoch 0 with legacy values present the
/// pepper and mesh data key are the imported legacy bytes (byte-identical to the
/// pre-vault env config, so tokens and envelopes do not change on cut-over); the
/// vault key is always HKDF-derived. Any epoch >= 1 is fully HKDF-derived.
pub fn derive(p: &VaultPayload) -> Derived {
    let pepper = match (p.pp_epoch, p.legacy_pp.as_ref()) {
        (0, Some(pp)) => Zeroizing::new(pp.clone()),
        _ => Zeroizing::new(hkdf32(&p.tmk, "pprl", p.pp_epoch).to_vec()),
    };
    let mdk = Zeroizing::new(mesh_key_for_epoch(p, p.mdk_epoch));
    let vk = Zeroizing::new(hkdf32(&p.tmk, "vault", p.vk_epoch));
    let dfk = Zeroizing::new(hkdf32(&p.tmk, "datafs", p.datafs_epoch));
    Derived {
        pepper,
        pp_epoch: p.pp_epoch,
        mdk,
        mdk_epoch: p.mdk_epoch,
        vk,
        vk_epoch: p.vk_epoch,
        dfk,
        dfk_epoch: p.datafs_epoch,
        prev_mdk: prev_epochs(&p.prev_mdk_epochs, p.mdk_epoch)
            .map(|e| (e, Zeroizing::new(mesh_key_for_epoch(p, e))))
            .collect(),
        prev_vk: prev_epochs(&p.prev_vk_epochs, p.vk_epoch)
            .map(|e| (e, Zeroizing::new(hkdf32(&p.tmk, "vault", e))))
            .collect(),
    }
}

/// The mesh data key of ONE epoch. Epoch 0 is the P0.5 bootstrap import (the
/// literal old `MESH_DATA_KEY` bytes) whenever the blob still carries it — that
/// is what keeps pre-rotation envelopes readable; every other epoch is HKDF.
fn mesh_key_for_epoch(p: &VaultPayload, epoch: u32) -> [u8; 32] {
    match (epoch, p.legacy_mdk.as_ref()) {
        (0, Some(mdk)) => *mdk,
        _ => hkdf32(&p.tmk, "mesh", epoch),
    }
}

/// Retired epochs, newest first, without the current one and without duplicates.
fn prev_epochs(list: &[u32], current: u32) -> std::vec::IntoIter<u32> {
    let mut sorted: Vec<u32> = list.iter().copied().filter(|e| *e != current).collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    sorted.into_iter()
}

// ─── rotation (P0.7, `.eck/PII_VAULT.md` §2 "Rotation") ──────────────────────

/// Bump the requested epochs by one and remember the retired ones so the ring
/// can still DECRYPT what they sealed. Pure: the caller seals, splits and pushes
/// the result. The pepper (`pp_epoch`) is deliberately NOT rotatable here — a new
/// pepper re-mints every PPRL token and needs the P1 vault to do it (P1b).
pub fn rotate(p: &VaultPayload, mdk: bool, vk: bool) -> VaultPayload {
    let mut next = p.clone();
    if mdk {
        next.prev_mdk_epochs.push(p.mdk_epoch);
        next.mdk_epoch = p.mdk_epoch + 1;
    }
    if vk {
        next.prev_vk_epochs.push(p.vk_epoch);
        next.vk_epoch = p.vk_epoch + 1;
    }
    next.prev_mdk_epochs.sort_unstable();
    next.prev_mdk_epochs.dedup();
    next.prev_vk_epochs.sort_unstable();
    next.prev_vk_epochs.dedup();
    next
}

/// Retire every previous epoch: the ring loses its decrypt fallback and the
/// epoch-0 imported `MESH_DATA_KEY` is dropped from the blob for good. Only call
/// this once the re-envelope sweep reports 100 % — afterwards a surviving
/// epoch-0 envelope anywhere in the fleet is unreadable, by design.
///
/// `legacy_pp` is kept: the pepper stays at epoch 0 until P1b re-mints tokens.
pub fn finalize(p: &VaultPayload) -> VaultPayload {
    let mut next = p.clone();
    next.prev_mdk_epochs.clear();
    next.prev_vk_epochs.clear();
    if next.mdk_epoch != 0 {
        next.legacy_mdk = None;
    }
    next
}

// ─── code / recovery keys ────────────────────────────────────────────────────

/// Wrapping key for enroll codes / recovery keys:
/// `SHA-256("9eck-vault-code-v1" || secret)`. Secrets are >= 160-bit random, so
/// no password stretching is needed.
pub fn code_key(secret: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"9eck-vault-code-v1");
    h.update(secret);
    h.finalize().into()
}

// ─── human-readable secrets (Crockford base32) ───────────────────────────────

/// Crockford base32 alphabet (excludes I, L, O, U).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encode a 32-byte random secret as uppercase Crockford base32 in groups of 4
/// separated by '-' (e.g. `A1B2-C3D4-...`). 256 bits → 52 symbols.
pub fn encode_secret(bytes: &[u8; 32]) -> String {
    let mut symbols = String::with_capacity(52);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes.iter() {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((acc >> bits) & 0x1f) as usize;
            symbols.push(CROCKFORD[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((acc << (5 - bits)) & 0x1f) as usize;
        symbols.push(CROCKFORD[idx] as char);
    }
    // Group in 4s with dashes.
    let mut out = String::with_capacity(symbols.len() + symbols.len() / 4);
    for (i, c) in symbols.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

/// Decode a value produced by [`encode_secret`]. Ignores case, dashes and
/// spaces; folds the visual confusables O→0 and I/L→1.
pub fn parse_secret(s: &str) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut n = 0usize;
    for ch in s.chars() {
        if ch == '-' || ch.is_whitespace() {
            continue;
        }
        let up = ch.to_ascii_uppercase();
        let folded = match up {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        };
        let idx = CROCKFORD
            .iter()
            .position(|&c| c as char == folded)
            .ok_or_else(|| format!("invalid base32 char '{ch}'"))?;
        acc = (acc << 5) | idx as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            if n >= 32 {
                return Err("secret too long (expected 32 bytes)".into());
            }
            out[n] = ((acc >> bits) & 0xff) as u8;
            n += 1;
        }
    }
    if n != 32 {
        return Err(format!("expected 32 bytes, decoded {n}"));
    }
    Ok(out)
}

// ─── serde base64 helpers (byte fields as compact strings in the blob JSON) ───

mod b64_32 {
    use super::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let b = STANDARD
            .decode(s.trim())
            .map_err(serde::de::Error::custom)?;
        <[u8; 32]>::try_from(b.as_slice())
            .map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

mod b64_32_opt {
    use super::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<[u8; 32]>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_some(&STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 32]>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            None => Ok(None),
            Some(s) => {
                let b = STANDARD
                    .decode(s.trim())
                    .map_err(serde::de::Error::custom)?;
                Ok(Some(
                    <[u8; 32]>::try_from(b.as_slice())
                        .map_err(|_| serde::de::Error::custom("expected 32 bytes"))?,
                ))
            }
        }
    }
}

mod b64_vec_opt {
    use super::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_some(&STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            None => Ok(None),
            Some(s) => Ok(Some(
                STANDARD.decode(s.trim()).map_err(serde::de::Error::custom)?,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pp_epoch: u32, mdk_epoch: u32, vk_epoch: u32) -> VaultPayload {
        VaultPayload {
            v: 1,
            tmk: [0x11; 32],
            legacy_pp: Some(b"legacy-pepper-bytes".to_vec()),
            legacy_mdk: Some([0x22; 32]),
            pp_epoch,
            mdk_epoch,
            vk_epoch,
            datafs_epoch: 0,
            prev_mdk_epochs: Vec::new(),
            prev_vk_epochs: Vec::new(),
            created_at: 1_725_000_000,
        }
    }

    // ── P0.7 rotation ────────────────────────────────────────────────────────

    #[test]
    fn rotate_mdk_bumps_the_epoch_and_keeps_the_old_one_for_decrypt() {
        let p0 = sample(0, 0, 0);
        let p1 = rotate(&p0, true, false);
        assert_eq!(p1.mdk_epoch, 1, "mdk epoch must advance");
        assert_eq!(p1.prev_mdk_epochs, vec![0]);
        // pp and vk are untouched by an mdk-only rotation.
        assert_eq!(p1.pp_epoch, 0);
        assert_eq!(p1.vk_epoch, 0);
        assert_eq!(p1.tmk, p0.tmk, "TMK never changes on rotation");

        let d0 = derive(&p0);
        let d1 = derive(&p1);
        // The new current key is HKDF(TMK,"mesh",1) and is NOT the old one…
        assert_eq!(*d1.mdk, hkdf32(&p0.tmk, "mesh", 1));
        assert_ne!(*d1.mdk, *d0.mdk);
        // …and the retired epoch-0 key (the imported MESH_DATA_KEY) is still
        // available for decrypt, byte-identical to what it was before.
        assert_eq!(d1.prev_mdk.len(), 1);
        assert_eq!(d1.prev_mdk[0].0, 0);
        assert_eq!(*d1.prev_mdk[0].1, *d0.mdk);
        // The pepper does NOT move — tokens stay linkable (P1b does that one).
        assert_eq!(d1.pepper.as_slice(), d0.pepper.as_slice());
    }

    #[test]
    fn rotate_vk_bumps_and_keeps_prev() {
        let p1 = rotate(&sample(0, 0, 0), false, true);
        assert_eq!(p1.vk_epoch, 1);
        assert_eq!(p1.mdk_epoch, 0, "vk-only rotation must not move the mdk");
        let d = derive(&p1);
        assert_eq!(*d.vk, hkdf32(&p1.tmk, "vault", 1));
        assert_eq!(d.prev_vk.len(), 1);
        assert_eq!(d.prev_vk[0].0, 0);
        assert_eq!(*d.prev_vk[0].1, hkdf32(&p1.tmk, "vault", 0));
    }

    #[test]
    fn rotate_twice_keeps_every_retired_epoch_newest_first() {
        let p2 = rotate(&rotate(&sample(0, 0, 0), true, true), true, true);
        assert_eq!(p2.mdk_epoch, 2);
        assert_eq!(p2.prev_mdk_epochs, vec![0, 1]);
        let d = derive(&p2);
        // Newest retired epoch first — decrypt tries the likeliest key early.
        assert_eq!(d.prev_mdk.iter().map(|(e, _)| *e).collect::<Vec<_>>(), vec![1, 0]);
        assert_eq!(d.prev_vk.iter().map(|(e, _)| *e).collect::<Vec<_>>(), vec![1, 0]);
        // Every retired mdk still derives to what it was when it was current.
        assert_eq!(*d.prev_mdk[1].1, *derive(&sample(0, 0, 0)).mdk);
        assert_eq!(*d.prev_mdk[0].1, hkdf32(&p2.tmk, "mesh", 1));
    }

    #[test]
    fn rotate_is_idempotent_in_shape_not_in_epoch() {
        // Rotating an already-rotated blob never duplicates a retired epoch.
        let mut p = rotate(&sample(0, 0, 0), true, false);
        p.prev_mdk_epochs.push(0); // simulate a sloppy caller / retry
        let p2 = rotate(&p, true, false);
        assert_eq!(p2.mdk_epoch, 2);
        assert_eq!(p2.prev_mdk_epochs, vec![0, 1], "no duplicates");
    }

    #[test]
    fn finalize_drops_prev_epochs_and_the_imported_legacy_mdk() {
        let p1 = rotate(&sample(0, 0, 0), true, true);
        let f = finalize(&p1);
        assert!(f.prev_mdk_epochs.is_empty());
        assert!(f.prev_vk_epochs.is_empty());
        assert!(f.legacy_mdk.is_none(), "epoch-0 MESH_DATA_KEY must be gone");
        // The pepper import STAYS — pp is still at epoch 0 until P1b.
        assert_eq!(f.pp_epoch, 0);
        assert!(f.legacy_pp.is_some());
        let d = derive(&f);
        assert!(d.prev_mdk.is_empty());
        assert!(d.prev_vk.is_empty());
        // Current keys are unchanged by finalize — only the fallback is gone.
        assert_eq!(*d.mdk, *derive(&p1).mdk);
        assert_eq!(d.pepper.as_slice(), b"legacy-pepper-bytes");
    }

    #[test]
    fn finalize_on_an_unrotated_blob_keeps_the_legacy_mdk() {
        // mdk_epoch is still 0 → the legacy bytes ARE the current key; dropping
        // them would brick every envelope in the fleet.
        let f = finalize(&sample(0, 0, 0));
        assert!(f.legacy_mdk.is_some());
        assert_eq!(*derive(&f).mdk, [0x22; 32]);
    }

    #[test]
    fn payload_without_prev_epoch_fields_still_opens() {
        // Blobs sealed before P0.7 carry neither list.
        let mut v = serde_json::to_value(sample(0, 0, 0)).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("prev_mdk_epochs");
        obj.remove("prev_vk_epochs");
        let p: VaultPayload = serde_json::from_value(v).unwrap();
        assert!(p.prev_mdk_epochs.is_empty());
        let d = derive(&p);
        assert!(d.prev_mdk.is_empty());
        assert_eq!(*d.mdk, [0x22; 32]);
    }

    #[test]
    fn seal_open_round_trip() {
        let p = sample(0, 0, 0);
        let key = [7u8; 32];
        let a = aad("tenant-1", "node-1", "keys");
        let blob = seal(&p, &key, &a).unwrap();
        let back = open(&blob, &key, &a).unwrap();
        assert_eq!(back.tmk, p.tmk);
        assert_eq!(back.legacy_pp, p.legacy_pp);
        assert_eq!(back.legacy_mdk, p.legacy_mdk);
        assert_eq!(back.created_at, p.created_at);
    }

    #[test]
    fn open_fails_on_wrong_key_or_aad() {
        let p = sample(0, 0, 0);
        let key = [7u8; 32];
        let a = aad("tenant-1", "node-1", "keys");
        let blob = seal(&p, &key, &a).unwrap();
        assert!(open(&blob, &[9u8; 32], &a).is_err(), "wrong key opened");
        let wrong_aad = aad("tenant-1", "node-2", "keys");
        assert!(open(&blob, &key, &wrong_aad).is_err(), "wrong aad opened");
    }

    #[test]
    fn derive_epoch0_legacy_is_byte_identical() {
        let p = sample(0, 0, 0);
        let d = derive(&p);
        // Epoch 0 with legacy present → the imported bytes, unchanged.
        assert_eq!(d.pepper.as_slice(), b"legacy-pepper-bytes");
        assert_eq!(*d.mdk, [0x22; 32]);
        assert_eq!(d.pp_epoch, 0);
        assert_eq!(d.mdk_epoch, 0);
        // vk is ALWAYS derived, even at epoch 0.
        assert_eq!(*d.vk, hkdf32(&p.tmk, "vault", 0));
    }

    #[test]
    fn derive_epoch1_is_hkdf_deterministic_and_distinct() {
        let p = sample(1, 1, 1);
        let d1 = derive(&p);
        let d2 = derive(&p);
        // Deterministic.
        assert_eq!(d1.pepper.as_slice(), d2.pepper.as_slice());
        assert_eq!(*d1.mdk, *d2.mdk);
        assert_eq!(*d1.vk, *d2.vk);
        // Distinct per label (pepper is 32 bytes here, HKDF-derived).
        assert_ne!(d1.pepper.as_slice(), &d1.mdk[..]);
        assert_ne!(&d1.mdk[..], &d1.vk[..]);
        assert_ne!(d1.pepper.as_slice(), &d1.vk[..]);
        // Epoch 1 ignores legacy imports.
        assert_ne!(d1.pepper.as_slice(), b"legacy-pepper-bytes");
    }

    #[test]
    fn derive_epoch_changes_output() {
        let p0 = sample(0, 0, 0);
        let p1 = sample(0, 0, 1);
        assert_ne!(*derive(&p0).vk, *derive(&p1).vk, "vk_epoch must change vk");
    }

    #[test]
    fn code_key_is_domain_separated() {
        let secret = [3u8; 32];
        let k = code_key(&secret);
        // Not the raw secret, and stable.
        assert_ne!(k, secret);
        assert_eq!(k, code_key(&secret));
    }

    #[test]
    fn encode_parse_round_trip() {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let s = encode_secret(&bytes);
        assert!(s.contains('-'), "expected grouped output: {s}");
        assert_eq!(parse_secret(&s).unwrap(), bytes);
    }

    #[test]
    fn parse_ignores_case_dashes_and_folds_confusables() {
        let bytes = [0xABu8; 32];
        let canonical = encode_secret(&bytes);
        // Lowercase, spaces instead of some dashes — still parses to the same.
        let messy = canonical.to_lowercase().replace('-', " ");
        assert_eq!(parse_secret(&messy).unwrap(), bytes);

        // Confusable folding: O→0, I/L→1 decode to the same value as 0/1.
        let zero32 = encode_secret(&[0u8; 32]); // all '0'
        let with_o = zero32.replace('0', "O");
        assert_eq!(parse_secret(&with_o).unwrap(), [0u8; 32]);
    }

    #[test]
    fn parse_rejects_wrong_length_and_bad_chars() {
        assert!(parse_secret("").is_err());
        assert!(parse_secret("ABCD").is_err()); // too short
        assert!(parse_secret(&format!("{}U", encode_secret(&[0u8; 32]))).is_err()); // 'U' not in alphabet + too long
    }

    #[test]
    fn datafs_key_is_deterministic_domain_separated_and_epoch_aware() {
        let p = sample(0, 0, 0);
        // Deterministic across calls — a node that re-derives always reopens
        // the same gocryptfs volume.
        assert_eq!(*derive(&p).dfk, *derive(&p).dfk);
        // Distinct from every other sub-key (own HKDF label).
        let d = derive(&p);
        assert_ne!(&d.dfk[..], &d.vk[..]);
        assert_ne!(&d.dfk[..], &d.mdk[..]);
        assert_ne!(&d.dfk[..], d.pepper.as_slice());
        assert_eq!(*d.dfk, hkdf32(&p.tmk, "datafs", 0));
        assert_eq!(d.dfk_epoch, 0);
        // Epoch-aware.
        let mut p1 = sample(0, 0, 0);
        p1.datafs_epoch = 1;
        assert_ne!(*derive(&p1).dfk, *d.dfk);
        assert_eq!(derive(&p1).dfk_epoch, 1);
        // A different TMK gives a different volume key.
        let mut p2 = sample(0, 0, 0);
        p2.tmk = [0x99; 32];
        assert_ne!(*derive(&p2).dfk, *d.dfk);
    }

    #[test]
    fn payload_without_datafs_epoch_still_opens() {
        // Blobs sealed before the datafs feature carry no `datafs_epoch`.
        let mut v = serde_json::to_value(sample(0, 0, 0)).unwrap();
        assert!(v.as_object_mut().unwrap().remove("datafs_epoch").is_some());
        let p: VaultPayload = serde_json::from_value(v).unwrap();
        assert_eq!(p.datafs_epoch, 0);
        assert_eq!(*derive(&p).dfk, hkdf32(&p.tmk, "datafs", 0));
    }

    #[test]
    fn debug_redacts_secrets() {
        let p = sample(0, 0, 0);
        let dbg = format!("{p:?}");
        assert!(dbg.contains("[redacted]"));
        assert!(!dbg.contains("legacy-pepper-bytes"));
    }
}
