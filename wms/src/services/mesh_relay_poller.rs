//! Cross-NAT mesh task receiver.
//!
//! Polls the relay's `/E/m/poll/<self_uuid>` for tasks addressed to this WMS,
//! interprets the envelope, applies the local effect, and acks with a result
//! body the dispatcher can read. Complementary to the direct-HTTP P2P path:
//! when two peers can't dial each other (different NATs), the sender writes
//! a task to the relay queue and this poller picks it up here.
//!
//! Envelope shape (constructed by `RelayClient::mesh_dispatch`):
//! ```json
//! { "envelope": {
//!     "target_uuid": "<my_uuid>",
//!     "sender_uuid": "<peer_uuid>",
//!     "kind": "pull_request" | "push",
//!     "payload": { ...kind-specific... }
//!   }
//! }
//! ```
//!
//! Currently handled kinds:
//! - `pull_request`    — `{entity_type, ids: [String]}` → ack `{entities: [...]}`
//! - `file_fetch`      — `{hash}` → ack `{found, size, enc}` (a CAS blob served
//!                       cross-NAT as an encrypted envelope; blind conduit)
//! - `push`            — `{entity_type, entities: [...], source_instance}`
//!                       → ack `{applied: N}`
//! - `device_register` — `{deviceId, devicePublicKey, signature, inviteToken?}`
//!                       → ack `{status, token}` (phone pairs to a NAT'd master
//!                       through a blind relay; eckN stay pure relays)

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::AppState;

/// Polling cadence. Adaptive: tightens when there's work, relaxes when idle.
/// Driven by `next_poll_in_seconds` hint from the relay (same pattern as the
/// xelixir poller).
const POLL_INTERVAL_BUSY_SECS: u64 = 3;
const POLL_INTERVAL_IDLE_SECS: u64 = 15;

pub async fn start_poller(state: Arc<AppState>) {
    info!(
        "[mesh_relay_poller] starting for instance {}",
        state.instance_id
    );

    let mut interval_secs = POLL_INTERVAL_IDLE_SECS;
    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        let tasks = match state.sync_engine.relay().mesh_poll().await {
            Ok(t) => t,
            Err(e) => {
                debug!("[mesh_relay_poller] poll failed (likely relay transient): {}", e);
                interval_secs = POLL_INTERVAL_IDLE_SECS;
                continue;
            }
        };

        if tasks.is_empty() {
            interval_secs = POLL_INTERVAL_IDLE_SECS;
            continue;
        }

        interval_secs = POLL_INTERVAL_BUSY_SECS;
        for task in tasks {
            handle_task(&state, task).await;
        }
    }
}

async fn handle_task(state: &Arc<AppState>, task: Value) {
    let task_id = task
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if task_id.is_empty() {
        warn!("[mesh_relay_poller] task without id, dropping");
        return;
    }

    // The envelope was wrapped by the dispatcher; unpack.
    let envelope = task.get("payload").and_then(|p| p.get("envelope"));
    let kind = envelope
        .and_then(|e| e.get("kind"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender = envelope
        .and_then(|e| e.get("sender_uuid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payload = envelope
        .and_then(|e| e.get("payload"))
        .cloned()
        .unwrap_or(Value::Null);

    let ack_body = match kind.as_str() {
        "pull_request" => handle_pull_request(state, &payload).await,
        "file_fetch" => handle_file_fetch(state, &payload).await,
        "push" => handle_push(state, &payload, &sender).await,
        "device_challenge" => handle_device_challenge(state).await,
        "device_register" => handle_device_register(state, &payload).await,
        "trip_upload" => handle_trip_upload(state, &payload).await,
        "image_upload" => handle_image_upload(state, &payload).await,
        "users_active" => handle_users_active(state, &payload).await,
        "users_verify_pin" => handle_users_verify_pin(state, &payload).await,
        "merkle_state" => handle_merkle_state(state, &payload).await,
        other => {
            warn!(
                "[mesh_relay_poller] task={} unknown kind '{}', acking with error",
                task_id, other
            );
            json!({"ok": false, "error": format!("unknown kind: {other}")})
        }
    };

    if let Err(e) = state.sync_engine.relay().mesh_ack(&task_id, ack_body).await {
        warn!("[mesh_relay_poller] ack task={} failed: {}", task_id, e);
    }
}

/// Relay-forwarded device challenge (phase 1 of NAT'd pairing). The phone can't
/// GET /api/auth/device-challenge from a NAT'd master, so it asks for the nonce
/// over the same relay mesh-queue. The master mints+stores it (single-use, same
/// table as the direct path) and acks `{ok, nonce}`; the phone then signs
/// `{deviceId,devicePublicKey,nonce}` and dispatches `device_register`.
async fn handle_device_challenge(state: &Arc<AppState>) -> Value {
    match crate::handlers::device::issue_device_challenge(&state.db).await {
        Ok(nonce) => json!({ "ok": true, "nonce": nonce }),
        Err(e) => {
            warn!("[mesh_relay_poller] device_challenge failed: {}", e);
            json!({ "ok": false, "error": e })
        }
    }
}

/// Relay-forwarded trip checkpoint / upload. A phone driving on mobile data
/// can't reach the (LAN-only) master over HTTP, so it dispatches its trip data
/// — periodic open checkpoints and the final ended trip — as a `trip_upload`
/// mesh-task. Same logic as `POST /api/trips` (`upload_trip_core`), which also
/// broadcasts a `TRIP_LIVE` marker for open checkpoints. Device auth parity: the
/// phone includes its JWT in `token` (the HTTP path is JWT-gated), validated here
/// before ingest; `upload_trip_core` additionally requires an active device.
/// Relay-forwarded merkle state: return this node's merkle node for
/// {entity_type, level, bucket} so a peer that can't reach us directly (different
/// LAN / NAT) can still diff + converge via the relay. Read-only; the peer then
/// pulls the differing ids over the `pull_request` mesh-task. entity_type is
/// whitelisted (it hits the merkle service by table).
async fn handle_merkle_state(state: &Arc<AppState>, payload: &Value) -> Value {
    let et = payload.get("entity_type").and_then(|v| v.as_str()).unwrap_or("");
    if !eck_core::sync::engine::is_mesh_entity_type(et) {
        return json!({ "ok": false, "error": format!("unknown entity_type '{et}'") });
    }
    let level = payload.get("level").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
    let bucket = payload.get("bucket").and_then(|v| v.as_str()).map(|s| s.to_string());
    // Cache nodes advertise only their authoritative subset (mirrors the HTTP
    // /api/mesh/merkle/state handler).
    let svc = if state.node_role == "cache" {
        eck_core::sync::merkle::MerkleService::new_cache_filtered(state.db.clone(), state.instance_id.clone())
    } else {
        eck_core::sync::merkle::MerkleService::new(state.db.clone(), state.instance_id.clone())
    };
    match svc
        .get_state(&eck_core::sync::merkle::MerkleRequest {
            entity_type: et.to_string(),
            level,
            bucket,
        })
        .await
    {
        Ok(node) => json!({ "ok": true, "node": node }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/// Relay-forwarded roster fetch — the phone is usually OFF the master's LAN
/// (different subnet / LTE, no global URL), so `GET /api/users/active` never
/// reaches it directly. Same mesh-queue pattern as trip_upload: device-JWT
/// gated, acks the exact `/api/users/active` response as `{ok, users}`.
async fn handle_users_active(state: &Arc<AppState>, payload: &Value) -> Value {
    let token = payload.get("token").and_then(|v| v.as_str()).unwrap_or("");
    if eck_core::auth::validate_token(token, &state.jwt_secret).is_err() {
        warn!("[mesh_relay_poller] users_active rejected: invalid/missing device token");
        return json!({"ok": false, "error": "invalid or missing device token"});
    }
    match crate::handlers::pda::active_users(axum::extract::State(state.clone())).await {
        Ok(axum::Json(users)) => {
            info!("[mesh_relay_poller] users_active ok ({} users)", users.len());
            json!({"ok": true, "users": users})
        }
        Err((code, msg)) => {
            warn!("[mesh_relay_poller] users_active failed ({}): {}", code, msg);
            json!({"ok": false, "error": msg, "code": code.as_u16()})
        }
    }
}

/// Relay-forwarded PIN login (`POST /api/users/verify-pin` twin) — without it a
/// synced roster is view-only off-LAN: switching who you LOOK at works, but
/// logging in (long-press + PIN) would still need the LAN. Device-JWT gated;
/// the bcrypt check itself runs on the master exactly like the HTTP path.
async fn handle_users_verify_pin(state: &Arc<AppState>, payload: &Value) -> Value {
    let token = payload.get("token").and_then(|v| v.as_str()).unwrap_or("");
    if eck_core::auth::validate_token(token, &state.jwt_secret).is_err() {
        warn!("[mesh_relay_poller] users_verify_pin rejected: invalid/missing device token");
        return json!({"ok": false, "error": "invalid or missing device token"});
    }
    let req: crate::handlers::pda::VerifyPinRequest = match serde_json::from_value(payload.clone()) {
        Ok(r) => r,
        Err(e) => return json!({"ok": false, "error": format!("bad users_verify_pin payload: {e}")}),
    };
    match crate::handlers::pda::verify_pin(axum::extract::State(state.clone()), axum::Json(req)).await {
        Ok(axum::Json(v)) => {
            let mut out = json!({"ok": true});
            if let (Some(o), Some(vo)) = (out.as_object_mut(), v.as_object()) {
                for (k, val) in vo {
                    o.insert(k.clone(), val.clone());
                }
            }
            out
        }
        // Wrong PIN comes back as 401 — keep ok:false but mark it a definitive
        // auth answer so the phone doesn't fall through to "master unreachable".
        Err((code, msg)) => json!({"ok": false, "error": msg, "code": code.as_u16(), "answered": true}),
    }
}

async fn handle_trip_upload(state: &Arc<AppState>, payload: &Value) -> Value {
    let token = payload.get("token").and_then(|v| v.as_str()).unwrap_or("");
    if eck_core::auth::validate_token(token, &state.jwt_secret).is_err() {
        warn!("[mesh_relay_poller] trip_upload rejected: invalid/missing device token");
        return json!({"ok": false, "error": "invalid or missing device token"});
    }
    let body: crate::handlers::trips::TripUpload = match serde_json::from_value(payload.clone()) {
        Ok(b) => b,
        Err(e) => return json!({"ok": false, "error": format!("bad trip_upload payload: {e}")}),
    };
    let trip_uuid = body.trip_uuid.clone();
    match crate::handlers::trips::upload_trip_core(state, body).await {
        Ok(v) => {
            info!("[mesh_relay_poller] trip_upload {} ok", trip_uuid);
            json!({"ok": true, "trip": v})
        }
        Err((code, msg)) => {
            warn!("[mesh_relay_poller] trip_upload {} failed ({}): {}", trip_uuid, code, msg);
            json!({"ok": false, "error": msg, "code": code.as_u16()})
        }
    }
}

/// Relay-forwarded device pairing. A phone on mobile data sees only blind
/// relays (no directly-reachable full WMS), so it dispatches its registration
/// as a `device_register` mesh-task targeting this (NAT'd) master's UUID. We
/// run the exact same logic as `POST /api/internal/register-device` and ack
/// `{status, token}`. This is what lets the eckN service nodes stay pure
/// relays — the master pairs the device through the reverse-fetch queue.
/// Relay-forwarded image/receipt upload. A phone on mobile data can't reach the
/// LAN master's `POST /api/upload/image`, so it base64-encodes the JPEG into an
/// `image_upload` mesh-task targeting the master's UUID. We decode it and run the
/// same CAS-save core as the HTTP path (device-JWT gated, mirroring trip_upload),
/// so OCR/receipt evidence lands in CAS off-LAN instead of being lost.
async fn handle_image_upload(state: &Arc<AppState>, payload: &Value) -> Value {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    let token = payload.get("token").and_then(|v| v.as_str()).unwrap_or("");
    if eck_core::auth::validate_token(token, &state.jwt_secret).is_err() {
        warn!("[mesh_relay_poller] image_upload rejected: invalid/missing device token");
        return json!({"ok": false, "error": "invalid or missing device token"});
    }
    let b64 = payload.get("image_b64").and_then(|v| v.as_str()).unwrap_or("");
    let content = match B64.decode(b64) {
        Ok(b) if !b.is_empty() => b,
        _ => return json!({"ok": false, "error": "missing/invalid image_b64"}),
    };
    let avatar_data = payload
        .get("avatar_b64")
        .and_then(|v| v.as_str())
        .and_then(|s| B64.decode(s).ok())
        .filter(|b| !b.is_empty());
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let image_id = s("image_id");
    let file_name = { let f = s("file_name"); if f.is_empty() { "upload".into() } else { f } };
    let mime_type = { let m = s("mime_type"); if m.is_empty() { "application/octet-stream".into() } else { m } };
    let params = crate::handlers::files::ImageUploadParams {
        content,
        file_name,
        mime_type,
        avatar_data,
        device_id: s("device_id"),
        context: s("context"),
        claimed_id: if image_id.is_empty() { None } else { Some(image_id.clone()) },
        entity_type: None,
        entity_id: None,
        scan_mode: s("scan_mode"),
        barcode_data: s("barcode_data"),
        order_id: s("order_id"),
        user_id: s("user_id"),
    };
    match crate::handlers::files::upload_image_core(state, params).await {
        Ok(v) => {
            info!("[mesh_relay_poller] image_upload {} ok", image_id);
            json!({"ok": true, "image_id": image_id, "file": v})
        }
        Err((code, msg)) => {
            warn!("[mesh_relay_poller] image_upload failed ({}): {}", code, msg);
            json!({"ok": false, "error": msg, "code": code.as_u16()})
        }
    }
}

/// Phase 2: the payload now carries the `nonce` from `device_challenge`.
///
/// Payload mirrors `DeviceRegisterRequest`:
/// `{deviceId, deviceName?, devicePublicKey, signature, inviteToken?}`.
async fn handle_device_register(state: &Arc<AppState>, payload: &Value) -> Value {
    let req: crate::handlers::device::DeviceRegisterRequest =
        match serde_json::from_value(payload.clone()) {
            Ok(r) => r,
            Err(e) => {
                return json!({"ok": false, "error": format!("bad device_register payload: {e}")})
            }
        };
    let device_id = req.device_id.clone();
    match crate::handlers::device::register_device_core(state, req).await {
        Ok(resp) => {
            info!(
                "[mesh_relay_poller] device_register {} -> status={}",
                device_id, resp.status
            );
            json!({
                "ok": true,
                "success": resp.success,
                "status": resp.status,
                "token": resp.token,
                "enc_key": resp.enc_key,
                "message": resp.message,
            })
        }
        Err((code, msg)) => {
            warn!(
                "[mesh_relay_poller] device_register {} failed ({}): {}",
                device_id, code, msg
            );
            json!({"ok": false, "error": msg, "code": code.as_u16()})
        }
    }
}

async fn handle_pull_request(state: &Arc<AppState>, payload: &Value) -> Value {
    let entity_type = payload
        .get("entity_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ids: Vec<String> = payload
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if entity_type.is_empty() || ids.is_empty() {
        return json!({"ok": false, "error": "missing entity_type or ids", "entities": []});
    }

    let query = format!(
        "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) IN $ids",
        entity_type
    );
    let entities: Vec<Value> = match state.db.query(&query).bind(("ids", ids.clone())).await {
        Ok(mut r) => r.take(0).unwrap_or_default(),
        Err(e) => {
            warn!(
                "[mesh_relay_poller] pull_request {} ids={:?} query failed: {}",
                entity_type,
                ids.len(),
                e
            );
            return json!({"ok": false, "error": e.to_string(), "entities": []});
        }
    };

    // Blind-cache invariant (same shared logic as handlers::mesh::sync_pull):
    // an owner (holds MESH_DATA_KEY) encrypts every entity before it leaves over
    // the relay, so the relay and any cache node only ever see ciphertext; the
    // receiver decrypts on arrival if it has the key. A keyless CACHE that
    // shouldn't be fulfilling here must NOT leak plaintext — withhold any row
    // that isn't already a ciphertext envelope.
    let n = entities.len();
    let has_key = eck_core::utils::crypto::data_key();
    let is_cache = state.node_role == "cache";
    let entities = eck_core::utils::crypto::prepare_outbound(entities, has_key, is_cache);
    let withheld = n - entities.len();
    if withheld > 0 {
        warn!(
            "[mesh_relay_poller] blind cache WITHHELD {}/{} {} plaintext rows",
            withheld, n, entity_type
        );
    }

    // Size the ack to what the relay will actually accept. The ack is ONE
    // POST; a relay rejects an oversized body by cutting the connection, the
    // ack never lands, and the task is redelivered forever (poison task —
    // bit us 2026-07-09..11 with a 100-document pull_request). Ids that don't
    // fit are simply not answered: the requester adopts what arrives and its
    // next merkle diff re-requests the rest, so big backlogs converge over a
    // few cycles instead of never.
    let kept = entities.len();
    let (entities, deferred, oversized) = fit_ack_budget(entities, relay_ack_byte_budget());
    if oversized > 0 {
        warn!(
            "[mesh_relay_poller] pull_request {}: WITHHELD {} single entities larger than the {} byte ack budget (they can only converge via the direct path, or raise RELAY_ACK_MAX_BYTES once the relay fleet runs the 32 MiB limit)",
            entity_type, oversized, relay_ack_byte_budget()
        );
    }
    if deferred > 0 {
        info!(
            "[mesh_relay_poller] pull_request {}: answering {}/{} entities (ack byte budget), remainder re-requested by the peer's next merkle cycle",
            entity_type,
            entities.len(),
            kept
        );
    }

    debug!(
        "[mesh_relay_poller] pull_request served {}/{} {} entities (encrypted={}, withheld={})",
        entities.len(),
        n,
        entity_type,
        has_key.is_some(),
        withheld
    );
    json!({
        "ok": true,
        "entity_type": entity_type,
        "entities": entities,
    })
}

/// Hard cap for a single relay-served CAS blob. Chunking is an explicit v1
/// non-goal — a blob over this size is refused (the requester falls back to the
/// direct path, or the file simply doesn't converge cross-NAT until v2 adds
/// chunking). Kept well under the relay's 32 MiB body limit with headroom for
/// the base64 + ciphertext-tag overhead the envelope adds.
const FILE_FETCH_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Relay-mediated CAS blob fetch — the cross-NAT blind-conduit companion to the
/// direct `serve_mesh_file` HTTP path. A requester that can't dial us directly
/// asks for a blob by sha256 over the relay queue; we look it up in our
/// filestore and ack the bytes encrypted under `MESH_DATA_KEY`, so the relay
/// only ever shuttles ciphertext (same zero-knowledge policy as `prepare_
/// outbound` on the entity path). The blob's file path is NOT run through
/// `fit_ack_budget` — that trims entity LISTS; a single blob is either under the
/// `FILE_FETCH_MAX_BYTES` cap and sent whole, or refused.
async fn handle_file_fetch(state: &Arc<AppState>, payload: &Value) -> Value {
    let hash = payload.get("hash").and_then(|v| v.as_str()).unwrap_or("");
    if hash.is_empty() {
        return json!({"found": false, "error": "missing hash"});
    }

    // Blind-cache invariant (companion to serve_mesh_file): a keyless cache is
    // not a file-content authority — it holds no canonical blobs and serving raw
    // bytes would leak plaintext. Skip the lookup entirely and refuse.
    let bytes: Option<Vec<u8>> = if state.node_role == "cache" {
        None
    } else {
        // Locate the blob by sha256 — same lookup as serve_mesh_file.
        let rows: Vec<Value> = match state
            .db
            .query("SELECT storage_path FROM file_resource WHERE hash = $hash LIMIT 1")
            .bind(("hash", hash.to_string()))
            .await
        {
            Ok(mut r) => r.take(0).unwrap_or_default(),
            Err(e) => {
                warn!("[mesh_relay_poller] file_fetch {} query failed: {}", hash, e);
                return json!({"found": false, "error": e.to_string()});
            }
        };
        let storage_path = rows.into_iter().next().and_then(|r| {
            r.get("storage_path")
                .and_then(|v| v.as_str())
                .map(String::from)
        });
        match storage_path {
            None => None,
            Some(path) => {
                let store = eck_core::utils::filestore::FileStore::new(".");
                match store.read(&path).await {
                    Ok(b) => Some(b),
                    Err(e) => {
                        // Metadata row exists but the blob isn't on our disk (we
                        // may only hold the synced thumbnail). Treat as a miss.
                        debug!("[mesh_relay_poller] file_fetch {} read miss: {}", hash, e);
                        None
                    }
                }
            }
        }
    };

    build_file_fetch_ack(
        &state.node_role,
        hash,
        bytes,
        eck_core::utils::crypto::data_key(),
    )
}

/// Pure decision + encoding for a `file_fetch` ack, factored out of the
/// AppState/DB-bound handler so the blind-cache refusal, the size cap, and the
/// encrypt/plaintext split are unit-testable without a live node.
///
/// - `node_role == "cache"` → `{found:false, reason:"cache"}` (blind-cache gate).
/// - `bytes = None`          → `{found:false}` (not held here / not on disk).
/// - `size > FILE_FETCH_MAX_BYTES` → `{found:false, reason:"too_large", size}`.
/// - key present → `{found:true, size, enc:<envelope>}` (encrypted).
/// - key absent  → `{found:true, size, bytes:<base64>}` (plaintext; WARNs — a
///   full node is expected to hold `MESH_DATA_KEY`).
fn build_file_fetch_ack(
    node_role: &str,
    hash: &str,
    bytes: Option<Vec<u8>>,
    key: Option<[u8; 32]>,
) -> Value {
    if node_role == "cache" {
        return json!({"found": false, "reason": "cache"});
    }
    let Some(bytes) = bytes else {
        return json!({"found": false});
    };
    let size = bytes.len();
    if size > FILE_FETCH_MAX_BYTES {
        warn!(
            "[mesh_relay_poller] file_fetch {} REFUSED: {} bytes over the {} byte cap (chunking is a v1 non-goal)",
            hash, size, FILE_FETCH_MAX_BYTES
        );
        return json!({"found": false, "reason": "too_large", "size": size});
    }
    match key {
        Some(k) => match eck_core::utils::crypto::encrypt_bytes(&k, &bytes) {
            Ok(enc) => {
                debug!(
                    "[mesh_relay_poller] file_fetch served {} ({} bytes, encrypted)",
                    hash, size
                );
                json!({"found": true, "size": size, "enc": enc})
            }
            Err(e) => {
                warn!("[mesh_relay_poller] file_fetch {} encrypt failed: {}", hash, e);
                json!({"found": false, "error": e})
            }
        },
        None => {
            warn!(
                "[mesh_relay_poller] file_fetch {} served as PLAINTEXT base64 — this full node holds no MESH_DATA_KEY",
                hash
            );
            json!({"found": true, "size": size, "bytes": STANDARD.encode(&bytes)})
        }
    }
}

/// Serialized-bytes budget for one `pull_request` ack. Defaults to 1.5 MB —
/// safely under axum's 2 MB DEFAULT body limit, so acks pass even through
/// relays that haven't been redeployed with the raised 32 MiB limit yet.
/// Overridable via `RELAY_ACK_MAX_BYTES` (raise once the relay fleet is
/// upgraded; keep comfortably under the relay's limit).
fn relay_ack_byte_budget() -> usize {
    std::env::var("RELAY_ACK_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_500_000)
}

/// Trim `entities` so their combined serialized size stays within `budget`
/// bytes. Order is preserved; scanning stops at the first entity that no
/// longer fits (the rest are re-requested by the peer's next merkle diff).
/// A single entity larger than the whole budget is skipped instead of sent:
/// an unackable ack poisons the relay queue forever, a withheld row merely
/// stays pending. Returns `(kept, deferred_for_budget, withheld_oversized)`.
fn fit_ack_budget(entities: Vec<Value>, budget: usize) -> (Vec<Value>, usize, usize) {
    let mut kept: Vec<Value> = Vec::with_capacity(entities.len());
    let mut used = 0usize;
    let mut deferred = 0usize;
    let mut oversized = 0usize;
    for e in entities {
        let size = serde_json::to_string(&e).map(|s| s.len()).unwrap_or(usize::MAX);
        if size > budget {
            oversized += 1;
            continue;
        }
        if used + size > budget {
            deferred += 1;
            continue;
        }
        used += size;
        kept.push(e);
    }
    (kept, deferred, oversized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity_of_len(len: usize) -> Value {
        // {"v":"aaaa…"} serializes to len bytes: 8 bytes of scaffolding + payload.
        json!({ "v": "a".repeat(len.saturating_sub(10)) })
    }

    #[test]
    fn fit_ack_budget_keeps_everything_under_budget() {
        let ents = vec![entity_of_len(100), entity_of_len(100)];
        let (kept, deferred, oversized) = fit_ack_budget(ents, 1000);
        assert_eq!((kept.len(), deferred, oversized), (2, 0, 0));
    }

    #[test]
    fn fit_ack_budget_defers_what_no_longer_fits() {
        let ents = vec![entity_of_len(400), entity_of_len(400), entity_of_len(400)];
        let (kept, deferred, oversized) = fit_ack_budget(ents, 1000);
        assert_eq!((kept.len(), deferred, oversized), (2, 1, 0));
    }

    #[test]
    fn fit_ack_budget_withholds_single_oversized_but_keeps_scanning() {
        // First entity alone exceeds the budget — it must be withheld, NOT
        // sent (an unackable ack is a poison task), and the small ones after
        // it must still go out.
        let ents = vec![entity_of_len(5000), entity_of_len(100), entity_of_len(100)];
        let (kept, deferred, oversized) = fit_ack_budget(ents, 1000);
        assert_eq!((kept.len(), deferred, oversized), (2, 0, 1));
    }

    #[test]
    fn fit_ack_budget_zero_budget_withholds_everything() {
        let ents = vec![entity_of_len(100)];
        let (kept, deferred, oversized) = fit_ack_budget(ents, 0);
        assert_eq!((kept.len(), deferred, oversized), (0, 0, 1));
    }

    #[test]
    fn file_fetch_cache_role_refuses() {
        // A blind cache must never serve blob content, even if it somehow holds
        // the bytes — refuse with reason:"cache" (mirrors serve_mesh_file).
        let ack = build_file_fetch_ack("cache", "deadbeef", Some(vec![1, 2, 3]), Some([7u8; 32]));
        assert_eq!(ack.get("found").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(ack.get("reason").and_then(|v| v.as_str()), Some("cache"));
    }

    #[test]
    fn file_fetch_size_cap_refuses() {
        let too_big = vec![0u8; FILE_FETCH_MAX_BYTES + 1];
        let ack = build_file_fetch_ack("full", "deadbeef", Some(too_big), Some([7u8; 32]));
        assert_eq!(ack.get("found").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(ack.get("reason").and_then(|v| v.as_str()), Some("too_large"));
        assert_eq!(
            ack.get("size").and_then(|v| v.as_u64()),
            Some((FILE_FETCH_MAX_BYTES + 1) as u64)
        );
    }

    #[test]
    fn file_fetch_missing_blob_returns_found_false() {
        let ack = build_file_fetch_ack("full", "deadbeef", None, Some([7u8; 32]));
        assert_eq!(ack.get("found").and_then(|v| v.as_bool()), Some(false));
        assert!(ack.get("reason").is_none());
    }

    #[test]
    fn file_fetch_full_node_encrypts_served_blob() {
        let key = [7u8; 32];
        let blob = b"odometer photo bytes".to_vec();
        let ack = build_file_fetch_ack("full", "deadbeef", Some(blob.clone()), Some(key));
        assert_eq!(ack.get("found").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(ack.get("size").and_then(|v| v.as_u64()), Some(blob.len() as u64));
        // The blob leaves as ciphertext; plaintext never appears on the wire.
        let enc = ack.get("enc").expect("encrypted envelope");
        assert!(ack.get("bytes").is_none());
        let round = eck_core::utils::crypto::decrypt_bytes(&key, enc).unwrap();
        assert_eq!(round, blob);
    }
}

async fn handle_push(state: &Arc<AppState>, payload: &Value, sender: &str) -> Value {
    let entity_type = payload
        .get("entity_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let entities: Vec<Value> = payload
        .get("entities")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // Sender from envelope is authoritative; payload's `source_instance` is a
    // hint from the original caller. Use whichever is non-empty.
    let source = if !sender.is_empty() {
        sender.to_string()
    } else {
        payload
            .get("source_instance")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };

    if entity_type.is_empty() {
        return json!({"ok": false, "error": "missing entity_type", "applied": 0});
    }

    let applied =
        crate::handlers::mesh::apply_pushed_entities(state, &entity_type, &entities, &source).await;

    json!({
        "ok": true,
        "entity_type": entity_type,
        "applied": applied,
    })
}
