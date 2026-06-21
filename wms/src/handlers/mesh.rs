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

/// GET /api/mesh/status — This node's identity and mesh membership.
pub async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let base_url =
        std::env::var("BASE_URL").unwrap_or_else(|_| format!("http://localhost:{}", state.port));

    Json(json!({
        "instance_id": state.instance_id,
        "instance_name": state.instance_id,
        "role": "master",
        "base_url": base_url,
        "mesh_id": state.sync_engine.mesh_id(),
    }))
}

/// GET /api/mesh/nodes — Online peers discovered via relay (tracker only).
///
/// Returns `{ relay: "online" | "offline", nodes: [...] }` so the frontend can
/// distinguish "relay unreachable" from "relay responded but no peers online".
pub async fn nodes(State(state): State<Arc<AppState>>) -> Json<Value> {
    let nodes = match state.sync_engine.relay().get_mesh_status().await {
        Ok(n) => n,
        Err(e) => {
            debug!("Relay unreachable: {}", e);
            return Json(json!({
                "relay": "offline",
                "nodes": [],
            }));
        }
    };

    let mapped: Vec<Value> = nodes
        .into_iter()
        .map(|n| {
            let base = match &n.base_url {
                Some(url) if !url.is_empty() => url.clone(),
                _ => format!("http://{}:{}", n.external_ip, n.port),
            };
            json!({
                "instance_id": n.instance_id,
                "status": n.status,
                "role": "peer",  // legacy field, kept for back-compat with existing clients
                "node_role": n.node_role.unwrap_or_else(|| "full".to_string()),
                "base_url": base,
                "last_seen": n.last_seen,
            })
        })
        .collect();

    Json(json!({
        "relay": "online",
        "nodes": mapped,
    }))
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

    let node = svc
        .get_state(&req)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

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
    if req.ids.is_empty() {
        return Ok(Json(json!({ "entities": [], "entity_type": req.entity_type })));
    }

    // Build SurrealQL: SELECT * FROM <table> WHERE record::id(id) IN $ids
    // Using record::id() to get clean string IDs (not Thing)
    let query = format!(
        "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) IN $ids",
        req.entity_type
    );

    let entities: Vec<Value> = state
        .db
        .query(&query)
        .bind(("ids", req.ids))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

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

    Ok(Json(
        json!({ "entities": entities, "entity_type": req.entity_type }),
    ))
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
            Ok(written) => {
                if written {
                    applied += 1;
                    // Update Merkle checksum
                    if let Err(e) = merkle_svc
                        .record_checksum(entity_type, &entity_id, entity)
                        .await
                    {
                        warn!("Checksum update failed for {}:{}: {}", entity_type, entity_id, e);
                    }
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
        .bind(("hash", hash))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let record = rows
        .into_iter()
        .next()
        .ok_or((StatusCode::NOT_FOUND, "File not found".into()))?;

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
