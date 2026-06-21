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
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let content = file_data.ok_or((StatusCode::BAD_REQUEST, "Missing file field".into()))?;

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
    if let (Some(et), Some(eid)) = (entity_type, entity_id) {
        if !et.is_empty() && !eid.is_empty() {
            let relate_result: Result<Vec<Value>, _> = state
                .db
                .query(
                    "RELATE type::record($et, $eid) -> has_attachment -> \
                     (SELECT id FROM file_resource WHERE cas_uuid = $fid LIMIT 1)[0].id \
                     SET created_at = time::now(), label = $ctx",
                )
                .bind(("et", et.clone()))
                .bind(("eid", eid.clone()))
                .bind(("fid", cas_id.clone()))
                .bind(("ctx", context.clone()))
                .await
                .map_err(db_err)?
                .take(0);

            match relate_result {
                Ok(_) => info!("Attached file {} to {}:{}", cas_id, et, eid),
                Err(e) => warn!("RELATE attachment failed (non-fatal): {}", e),
            }
        }
    }

    Ok((StatusCode::CREATED, Json(file_record)))
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
    let bytes = store
        .read(storage_path)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &mime)
        .header("x-content-source", "disk-cas")
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

    let result: Vec<Value> = state
        .db
        .query(
            "RELATE type::record($et, $eid) -> has_attachment -> \
             (SELECT id FROM file_resource WHERE cas_uuid = $fid LIMIT 1)[0].id \
             SET created_at = time::now(), label = $label",
        )
        .bind(("et", et))
        .bind(("eid", eid))
        .bind(("fid", file_id))
        .bind(("label", label))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    match result.into_iter().next() {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err(db_err("RELATE returned nothing")),
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
