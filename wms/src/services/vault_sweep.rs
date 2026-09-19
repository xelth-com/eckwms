//! Re-envelope sweep — the half of an MDK rotation that actually removes the
//! old key's reach (`.eck/PII_VAULT.md` §2 "Rotation", P0.7).
//!
//! Rotating `MDK` (`POST /admin/vault/rotate {"mdk":true}`) changes which key
//! seals NEW envelopes. It does nothing to the epoch-0 ciphertext already
//! sitting on the cache nodes eck1/2/3 — and epoch 0 is exactly the old
//! `MESH_DATA_KEY` that spent months in a world-readable `.env` on every node
//! and in every backup. Until those stored envelopes are rewritten, the rotation
//! has bought nothing for data at rest on the caches. This sweep rewrites them.
//!
//! ## Mechanism
//! For each cache peer, per entity class, paged by record id:
//!
//! 1. `GET  {peer}/api/mesh/re-envelope/ids` — the cache hands back the opaque
//!    envelopes it holds, with the epoch each one claims.
//! 2. Locally: open each old envelope with the RETIRED key the ring kept
//!    (`keyring::mesh_data_keys_for_decrypt`) and re-seal the identical
//!    plaintext under the current key + epoch.
//! 3. `POST {peer}/api/mesh/re-envelope` — the cache replaces the envelope bytes
//!    of rows it already has. Nothing else about the row moves: no vclock bump,
//!    no business field, no new rows.
//!
//! Rows already at the target epoch are skipped without a round trip, which is
//! what makes the sweep idempotent and cheap to re-run. The per-(peer, class)
//! cursor is persisted in `vault_sweep_progress`, so a restart resumes where it
//! stopped rather than re-walking from the top.
//!
//! ## What it deliberately does NOT do
//! * It never re-reads the home node's own row to build the new envelope — that
//!   would silently ship post-rotation edits under the banner of a key rotation,
//!   and would lose rows the home no longer has. Same ciphertext in, same
//!   plaintext out, new key.
//! * It never pushes a row to a cache that does not already hold it. A blind
//!   cache's holdings cannot grow through this path.
//! * It does not touch this node's own storage: full nodes hold plaintext at
//!   rest (that is P1 / the encrypted data volume, not MDK's job).
//!
//! Rate limiting: one page in flight at a time, `ECK_SWEEP_PAGE_DELAY_MS`
//! (default 250 ms) between pages, page size `ECK_SWEEP_PAGE_SIZE` (default 25).
//! Progress and the last error are exposed through `GET /admin/vault/status`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use eck_core::sync::mesh_client;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::AppState;

/// Entity classes a cache can hold as envelopes — must match
/// `handlers::mesh::RE_ENVELOPE_TYPES` on the receiving side.
const SWEEP_TYPES: &[&str] = &["document", "file_resource", "document_raw"];

fn page_size() -> usize {
    std::env::var("ECK_SWEEP_PAGE_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(25)
        .clamp(1, 200)
}

fn page_delay() -> Duration {
    Duration::from_millis(
        std::env::var("ECK_SWEEP_PAGE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(250)
            .clamp(0, 60_000),
    )
}

// ─── progress (process-global, mirrored into the DB for resume) ──────────────

#[derive(Clone, Default)]
pub struct SweepProgress {
    pub running: bool,
    pub target_epoch: u32,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// Envelopes seen at the target epoch already (nothing to do).
    pub already_current: u64,
    /// Envelopes successfully rewritten on a cache.
    pub rewritten: u64,
    /// Envelopes this node could not open under ANY epoch it still holds —
    /// left alone, and the reason a sweep can legitimately never hit 100 %.
    pub unreadable: u64,
    /// Rows the cache refused (gone, or plaintext it should not hold).
    pub not_applied: u64,
    /// Plaintext rows a cache reported holding (a pre-blind-cache leftover).
    pub plaintext_on_cache: u64,
    pub peers: Vec<String>,
    pub last_error: Option<String>,
    /// True once every (peer, class) pair walked to its end without an error.
    pub complete: bool,
}

static PROGRESS: OnceLock<RwLock<SweepProgress>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

fn progress() -> &'static RwLock<SweepProgress> {
    PROGRESS.get_or_init(|| RwLock::new(SweepProgress::default()))
}

fn with_progress(f: impl FnOnce(&mut SweepProgress)) {
    if let Ok(mut g) = progress().write() {
        f(&mut g);
    }
}

/// Snapshot for `GET /admin/vault/status`.
pub fn status_json() -> Value {
    let Ok(p) = progress().read() else {
        return json!({ "state": "poisoned" });
    };
    let state = if p.running {
        "running"
    } else if p.complete {
        "complete"
    } else if p.started_at.is_some() {
        "stopped"
    } else {
        "idle"
    };
    json!({
        "state": state,
        "target_epoch": p.target_epoch,
        "started_at": p.started_at,
        "finished_at": p.finished_at,
        "already_current": p.already_current,
        "rewritten": p.rewritten,
        "unreadable": p.unreadable,
        "not_applied": p.not_applied,
        "plaintext_on_cache": p.plaintext_on_cache,
        "cache_peers": p.peers,
        "complete": p.complete,
        "last_error": p.last_error,
    })
}

/// True when the last sweep walked every cache peer to the end with no error
/// and left nothing at an old epoch. `{"finalize":true}` requires this.
pub fn is_complete() -> bool {
    progress().read().map(|p| p.complete).unwrap_or(false)
}

/// The epoch the completed sweep targeted (0 if none ever ran).
pub fn completed_epoch() -> u32 {
    progress().read().map(|p| p.target_epoch).unwrap_or(0)
}

/// Ask a running sweep to stop after the current page.
pub fn cancel() {
    CANCEL.store(true, Ordering::Release);
}

/// Forget the progress record so the next `start` walks from the top again.
pub fn reset() -> Result<(), String> {
    if RUNNING.load(Ordering::Acquire) {
        return Err("sweep is running; stop it first".into());
    }
    with_progress(|p| *p = SweepProgress::default());
    Ok(())
}

// ─── the sweep ───────────────────────────────────────────────────────────────

/// Spawn the sweep. `Err` if one is already running or the node has no mesh
/// data key to seal with.
pub fn start(state: Arc<AppState>) -> Result<u32, String> {
    let Some(key) = eck_core::utils::crypto::data_key() else {
        return Err("this node holds no mesh data key — nothing to re-envelope with".into());
    };
    let target = eck_core::utils::crypto::data_key_epoch();
    if RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("a re-envelope sweep is already running".into());
    }
    CANCEL.store(false, Ordering::Release);
    with_progress(|p| {
        *p = SweepProgress {
            running: true,
            target_epoch: target,
            started_at: Some(chrono::Utc::now().timestamp()),
            ..Default::default()
        };
    });

    tokio::spawn(async move {
        let outcome = run(state, key, target).await;
        let (complete, err) = match outcome {
            Ok(complete) => (complete, None),
            Err(e) => {
                warn!("[vault_sweep] aborted: {e}");
                (false, Some(e))
            }
        };
        with_progress(|p| {
            p.running = false;
            p.complete = complete;
            p.finished_at = Some(chrono::Utc::now().timestamp());
            if err.is_some() {
                p.last_error = err;
            }
        });
        RUNNING.store(false, Ordering::Release);
        info!("[vault_sweep] finished: {}", status_json());
    });
    Ok(target)
}

/// Walk every cache peer × class. Returns `true` when all of them reached the
/// end of their pages with nothing left at an old epoch.
async fn run(state: Arc<AppState>, key: [u8; 32], target: u32) -> Result<bool, String> {
    let peers = mesh_client::discover_peers(
        state.sync_engine.relay(),
        &state.instance_id,
        state.sync_secret.as_deref(),
    )
    .await;
    let caches: Vec<_> = peers.into_iter().filter(|p| p.is_cache()).collect();
    if caches.is_empty() {
        return Err("no cache peers discovered — nothing to sweep (is the relay reachable?)".into());
    }
    with_progress(|p| p.peers = caches.iter().map(|c| c.peer_url().to_string()).collect());
    info!(
        "[vault_sweep] starting: {} cache peer(s), target mdk_epoch={}",
        caches.len(),
        target
    );

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut all_clean = true;
    for peer in &caches {
        for entity_type in SWEEP_TYPES {
            let clean = sweep_one(&state, &http, peer.peer_url(), entity_type, &key, target).await?;
            all_clean &= clean;
            if CANCEL.load(Ordering::Acquire) {
                return Ok(false);
            }
        }
    }
    Ok(all_clean)
}

/// One (peer, class) pair, paged from the persisted cursor to the end.
async fn sweep_one(
    state: &Arc<AppState>,
    http: &reqwest::Client,
    peer_url: &str,
    entity_type: &str,
    key: &[u8; 32],
    target: u32,
) -> Result<bool, String> {
    let auth = state.sync_secret.clone();
    let mut cursor = load_cursor(state, peer_url, entity_type).await;
    let limit = page_size();
    let mut clean = true;

    loop {
        if CANCEL.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut req = http
            .get(format!("{peer_url}/api/mesh/re-envelope/ids"))
            .query(&[("entity_type", entity_type), ("limit", &limit.to_string())]);
        if let Some(c) = cursor.as_deref() {
            req = req.query(&[("after", c)]);
        }
        if let Some(s) = auth.as_deref() {
            req = req.header("authorization", format!("Bearer {s}"));
        }
        let res = req
            .send()
            .await
            .map_err(|e| format!("{peer_url} {entity_type}: enumerate: {e}"))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            // Peer is on a pre-P0.7 binary: it cannot be swept until it is
            // redeployed. Loud, and NOT counted as clean.
            warn!(
                "[vault_sweep] {peer_url} has no /api/mesh/re-envelope endpoint (pre-P0.7 binary) \
                 — deploy the new wms there before finalizing"
            );
            with_progress(|p| {
                p.last_error = Some(format!("{peer_url}: endpoint missing (old binary)"));
            });
            return Ok(false);
        }
        if !res.status().is_success() {
            return Err(format!(
                "{peer_url} {entity_type}: enumerate returned {}",
                res.status()
            ));
        }
        let body: Value = res
            .json()
            .await
            .map_err(|e| format!("{peer_url} {entity_type}: enumerate decode: {e}"))?;

        let plaintext = body.get("plaintext_rows").and_then(|v| v.as_u64()).unwrap_or(0);
        if plaintext > 0 {
            with_progress(|p| p.plaintext_on_cache += plaintext);
        }
        let rows = body
            .get("rows")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut batch = Vec::new();
        for row in &rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let epoch = row.get("epoch").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if epoch == target {
                with_progress(|p| p.already_current += 1);
                continue;
            }
            let Some(enc) = row.get("enc") else { continue };
            // Open with whichever retired epoch the ring still holds, re-seal
            // under the current one. Identical plaintext, new key.
            let plain = match eck_core::utils::crypto::decrypt_json_any(enc) {
                Ok(v) => v,
                Err(e) => {
                    warn!("[vault_sweep] {entity_type}:{id} unreadable, left as-is: {e}");
                    with_progress(|p| p.unreadable += 1);
                    clean = false;
                    continue;
                }
            };
            match eck_core::utils::crypto::encrypt_json_epoch(key, target, &plain) {
                Ok(fresh) => batch.push(json!({ "id": id, "enc": fresh })),
                Err(e) => {
                    warn!("[vault_sweep] {entity_type}:{id} re-seal failed: {e}");
                    clean = false;
                }
            }
        }

        if !batch.is_empty() {
            let n = batch.len();
            let mut post = http
                .post(format!("{peer_url}/api/mesh/re-envelope"))
                .json(&json!({
                    "entity_type": entity_type,
                    "mdk_epoch": target,
                    "rows": batch,
                    "source_instance": state.instance_id,
                }));
            if let Some(s) = auth.as_deref() {
                post = post.header("authorization", format!("Bearer {s}"));
            }
            let res = post
                .send()
                .await
                .map_err(|e| format!("{peer_url} {entity_type}: apply: {e}"))?;
            if !res.status().is_success() {
                return Err(format!(
                    "{peer_url} {entity_type}: apply returned {}",
                    res.status()
                ));
            }
            let ack: Value = res.json().await.unwrap_or(Value::Null);
            let applied = ack.get("applied").and_then(|v| v.as_u64()).unwrap_or(0);
            let refused = n as u64 - applied;
            with_progress(|p| {
                p.rewritten += applied;
                p.not_applied += refused;
            });
            if refused > 0 {
                clean = false;
            }
        }

        let more = body.get("more").and_then(|v| v.as_bool()).unwrap_or(false);
        cursor = body
            .get("next_cursor")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or(cursor);
        save_cursor(state, peer_url, entity_type, cursor.as_deref(), more).await;
        if !more {
            return Ok(clean);
        }
        tokio::time::sleep(page_delay()).await;
    }
}

// ─── cursor persistence (resume across restarts) ─────────────────────────────

fn cursor_key(peer_url: &str, entity_type: &str) -> String {
    // Record ids may not contain '/' or ':' — hash the peer url into a stable
    // opaque key instead of trying to escape it.
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(peer_url.as_bytes());
    h.update(b"|");
    h.update(entity_type.as_bytes());
    hex::encode(&h.finalize()[..16])
}

async fn load_cursor(state: &Arc<AppState>, peer_url: &str, entity_type: &str) -> Option<String> {
    let rows: Vec<Value> = state
        .db
        .query("SELECT * FROM vault_sweep_progress WHERE key = $k AND done = false LIMIT 1")
        .bind(("k", cursor_key(peer_url, entity_type)))
        .await
        .ok()?
        .take(0)
        .ok()?;
    rows.into_iter()
        .next()
        .and_then(|r| r.get("cursor").and_then(|v| v.as_str()).map(String::from))
        .filter(|s| !s.is_empty())
}

async fn save_cursor(
    state: &Arc<AppState>,
    peer_url: &str,
    entity_type: &str,
    cursor: Option<&str>,
    more: bool,
) {
    let _ = state
        .db
        .query(
            "UPSERT vault_sweep_progress SET key = $k, peer = $p, entity_type = $et, \
             cursor = $c, done = $done, updated_at = time::now() WHERE key = $k",
        )
        .bind(("k", cursor_key(peer_url, entity_type)))
        .bind(("p", peer_url.to_string()))
        .bind(("et", entity_type.to_string()))
        .bind(("c", cursor.unwrap_or_default().to_string()))
        .bind(("done", !more))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_key_is_stable_and_pair_specific() {
        let a = cursor_key("https://eck1.9eck.com:3201", "document");
        assert_eq!(a, cursor_key("https://eck1.9eck.com:3201", "document"));
        assert_ne!(a, cursor_key("https://eck1.9eck.com:3201", "file_resource"));
        assert_ne!(a, cursor_key("https://eck2.9eck.com:3201", "document"));
        // Usable as a SurrealDB field value / record fragment: hex only.
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn page_knobs_are_clamped() {
        std::env::set_var("ECK_SWEEP_PAGE_SIZE", "100000");
        assert_eq!(page_size(), 200);
        std::env::set_var("ECK_SWEEP_PAGE_SIZE", "0");
        assert_eq!(page_size(), 1);
        std::env::remove_var("ECK_SWEEP_PAGE_SIZE");
        assert_eq!(page_size(), 25);
    }

    #[test]
    fn idle_status_is_reported_as_idle() {
        let s = status_json();
        assert_eq!(s["state"], "idle");
        assert_eq!(s["rewritten"], 0);
        assert_eq!(s["complete"], false);
    }
}
