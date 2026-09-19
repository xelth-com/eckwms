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

/// The `file_resource` row for a CAS uuid, exactly as stored — no chain walk.
async fn fetch_file_row(db: &eck_core::db::SurrealDb, cas_uuid: &str) -> Option<Value> {
    let rows: Result<Vec<Value>, _> = db
        .query("SELECT * FROM file_resource WHERE cas_uuid = $id LIMIT 1")
        .bind(("id", cas_uuid.to_string()))
        .await
        .and_then(|mut r| r.take(0));
    rows.ok()?.pop()
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

        let record = fetch_file_row(db, &current_id).await?;
        if let Some(next_id) = record
            .get("replaced_by")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            current_id = next_id.to_string();
            depth += 1;
            continue;
        }
        return Some(record);
    }
}

/// Where a row's OWN bytes live in the CAS.
///
/// Normally that is its `storage_path`. The AVIF optimizer, however, MERGEs
/// `storage_path: NONE` onto the row it replaced (it keeps only `replaced_by`)
/// while the original blob stays on disk — so for such a row the path is
/// reconstructed from its content sha: `FileStore::save` derives the path purely
/// from `sha256` + the original extension, so the reconstruction is exact.
fn own_storage_path(record: &Value) -> Option<String> {
    let path = record
        .get("storage_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if !path.is_empty() {
        return Some(path.to_string());
    }
    derive_cas_path(
        record.get("hash").and_then(|v| v.as_str()).unwrap_or(""),
        record.get("original_name").and_then(|v| v.as_str()).unwrap_or(""),
    )
}

/// `data/filestore/{sha[0..2]}/{sha[2..4]}/{sha}.{ext}` — mirrors `FileStore::save`.
fn derive_cas_path(hash: &str, original_name: &str) -> Option<String> {
    if hash.len() < 4 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let ext = std::path::Path::new(original_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e))
        .unwrap_or_default();
    Some(format!(
        "data/filestore/{}/{}/{}{}",
        &hash[0..2],
        &hash[2..4],
        hash,
        ext
    ))
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

// ─── Scan frames (low-priority evidence) ─────────────────────────────────────

/// `context` value that marks an upload as a *scan frame*: the camera frame a
/// barcode / OCR text was recognised from. Evidence, not a photo — it never
/// counts as a real photo anywhere and it is reaped after its TTL.
pub const SCAN_FRAME_CONTEXT: &str = "scan_frame";
const SCAN_FRAME_DEFAULT_TTL_DAYS: u32 = 14;
const SCAN_FRAME_MAX_TTL_DAYS: u32 = 30;
/// Cap on the stored recognition text (barcode value or OCR block).
const RECOGNIZED_MAX_BYTES: usize = 4096;

fn clamp_ttl_days(requested: Option<u32>) -> u32 {
    requested
        .unwrap_or(SCAN_FRAME_DEFAULT_TTL_DAYS)
        .clamp(1, SCAN_FRAME_MAX_TTL_DAYS)
}

/// Byte-safe truncation — an OCR block is arbitrary UTF-8, `&s[..n]` would panic.
fn trunc_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Stamp a freshly stored (or deduplicated) file_resource row as a low-priority
/// scan frame: `temp = true`, `priority = 'low'`, a native `expires_at` datetime
/// and the recognised text. Returns the updated row.
///
/// Kept as a free function on the raw db handle so the DB contract is testable
/// without an `AppState`; the HTTP and relay upload paths both call it.
pub async fn stamp_scan_frame(
    db: &eck_core::db::SurrealDb,
    cas_uuid: &str,
    ttl_days: Option<u32>,
    recognized: &str,
) -> Option<Value> {
    let days = clamp_ttl_days(ttl_days);
    let rec = trunc_bytes(recognized, RECOGNIZED_MAX_BYTES).to_string();
    // `days` is a clamped integer, so the inline duration literal is safe; this
    // SurrealDB has no way to bind a duration into `time::now() + $d`.
    let sql = format!(
        "UPDATE file_resource SET temp = true, priority = 'low', \
         context = '{SCAN_FRAME_CONTEXT}', expires_at = time::now() + {days}d, \
         recognized_text = $rec WHERE cas_uuid = $fid RETURN AFTER"
    );
    let rows: Result<Vec<Value>, _> = db
        .query(sql)
        .bind(("rec", rec))
        .bind(("fid", cas_uuid.to_string()))
        .await
        .and_then(|mut r| r.take(0));
    match rows {
        Ok(mut rows) => rows.pop(),
        Err(e) => {
            warn!("stamp_scan_frame({}) failed: {}", cas_uuid, e);
            None
        }
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
    let mut user_id = String::new();
    // Scan-frame evidence extras
    let mut ttl_days: Option<u32> = None;
    let mut recognized = String::new();

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
            "ttl_days" | "ttlDays" => {
                ttl_days = field.text().await.ok().and_then(|t| t.trim().parse::<u32>().ok());
            }
            "recognized" | "recognized_text" | "recognizedText" => {
                recognized = field.text().await.unwrap_or_default();
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
        ttl_days, recognized,
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
    /// Scan-frame TTL in days (clamped 1..=30, default 14). Only honoured when
    /// `context == "scan_frame"`.
    pub ttl_days: Option<u32>,
    /// The barcode value or OCR text this frame was recognised from — stored as
    /// `recognized_text` (≤ 4 kB). Scan frames only.
    pub recognized: String,
}

/// CAS-save + dedupe + optional RELATE attachment, callable off the HTTP path.
pub async fn upload_image_core(state: &Arc<AppState>, p: ImageUploadParams) -> ApiResult<Value> {
    let ImageUploadParams {
        content, file_name, mime_type, avatar_data, device_id, mut context,
        claimed_id, mut entity_type, mut entity_id, scan_mode, barcode_data, order_id, user_id,
        ttl_days, recognized,
    } = p;

    let is_scan_frame = context == SCAN_FRAME_CONTEXT;

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

    // Low-priority scan-frame evidence: temp + priority + a real `expires_at`
    // datetime the reaper sweeps. Applied to deduplicated rows as well — the
    // same frame arriving twice just refreshes its TTL, it never becomes a photo.
    let file_record = if is_scan_frame {
        let target = file_record
            .get("cas_uuid")
            .and_then(|v| v.as_str())
            .unwrap_or(&cas_id)
            .to_string();
        stamp_scan_frame(&state.db, &target, ttl_days, &recognized)
            .await
            .unwrap_or(file_record)
    } else {
        file_record
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
    // Scan frames are never parked on a user: they already carry their own TTL
    // and they must not show up in the user's "Meine Fotos" review list.
    if !attached && !is_scan_frame && !user_id.is_empty() {
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

/// Query of `GET /api/files/:id`.
///
/// `original=1` (alias `raw=1`) asks for the record that was actually requested
/// instead of the tail of the optimizer's `replaced_by` chain: a 2 MB JPEG scan
/// re-encoded to AVIF is still on disk, and OCR / archival callers want the
/// bytes the customer sent, not our re-encode. Absent (the default), nothing
/// changes — the chain is walked exactly as before.
#[derive(Deserialize, Default)]
pub struct DownloadQuery {
    pub original: Option<String>,
    pub raw: Option<String>,
}

/// `?original`, `?original=1`, `=true`, `=yes`, `=on` are all "on"; `=0` is off.
fn flag_on(v: &Option<String>) -> bool {
    match v {
        None => false,
        Some(s) => {
            let s = s.trim();
            s.is_empty() || matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        }
    }
}

/// 404 with a body that says WHY — a bare 404 is indistinguishable from "no
/// such file", and the owner's rule is that everything is read through the API.
fn bytes_missing_404(hash: &str) -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "error": "bytes not on this node or any reachable peer",
                "hash": hash,
            })
            .to_string(),
        ))
        .unwrap()
}

/// GET /api/files/:id — Serve file content by CAS UUID.
/// Returns the avatar (fast, from DB) or the full file from disk.
///
/// `?original=1` / `?raw=1` serves the requested record itself when its bytes
/// are still in the local CAS; if they are gone it falls back to the
/// `replaced_by` chain and marks the answer `X-Eck-Served: replacement`.
pub async fn download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<DownloadQuery>,
) -> Result<Response, (StatusCode, String)> {
    let want_original = flag_on(&q.original) || flag_on(&q.raw);
    let store = filestore();

    // `original=1`: the row as asked for, no chain walk — but only while its own
    // bytes are here. A mesh hydrate cannot help: only the chain TAIL is what
    // peers hold under their own hash, so a vanished original degrades to the
    // replacement rather than to a 404.
    let mut served = "original";
    let mut requested_row: Option<Value> = None;
    if want_original {
        if let Some(row) = fetch_file_row(&state.db, &id).await {
            if row.get("deleted_at").map(|v| !v.is_null()).unwrap_or(false) {
                return Err((StatusCode::NOT_FOUND, "File deleted".into()));
            }
            let local = own_storage_path(&row)
                .map(|p| store.exists(&p))
                .unwrap_or(false);
            if local {
                requested_row = Some(row);
            } else {
                served = "replacement";
            }
        }
    }

    let serving_original = requested_row.is_some();
    let record = match requested_row {
        Some(row) => row,
        None => resolve_latest_file(&state.db, &id)
            .await
            .ok_or((StatusCode::NOT_FOUND, "File not found".into()))?,
    };

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

    // Try inline avatar first (fast path). Skipped for `original=1`: the avatar
    // is a thumbnail, and a caller asking for the original wants the real bytes.
    if !serving_original {
        if let Some(avatar_b64) = record["avatar_b64"].as_str() {
            if !avatar_b64.is_empty() {
                if let Ok(bytes) = B64.decode(avatar_b64) {
                    let mut b = Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, &mime)
                        .header("x-content-source", "db-avatar")
                        .header(
                            header::CONTENT_DISPOSITION,
                            format!("inline; filename=\"{}\"", original_name),
                        )
                        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
                    if want_original {
                        b = b.header("x-eck-served", served);
                    }
                    return Ok(b.body(Body::from(bytes)).unwrap());
                }
            }
        }
    }

    // Fall back to disk
    let storage_path = own_storage_path(&record)
        .ok_or((StatusCode::NOT_FOUND, "No storage path".into()))?;
    let storage_path = storage_path.as_str();

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
            let Some(fetched) = state.sync_engine.fetch_file_from_peers(&hash).await else {
                warn!(
                    "files::download {}: {} and no mesh peer served {}",
                    id, disk_err, hash
                );
                return Ok(bytes_missing_404(&hash));
            };
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

    let mut b = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &mime)
        .header("x-content-source", source)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", original_name),
        )
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
    if want_original {
        b = b.header("x-eck-served", served);
    }
    Ok(b.body(Body::from(bytes)).unwrap())
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
                out.original_cas_uuid AS original_cas_uuid, \
                out.hash AS cas_hash, \
                out.storage_path AS cas_path, \
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

    // `original_available`: are the PRE-optimization bytes still on this node?
    // The optimizer relinks the attachment edge to the AVIF row and leaves
    // `original_cas_uuid` pointing back, so an optimized row costs one lookup;
    // a never-optimized row IS its own original and costs none (its hash and
    // path already came back above — both are stripped before answering, a
    // storage path is not the caller's business).
    let store = filestore();
    let mut out = Vec::with_capacity(files.len());
    for mut f in files {
        let original_cas = f
            .get("original_cas_uuid")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let path = match &original_cas {
            Some(cas) => match fetch_file_row(&state.db, cas).await {
                Some(row) => own_storage_path(&row),
                None => None,
            },
            None => own_storage_path(&serde_json::json!({
                "storage_path": f.get("cas_path").cloned().unwrap_or(Value::Null),
                "hash": f.get("cas_hash").cloned().unwrap_or(Value::Null),
                "original_name": f.get("original_name").cloned().unwrap_or(Value::Null),
            })),
        };
        let available = path.map(|p| store.exists(&p)).unwrap_or(false);
        if let Some(obj) = f.as_object_mut() {
            obj.remove("cas_hash");
            obj.remove("cas_path");
            obj.insert("original_available".into(), Value::Bool(available));
        }
        out.push(f);
    }

    Ok(Json(out))
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

/// Rows touched by one [`reap_expired_files`] run — the sweep is bounded so a
/// backlog drains over several hours instead of stalling the scheduler task.
const REAP_BATCH: usize = 500;

/// Hourly reaper for TTL'd evidence (scan frames): every `file_resource` whose
/// `expires_at` has passed loses its `has_attachment` edges and its CAS bytes,
/// and the row itself is tombstoned exactly like [`delete_temp`] does
/// (`deleted_at` is an ordinary field update, so it rides the existing mesh sync
/// path — no separate delete protocol). The bytes go only when no *other* live
/// `file_resource` row shares the hash: CAS is deduplicated, a real photo may
/// point at the same blob.
///
/// Idempotent (tombstoned rows are skipped) and bounded to [`REAP_BATCH`].
/// Returns `(rows_reaped, blobs_removed)`.
pub async fn reap_expired_files(db: &eck_core::db::SurrealDb) -> (usize, usize) {
    let rows: Result<Vec<Value>, _> = db
        .query(format!(
            "SELECT cas_uuid, storage_path, hash FROM file_resource \
             WHERE expires_at != NONE AND expires_at < time::now() AND deleted_at IS NONE \
             LIMIT {REAP_BATCH}"
        ))
        .await
        .and_then(|mut r| r.take(0));
    let Ok(rows) = rows else {
        warn!("[reap] expired-file query failed");
        return (0, 0);
    };
    if rows.is_empty() {
        return (0, 0);
    }

    let mut reaped = 0usize;
    let mut blobs = 0usize;
    for row in rows {
        let Some(cas) = row.get("cas_uuid").and_then(|v| v.as_str()) else { continue };
        let cas = cas.to_string();
        let Some(rid) = file_record_id(db, &cas).await else { continue };

        // Bytes: only when nothing else live points at the same content.
        let hash = row.get("hash").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let shared: i64 = if hash.is_empty() {
            0
        } else {
            db.query(
                "SELECT count() AS n FROM file_resource \
                 WHERE hash = $h AND cas_uuid != $fid AND deleted_at IS NONE GROUP ALL",
            )
            .bind(("h", hash.clone()))
            .bind(("fid", cas.clone()))
            .await
            .and_then(|mut r| r.take::<Vec<Value>>(0))
            .ok()
            .and_then(|r| r.first().and_then(|x| x.get("n").and_then(|v| v.as_i64())))
            .unwrap_or(0)
        };

        let deleted = db
            .query(
                "DELETE has_attachment WHERE out = type::record('file_resource', $rid); \
                 UPDATE file_resource SET deleted_at = time::now(), expires_at = NONE, \
                 temp_expires_at = NONE, avatar_b64 = NONE WHERE cas_uuid = $fid",
            )
            .bind(("rid", rid))
            .bind(("fid", cas.clone()))
            .await;
        if let Err(e) = deleted {
            warn!("[reap] {} tombstone failed: {}", cas, e);
            continue;
        }
        reaped += 1;

        if shared == 0 {
            if let Some(path) = row.get("storage_path").and_then(|v| v.as_str()) {
                if !path.is_empty() {
                    match tokio::fs::remove_file(path).await {
                        Ok(()) => blobs += 1,
                        Err(e) => warn!("[reap] disk remove {} failed: {}", path, e),
                    }
                }
            }
        }
    }
    info!("[reap] {} expired file(s) reaped, {} CAS blob(s) removed", reaped, blobs);
    (reaped, blobs)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Same shape as the receiving tests: a `mem://` store with the tables the
    /// file paths touch DEFINEd exactly as `main.rs` does at boot.
    async fn test_db() -> eck_core::db::SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();
        db.query(
            "DEFINE TABLE IF NOT EXISTS file_resource SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS receipt SCHEMALESS;
             DEFINE TABLE OVERWRITE has_attachment TYPE RELATION SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    async fn make_file(db: &eck_core::db::SurrealDb, cas: &str, hash: &str, path: &str) {
        db.query(
            "CREATE file_resource SET cas_uuid = $cas, hash = $hash, storage_path = $path, \
             mime_type = 'image/webp', created_at = time::now()",
        )
        .bind(("cas", cas.to_string()))
        .bind(("hash", hash.to_string()))
        .bind(("path", path.to_string()))
        .await
        .unwrap()
        .check()
        .unwrap();
    }

    /// Attach `cas` to a receipt, returning nothing — the reaper must clear it.
    async fn attach_to_receipt(db: &eck_core::db::SurrealDb, cas: &str, receipt: &str) {
        let rid = file_record_id(db, cas).await.expect("file row");
        db.query(
            "RELATE (type::record('receipt', $r)) -> has_attachment -> \
             (type::record('file_resource', $rid)) SET created_at = time::now()",
        )
        .bind(("r", receipt.to_string()))
        .bind(("rid", rid))
        .await
        .unwrap()
        .check()
        .unwrap();
    }

    async fn edge_count(db: &eck_core::db::SurrealDb) -> i64 {
        let rows: Vec<Value> = db
            .query("SELECT count() AS n FROM has_attachment GROUP ALL")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        rows.first()
            .and_then(|r| r.get("n").and_then(|v| v.as_i64()))
            .unwrap_or(0)
    }

    #[test]
    fn ttl_is_clamped_to_the_allowed_window() {
        assert_eq!(clamp_ttl_days(None), 14);
        assert_eq!(clamp_ttl_days(Some(1)), 1);
        assert_eq!(clamp_ttl_days(Some(0)), 1);
        assert_eq!(clamp_ttl_days(Some(30)), 30);
        assert_eq!(clamp_ttl_days(Some(9999)), 30);
    }

    /// `?original` with no value is "on"; an explicit falsy value is not.
    #[test]
    fn original_flag_parsing() {
        assert!(!flag_on(&None));
        assert!(flag_on(&Some(String::new())));
        assert!(flag_on(&Some("1".into())));
        assert!(flag_on(&Some("TRUE".into())));
        assert!(!flag_on(&Some("0".into())));
        assert!(!flag_on(&Some("false".into())));
    }

    /// The optimizer blanks the original's `storage_path`, so the download path
    /// has to rebuild it from the sha — byte-identically to `FileStore::save`.
    #[test]
    fn optimizer_blanked_original_still_resolves_to_its_cas_path() {
        let sha = "1f721045aabbccddeeff00112233445566778899aabbccddeeff001122334455";
        let row = serde_json::json!({
            "storage_path": Value::Null,
            "hash": sha,
            "original_name": "Scan 2026-09-11.jpg",
            "replaced_by": "some-avif-cas",
        });
        assert_eq!(
            own_storage_path(&row).unwrap(),
            format!("data/filestore/1f/72/{sha}.jpg")
        );

        // A live path always wins over the reconstruction.
        let live = serde_json::json!({
            "storage_path": "data/filestore/aa/bb/aabb.avif",
            "hash": sha,
            "original_name": "x.jpg",
        });
        assert_eq!(own_storage_path(&live).unwrap(), "data/filestore/aa/bb/aabb.avif");

        // Nothing to go on — no guessing.
        assert!(own_storage_path(&serde_json::json!({ "hash": "" })).is_none());
        assert!(derive_cas_path("not-hex-at-all", "x.jpg").is_none());
        // No extension is legal (FileStore::save leaves it off too).
        assert_eq!(
            derive_cas_path(sha, "noext").unwrap(),
            format!("data/filestore/1f/72/{sha}")
        );
    }

    #[test]
    fn recognized_text_truncation_never_splits_a_char() {
        let s = "ä".repeat(4000); // 8000 bytes
        let cut = trunc_bytes(&s, RECOGNIZED_MAX_BYTES);
        assert!(cut.len() <= RECOGNIZED_MAX_BYTES);
        assert!(cut.chars().all(|c| c == 'ä'));
    }

    /// The upload contract: a scan frame is `temp`, low priority, carries the
    /// recognised text and a REAL `expires_at` datetime in the future.
    #[tokio::test]
    async fn stamping_a_scan_frame_sets_a_native_expiry() {
        let db = test_db().await;
        make_file(&db, "cas-sf", "h-sf", "").await;

        let row = stamp_scan_frame(&db, "cas-sf", Some(14), "00340434161094123456")
            .await
            .expect("stamped row");
        assert_eq!(row["temp"], serde_json::json!(true));
        assert_eq!(row["priority"], serde_json::json!("low"));
        assert_eq!(row["context"], serde_json::json!(SCAN_FRAME_CONTEXT));
        assert_eq!(row["recognized_text"], serde_json::json!("00340434161094123456"));
        assert!(!row["expires_at"].is_null());

        // Native datetime, not a string: SurrealDB itself must be able to
        // compare it against `time::now()` (the string/datetime mismatch is the
        // exact trap that dropped fresh shipments out of Receiving).
        let live: Vec<Value> = db
            .query(
                "SELECT cas_uuid FROM file_resource \
                 WHERE expires_at != NONE AND expires_at > time::now()",
            )
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(live.len(), 1, "expiry must compare as a datetime");
    }

    /// The reaper takes expired rows (edges + bytes + tombstone), leaves
    /// unexpired ones alone, and running it twice changes nothing.
    #[tokio::test]
    async fn reaper_takes_expired_rows_and_keeps_the_rest() {
        let db = test_db().await;
        let dir = std::env::temp_dir().join(format!("eck-reap-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let old_path = dir.join("old.webp");
        let live_path = dir.join("live.webp");
        tokio::fs::write(&old_path, b"old").await.unwrap();
        tokio::fs::write(&live_path, b"live").await.unwrap();

        make_file(&db, "cas-old", "h-old", old_path.to_str().unwrap()).await;
        make_file(&db, "cas-live", "h-live", live_path.to_str().unwrap()).await;
        attach_to_receipt(&db, "cas-old", "r-1").await;
        attach_to_receipt(&db, "cas-live", "r-1").await;
        db.query(
            "UPDATE file_resource SET temp = true, priority = 'low', \
             expires_at = time::now() - 1d WHERE cas_uuid = 'cas-old'; \
             UPDATE file_resource SET temp = true, priority = 'low', \
             expires_at = time::now() + 14d WHERE cas_uuid = 'cas-live'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        assert_eq!(edge_count(&db).await, 2);

        let (reaped, blobs) = reap_expired_files(&db).await;
        assert_eq!((reaped, blobs), (1, 1));
        assert_eq!(edge_count(&db).await, 1, "only the expired file loses its edge");
        assert!(!old_path.exists(), "expired CAS bytes are gone");
        assert!(live_path.exists(), "unexpired bytes are untouched");

        // Tombstoned like delete_temp does — the row still exists so the
        // deletion rides the normal mesh update path.
        let dead: Vec<Value> = db
            .query("SELECT deleted_at, expires_at FROM file_resource WHERE cas_uuid = 'cas-old'")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert!(!dead[0]["deleted_at"].is_null());
        assert!(dead[0]["expires_at"].is_null());

        // Idempotent: a second pass finds nothing left to do.
        assert_eq!(reap_expired_files(&db).await, (0, 0));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// CAS is deduplicated: an expired frame whose bytes a live row still points
    /// at loses its row, never the blob.
    #[tokio::test]
    async fn reaper_keeps_bytes_shared_with_a_live_row() {
        let db = test_db().await;
        let dir = std::env::temp_dir().join(format!("eck-reap-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let shared = dir.join("shared.webp");
        tokio::fs::write(&shared, b"shared").await.unwrap();

        make_file(&db, "cas-frame", "same-hash", shared.to_str().unwrap()).await;
        make_file(&db, "cas-photo", "same-hash", shared.to_str().unwrap()).await;
        db.query(
            "UPDATE file_resource SET expires_at = time::now() - 1d WHERE cas_uuid = 'cas-frame'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let (reaped, blobs) = reap_expired_files(&db).await;
        assert_eq!((reaped, blobs), (1, 0));
        assert!(shared.exists(), "the real photo still needs those bytes");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
