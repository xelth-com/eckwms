use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    Extension, Json,
};
use eck_core::auth::Claims;
use eck_core::sync::merkle::{self, MerkleRequest};
use eck_core::utils::filestore::FileStore;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ─── Discovery / Status ──────────────────────────────────────────────────────

/// The mesh master/home, if one has been designated. Stored in the mesh-synced
/// `system_config:mesh_master` record (instance_id of the master node), so every
/// node agrees on who's master regardless of which one serves the dashboard.
/// `None` = not designated yet (every node is a plain peer until claimed).
async fn current_master(state: &AppState) -> Option<String> {
    state
        .db
        .query("SELECT VALUE instance_id FROM system_config:mesh_master LIMIT 1")
        .await
        .ok()
        .and_then(|mut r| r.take::<Vec<Value>>(0).ok())
        .and_then(|v| v.into_iter().next())
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty())
}

/// Tables covered by the cross-node parity audit. A fixed, cheap set — one
/// `count()` each per call, served at most hourly per peer.
const PARITY_TABLES: &[&str] = &[
    "document", "order", "product", "partner", "location", "quant",
    "picking", "trip", "translation", "user", "registered_device",
    "stock_position",
];

/// Per-table row counts + document AI-derivative counts for the parity audit.
/// Shared by the peer-facing endpoint below and the observer's local side.
pub async fn parity_stats(state: &AppState) -> Value {
    let mut tables = serde_json::Map::new();
    for t in PARITY_TABLES {
        let n: i64 = state.db
            .query(format!("SELECT count() AS n FROM {t} GROUP ALL"))
            .await
            .ok()
            .and_then(|mut r| r.take::<Option<Value>>(0).ok())
            .flatten()
            .and_then(|v| v.get("n")?.as_i64())
            .unwrap_or(0);
        tables.insert(t.to_string(), json!(n));
    }
    let summarized: i64 = state.db
        .query("SELECT count() AS n FROM document WHERE ai_summary != NONE AND ai_summary != '' GROUP ALL")
        .await
        .ok()
        .and_then(|mut r| r.take::<Option<Value>>(0).ok())
        .flatten()
        .and_then(|v| v.get("n")?.as_i64())
        .unwrap_or(0);
    json!({
        "instance_id": state.instance_id,
        "tables": tables,
        "document_summarized": summarized,
    })
}

/// GET /api/mesh/parity — peer-facing (SYNC_SECRET-authed) parity snapshot.
/// The observer on each full node compares peers' snapshots against its own
/// and alerts on persistent drift — the automated version of the manual audit
/// that caught the 195-frozen-summaries bug (2026-07-09).
pub async fn parity(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(parity_stats(&state).await)
}

/// POST /api/mesh/tasks/nudge — a peer just queued a `mesh_task` targeting us;
/// run the task queue immediately instead of waiting for the next poll cycle.
/// Drops reverse-fetch latency from ~30-60s to seconds when the requester can
/// reach us directly (kiosk→scraper on LAN); NAT'd requesters still fall back
/// to the poll. A single in-flight guard prevents a nudge burst from spawning
/// concurrent duplicate `process_tasks` runs.
pub async fn nudge_tasks(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return (StatusCode::ACCEPTED, Json(json!({ "status": "already_running" })));
    }
    let engine = Arc::clone(&state.sync_engine);
    tokio::spawn(async move {
        match engine.process_tasks().await {
            Ok(n) if n > 0 => info!("[mesh] nudge: processed {} task(s)", n),
            Ok(_) => {}
            Err(e) => warn!("[mesh] nudge: process_tasks failed: {}", e),
        }
        IN_FLIGHT.store(false, Ordering::SeqCst);
    });
    (StatusCode::ACCEPTED, Json(json!({ "status": "scheduled" })))
}

/// Fire-and-forget: tell `target_instance_id` to drain its task queue now.
/// Call right after inserting a `mesh_task` for it. Resolves the peer via
/// relay discovery; silently gives up if the peer isn't directly reachable
/// (the poll cycle remains the fallback transport).
pub fn spawn_task_nudge(state: &Arc<AppState>, target_instance_id: String) {
    let engine = Arc::clone(&state.sync_engine);
    let self_id = state.instance_id.clone();
    let secret = state.sync_secret.clone();
    tokio::spawn(async move {
        let peers = eck_core::sync::mesh_client::discover_peers(
            engine.relay(),
            &self_id,
            secret.as_deref(),
        )
        .await;
        if let Some(peer) = peers.iter().find(|p| p.target_instance_id() == target_instance_id) {
            match peer.nudge_tasks().await {
                Ok(()) => debug!("[mesh] nudged {} to drain its task queue", target_instance_id),
                Err(e) => debug!("[mesh] task nudge to {} failed (poll fallback): {}", target_instance_id, e),
            }
        }
    });
}

/// GET /api/mesh/status — This node's identity and mesh membership.
pub async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let base_url =
        std::env::var("BASE_URL").unwrap_or_else(|_| format!("http://localhost:{}", state.port));

    // Honest role: "master" only if THIS node is the designated mesh master,
    // else "peer" (was previously hardcoded "master" on every node).
    let master = current_master(&state).await;
    let role = if master.as_deref() == Some(state.instance_id.as_str()) { "master" } else { "peer" };

    Json(json!({
        "instance_id": state.instance_id,
        "instance_name": state.instance_id,
        "role": role,
        "mesh_master": master,
        "base_url": base_url,
        "mesh_id": state.sync_engine.mesh_id(),
    }))
}

/// GET /api/mesh/nodes — Online peers discovered via relay (tracker only).
///
/// Returns `{ relay: "online" | "offline", nodes: [...] }` so the frontend can
/// distinguish "relay unreachable" from "relay responded but no peers online".
pub async fn nodes(State(state): State<Arc<AppState>>) -> Json<Value> {
    let board_url = std::env::var("ECK_BOARD_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://9eck.com".to_string());
    let relay = state.sync_engine.relay();
    // The relay polygon this node heartbeats to (same precedence as
    // relay_client: RELAY_URLS > RELAY_URL > public board). Surfaced so the
    // dashboard can render the paid relay cluster as connected servers.
    let relay_urls: Vec<String> = std::env::var("RELAY_URLS")
        .or_else(|_| std::env::var("RELAY_URL"))
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let relay_checks = futures_util::future::join_all(
        relay_urls.iter().map(|u| relay.health_check(u)),
    );
    let (nodes_res, board_ok, relay_ok) =
        tokio::join!(relay.get_mesh_status(), relay.health_check(&board_url), relay_checks);
    let board = if board_ok { "online" } else { "offline" };
    let relays: Vec<Value> = relay_urls
        .iter()
        .zip(relay_ok)
        .map(|(url, ok)| json!({ "url": url, "status": if ok { "online" } else { "offline" } }))
        .collect();

    let nodes = match nodes_res {
        Ok(n) => n,
        Err(e) => {
            debug!("Relay unreachable: {}", e);
            return Json(json!({
                "relay": "offline",
                "nodes": [],
                "board": board,
                "board_url": board_url,
                "relays": relays,
            }));
        }
    };

    let master = current_master(&state).await;
    let mapped: Vec<Value> = nodes
        .into_iter()
        .map(|n| {
            let base = match &n.base_url {
                Some(url) if !url.is_empty() => url.clone(),
                _ => format!("http://{}:{}", n.external_ip, n.port),
            };
            // Honest role: "master" only for the designated mesh master.
            let role = if master.as_deref() == Some(n.instance_id.as_str()) { "master" } else { "peer" };
            json!({
                "instance_id": n.instance_id,
                "status": n.status,
                "role": role,
                "node_role": n.node_role.unwrap_or_else(|| "full".to_string()),
                "base_url": base,
                "last_seen": n.last_seen,
            })
        })
        .collect();

    Json(json!({
        "relay": "online",
        "mesh_master": master,
        "nodes": mapped,
        "board": board,
        "board_url": board_url,
        "relays": relays,
    }))
}

#[derive(Deserialize)]
pub struct SetMasterRequest {
    #[serde(rename = "instanceId")]
    pub instance_id: String,
}

/// POST /api/admin/mesh/master — designate (or transfer) the mesh master/home.
///
/// Writes the chosen node's `instance_id` to the mesh-synced
/// `system_config:mesh_master` record and advances this node's `_vclock` so the
/// designation wins conflict-resolution and propagates to every peer. Admin-only.
/// This is the mesh-level counterpart to `support::claim_home` (which transfers
/// per-entity `home_instance_id`).
pub async fn set_master(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<SetMasterRequest>,
) -> ApiResult<Json<Value>> {
    if claims.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "admin only".into()));
    }
    let iid = body.instance_id.trim().to_string();
    if iid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "instanceId required".into()));
    }

    state
        .db
        .query("UPSERT system_config:mesh_master SET instance_id = $iid, updated_at = time::now()")
        .bind(("iid", iid.clone()))
        .await
        .map_err(db_err)?;
    // Bump vclock so this designation dominates any concurrent peer copy and
    // converges (system_config is mesh-synced; _vclock is IGNORED in the hash).
    let _ = eck_core::sync::conflict::bump_local_vclock(
        &state.db,
        "system_config:mesh_master",
        &state.instance_id,
    )
    .await;

    info!("Mesh master set to {} by {}", iid, state.instance_id);
    Ok(Json(json!({ "mesh_master": iid })))
}

/// GET /api/admin/known-nodes — ALL nodes across ALL meshes (admin only).
///
/// Proxies the relay's `/E/registry` (cross-tenant, gated by `RELAY_ADMIN_TOKEN`)
/// so the cloud admin UI can list kiosks regardless of which mesh they're in —
/// the "Request access" flow no longer needs the operator to know the UUID by hand.
pub async fn known_nodes(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    if claims.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "admin only".into()));
    }
    let token = std::env::var("RELAY_ADMIN_TOKEN").unwrap_or_default();
    if token.trim().is_empty() {
        return Ok(Json(json!({
            "nodes": [],
            "note": "RELAY_ADMIN_TOKEN not configured — cross-mesh registry disabled on this relay",
        })));
    }
    match state.sync_engine.relay().fetch_registry(&token).await {
        Ok(nodes) => Ok(Json(json!({ "nodes": nodes }))),
        Err(e) => Err((StatusCode::BAD_GATEWAY, format!("relay registry: {e}"))),
    }
}

// ─── Merkle Tree (P2P) ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MerkleQuery {
    pub entity_type: String,
    pub level: u8,
    pub bucket: Option<String>,
}

/// GET /api/mesh/merkle/state?entity_type=order&level=0[&bucket=a]
///
/// Returns this node's Merkle tree state for a given entity type.
/// Peers call this to compare roots/buckets and determine what to sync.
pub async fn merkle_state(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MerkleQuery>,
) -> ApiResult<Json<merkle::MerkleNode>> {
    if !eck_core::sync::engine::is_mesh_entity_type(&q.entity_type) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown entity_type '{}'", q.entity_type),
        ));
    }
    // Cache nodes advertise only their authoritative subset (is_cache=false).
    // Full peers see the whole tree.
    let svc = if state.node_role == "cache" {
        merkle::MerkleService::new_cache_filtered(state.db.clone(), state.instance_id.clone())
    } else {
        merkle::MerkleService::new(state.db.clone(), state.instance_id.clone())
    };

    let req = MerkleRequest {
        entity_type: q.entity_type,
        level: q.level,
        bucket: q.bucket,
    };

    let mut node = svc
        .get_state(&req)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Always advertise this build's content-hash schema (mixed-build guard,
    // layer 2). ADDITIVE: a peer on an older binary omits it and is grandfathered
    // as compatible; a peer on a DIFFERENT field-set/algorithm skips tree-repair
    // with us (roots can never agree). Never participates in any hash.
    node.hash_schema = Some(eck_core::sync::merkle::hash_schema_version().to_string());

    Ok(Json(node))
}

// ─── P2P Pull (peer requests entities from us) ──────────────────────────────

#[derive(Deserialize)]
pub struct PullRequest {
    pub entity_type: String,
    pub ids: Vec<String>,
}

/// POST /api/mesh/sync/pull — Peer requests specific entities by ID.
///
/// Returns the raw SurrealDB documents so the peer can upsert them.
/// Leverages SurrealDB's schemaless nature: no per-entity-type match arms.
pub async fn sync_pull(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PullRequest>,
) -> ApiResult<Json<Value>> {
    // entity_type lands in the query's table position via format! (not bindable)
    // — whitelist it or this is arbitrary SurrealQL for anyone past mesh_auth.
    if !eck_core::sync::engine::is_mesh_entity_type(&req.entity_type) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown entity_type '{}'", req.entity_type),
        ));
    }
    if req.ids.is_empty() {
        return Ok(Json(json!({
            "entities": [],
            "entity_type": req.entity_type,
            "hash_schema": eck_core::sync::merkle::hash_schema_version(),
        })));
    }

    // Build SurrealQL: SELECT * FROM <table> WHERE record::id(id) IN $ids
    // Using record::id() to get clean string IDs (not Thing)
    let query = format!(
        "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) IN $ids",
        req.entity_type
    );

    let requested = req.ids.clone();
    let mut entities: Vec<Value> = state
        .db
        .query(&query)
        .bind(("ids", req.ids))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    // TOMBSTONES: a requested id with no live row but a `deleted` checksum is a
    // deletion the peer must learn — serve it as a `{id, _deleted, _vclock}` marker
    // so conflict::resolve can apply it (instead of the peer re-advertising the id =
    // resurrection). Only for ids that came back empty above.
    let missing: Vec<String> = requested
        .into_iter()
        .filter(|id| !entities.iter().any(|e| e.get("id").and_then(|v| v.as_str()) == Some(id.as_str())))
        .collect();
    if !missing.is_empty() {
        let tombs: Vec<Value> = state
            .db
            .query(
                "SELECT entity_id, vclock FROM entity_checksum \
                 WHERE entity_type = $et AND deleted = true AND entity_id IN $ids",
            )
            .bind(("et", req.entity_type.clone()))
            .bind(("ids", missing))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?;
        for t in tombs {
            if let Some(eid) = t.get("entity_id").and_then(|v| v.as_str()) {
                entities.push(json!({
                    "id": eid,
                    "_deleted": true,
                    "_vclock": t.get("vclock").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }

    // Blind-cache invariant (shared logic in eck_core::utils::crypto):
    //   owner (has key)      → encrypt every row before it leaves the wire;
    //   blind cache (no key) → serve ONLY ciphertext, WITHHOLD any plaintext it
    //                          should never have held (e.g. full-era legacy);
    //   plain full (no key)  → serve as-is.
    let n = entities.len();
    let has_key = eck_core::utils::crypto::data_key();
    let is_cache = state.node_role == "cache";
    let entities = eck_core::utils::crypto::prepare_outbound(entities, has_key, is_cache);
    let withheld = n - entities.len();
    if withheld > 0 {
        warn!(
            "P2P pull: blind cache WITHHELD {}/{} {} plaintext rows (must never serve cleartext a cache shouldn't hold)",
            withheld, n, req.entity_type
        );
    }

    info!(
        "P2P pull: serving {}/{} {} entities (encrypted={}, withheld={})",
        entities.len(),
        n,
        req.entity_type,
        has_key.is_some(),
        withheld
    );

    Ok(Json(json!({
        "entities": entities,
        "entity_type": req.entity_type,
        // ADDITIVE: this build's content-hash schema (mixed-build guard, layer 2).
        "hash_schema": eck_core::sync::merkle::hash_schema_version(),
    })))
}

// ─── P2P Push (peer sends entities to us) ────────────────────────────────────

#[derive(Deserialize)]
pub struct PushRequest {
    pub entity_type: String,
    pub entities: Vec<Value>,
    pub source_instance: String,
}

/// POST /api/mesh/sync/push — Peer pushes entities to us.
///
/// Generic UPSERT: leverages SurrealDB's schemaless nature to accept any
/// entity shape. The entity_type maps directly to a SurrealDB table.
pub async fn sync_push(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PushRequest>,
) -> ApiResult<Json<Value>> {
    let applied = apply_pushed_entities(
        &state,
        &req.entity_type,
        &req.entities,
        &req.source_instance,
    )
    .await;

    Ok(Json(json!({
        "success": true,
        "applied": applied,
        "entity_type": req.entity_type,
    })))
}

/// Reusable helper: applies a batch of pushed entities (conflict-resolve + merkle
/// checksum update). Called by both the direct HTTP handler (`sync_push`) and
/// the relay-routed mesh poller (`mesh_relay_poller`). Returns the count of
/// entities actually written (a no-op upsert from VectorClock conflict
/// resolution doesn't count).
pub async fn apply_pushed_entities(
    state: &Arc<AppState>,
    entity_type: &str,
    entities: &[Value],
    source_instance: &str,
) -> usize {
    // Same whitelist as sync_pull: a pushed entity_type names the table we
    // write into (and gets interpolated into resolve_and_upsert's SELECT), so
    // an unvalidated value = write-to-any-table (system_config…) + injection.
    if !eck_core::sync::engine::is_mesh_entity_type(entity_type) {
        warn!(
            "P2P push: REJECTED unknown entity_type '{}' from {} ({} entities)",
            entity_type,
            source_instance,
            entities.len()
        );
        return 0;
    }
    let started = std::time::Instant::now();
    let mut applied = 0usize;
    let merkle_svc = merkle::MerkleService::new(state.db.clone(), state.instance_id.clone());

    for entity in entities {
        // Prefer the canonical foo_id column (a bare UUID) for tables that
        // carry one — that's what conflict::resolve_and_upsert and the merkle
        // tree both use as the record key. Fall back to extracting the leaf
        // from the implicit Thing id for tables without a dedicated column.
        let id_field = match entity_type {
            "registered_device" => Some("device_id"),
            "order" => Some("order_id"),
            _ => None,
        };
        let entity_id_opt = id_field
            .and_then(|f| entity.get(f).and_then(|v| v.as_str()).map(String::from))
            .or_else(|| entity.get("id").and_then(eck_core::sync::merkle::extract_entity_leaf_id));
        let entity_id = match entity_id_opt {
            Some(id) => id,
            None => {
                warn!(
                    "P2P push: skipping {} entity without id field",
                    entity_type
                );
                continue;
            }
        };

        // Conflict-aware upsert using VectorClock causality
        match eck_core::sync::conflict::resolve_and_upsert(
            &state.db,
            entity_type,
            &entity_id,
            entity.clone(),
            &state.instance_id,
        )
        .await
        {
            Ok(outcome) => {
                use eck_core::sync::conflict::ResolveOutcome;
                // Record the checksum of the ACTUALLY-stored content on every
                // resolved path (write / equal / local-wins), so a converged or
                // local-wins leaf stops being re-pushed each cycle.
                let checksum_of: Option<&serde_json::Value> = match &outcome {
                    ResolveOutcome::Wrote => { applied += 1; Some(&entity) }
                    ResolveOutcome::AlreadyEqual(local) => Some(local),
                    ResolveOutcome::LocalNewer(local) => Some(local),
                    ResolveOutcome::Tombstoned => None,
                };
                if let Some(v) = checksum_of {
                    // A cache node adopting a pushed row must flag the checksum
                    // is_cache=true: its advertised merkle view filters those
                    // out, and a keyless blind cache would WITHHOLD the plain-
                    // text row in sync_pull anyway — an un-flagged checksum is
                    // an advertisement it can never honor (permanent
                    // "pulling 1 → pulled 0" loop on every full peer). Rows the
                    // node actually OWNS (home_instance_id = self) stay
                    // authoritative and advertised.
                    let owned_by_me = v
                        .get("home_instance_id")
                        .and_then(|h| h.as_str())
                        == Some(state.instance_id.as_str());
                    let res = if state.node_role == "cache" && !owned_by_me {
                        merkle_svc
                            .record_checksum_cached(entity_type, &entity_id, v)
                            .await
                    } else {
                        merkle_svc.record_checksum(entity_type, &entity_id, v).await
                    };
                    if let Err(e) = res {
                        warn!("Checksum update failed for {}:{}: {}", entity_type, entity_id, e);
                    }
                }

                // Cross-node UX: a FINISHED translation just arrived from a peer —
                // re-broadcast the same `translation_ready` event on THIS node's
                // dashboard WS so open pages here refresh instantly instead of
                // waiting for the client's 15s poll fallback. Only on an actual
                // write of a `done` row (a claim/failed arriving is not viewer-
                // relevant). Note: this covers the PUSH-arrival path; entities a
                // peer serves us via merkle PULL are applied in core's sync engine
                // (no ws_tx there) and still rely on the 15s fallback.
                if entity_type == "translation"
                    && matches!(outcome, ResolveOutcome::Wrote)
                    && entity.get("status").and_then(|v| v.as_str()) == Some("done")
                {
                    let evt = json!({
                        "type": "translation_ready",
                        "source": entity.get("source").and_then(|v| v.as_str()).unwrap_or_default(),
                        "field": entity.get("field").and_then(|v| v.as_str()).unwrap_or_default(),
                        "lang": entity.get("lang").and_then(|v| v.as_str()).unwrap_or_default(),
                    });
                    let _ = state.ws_tx.send(evt.to_string());
                }
            }
            Err(e) => {
                warn!(
                    "P2P push: conflict resolve failed for {}:{}: {}",
                    entity_type, entity_id, e
                );
            }
        }
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let n = entities.len();
    let per_row_us = if n > 0 {
        (elapsed_ms as u64 * 1000) / n as u64
    } else {
        0
    };
    info!(
        "P2P push: applied {}/{} {} entities from {} in {} ms ({} us/row avg)",
        applied, n, entity_type, source_instance, elapsed_ms, per_row_us
    );

    applied
}

// ─── File Serve (P2P) ────────────────────────────────────────────────────────

/// GET /api/mesh/file/:hash — Serve CAS file content for mesh peers.
///
/// Peers call this to hydrate their FileStore after pulling file_resource metadata.
pub async fn serve_mesh_file(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> Result<Response, (StatusCode, String)> {
    // Blind-cache invariant (companion to sync_pull): a keyless cache must not be
    // a file-content authority. CAS blobs are NOT envelope-encrypted, so serving
    // raw bytes (e.g. odometer / Kennzeichen photos) would leak readable content a
    // cache should never expose. Consumers hydrate files from the data owner.
    if state.node_role == "cache" {
        return Err((
            StatusCode::NOT_FOUND,
            "blind cache does not serve file content".into(),
        ));
    }
    // Look up file_resource by SHA-256 hash
    let rows: Vec<Value> = state
        .db
        .query("SELECT * FROM file_resource WHERE hash = $hash LIMIT 1")
        .bind(("hash", hash.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let record = match rows.into_iter().next() {
        Some(r) => r,
        // No `file_resource` row: a WASM plugin binary lives in the CAS by
        // sha256 with NO metadata row — the registry stores it CAS-only so the
        // blob never merkle-syncs (design .eck/WASM_ARCHITECTURE.md §5/§6). §6
        // names THIS endpoint (`GET /api/mesh/file/:hash`) as the transport a
        // peer uses to lazily hydrate a plugin it received the record for, so
        // fall back to serving the content-addressed `.wasm` blob directly.
        None => return serve_plugin_cas_blob(&hash).await,
    };

    let storage_path = record["storage_path"]
        .as_str()
        .ok_or((StatusCode::NOT_FOUND, "No storage path".into()))?;
    let mime = record["mime_type"]
        .as_str()
        .unwrap_or("application/octet-stream");

    let store = FileStore::new(".");
    let bytes = store
        .read(storage_path)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .unwrap())
}

/// Serve a WASM plugin binary straight from the content-addressed CAS path.
///
/// Plugin blobs (`{sha}.wasm`) are written by `PluginRegistry::install` with NO
/// `file_resource` row (design §5: the blob is CAS-only and does NOT
/// merkle-sync), so `serve_mesh_file`'s `file_resource`-by-hash lookup misses
/// them. This is the §6-named fallback that lets a peer lazily hydrate a plugin
/// it received the record for. Reached only from `serve_mesh_file`, i.e. AFTER
/// its blind-cache guard, so caches never serve plugin content either.
///
/// Security: the sha is validated as 64 lowercase-hex before it touches a path
/// (no traversal — the path is a pure function of the content address), and the
/// bytes are re-verified against it before serving (CAS integrity). Only files
/// the registry itself wrote (`.wasm`, at the content-addressed path) are
/// reachable; the requester must already know the exact sha, the same trust
/// model as every CAS-by-sha serve.
async fn serve_plugin_cas_blob(hash: &str) -> Result<Response, (StatusCode, String)> {
    let is_sha256 = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_sha256 {
        return Err((StatusCode::NOT_FOUND, "File not found".into()));
    }
    let path = format!("data/filestore/{}/{}/{}.wasm", &hash[0..2], &hash[2..4], hash);
    let store = FileStore::new(".");
    let bytes = store
        .read(&path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "File not found".to_string()))?;
    if !eck_core::utils::filestore::verify_sha256(&bytes, hash) {
        return Err((StatusCode::NOT_FOUND, "File not found".into()));
    }
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/wasm")
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .unwrap())
}

// ─── Task Queue (Reverse-Fetch) ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TaskQuery {
    pub instance_id: String,
}

/// GET /api/mesh/tasks?instance_id=xxx — Return pending tasks for the calling node.
pub async fn get_tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TaskQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let rows: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, target_instance_id, action, ticket_id, created_at FROM mesh_task WHERE target_instance_id = $caller_id ORDER BY created_at ASC")
        .bind(("caller_id", q.instance_id))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    Ok(Json(rows))
}

/// DELETE /api/mesh/tasks/:id — Mark a task as completed (delete it).
pub async fn delete_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let _: Option<Value> = state.db
        .delete(("mesh_task", task_id.as_str()))
        .await
        .map_err(db_err)?;

    Ok(Json(json!({ "success": true })))
}

// ─── Raw Document Fetch (P2P) ───────────────────────────────────────────────

/// GET /api/mesh/raw-docs/:ticket_id — return document_raw records for a ticket.
/// Used by thin nodes to lazy-load heavy payloads from the fat node that imported them.
pub async fn raw_docs(
    State(state): State<Arc<AppState>>,
    Path(ticket_id): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    // Blind-cache invariant: `document_raw` payloads are never envelope-encrypted
    // (and are intentionally never synced to caches in the first place). A keyless
    // cache must not serve raw doc bodies — refuse on cache nodes (defense-in-depth).
    if state.node_role == "cache" {
        return Ok(Json(vec![]));
    }
    let rows: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, type, ticket_id, payload, updated_at FROM document_raw WHERE record::id(id) = $tid OR ticket_id = $tid ORDER BY updated_at ASC")
        .bind(("tid", ticket_id))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    Ok(Json(rows))
}
