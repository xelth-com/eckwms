use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
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
pub async fn nodes(State(state): State<Arc<AppState>>) -> Json<Value> {
    let nodes = match state.sync_engine.relay().get_mesh_status().await {
        Ok(n) => n,
        Err(e) => {
            debug!("Relay unreachable, returning empty nodes: {}", e);
            return Json(json!([]));
        }
    };

    let mapped: Vec<Value> = nodes
        .into_iter()
        .map(|n| {
            json!({
                "instance_id": n.instance_id,
                "status": n.status,
                "role": "peer",
                "base_url": format!("http://{}:{}", n.external_ip, n.port),
                "last_seen": n.last_seen,
            })
        })
        .collect();

    Json(json!(mapped))
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
    let svc = merkle::MerkleService::new(state.db.clone(), state.instance_id.clone());

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

    info!(
        "P2P pull: serving {} {} entities",
        entities.len(),
        req.entity_type
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
    let mut applied = 0usize;
    let merkle_svc = merkle::MerkleService::new(state.db.clone(), state.instance_id.clone());

    for entity in &req.entities {
        // Extract ID from the entity — try "id" field
        let entity_id = match entity.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                warn!(
                    "P2P push: skipping {} entity without id field",
                    req.entity_type
                );
                continue;
            }
        };

        // Strip "id" from the content to avoid SurrealDB conflicts with record ID
        let mut clean = entity.clone();
        if let Some(obj) = clean.as_object_mut() {
            obj.remove("id");
        }

        // UPSERT into SurrealDB — generic, works for any entity type
        let result: Result<Option<Value>, _> = state
            .db
            .upsert((&req.entity_type as &str, &entity_id as &str))
            .content(clean)
            .await;

        match result {
            Ok(_) => {
                applied += 1;
                // Update Merkle checksum
                if let Err(e) = merkle_svc
                    .record_checksum(&req.entity_type, &entity_id, entity)
                    .await
                {
                    warn!("Checksum update failed for {}:{}: {}", req.entity_type, entity_id, e);
                }
            }
            Err(e) => {
                warn!(
                    "P2P push: UPSERT failed for {}:{}: {}",
                    req.entity_type, entity_id, e
                );
            }
        }
    }

    info!(
        "P2P push: applied {}/{} {} entities from {}",
        applied,
        req.entities.len(),
        req.entity_type,
        req.source_instance
    );

    Ok(Json(json!({
        "success": true,
        "applied": applied,
        "entity_type": req.entity_type,
    })))
}

// ─── File Serve (P2P) ────────────────────────────────────────────────────────

/// GET /api/mesh/file/:hash — Serve CAS file content for mesh peers.
///
/// Peers call this to hydrate their FileStore after pulling file_resource metadata.
pub async fn serve_mesh_file(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> Result<Response, (StatusCode, String)> {
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
