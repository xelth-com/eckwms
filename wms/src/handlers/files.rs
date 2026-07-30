use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chrono::Utc;
use eck_core::utils::filestore::FileStore;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tracing::{info, warn};

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn filestore() -> FileStore {
    FileStore::new(".")
}

/// Resolves `replaced_by` stubs to find the latest version of a file
async fn resolve_latest_file(db: &eck_core::db::SurrealDb, initial_id: &str) -> Option<Value> {
    let mut current_id = initial_id.to_string();
    let mut depth = 0;

    loop {
        if depth > 10 {
            warn!("FileStore: Too many redirects for {}", initial_id);
            return None;
        }

        let rows: Result<Vec<Value>, _> = db.query("SELECT * FROM file_resource WHERE cas_uuid = $id LIMIT 1")
            .bind(("id", current_id.clone())).await.and_then(|mut r| r.take(0));

        if let Ok(mut records) = rows {
            if let Some(record) = records.pop() {
                if let Some(next_id) = record.get("replaced_by").and_then(|v| v.as_str()) {
                    current_id = next_id.to_string();
                    depth += 1;
                    continue;
                }
                return Some(record);
            }
        }
        return None;
    }
}

/// record::id() of the file_resource row for a CAS uuid — RELATE targets must
/// be bound record ids: this SurrealDB's RELATE grammar accepts neither bare
/// function calls nor indexed subquery paths (`(SELECT …)[0].id`) inline.
async fn file_record_id(db: &eck_core::db::SurrealDb, cas_uuid: &str) -> Option<String> {
    let rows: Vec<Value> = db
        .query("SELECT record::id(id) AS rid FROM file_resource WHERE cas_uuid = $fid LIMIT 1")
        .bind(("fid", cas_uuid.to_string()))
        .await
        .ok()?
        .take(0)
        .ok()?;
    rows.first()
        .and_then(|r| r.get("rid"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ─── Upload ──────────────────────────────────────────────────────────────────

/// POST /api/files/upload — Multipart file upload with CAS deduplication.
///
/// Form fields:
/// - `file` (required): the file content
/// - `avatar`: optional client-generated thumbnail
/// - `device_id`: uploading device identifier
/// - `context`: semantic context string (e.g. "scan:order-123")
/// - `image_id`: optional claimed CAS UUID (verified against Murmur3)
/// - `entity_type` + `entity_id`: optional — auto-creates a RELATE attachment
pub async fn upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name = String::from("upload");
    let mut mime_type = String::from("application/octet-stream");
    let mut avatar_data: Option<Vec<u8>> = None;
    let mut device_id = String::new();
    let mut context = String::new();
    let mut claimed_id: Option<String> = None;
    let mut entity_type: Option<String> = None;
    let mut entity_id: Option<String> = None;
    // PDA (movFast) extras — /api/upload/image sends these instead of context
    let mut scan_mode = String::new();
    let mut barcode_data = String::new();
    let mut order_id = String::new();
    let mut user_id = String::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (StatusCode::BAD_REQUEST, format!("Multipart error: {}", e))
    })? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" | "image" => {
                if let Some(fname) = field.file_name() {
                    file_name = fname.to_string();
                }
                if let Some(ct) = field.content_type() {
                    mime_type = ct.to_string();
                }
                file_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Read error: {}", e)))?
                        .to_vec(),
                );
            }
            "avatar" => {
                let bytes = field.bytes().await.unwrap_or_default().to_vec();
                if !bytes.is_empty() {
                    avatar_data = Some(bytes);
                }
            }
            "device_id" | "deviceId" => {
                device_id = field.text().await.unwrap_or_default();
            }
            "context" => {
                context = field.text().await.unwrap_or_default();
            }
            "image_id" | "imageId" => {
                claimed_id = Some(field.text().await.unwrap_or_default());
            }
            "entity_type" | "entityType" => {
                entity_type = Some(field.text().await.unwrap_or_default());
            }
            "entity_id" | "entityId" => {
                entity_id = Some(field.text().await.unwrap_or_default());
            }
            "scanMode" => {
                scan_mode = field.text().await.unwrap_or_default();
            }
            "barcodeData" => {
                barcode_data = field.text().await.unwrap_or_default();
            }
            "orderId" => {
                order_id = field.text().await.unwrap_or_default();
            }
            "user_id" | "userId" => {
                user_id = field.text().await.unwrap_or_default();
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let content = file_data.ok_or((StatusCode::BAD_REQUEST, "Missing file field".into()))?;

    let record = upload_image_core(&state, ImageUploadParams {
        content, file_name, mime_type, avatar_data, device_id, context,
        claimed_id, entity_type, entity_id, scan_mode, barcode_data, order_id, user_id,
    }).await?;
    Ok((StatusCode::CREATED, Json(record)))
}

/// Parsed inputs of an image/file upload — shared by the multipart HTTP path
/// (`upload`) and the relay `image_upload` mesh-task (which base64-decodes the
/// bytes off the mesh queue), so a phone on mobile data can still land a photo in
/// CAS through the relay polygon when the LAN master is unreachable.
pub struct ImageUploadParams {
    pub content: Vec<u8>,
    pub file_name: String,
    pub mime_type: String,
    pub avatar_data: Option<Vec<u8>>,
    pub device_id: String,
    pub context: String,
    pub claimed_id: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub scan_mode: String,
    pub barcode_data: String,
    pub order_id: String,
    /// Logged-in PDA user (server `user` record id). An upload that resolves to
    /// no order/entity gets parked on this user as a `temp` attachment with a
    /// one-week TTL instead of landing orphaned.
    pub user_id: String,
}

/// CAS-save + dedupe + optional RELATE attachment, callable off the HTTP path.
pub async fn upload_image_core(state: &Arc<AppState>, p: ImageUploadParams) -> ApiResult<Value> {
    let ImageUploadParams {
        content, file_name, mime_type, avatar_data, device_id, mut context,
        claimed_id, mut entity_type, mut entity_id, scan_mode, barcode_data, order_id, user_id,
    } = p;

    // PDA upload: synthesize a context string from the repair metadata
    if context.is_empty() && !scan_mode.is_empty() {
        context = match (barcode_data.is_empty(), order_id.is_empty()) {
            (false, _) => format!("{}:{}", scan_mode, barcode_data),
            (true, false) => format!("{}:order-{}", scan_mode, order_id),
            _ => scan_mode.clone(),
        };
    }

    // CAS save to disk
    let store = filestore();
    let saved = store
        .save(
            &content,
            &file_name,
            avatar_data.as_deref(),
            claimed_id.as_deref(),
        )
        .await
        .map_err(|e| (StatusCode::CONFLICT, e))?;

    let cas_id = saved.cas_uuid.to_string();

    // Check for duplicate in DB, following stubs if the file was optimized
    let final_record = resolve_latest_file(&state.db, &cas_id).await;

    let file_record = if let Some(existing) = final_record {
        let final_id = existing.get("cas_uuid").and_then(|v| v.as_str()).unwrap_or(&cas_id);
        // Re-upload of a previously deleted temp photo: the bytes are back on
        // disk (store.save above rewrote them), so lift the tombstone.
        if existing.get("deleted_at").map(|v| !v.is_null()).unwrap_or(false) {
            let _ = state
                .db
                .query("UPDATE file_resource SET deleted_at = NONE WHERE cas_uuid = $fid")
                .bind(("fid", final_id.to_string()))
                .await;
        }
        info!(
            "FileStore: deduplicated upload {} -> existing/optimized {}",
            file_name, final_id
        );
        existing
    } else {
        // Encode avatar as base64 for SurrealDB (bytes can't round-trip through Value)
        let avatar_b64: Option<String> = saved.avatar_data.as_ref().map(|a| B64.encode(a));

        let now = Utc::now();
        let record: Option<Value> = state
            .db
            .query(
                "INSERT INTO file_resource { \
                    cas_uuid: $cas_uuid, \
                    hash: $hash, \
                    original_name: $name, \
                    mime_type: $mime, \
                    size_bytes: $size, \
                    avatar_b64: $avatar, \
                    storage_path: $path, \
                    created_by_device: $device, \
                    context: $ctx, \
                    created_at: $now, \
                    updated_at: $now \
                }",
            )
            .bind(("cas_uuid", cas_id.clone()))
            .bind(("hash", saved.sha256.clone()))
            .bind(("name", file_name.clone()))
            .bind(("mime", mime_type.clone()))
            .bind(("size", saved.size_bytes))
            .bind(("avatar", avatar_b64))
            .bind(("path", saved.storage_path.clone()))
            .bind(("device", device_id.clone()))
            .bind(("ctx", context.clone()))
            .bind(("now", now))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?;

        record.ok_or_else(|| db_err("INSERT returned nothing"))?
    };

    // PDA repair photos: attach to the open repair order for the scanned
    // serial (barcodeData carries the target device serial in repair mode).
    if entity_type.is_none() && (scan_mode == "repair_photo" || !order_id.is_empty()) {
        let order_row: Result<Vec<Value>, _> = if !order_id.is_empty() {
            state
                .db
                .query(
                    "SELECT record::id(id) AS rid FROM order \
                     WHERE order_number = $key OR record::id(id) = $key LIMIT 1",
                )
                .bind(("key", order_id.clone()))
                .await
                .and_then(|mut r| r.take(0))
        } else {
            state
                .db
                .query(
                    "SELECT record::id(id) AS rid FROM order \
                     WHERE serial_number = $key AND status NOT IN ['completed', 'closed', 'cancelled', 'done'] \
                     LIMIT 1",
                )
                .bind(("key", barcode_data.clone()))
                .await
                .and_then(|mut r| r.take(0))
        };

        if let Ok(rows) = order_row {
            if let Some(rid) = rows.first().and_then(|r| r.get("rid")).and_then(|v| v.as_str()) {
                entity_type = Some("order".to_string());
                entity_id = Some(rid.to_string());
            }
        }
    }

    // Auto-attach via RELATE if entity_type + entity_id provided
    let mut attached = false;
    if let (Some(et), Some(eid)) = (entity_type, entity_id) {
        if !et.is_empty() && !eid.is_empty() {
            let relate_result: Result<Vec<Value>, String> = match file_record_id(&state.db, &cas_id).await {
                Some(rid) => state
                    .db
                    .query(
                        "RELATE (type::record($et, $eid)) -> has_attachment -> \
                         (type::record('file_resource', $rid)) \
                         SET created_at = time::now(), label = $ctx",
                    )
                    .bind(("et", et.clone()))
                    .bind(("eid", eid.clone()))
                    .bind(("rid", rid))
                    .bind(("ctx", context.clone()))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|mut r| r.take(0).map_err(|e| e.to_string())),
                None => Err("file_resource row not found".into()),
            };

            match relate_result {
                Ok(_) => {
                    attached = true;
                    info!("Attached file {} to {}:{}", cas_id, et, eid);
                }
                Err(e) => warn!("RELATE attachment failed (non-fatal): {}", e),
            }
        }
    }

    // Orphaned upload (no order/entity resolved): park it on the uploading USER
    // as a `temp` attachment with a one-week TTL — nothing silently vanishes,
    // but junk doesn't accumulate either. The PDA settings screen lists these
    // for review; attaching to a real entity later (attach/redirect) clears the
    // TTL, and the scheduler sweeps whatever is still unclaimed after a week.
    if !attached && !user_id.is_empty() {
        if let Some(rid) = file_record_id(&state.db, &cas_id).await {
            // Skip when the file already hangs on anything (incl. an earlier temp
            // park of the same bytes) — a dedup re-upload must not re-park or
            // refresh the TTL.
            let edge_count: Result<Vec<Value>, _> = state
                .db
                .query(
                    "SELECT count() AS n FROM has_attachment \
                     WHERE out = type::record('file_resource', $rid) GROUP ALL",
                )
                .bind(("rid", rid.clone()))
                .await
                .and_then(|mut r| r.take(0));
            let has_edges = edge_count
                .ok()
                .and_then(|rows| rows.first().and_then(|r| r.get("n").and_then(|v| v.as_i64())))
                .unwrap_or(0)
                > 0;
            if !has_edges {
                let park: Result<Vec<Value>, _> = state
                    .db
                    .query(
                        "RELATE (type::record('user', $uid)) -> has_attachment -> \
                         (type::record('file_resource', $rid)) \
                         SET created_at = time::now(), label = 'temp'; \
                         UPDATE file_resource SET temp_expires_at = time::now() + 1w \
                         WHERE cas_uuid = $fid",
                    )
                    .bind(("uid", user_id.clone()))
                    .bind(("rid", rid))
                    .bind(("fid", cas_id.clone()))
                    .await
                    .and_then(|mut r| r.take(0));
                match park {
                    Ok(_) => info!("Orphaned upload {} parked on user:{} (temp, 1w TTL)", cas_id, user_id),
                    Err(e) => warn!("temp park on user failed (non-fatal): {}", e),
                }
            }
        }
    }

    Ok(file_record)
}

// ─── Download / Serve ────────────────────────────────────────────────────────

/// GET /api/files/:id — Serve file content by CAS UUID.
/// Returns the avatar (fast, from DB) or the full file from disk.
pub async fn download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, (StatusCode, String)> {
    let record = resolve_latest_file(&state.db, &id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "File not found".into()))?;

    // Deleted temp photos are tombstoned (deleted_at), not dropped — the
    // tombstone syncs to mesh peers like any record update. Serve nothing.
    if record.get("deleted_at").map(|v| !v.is_null()).unwrap_or(false) {
        return Err((StatusCode::NOT_FOUND, "File deleted".into()));
    }

    let mime = record["mime_type"]
        .as_str()
        .unwrap_or("application/octet-stream")
        .to_string();
    let original_name = record["original_name"]
        .as_str()
        .unwrap_or("file")
        .to_string();

    // Try inline avatar first (fast path)
    if let Some(avatar_b64) = record["avatar_b64"].as_str() {
        if !avatar_b64.is_empty() {
            if let Ok(bytes) = B64.decode(avatar_b64) {
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, &mime)
                    .header("x-content-source", "db-avatar")
                    .header(
                        header::CONTENT_DISPOSITION,
                        format!("inline; filename=\"{}\"", original_name),
                    )
                    .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
                    .body(Body::from(bytes))
                    .unwrap());
            }
        }
    }

    // Fall back to disk
    let storage_path = record["storage_path"]
        .as_str()
        .ok_or((StatusCode::NOT_FOUND, "No storage path".into()))?;

    let store = filestore();
    let mut source = "disk-cas";
    let bytes = match store.read(storage_path).await {
        Ok(b) => b,
        Err(disk_err) => {
            // Local CAS miss on a row that DID arrive over the mesh: only the
            // `file_resource` metadata (+ inline avatar) replicates, never the
            // blob. Pull it from the owning peer once, verify the sha, and land
            // it in our own CAS — from then on this is a plain disk hit. Lazy by
            // design: a peer only stores the attachments someone actually opened.
            let hash = record["hash"].as_str().unwrap_or_default().to_string();
            if hash.is_empty() {
                return Err((StatusCode::NOT_FOUND, disk_err));
            }
            let fetched = state
                .sync_engine
                .fetch_file_from_peers(&hash)
                .await
                .ok_or((
                    StatusCode::NOT_FOUND,
                    format!("{} (and no mesh peer served {})", disk_err, hash),
                ))?;
            if let Err(e) = store.write_verified(storage_path, &fetched, &hash).await {
                // Serve what we verified-or-failed to cache, but never hand out
                // bytes that failed the hash check.
                warn!("Mesh file hydrate {} failed: {}", hash, e);
                return Err((StatusCode::NOT_FOUND, e));
            }
            source = "mesh-peer";
            fetched
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &mime)
        .header("x-content-source", source)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", original_name),
        )
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .unwrap())
}

// ─── Attachments ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AttachmentQuery {
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
}

/// GET /api/files/attachments?entity_type=order&entity_id=abc
/// Lists file_resource records attached to an entity via the `has_attachment` graph edge.
pub async fn list_attachments(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AttachmentQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let (Some(et), Some(eid)) = (q.entity_type, q.entity_id) else {
        return Err((
            StatusCode::BAD_REQUEST,
            "entity_type and entity_id are required".into(),
        ));
    };

    let files: Vec<Value> = state
        .db
        .query(
            "SELECT \
                record::id(out.id) AS id, \
                out.cas_uuid AS cas_uuid, \
                out.original_name AS original_name, \
                out.mime_type AS mime_type, \
                out.size_bytes AS size_bytes, \
                out.created_at AS file_created_at, \
                record::id(id) AS edge_id, \
                label, \
                created_at AS attached_at \
             FROM has_attachment \
             WHERE in = type::record($et, $eid) \
             ORDER BY created_at DESC",
        )
        .bind(("et", et))
        .bind(("eid", eid))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(files))
}

/// POST /api/files/attach — Create a graph edge between an entity and a file_resource.
/// Body: { "entity_type": "order", "entity_id": "abc", "file_id": "<cas_uuid>", "label": "photo" }
pub async fn attach(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let et = body["entity_type"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "entity_type required".into()))?
        .to_string();
    let eid = body["entity_id"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "entity_id required".into()))?
        .to_string();
    let file_id = body["file_id"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "file_id required".into()))?
        .to_string();
    let label = body["label"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let rid = file_record_id(&state.db, &file_id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "unknown file_id".into()))?;
    let result: Vec<Value> = state
        .db
        .query(
            "RELATE (type::record($et, $eid)) -> has_attachment -> \
             (type::record('file_resource', $rid)) \
             SET created_at = time::now(), label = $label",
        )
        .bind(("et", et.clone()))
        .bind(("eid", eid))
        .bind(("rid", rid))
        .bind(("label", label))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    match result.into_iter().next() {
        Some(v) => {
            // A real attachment claims the file — clear the temp TTL and drop
            // the user park edge(s) so it no longer shows as "unbound".
            if et != "user" {
                clear_temp_park(&state.db, &file_id).await;
            }
            Ok((StatusCode::CREATED, Json(v)))
        }
        None => Err(db_err("RELATE returned nothing")),
    }
}

/// Remove the temp-park state of a file: TTL off, `temp` user edges gone.
/// Called whenever the file gets attached to a real entity.
async fn clear_temp_park(db: &eck_core::db::SurrealDb, cas_uuid: &str) {
    let Some(rid) = file_record_id(db, cas_uuid).await else { return };
    let r = db
        .query(
            "UPDATE file_resource SET temp_expires_at = NONE WHERE cas_uuid = $fid; \
             DELETE has_attachment \
             WHERE out = type::record('file_resource', $rid) AND label = 'temp'",
        )
        .bind(("fid", cas_uuid.to_string()))
        .bind(("rid", rid))
        .await;
    if let Err(e) = r {
        warn!("clear_temp_park({}) failed: {}", cas_uuid, e);
    }
}

/// POST /api/files/redirect — re-home a user-parked temp photo onto the open
/// repair order for a scanned serial (same resolution as the upload path).
/// Body: { "file_id": "<cas_uuid>", "barcode": "<serial>" }
pub async fn redirect(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let file_id = body["file_id"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "file_id required".into()))?
        .to_string();
    let barcode = body["barcode"]
        .as_str()
        .ok_or((StatusCode::BAD_REQUEST, "barcode required".into()))?
        .to_string();

    let rows: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS rid, order_number FROM order \
             WHERE serial_number = $key AND status NOT IN ['completed', 'closed', 'cancelled', 'done'] \
             LIMIT 1",
        )
        .bind(("key", barcode.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let Some(row) = rows.first() else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no open order for serial {barcode} — bind it in repair mode first"),
        ));
    };
    let rid = row.get("rid").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let order_number = row.get("order_number").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    let file_rid = file_record_id(&state.db, &file_id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "unknown file_id".into()))?;
    let relate: Vec<Value> = state
        .db
        .query(
            "RELATE (type::record('order', $eid)) -> has_attachment -> \
             (type::record('file_resource', $frid)) \
             SET created_at = time::now(), label = $label",
        )
        .bind(("eid", rid.clone()))
        .bind(("frid", file_rid))
        .bind(("label", format!("redirect:{barcode}")))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    if relate.is_empty() {
        return Err(db_err("RELATE returned nothing — unknown file_id?"));
    }

    clear_temp_park(&state.db, &file_id).await;
    info!("Redirected temp file {} -> order {} ({})", file_id, rid, order_number);
    Ok(Json(serde_json::json!({
        "ok": true, "order_id": rid, "order_number": order_number
    })))
}

/// DELETE /api/files/temp/:id — delete a user-parked temp photo for good:
/// attachment edges off, disk bytes off, row tombstoned (`deleted_at` syncs to
/// mesh peers as a normal update). Refuses when a real attachment exists.
pub async fn delete_temp(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let rows: Vec<Value> = state
        .db
        .query("SELECT cas_uuid, storage_path FROM file_resource WHERE cas_uuid = $fid AND deleted_at IS NONE LIMIT 1")
        .bind(("fid", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let Some(record) = rows.first() else {
        return Err((StatusCode::NOT_FOUND, "File not found".into()));
    };

    let rid = file_record_id(&state.db, &id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "File not found".into()))?;
    let real_edges: Vec<Value> = state
        .db
        .query(
            "SELECT count() AS n FROM has_attachment \
             WHERE out = type::record('file_resource', $rid) \
             AND label != 'temp' GROUP ALL",
        )
        .bind(("rid", rid.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    if real_edges
        .first()
        .and_then(|r| r.get("n").and_then(|v| v.as_i64()))
        .unwrap_or(0)
        > 0
    {
        return Err((
            StatusCode::CONFLICT,
            "file is attached to a real entity — detach first".into(),
        ));
    }

    let storage_path = record
        .get("storage_path")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    state
        .db
        .query(
            "DELETE has_attachment WHERE out = type::record('file_resource', $rid); \
             UPDATE file_resource SET deleted_at = time::now(), temp_expires_at = NONE, \
             avatar_b64 = NONE WHERE cas_uuid = $fid",
        )
        .bind(("rid", rid))
        .bind(("fid", id.clone()))
        .await
        .map_err(db_err)?;

    if !storage_path.is_empty() {
        if let Err(e) = tokio::fs::remove_file(&storage_path).await {
            warn!("delete_temp: disk remove {} failed: {}", storage_path, e);
        }
    }
    info!("Deleted temp file {}", id);
    Ok(StatusCode::NO_CONTENT)
}

/// Scheduler sweep: user-parked temp files past their one-week TTL are removed
/// exactly like [`delete_temp`] — unless a real attachment appeared meanwhile,
/// in which case the TTL is simply lifted.
pub async fn sweep_expired_temp(db: &eck_core::db::SurrealDb) {
    let rows: Result<Vec<Value>, _> = db
        .query(
            "SELECT cas_uuid, storage_path FROM file_resource \
             WHERE temp_expires_at != NONE AND temp_expires_at < time::now() AND deleted_at IS NONE",
        )
        .await
        .and_then(|mut r| r.take(0));
    let Ok(rows) = rows else {
        warn!("[temp-sweep] query failed");
        return;
    };
    if rows.is_empty() {
        return;
    }
    info!("[temp-sweep] {} expired temp file(s)", rows.len());
    for row in rows {
        let Some(cas) = row.get("cas_uuid").and_then(|v| v.as_str()) else { continue };
        let cas = cas.to_string();
        let Some(rid) = file_record_id(db, &cas).await else { continue };
        let real: i64 = db
            .query(
                "SELECT count() AS n FROM has_attachment \
                 WHERE out = type::record('file_resource', $rid) \
                 AND label != 'temp' GROUP ALL",
            )
            .bind(("rid", rid.clone()))
            .await
            .and_then(|mut r| r.take::<Vec<Value>>(0))
            .ok()
            .and_then(|r| r.first().and_then(|x| x.get("n").and_then(|v| v.as_i64())))
            .unwrap_or(0);
        if real > 0 {
            let _ = db
                .query("UPDATE file_resource SET temp_expires_at = NONE WHERE cas_uuid = $fid")
                .bind(("fid", cas.clone()))
                .await;
            continue;
        }
        let _ = db
            .query(
                "DELETE has_attachment WHERE out = type::record('file_resource', $rid); \
                 UPDATE file_resource SET deleted_at = time::now(), temp_expires_at = NONE, \
                 avatar_b64 = NONE WHERE cas_uuid = $fid",
            )
            .bind(("rid", rid))
            .bind(("fid", cas.clone()))
            .await;
        if let Some(path) = row.get("storage_path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                if let Err(e) = tokio::fs::remove_file(path).await {
                    warn!("[temp-sweep] disk remove {} failed: {}", path, e);
                }
            }
        }
        info!("[temp-sweep] removed expired temp file {}", cas);
    }
}

/// DELETE /api/files/attachments/:edge_id — Remove an attachment edge (soft: the file remains).
pub async fn detach(
    State(state): State<Arc<AppState>>,
    Path(edge_id): Path<String>,
) -> ApiResult<StatusCode> {
    let _: Option<Value> = state
        .db
        .delete(("has_attachment", edge_id.as_str()))
        .await
        .map_err(db_err)?;

    Ok(StatusCode::NO_CONTENT)
}
