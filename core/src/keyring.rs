//! Process-global key material — the PPRL pepper, the mesh data key, and the
//! vault key — sourced from the zero-knowledge safe on eck1/2/3 (managed nodes),
//! a local keyfile (self-hosted), or the `.env` fallback (dev / pre-vault), in
//! that order. See `.eck/PII_VAULT.md` §2 and §6 P0.5.
//!
//! **9eck never holds a key that opens data.** The safe servers store only opaque
//! Shamir shares of a KEK-sealed blob; the KEK never leaves the node. A node
//! fetches any two shares, rebuilds and opens the blob with its own KEK, and
//! derives the ring keys here (`core/src/vault`). Every pepper / data-key
//! consumer reads its key from here instead of `std::env` directly, so a managed
//! node keeps no key material on disk beyond its KEK. Behaviour with env-only
//! config is byte-identical to the old direct-`env` reads (same pepper bytes →
//! same PPRL tokens, same `MESH_DATA_KEY` → same envelopes) — and at safe epoch 0
//! the imported legacy bytes are handed back verbatim, so switching a fleet onto
//! the safe changes no tokens or envelopes until an operator rotates.
//!
//! ## Loader order (`init` at boot)
//! 1. `ECK_VAULT_NODES` set → `vault::client::load_from_safe` (fetch >= 2 shares,
//!    rebuild + open the blob with the node KEK, derive) → source `Safe`,
//!    refreshed on [`vault::SAFE_TTL_SECS`].
//! 2. else `ECK_VAULT_KEYFILE` set → keys JSON read from that file.
//! 3. else env: `pepper = SYNC_SECRET`, `mdk = MESH_DATA_KEY` (64 hex), no vault
//!    key — logged `WARN keys from env`.
//!
//! Safe / keyfile failure falls back to env with an `ERROR` log; it never panics.
//! Keys never touch disk (the KEK aside). `Debug` prints `[redacted]`.
//!
//! ## Rotation window (P0.7)
//! After `POST /admin/vault/rotate {"mdk":true}` on the home node the ring holds
//! the NEW mesh data key plus every retired epoch, decrypt-only
//! ([`mesh_data_keys_for_decrypt`]). Encryption always uses [`mesh_data_key`],
//! stamped with [`mesh_data_key_epoch`]. Other full nodes pick the new epoch up
//! on their next TTL refresh ([`vault::SAFE_TTL_SECS`]) — or immediately, when a
//! decrypt miss calls [`request_refresh`]. `{"finalize":true}` empties the
//! retired list once the re-envelope sweep is complete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{error, info, warn};
use zeroize::Zeroizing;

use crate::vault::{self, blob::Derived, VaultPayload};

/// Where the live key material came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// Derived from shares fetched from the zero-knowledge safe on eck1/2/3
    /// (`ECK_VAULT_NODES`).
    Safe,
    /// Loaded from a local keyfile (`ECK_VAULT_KEYFILE`).
    Keyfile,
    /// Read from `.env` (`SYNC_SECRET` / `MESH_DATA_KEY`) — the dev / pre-vault fallback.
    Env,
}

impl KeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Safe => "safe",
            KeySource::Keyfile => "keyfile",
            KeySource::Env => "env",
        }
    }
}

impl std::fmt::Debug for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Process-global key material. All secret fields are wiped on drop (`Zeroizing`);
/// `Debug` never prints bytes.
pub struct KeyRing {
    pepper: Zeroizing<Vec<u8>>,
    pepper_epoch: u32,
    mesh_data_key: Option<Zeroizing<[u8; 32]>>,
    mdk_epoch: u32,
    vault_key: Option<Zeroizing<[u8; 32]>>,
    vault_epoch: u32,
    /// Data-at-rest volume key (`DFK`) for the encrypted `data/` gocryptfs
    /// volume — `.eck/DATA_AT_REST.md`. Present on safe-sourced nodes (HKDF
    /// from the TMK) and on keyfile nodes that carry a `dfk` field. NEVER on
    /// the `.env` fallback: a key that lives on the same disk as the volume it
    /// opens protects nothing (see [`datafs_key`] for the explicit
    /// `ECK_DATAFS_KEYFILE` escape hatch).
    datafs_key: Option<Zeroizing<[u8; 32]>>,
    dfk_epoch: u32,
    /// Older vault epochs still needed to unwrap not-yet-rewrapped DEKs.
    prev_vault: Vec<(u32, Zeroizing<[u8; 32]>)>,
    /// Older mesh-data-key epochs, newest first — **decrypt only**. Populated
    /// after an MDK rotation (`.eck/PII_VAULT.md` §2 "Rotation", P0.7) so this
    /// node keeps opening envelopes that cache nodes / not-yet-refreshed peers
    /// still hold under the retired key. New envelopes always use
    /// [`mesh_data_key`]; `{"finalize":true}` empties this list.
    prev_mdk: Vec<(u32, Zeroizing<[u8; 32]>)>,
    source: KeySource,
    #[allow(dead_code)]
    fetched_at: Instant,
    ttl: Duration,
    /// Safe blob version this ring was built from (0 for env/keyfile).
    version: u32,
    /// The sealed payload, kept in RAM on safe-sourced nodes so the admin
    /// bootstrap/enroll/recover handlers can re-seal it under a code / a new
    /// node's KEK. `None` on env / keyfile sources. Wiped on drop.
    payload: Option<VaultPayload>,
}

impl std::fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRing")
            .field("source", &self.source)
            .field("pepper", &"[redacted]")
            .field("pepper_epoch", &self.pepper_epoch)
            .field("mesh_data_key", &self.mesh_data_key.as_ref().map(|_| "[redacted]"))
            .field("mdk_epoch", &self.mdk_epoch)
            .field("vault_key", &self.vault_key.as_ref().map(|_| "[redacted]"))
            .field("vault_epoch", &self.vault_epoch)
            .field("datafs_key", &self.datafs_key.as_ref().map(|_| "[redacted]"))
            .field("dfk_epoch", &self.dfk_epoch)
            .field("prev_vault", &format_args!("[{} redacted]", self.prev_vault.len()))
            .field("prev_mdk", &format_args!("[{} redacted]", self.prev_mdk.len()))
            .field("ttl", &self.ttl)
            .field("version", &self.version)
            .field("payload", &self.payload.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Wire / keyfile JSON shape (`.eck/PII_VAULT.md` §2):
/// `{pp, pp_epoch, mdk, mdk_epoch, vk, vk_epoch, prev:[{vk,vk_epoch}], expires_in_secs}`.
/// `pp` is base64 pepper bytes; `mdk`/`vk` are base64 of a 32-byte key or null.
#[derive(Deserialize)]
struct WireKeys {
    pp: String,
    #[serde(default)]
    pp_epoch: u32,
    #[serde(default)]
    mdk: Option<String>,
    #[serde(default)]
    mdk_epoch: u32,
    #[serde(default)]
    vk: Option<String>,
    #[serde(default)]
    vk_epoch: u32,
    /// Base64 of the 32-byte data-at-rest volume key, or null/absent on nodes
    /// that do not run an encrypted `data/` volume.
    #[serde(default)]
    dfk: Option<String>,
    #[serde(default)]
    dfk_epoch: u32,
    #[serde(default)]
    prev: Vec<WirePrevVk>,
    /// Retired mesh data keys kept for DECRYPT only (P0.7 rotation window):
    /// `[{"mdk": "<b64-32>", "mdk_epoch": 0}]`. Absent on unrotated fleets.
    #[serde(default)]
    prev_mdk: Vec<WirePrevMdk>,
    #[serde(default)]
    expires_in_secs: u64,
}

#[derive(Deserialize)]
struct WirePrevVk {
    vk: String,
    #[serde(default)]
    vk_epoch: u32,
}

#[derive(Deserialize)]
struct WirePrevMdk {
    mdk: String,
    #[serde(default)]
    mdk_epoch: u32,
}

fn key32_from_b64(s: &str) -> Result<[u8; 32], String> {
    let bytes = STANDARD
        .decode(s.trim())
        .map_err(|e| format!("base64: {e}"))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| format!("expected 32 key bytes, got {}", bytes.len()))
}

impl KeyRing {
    /// Build from the keyfile JSON.
    fn from_wire(w: WireKeys, source: KeySource) -> Result<KeyRing, String> {
        let pepper = STANDARD
            .decode(w.pp.trim())
            .map_err(|e| format!("pp base64: {e}"))?;
        if pepper.is_empty() {
            return Err("pp (pepper) is empty".into());
        }
        let mesh_data_key = match w.mdk.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => Some(Zeroizing::new(key32_from_b64(s).map_err(|e| format!("mdk {e}"))?)),
            None => None,
        };
        let vault_key = match w.vk.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => Some(Zeroizing::new(key32_from_b64(s).map_err(|e| format!("vk {e}"))?)),
            None => None,
        };
        let datafs_key = match w.dfk.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => Some(Zeroizing::new(key32_from_b64(s).map_err(|e| format!("dfk {e}"))?)),
            None => None,
        };
        let mut prev_vault = Vec::with_capacity(w.prev.len());
        for p in &w.prev {
            if p.vk.trim().is_empty() {
                continue;
            }
            prev_vault.push((
                p.vk_epoch,
                Zeroizing::new(key32_from_b64(&p.vk).map_err(|e| format!("prev vk {e}"))?),
            ));
        }
        let mut prev_mdk = Vec::with_capacity(w.prev_mdk.len());
        for p in &w.prev_mdk {
            if p.mdk.trim().is_empty() {
                continue;
            }
            prev_mdk.push((
                p.mdk_epoch,
                Zeroizing::new(key32_from_b64(&p.mdk).map_err(|e| format!("prev mdk {e}"))?),
            ));
        }
        prev_mdk.sort_by(|a, b| b.0.cmp(&a.0));
        let ttl = Duration::from_secs(if w.expires_in_secs == 0 { 300 } else { w.expires_in_secs });
        Ok(KeyRing {
            pepper: Zeroizing::new(pepper),
            pepper_epoch: w.pp_epoch,
            mesh_data_key,
            mdk_epoch: w.mdk_epoch,
            vault_key,
            vault_epoch: w.vk_epoch,
            datafs_key,
            dfk_epoch: w.dfk_epoch,
            prev_vault,
            prev_mdk,
            source,
            fetched_at: Instant::now(),
            ttl,
            version: 0,
            payload: None,
        })
    }

    /// Build a ring from safe-derived keys (source `Safe`).
    fn from_derived(d: Derived, ttl: Duration, version: u32) -> KeyRing {
        KeyRing {
            pepper: Zeroizing::new(d.pepper.to_vec()),
            pepper_epoch: d.pp_epoch,
            mesh_data_key: Some(Zeroizing::new(*d.mdk)),
            mdk_epoch: d.mdk_epoch,
            vault_key: Some(Zeroizing::new(*d.vk)),
            vault_epoch: d.vk_epoch,
            datafs_key: Some(Zeroizing::new(*d.dfk)),
            dfk_epoch: d.dfk_epoch,
            prev_vault: d
                .prev_vk
                .iter()
                .map(|(e, k)| (*e, Zeroizing::new(**k)))
                .collect(),
            prev_mdk: d
                .prev_mdk
                .iter()
                .map(|(e, k)| (*e, Zeroizing::new(**k)))
                .collect(),
            source: KeySource::Safe,
            fetched_at: Instant::now(),
            ttl,
            version,
            payload: None,
        }
    }

    /// Parse the keyfile JSON text into a ring.
    fn from_wire_json(text: &str, source: KeySource) -> Result<KeyRing, String> {
        let w: WireKeys = serde_json::from_str(text).map_err(|e| format!("json: {e}"))?;
        KeyRing::from_wire(w, source)
    }

    /// The env fallback: `pepper = SYNC_SECRET` bytes (epoch 0),
    /// `mdk = MESH_DATA_KEY` (64 hex → 32 bytes, exactly as the old
    /// `crypto::data_key`), no vault key. Missing/empty SYNC_SECRET yields an
    /// empty pepper (→ `pepper()` returns `None`, callers fail closed).
    fn from_env() -> KeyRing {
        let pepper = std::env::var("SYNC_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .map(String::into_bytes)
            .unwrap_or_default();
        let mesh_data_key = std::env::var("MESH_DATA_KEY")
            .ok()
            .and_then(|raw| hex::decode(raw.trim()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .map(Zeroizing::new);
        KeyRing {
            pepper: Zeroizing::new(pepper),
            pepper_epoch: 0,
            mesh_data_key,
            mdk_epoch: 0,
            vault_key: None,
            vault_epoch: 0,
            // No datafs key from env — see the field docs.
            datafs_key: None,
            dfk_epoch: 0,
            prev_vault: Vec::new(),
            prev_mdk: Vec::new(),
            source: KeySource::Env,
            fetched_at: Instant::now(),
            ttl: Duration::MAX,
            version: 0,
            payload: None,
        }
    }
}

// ─── globals ────────────────────────────────────────────────────────────────

static RING: OnceLock<RwLock<KeyRing>> = OnceLock::new();
/// Flipped `true` once [`init`] has installed the ring. Until then, accessors
/// read env FRESH on every call — this keeps the dev / test path byte-identical
/// to the historical direct-`env` reads (and order-independent w.r.t. `init`).
static INITED: AtomicBool = AtomicBool::new(false);

/// Context the background safe-refresh loop needs (set once by [`init`]).
struct RefreshCtx {
    http: HttpClient,
    node_id: String,
}
static REFRESH_CTX: OnceLock<RefreshCtx> = OnceLock::new();
/// Ensures at most one refresh loop runs (both `init` and `install` may start it).
static REFRESH_RUNNING: AtomicBool = AtomicBool::new(false);
/// Wakes the refresh loop early (see [`request_refresh`]).
static REFRESH_NUDGE: OnceLock<Arc<tokio::sync::Notify>> = OnceLock::new();
/// Epoch-seconds of the last accepted nudge — throttles a decrypt-miss storm to
/// one extra safe round-trip per [`NUDGE_MIN_INTERVAL_SECS`].
static LAST_NUDGE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
/// Minimum spacing between decrypt-miss-triggered refreshes.
const NUDGE_MIN_INTERVAL_SECS: i64 = 60;

fn nudge_handle() -> &'static Arc<tokio::sync::Notify> {
    REFRESH_NUDGE.get_or_init(|| Arc::new(tokio::sync::Notify::new()))
}

/// Ask the safe-refresh loop to re-fetch NOW instead of waiting out the TTL.
///
/// Called on a decrypt miss (`crate::utils::crypto`): an envelope this node
/// cannot open under any epoch it knows is the exact signature of "a peer has
/// rotated and I am still on the old ring". Throttled to one per minute and a
/// no-op on env / keyfile nodes, so it can be called from any hot path. Safe to
/// call outside a tokio context — `Notify::notify_one` does not await.
pub fn request_refresh() {
    if source() != KeySource::Safe {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let last = LAST_NUDGE.load(Ordering::Acquire);
    if now - last < NUDGE_MIN_INTERVAL_SECS {
        return;
    }
    if LAST_NUDGE
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return; // someone else just nudged
    }
    info!("vault key refresh nudged early (envelope from an unknown key epoch)");
    nudge_handle().notify_one();
}

fn ring() -> &'static RwLock<KeyRing> {
    RING.get_or_init(|| RwLock::new(KeyRing::from_env()))
}

/// Replace the process-global ring (creating the lock if the lazy env ring was
/// never touched).
fn install_ring(kr: KeyRing) {
    match RING.get() {
        Some(lock) => {
            if let Ok(mut g) = lock.write() {
                *g = kr;
            }
        }
        None => {
            let _ = RING.set(RwLock::new(kr));
        }
    }
    INITED.store(true, Ordering::Release);
}

// ─── accessors ────────────────────────────────────────────────────────────────

/// The PPRL pepper, or `None` when none is configured (callers fail closed).
/// Returns an owned `Arc` so it outlives the internal lock; the bytes are wiped
/// when the last clone drops.
pub fn pepper() -> Option<Arc<Zeroizing<Vec<u8>>>> {
    if !INITED.load(Ordering::Acquire) {
        // Pre-init env-fresh path (dev / tests): identical bytes to `.env`.
        let p = std::env::var("SYNC_SECRET").ok().filter(|s| !s.is_empty())?;
        return Some(Arc::new(Zeroizing::new(p.into_bytes())));
    }
    let g = ring().read().ok()?;
    if g.pepper.is_empty() {
        return None;
    }
    Some(Arc::new(Zeroizing::new(g.pepper.to_vec())))
}

/// Run `f` with a borrow of the pepper bytes, or `None` if unset.
pub fn with_pepper<R>(f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    pepper().map(|p| f(p.as_slice()))
}

/// The 32-byte mesh data key (blind-cache envelope key), or `None` on nodes
/// that lack it (cache nodes). Env fallback = `MESH_DATA_KEY` (64 hex).
pub fn mesh_data_key() -> Option<[u8; 32]> {
    if !INITED.load(Ordering::Acquire) {
        return std::env::var("MESH_DATA_KEY")
            .ok()
            .and_then(|raw| hex::decode(raw.trim()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
    }
    let g = ring().read().ok()?;
    g.mesh_data_key.as_ref().map(|k| **k)
}

/// Epoch of the key [`mesh_data_key`] returns — stamped into every NEW envelope
/// (`crate::utils::crypto`) so a reader knows which key opens it without trying
/// them all. 0 on env / keyfile / unrotated fleets.
pub fn mesh_data_key_epoch() -> u32 {
    if !INITED.load(Ordering::Acquire) {
        return 0;
    }
    ring().read().map(|g| g.mdk_epoch).unwrap_or(0)
}

/// Every mesh data key this node may use to DECRYPT, current first then the
/// retired epochs newest-first (`.eck/PII_VAULT.md` §2 "Rotation"). Encryption
/// uses [`mesh_data_key`] only — never an entry from the tail of this list.
///
/// During a rotation window the tail is what keeps epoch-0 envelopes readable:
/// cache nodes still hold them until the re-envelope sweep rewrites them, and a
/// full node that has not yet refreshed its ring still serves them.
pub fn mesh_data_keys_for_decrypt() -> Vec<(u32, [u8; 32])> {
    if !INITED.load(Ordering::Acquire) {
        // Pre-init env-fresh path: exactly the one env key, epoch 0.
        return mesh_data_key().map(|k| vec![(0, k)]).into_iter().flatten().collect();
    }
    let Ok(g) = ring().read() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(1 + g.prev_mdk.len());
    if let Some(k) = g.mesh_data_key.as_ref() {
        out.push((g.mdk_epoch, **k));
    }
    for (e, k) in &g.prev_mdk {
        out.push((*e, **k));
    }
    out
}

/// The retired mesh-data-key epochs the ring still accepts (newest first).
pub fn prev_mdk_epochs() -> Vec<u32> {
    if !INITED.load(Ordering::Acquire) {
        return Vec::new();
    }
    ring()
        .read()
        .map(|g| g.prev_mdk.iter().map(|(e, _)| *e).collect())
        .unwrap_or_default()
}

/// The current vault key (reveal-capable nodes only; P1). `None` on env / cache.
pub fn vault_key() -> Option<[u8; 32]> {
    if !INITED.load(Ordering::Acquire) {
        return None;
    }
    let g = ring().read().ok()?;
    g.vault_key.as_ref().map(|k| **k)
}

/// The 32-byte data-at-rest volume key that opens the encrypted `data/`
/// gocryptfs volume (`crate::vault::datafs`, `.eck/DATA_AT_REST.md`).
///
/// Sources, in order:
/// 1. the live ring, on `Safe` (HKDF(TMK,"datafs",epoch)) or `Keyfile` (`dfk`)
///    nodes — the whole point: the key is NOT on the disk it decrypts;
/// 2. `ECK_DATAFS_KEYFILE`, an explicit operator opt-out for boxes with no safe
///    (the file must live on a different medium than the volume — the binary
///    cannot check that for you).
///
/// The `.env` fallback (`KeySource::Env`) NEVER yields a key on its own.
/// The file may hold 32 raw bytes, 64 hex chars, or base64 — whitespace-trimmed.
pub fn datafs_key() -> Option<[u8; 32]> {
    if INITED.load(Ordering::Acquire) {
        if let Ok(g) = ring().read() {
            if let Some(k) = g.datafs_key.as_ref() {
                return Some(**k);
            }
        }
    }
    datafs_key_from_file()
}

/// Read `ECK_DATAFS_KEYFILE` (raw-32 / hex / base64). `None` when unset.
fn datafs_key_from_file() -> Option<[u8; 32]> {
    let path = std::env::var("ECK_DATAFS_KEYFILE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let raw = match std::fs::read(&path) {
        Ok(b) => Zeroizing::new(b),
        Err(e) => {
            error!("ECK_DATAFS_KEYFILE {path} unreadable: {e}");
            return None;
        }
    };
    if let Ok(k) = <[u8; 32]>::try_from(raw.as_slice()) {
        return Some(k);
    }
    let text = String::from_utf8_lossy(&raw);
    let text = text.trim();
    if let Ok(b) = hex::decode(text) {
        if let Ok(k) = <[u8; 32]>::try_from(b.as_slice()) {
            return Some(k);
        }
    }
    if let Ok(k) = key32_from_b64(text) {
        return Some(k);
    }
    error!("ECK_DATAFS_KEYFILE {path} is not 32 raw bytes, 64 hex chars or base64-32");
    None
}

/// Older vault epochs `(epoch, key)` still needed to unwrap legacy DEKs (P1).
pub fn prev_vault_keys() -> Vec<(u32, [u8; 32])> {
    if !INITED.load(Ordering::Acquire) {
        return Vec::new();
    }
    ring()
        .read()
        .map(|g| g.prev_vault.iter().map(|(e, k)| (*e, **k)).collect())
        .unwrap_or_default()
}

/// The safe blob version the live ring was built from (0 for env / keyfile).
/// Used by the admin enroll / join / recover handlers to stamp share PUTs.
pub fn version() -> u32 {
    if !INITED.load(Ordering::Acquire) {
        return 0;
    }
    ring().read().map(|g| g.version).unwrap_or(0)
}

/// Where the live keys came from.
pub fn source() -> KeySource {
    if !INITED.load(Ordering::Acquire) {
        return KeySource::Env;
    }
    ring().read().map(|g| g.source).unwrap_or(KeySource::Env)
}

/// Status for `/api/status` — epochs + source + presence flags, NEVER bytes.
pub fn status_json() -> Value {
    if !INITED.load(Ordering::Acquire) {
        return json!({
            "source": "env",
            "initialized": false,
            "pepper_present": std::env::var("SYNC_SECRET").map(|s| !s.is_empty()).unwrap_or(false),
        });
    }
    match ring().read() {
        Ok(g) => json!({
            "source": g.source.as_str(),
            "initialized": true,
            "pepper_present": !g.pepper.is_empty(),
            "pepper_epoch": g.pepper_epoch,
            "mdk_present": g.mesh_data_key.is_some(),
            "mdk_epoch": g.mdk_epoch,
            "vk_present": g.vault_key.is_some(),
            "vk_epoch": g.vault_epoch,
            "dfk_present": g.datafs_key.is_some(),
            "dfk_epoch": g.dfk_epoch,
            "prev_vk_count": g.prev_vault.len(),
            "prev_vk_epochs": g.prev_vault.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            "prev_mdk_count": g.prev_mdk.len(),
            "prev_mdk_epochs": g.prev_mdk.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            "version": g.version,
        }),
        Err(_) => json!({ "source": "poisoned", "initialized": true }),
    }
}

// ─── loading ────────────────────────────────────────────────────────────────

/// Log the env-fallback warning and build the env ring.
fn env_with_warn() -> KeyRing {
    warn!(
        "keys loaded from env (.env next to the DB) — set ECK_VAULT_NODES (safe on \
         eck1/2/3) or ECK_VAULT_KEYFILE to source keys from the vault instead"
    );
    KeyRing::from_env()
}

/// Read the keyfile (the `WireKeys` JSON shape).
fn load_keyfile(path: &str) -> Result<KeyRing, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    KeyRing::from_wire_json(&text, KeySource::Keyfile)
}

/// Load key material at boot (loader order in the module docs) and install the
/// process-global ring. Idempotent-ish: only the first call installs the ring;
/// safe mode additionally spawns a background refresh. Never panics — safe /
/// keyfile failure falls back to env with an ERROR.
pub async fn init(http: &HttpClient, node_id: &str) {
    if INITED.load(Ordering::Acquire) {
        return;
    }
    // Remember the context the refresh loop needs (safe source).
    let _ = REFRESH_CTX.set(RefreshCtx {
        http: http.clone(),
        node_id: node_id.to_string(),
    });

    let nodes_set = !vault::client::safe_nodes_from_env().is_empty();
    let keyfile = std::env::var("ECK_VAULT_KEYFILE")
        .ok()
        .filter(|s| !s.trim().is_empty());

    let (built, payload) = if nodes_set {
        match vault::client::load_payload_from_safe(http, node_id, None).await {
            Ok((payload, version)) => {
                let d = vault::blob::derive(&payload);
                info!(
                    "vault keys loaded from safe (pp_epoch={}, mdk_epoch={}, vk_epoch={}, version={version})",
                    d.pp_epoch, d.mdk_epoch, d.vk_epoch
                );
                let ttl = Duration::from_secs(vault::SAFE_TTL_SECS);
                (KeyRing::from_derived(d, ttl, version), Some(payload))
            }
            Err(e) => {
                error!("vault safe fetch failed ({e}); falling back to env");
                (env_with_warn(), None)
            }
        }
    } else if let Some(path) = keyfile.as_deref() {
        match load_keyfile(path) {
            Ok(kr) => {
                info!("vault keys loaded from keyfile {path} (pp_epoch={})", kr.pepper_epoch);
                (kr, None)
            }
            Err(e) => {
                error!("vault keyfile load failed ({e}); falling back to env");
                (env_with_warn(), None)
            }
        }
    } else {
        (env_with_warn(), None)
    };

    let is_safe = built.source == KeySource::Safe;
    let mut built = built;
    built.payload = payload;
    install_ring(built);

    if is_safe {
        spawn_refresh();
    }
}

/// Install a ring built from safe-derived keys, live, without a restart — used by
/// the admin bootstrap / join / recover handlers. Sets source `Safe`, marks the
/// ring initialised, and starts the refresh loop if it is not already running.
///
/// This variant does NOT retain the sealed payload; prefer [`install_payload`]
/// where later enroll/recover operations need [`current_payload`].
pub fn install(derived: Derived, version: u32) {
    let kr = KeyRing::from_derived(derived, Duration::from_secs(vault::SAFE_TTL_SECS), version);
    install_ring(kr);
    spawn_refresh();
}

/// Like [`install`] but from the sealed payload, retaining it in RAM so the
/// enroll / recover handlers can re-seal it (see [`current_payload`]).
pub fn install_payload(payload: VaultPayload, version: u32) {
    let d = vault::blob::derive(&payload);
    let mut kr = KeyRing::from_derived(d, Duration::from_secs(vault::SAFE_TTL_SECS), version);
    kr.payload = Some(payload);
    install_ring(kr);
    spawn_refresh();
}

/// The sealed payload of the live ring, on safe-sourced nodes only. Used by the
/// admin enroll / recover handlers to re-seal the tenant master key under a code
/// or a newcomer's KEK. `None` on env / keyfile sources.
pub fn current_payload() -> Option<VaultPayload> {
    if !INITED.load(Ordering::Acquire) {
        return None;
    }
    ring().read().ok()?.payload.clone()
}

/// Background refresh of the safe-sourced ring (source `Safe` only). Idempotent:
/// only one loop ever runs. On failure the current keys stay valid; 3 consecutive
/// failures escalate to `error!`. Re-fetching within the TTL honours revocation.
fn spawn_refresh() {
    if source() != KeySource::Safe {
        return;
    }
    // Start at most one loop.
    if REFRESH_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let Some(ctx) = REFRESH_CTX.get() else {
        REFRESH_RUNNING.store(false, Ordering::Release);
        return;
    };
    let http = ctx.http.clone();
    let node_id = ctx.node_id.clone();
    tokio::spawn(async move {
        let mut fails: u32 = 0;
        let nudge = nudge_handle().clone();
        loop {
            // Either the TTL elapses, or a decrypt miss nudges us early
            // (`request_refresh`) — the rotation window's fast path.
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(vault::SAFE_TTL_SECS)) => {}
                _ = nudge.notified() => {}
            }
            match vault::client::load_payload_from_safe(&http, &node_id, None).await {
                Ok((payload, version)) => {
                    let d = vault::blob::derive(&payload);
                    let mut kr =
                        KeyRing::from_derived(d, Duration::from_secs(vault::SAFE_TTL_SECS), version);
                    kr.payload = Some(payload);
                    if let Some(lock) = RING.get() {
                        if let Ok(mut g) = lock.write() {
                            *g = kr;
                        }
                    }
                    if fails > 0 {
                        info!("vault key refresh recovered after {fails} failure(s)");
                    }
                    fails = 0;
                }
                Err(e) => {
                    fails += 1;
                    if fails >= 3 {
                        error!("vault key refresh failed {fails}x ({e}); keeping current keys");
                    } else {
                        warn!("vault key refresh failed ({e}); keeping current keys");
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_fallback_round_trips_pepper_and_mdk() {
        let kr = {
            std::env::set_var("SYNC_SECRET", "test_secret");
            std::env::set_var(
                "MESH_DATA_KEY",
                "0101010101010101010101010101010101010101010101010101010101010101",
            );
            KeyRing::from_env()
        };
        // Pepper bytes are EXACTLY the SYNC_SECRET bytes — the old direct-env
        // path — so PPRL tokens are unchanged across the switch to the KeyRing.
        assert_eq!(kr.pepper.as_slice(), b"test_secret");
        assert_eq!(kr.pepper_epoch, 0);
        assert_eq!(kr.mesh_data_key.as_deref(), Some(&[1u8; 32]));
        assert_eq!(kr.source, KeySource::Env);
    }

    #[test]
    fn pepper_bytes_match_legacy_sync_secret() {
        // The pepper the KeyRing hands to anonymizer must be byte-for-byte the
        // SYNC_SECRET value, whatever the source — this is what keeps tokens
        // stable. Env source:
        std::env::set_var("SYNC_SECRET", "мой-пеппер-42");
        let env = KeyRing::from_env();
        assert_eq!(env.pepper.as_slice(), "мой-пеппер-42".as_bytes());
        // Authority/keyfile source carrying the SAME bytes (base64) → identical:
        let pp_b64 = STANDARD.encode("мой-пеппер-42".as_bytes());
        let wire = KeyRing::from_wire_json(
            &format!(r#"{{"pp":"{pp_b64}","pp_epoch":0}}"#),
            KeySource::Keyfile,
        )
        .unwrap();
        assert_eq!(wire.pepper.as_slice(), env.pepper.as_slice());
    }

    #[test]
    fn keyfile_json_parses_full_shape() {
        let mdk = STANDARD.encode([2u8; 32]);
        let vk = STANDARD.encode([3u8; 32]);
        let prev_vk = STANDARD.encode([4u8; 32]);
        let pp = STANDARD.encode(b"pepper-bytes");
        let text = format!(
            r#"{{"pp":"{pp}","pp_epoch":2,"mdk":"{mdk}","mdk_epoch":5,
                "vk":"{vk}","vk_epoch":7,
                "prev":[{{"vk":"{prev_vk}","vk_epoch":6}}],
                "expires_in_secs":900}}"#
        );
        let kr = KeyRing::from_wire_json(&text, KeySource::Keyfile).unwrap();
        assert_eq!(kr.pepper.as_slice(), b"pepper-bytes");
        assert_eq!(kr.pepper_epoch, 2);
        assert_eq!(kr.mesh_data_key.as_deref(), Some(&[2u8; 32]));
        assert_eq!(kr.mdk_epoch, 5);
        assert_eq!(kr.vault_key.as_deref(), Some(&[3u8; 32]));
        assert_eq!(kr.vault_epoch, 7);
        assert_eq!(kr.prev_vault.len(), 1);
        assert_eq!(kr.prev_vault[0].0, 6);
        assert_eq!(*kr.prev_vault[0].1, [4u8; 32]);
        assert_eq!(kr.ttl, Duration::from_secs(900));
        assert_eq!(kr.source, KeySource::Keyfile);
    }

    #[test]
    fn keyfile_json_parses_with_null_vk() {
        // Cache-role nodes get PP (+ maybe MDK) but a null vk — must parse.
        let pp = STANDARD.encode(b"pp");
        let mdk = STANDARD.encode([9u8; 32]);
        let text = format!(
            r#"{{"pp":"{pp}","pp_epoch":1,"mdk":"{mdk}","mdk_epoch":1,
                "vk":null,"vk_epoch":0,"prev":[],"expires_in_secs":300}}"#
        );
        let kr = KeyRing::from_wire_json(&text, KeySource::Keyfile).unwrap();
        assert_eq!(kr.pepper.as_slice(), b"pp");
        assert_eq!(kr.mesh_data_key.as_deref(), Some(&[9u8; 32]));
        assert!(kr.vault_key.is_none());
        assert!(kr.prev_vault.is_empty());
        assert_eq!(kr.source, KeySource::Keyfile);
    }

    #[test]
    fn missing_optional_fields_default() {
        // Bare env-style managed response: only pp present.
        let pp = STANDARD.encode(b"x");
        let kr = KeyRing::from_wire_json(&format!(r#"{{"pp":"{pp}"}}"#), KeySource::Keyfile)
            .unwrap();
        assert!(kr.mesh_data_key.is_none());
        assert!(kr.vault_key.is_none());
        assert_eq!(kr.pepper_epoch, 0);
        assert_eq!(kr.ttl, Duration::from_secs(300)); // 0 → default
    }

    #[test]
    fn empty_pepper_is_rejected() {
        let text = r#"{"pp":""}"#;
        assert!(KeyRing::from_wire_json(text, KeySource::Keyfile).is_err());
    }

    #[test]
    fn debug_redacts_all_key_material() {
        let mdk = STANDARD.encode([2u8; 32]);
        let pp = STANDARD.encode(b"super-secret-pepper");
        let kr = KeyRing::from_wire_json(
            &format!(r#"{{"pp":"{pp}","mdk":"{mdk}"}}"#),
            KeySource::Keyfile,
        )
        .unwrap();
        let dbg = format!("{kr:?}");
        assert!(dbg.contains("[redacted]"), "no redaction marker: {dbg}");
        assert!(!dbg.contains("super-secret-pepper"), "pepper leaked: {dbg}");
        // The raw base64 must not appear either.
        assert!(!dbg.contains(&pp), "pepper b64 leaked: {dbg}");
        assert!(!dbg.contains(&mdk), "mdk b64 leaked: {dbg}");
    }

    #[test]
    fn ring_from_derived_epoch0_legacy_yields_legacy_pepper() {
        // A safe-sourced ring at epoch 0 must hand back the imported legacy bytes
        // byte-for-byte — this is what keeps PPRL tokens / envelopes stable when a
        // fleet switches onto the vault.
        let payload = crate::vault::VaultPayload {
            v: 1,
            tmk: [0x33; 32],
            legacy_pp: Some(b"the-legacy-pepper".to_vec()),
            legacy_mdk: Some([0x44; 32]),
            pp_epoch: 0,
            mdk_epoch: 0,
            vk_epoch: 0,
            datafs_epoch: 0,
            prev_mdk_epochs: Vec::new(),
            prev_vk_epochs: Vec::new(),
            created_at: 0,
        };
        let d = crate::vault::blob::derive(&payload);
        let kr = KeyRing::from_derived(d, Duration::from_secs(3600), 7);
        assert_eq!(kr.pepper.as_slice(), b"the-legacy-pepper");
        assert_eq!(kr.mesh_data_key.as_deref(), Some(&[0x44u8; 32]));
        assert_eq!(kr.source, KeySource::Safe);
        assert_eq!(kr.version, 7);
        // vault key is always HKDF-derived, even at epoch 0.
        assert!(kr.vault_key.is_some());
        // …and so is the data-at-rest volume key.
        assert!(kr.datafs_key.is_some());
    }

    #[test]
    fn rotated_ring_keeps_the_retired_mdk_for_decrypt_only() {
        // The P0.7 shape: mdk_epoch=1, epoch 0 (the imported MESH_DATA_KEY)
        // retired but still openable.
        let payload = crate::vault::VaultPayload {
            v: 1,
            tmk: [0x33; 32],
            legacy_pp: Some(b"the-legacy-pepper".to_vec()),
            legacy_mdk: Some([0x44; 32]),
            pp_epoch: 0,
            mdk_epoch: 1,
            vk_epoch: 1,
            datafs_epoch: 0,
            prev_mdk_epochs: vec![0],
            prev_vk_epochs: vec![0],
            created_at: 0,
        };
        let kr = KeyRing::from_derived(
            crate::vault::blob::derive(&payload),
            Duration::from_secs(3600),
            2,
        );
        // Current key is the rotated one, NOT the imported legacy bytes.
        assert_ne!(kr.mesh_data_key.as_deref(), Some(&[0x44u8; 32]));
        assert_eq!(kr.mdk_epoch, 1);
        // The retired epoch 0 is present, and IS the imported legacy key.
        assert_eq!(kr.prev_mdk.len(), 1);
        assert_eq!(kr.prev_mdk[0].0, 0);
        assert_eq!(*kr.prev_mdk[0].1, [0x44u8; 32]);
        assert_eq!(kr.prev_vault.len(), 1);
        // The pepper is untouched by an MDK/VK rotation (P1b owns pp).
        assert_eq!(kr.pepper.as_slice(), b"the-legacy-pepper");
        // …and Debug still leaks nothing.
        assert!(!format!("{kr:?}").contains("the-legacy-pepper"));
    }

    #[test]
    fn keyfile_json_parses_prev_mdk_newest_first() {
        let pp = STANDARD.encode(b"pp");
        let mdk = STANDARD.encode([7u8; 32]);
        let old0 = STANDARD.encode([1u8; 32]);
        let old1 = STANDARD.encode([2u8; 32]);
        // Deliberately out of order on the wire.
        let text = format!(
            r#"{{"pp":"{pp}","mdk":"{mdk}","mdk_epoch":2,
                "prev_mdk":[{{"mdk":"{old0}","mdk_epoch":0}},{{"mdk":"{old1}","mdk_epoch":1}}]}}"#
        );
        let kr = KeyRing::from_wire_json(&text, KeySource::Keyfile).unwrap();
        assert_eq!(kr.mdk_epoch, 2);
        assert_eq!(
            kr.prev_mdk.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            vec![1, 0],
            "retired epochs must be newest-first so decrypt tries the likeliest key early"
        );
        assert_eq!(*kr.prev_mdk[0].1, [2u8; 32]);
        // Absent field → empty (every pre-P0.7 keyfile).
        let bare = KeyRing::from_wire_json(&format!(r#"{{"pp":"{pp}"}}"#), KeySource::Keyfile)
            .unwrap();
        assert!(bare.prev_mdk.is_empty());
    }

    #[test]
    fn env_ring_has_no_datafs_key() {
        // The whole point of the encrypted data volume: no key on the same disk.
        std::env::set_var("SYNC_SECRET", "x");
        std::env::set_var(
            "MESH_DATA_KEY",
            "0101010101010101010101010101010101010101010101010101010101010101",
        );
        let kr = KeyRing::from_env();
        assert!(kr.datafs_key.is_none());
    }

    #[test]
    fn keyfile_can_carry_a_datafs_key() {
        let pp = STANDARD.encode(b"pp");
        let dfk = STANDARD.encode([5u8; 32]);
        let kr = KeyRing::from_wire_json(
            &format!(r#"{{"pp":"{pp}","dfk":"{dfk}","dfk_epoch":3}}"#),
            KeySource::Keyfile,
        )
        .unwrap();
        assert_eq!(kr.datafs_key.as_deref(), Some(&[5u8; 32]));
        assert_eq!(kr.dfk_epoch, 3);
        // …and stays absent when the field is not there.
        let bare = KeyRing::from_wire_json(&format!(r#"{{"pp":"{pp}"}}"#), KeySource::Keyfile)
            .unwrap();
        assert!(bare.datafs_key.is_none());
    }
}
