//! NAS repair-tree ingester (`.eck/REPAIR_LIFECYCLE.md` §7).
//!
//! Walks `<ECK_NAS_INGEST_ROOT>\<year>\<case-folder>\…` (a NAS document tree) and
//! lands every file into the SAME content-addressable store the Zoho attachment
//! path uses (`FileStore` + `file_resource`, dedup by `cas_uuid`), plus a
//! `repair_case` entity and a typed `has_evidence` EDGE per file.
//!
//! THE FOLDERS DIE AT INGEST (owner 2026-08-31). The `01_Fotos` / `05_QC-Report`
//! subfolder taxonomy is an IMPORT-TIME translation table only — see
//! [`evidence_type_for_subfolder`] / [`evidence_type_for_loose_file`]. Our native
//! model is `repair_case --has_evidence--> file_resource{evidence_type}`; the same
//! shape a future PDA photo or QC-station export lands in, folder-free. If folders
//! are ever needed again that is a read-only PROJECTION, never storage.
//!
//! `case_ref` (CS-DE-…, GER2608-…) is a purely EXTERNAL label carried on the
//! entity; our identity is the deterministic record id (a content hash of
//! year+folder). A future numbering scheme is just a new `case_ref` string on the
//! same entity shape — nothing about our model changes.
//!
//! DRY-RUN IS THE DEFAULT: it hashes + reports (cases, evidence-type counts, bytes,
//! CAS collisions, skips) and writes NOTHING. A real run streams the same counters
//! and performs the CAS/DB writes. Videos are ingested metadata+edge only (blob
//! copy skipped in v1 — keeps CAS small). Zero-byte files are skipped with a log.

use eck_core::db::SurrealDb;
use eck_core::utils::filestore::{content_hash_uuid, FileStore};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// How many entries a `skipped` / `collisions` sample list is capped at in the
/// report (the counters are exact; the lists are for eyeballing).
const SAMPLE_CAP: usize = 50;

// ─── naming-scheme + case identity (pure) ───────────────────────────────────

/// Classify a top-level repair folder name into its naming scheme.
///
/// `cs_de` is the LEGACY 2026 format; `ger2608` is the tenant's CURRENT going-forward
/// Reparaturorder numbering (owner 2026-08-31 — NOT migration noise, not an
/// anomaly). `ger26_placeholder` is the batch-created `GER26 (N)` placeholder
/// folders; `anomalous` is the bare `CS-DE-` (no number) and anything unrecognized.
pub fn naming_scheme(folder_name: &str) -> &'static str {
    let name = folder_name.trim();
    if name.starts_with("GER2608-") {
        "ger2608"
    } else if name.starts_with("GER26 (") {
        "ger26_placeholder"
    } else if name.starts_with("CS-DE-") {
        // A real case_ref has digits after the prefix; the bare `CS-DE-` is the
        // known anomaly (empty folder, 2025 leftover).
        if name["CS-DE-".len()..].chars().any(|c| c.is_ascii_digit()) {
            "cs_de"
        } else {
            "anomalous"
        }
    } else {
        "anomalous"
    }
}

/// Split a folder name `<case_ref>_<customer>` into `(case_ref, customer_label)`.
/// No underscore (e.g. `GER26 (7)`, bare `CS-DE-`) ⇒ empty customer label.
pub fn parse_case_identity(folder_name: &str) -> (String, String) {
    let name = folder_name.trim();
    match name.split_once('_') {
        Some((cref, cust)) => (cref.trim().to_string(), cust.trim().to_string()),
        None => (name.to_string(), String::new()),
    }
}

/// The digits-only tail of a case_ref (`CS-DE-2601040` ⇒ `2601040`,
/// `GER2608-016` ⇒ `2608016`) — the alternate key tried against the ClaimList
/// `cs_de_no` column when linking.
fn case_ref_digits(case_ref: &str) -> String {
    case_ref.chars().filter(|c| c.is_ascii_digit()).collect()
}

// ─── translation table: folder → evidence_type (pure, dies at ingest) ────────

/// The numbered-taxonomy prefix of a subfolder name, matched as `^\d\d_` so the
/// non-standard `2.Gerät_570` (single digit + dot) does NOT masquerade as a
/// numbered folder. Returns e.g. `Some(5)` for `05_QC-Report`.
fn numbered_taxonomy_prefix(name: &str) -> Option<u32> {
    let b = name.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_digit() && b[1].is_ascii_digit() && b[2] == b'_' {
        Some(((b[0] - b'0') as u32) * 10 + (b[1] - b'0') as u32)
    } else {
        None
    }
}

/// Image / video file extensions that mark a loose file as `photo` evidence
/// (device media). Videos land here too — their blob is skipped, but they are
/// still photo-class evidence.
fn is_media_ext(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(),
        Some("jpg") | Some("jpeg") | Some("png") | Some("webp") | Some("avif")
            | Some("heic") | Some("bmp") | Some("tif") | Some("tiff") | Some("gif")
            | Some("mp4") | Some("mov")
    )
}

/// Extra first-level subfolder names (lowercased) that classify as `photo`
/// evidence, beyond the generic `fotos*` / `2.gerät*` folders — e.g. a
/// brand-named media folder on the legacy NAS. Deployment data:
/// `ECK_NAS_PHOTO_SUBFOLDERS` (comma-separated), empty by default.
fn extra_photo_subfolders() -> &'static Vec<String> {
    static V: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        std::env::var("ECK_NAS_PHOTO_SUBFOLDERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Map a FIRST-LEVEL subfolder name to an evidence_type. Numeric prefix 01..12 →
/// the taxonomy; the ad-hoc photo/video folders (`Fotos und Videos`, `Fotos`,
/// `2.Gerät_*`, plus any `ECK_NAS_PHOTO_SUBFOLDERS` aliases) → `photo`; anything
/// else → `unclassified`.
pub fn evidence_type_for_subfolder(name: &str) -> &'static str {
    if let Some(n) = numbered_taxonomy_prefix(name) {
        return match n {
            1 => "photo",
            2 => "ra",
            3 => "db_backup",
            4 => "db_export",
            5 => "qc_report",
            6 => "wartung_stk",
            7 => "kv",
            8 => "kv_signed",
            9 => "rechnung",
            10 => "email",
            11 => "dsgvo",
            12 => "paket_nl",
            _ => "unclassified",
        };
    }
    let low = name.to_lowercase();
    if low.starts_with("fotos")
        || low.starts_with("2.gerät")
        || low.starts_with("2.gerat")
        || extra_photo_subfolders().iter().any(|s| s == &low)
    {
        "photo"
    } else {
        "unclassified"
    }
}

/// Map a loose file sitting at the case-folder root (the Aug-2026 `GER2608-*` /
/// `GER26 (N)` intake dropped its files here, taxonomy-free) to an evidence_type
/// by filename prefix. Media by extension → `photo`; `uKV_*` → `kv_signed`
/// (checked before `KV_*`); `KV_*` → `kv`; `RP_*` → `qc_report` (Reparatur-
/// protokoll); `qcreport_*` → `qc_report`; else `unclassified`.
pub fn evidence_type_for_loose_file(filename: &str) -> &'static str {
    if is_media_ext(filename) {
        "photo"
    } else if filename.starts_with("uKV_") {
        "kv_signed"
    } else if filename.starts_with("KV_") {
        "kv"
    } else if filename.starts_with("RP_") {
        "qc_report"
    } else if filename.starts_with("qcreport_") {
        "qc_report"
    } else {
        "unclassified"
    }
}

/// The evidence_type of a file, given its path components RELATIVE to the case
/// folder (`["01_Fotos", "0.jpg"]` or `["PXL_….jpg"]`). A file directly in the
/// case root is classified by filename; anything under a subfolder inherits that
/// FIRST-level subfolder's type (deeper nesting inherits too).
pub fn evidence_type_for_relpath(components: &[String]) -> &'static str {
    match components {
        [] => "unclassified",
        [only] => evidence_type_for_loose_file(only),
        [top, ..] => evidence_type_for_subfolder(top),
    }
}

// ─── mime (pure) ─────────────────────────────────────────────────────────────

/// `(mime, ext_missing, is_video)` for a filename. Extensionless files are the
/// native `qcreport_*` QC dumps — `application/msword`-ish, `ext_missing:true`.
pub fn mime_for_filename(name: &str) -> (&'static str, bool, bool) {
    let ext = name.rsplit('.').next().filter(|e| *e != name).map(str::to_ascii_lowercase);
    match ext.as_deref() {
        None => ("application/msword", true, false), // extensionless qcreport_* dumps
        Some("jpg") | Some("jpeg") => ("image/jpeg", false, false),
        Some("png") => ("image/png", false, false),
        Some("webp") => ("image/webp", false, false),
        Some("avif") => ("image/avif", false, false),
        Some("heic") => ("image/heic", false, false),
        Some("gif") => ("image/gif", false, false),
        Some("pdf") => ("application/pdf", false, false),
        Some("doc") => ("application/msword", false, false),
        Some("docx") => (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            false,
            false,
        ),
        Some("xls") | Some("xlsm") | Some("xlk") => ("application/vnd.ms-excel", false, false),
        Some("xlsx") => (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            false,
            false,
        ),
        Some("csv") => ("text/csv", false, false),
        Some("txt") => ("text/plain", false, false),
        Some("msg") => ("application/vnd.ms-outlook", false, false),
        Some("eml") => ("message/rfc822", false, false),
        Some("zip") => ("application/zip", false, false),
        Some("mp4") => ("video/mp4", false, true),
        Some("mov") => ("video/quicktime", false, true),
        Some("inb") | Some("bin") | Some("enc") => ("application/octet-stream", false, false),
        Some(_) => ("application/octet-stream", false, false),
    }
}

// ─── report ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
pub struct IngestReport {
    pub root: String,
    pub year: String,
    pub dry_run: bool,
    pub cases_found: u64,
    /// case count per naming scheme (cs_de / ger2608 / ger26_placeholder / anomalous).
    pub cases_by_scheme: BTreeMap<String, u64>,
    pub cases_linked: u64,
    pub cases_unlinked: u64,
    /// file count per evidence_type.
    pub files_by_evidence_type: BTreeMap<String, u64>,
    pub files_total: u64,
    /// non-video, non-zero-byte blobs whose content is NOT already in CAS.
    pub file_resources_new: u64,
    /// evidence edges written (or, in dry-run, that would be written).
    pub edges: u64,
    /// blobs actually written to disk (always 0 in dry-run).
    pub blobs_written: u64,
    /// total bytes of all non-video candidate files.
    pub bytes_total: u64,
    /// bytes of the non-collision candidates (what a real run would newly store).
    pub bytes_new: u64,
    pub videos_meta_only: u64,
    pub zero_byte_skipped: u64,
    pub unclassified_files: u64,
    /// files whose content hash was already present in CAS (expected overlap with
    /// Zoho ticket attachments).
    pub collisions: u64,
    pub collision_samples: Vec<String>,
    pub video_samples: Vec<String>,
    pub zero_byte_samples: Vec<String>,
    pub unclassified_samples: Vec<String>,
    pub errors: Vec<String>,
}

impl IngestReport {
    fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
        *map.entry(key.to_string()).or_insert(0) += 1;
    }
    fn sample(list: &mut Vec<String>, item: String) {
        if list.len() < SAMPLE_CAP {
            list.push(item);
        }
    }
}

// ─── entry point ─────────────────────────────────────────────────────────────

/// Resolve the year folder under `root`: `root/<year>` when it exists, else
/// `root` itself (so `ECK_NAS_INGEST_ROOT` may point at either the `Dokumente`
/// dir or the year folder directly).
fn resolve_year_dir(root: &Path, year: &str) -> PathBuf {
    let candidate = root.join(year);
    if candidate.is_dir() {
        candidate
    } else {
        root.to_path_buf()
    }
}

/// Walk `year_dir`, ingesting each case folder. `instance_id` is the sync-engine
/// instance id used to author the `repair_case` vclock; `None` (tests / pre-mesh)
/// skips the vclock read but still performs the writes. `store_base` is the
/// `FileStore` root (production: `"."`, the WMS working dir — same as the Zoho
/// attachment path; tests point it at a scratch dir).
pub async fn run_ingest(
    db: &SurrealDb,
    instance_id: Option<&str>,
    root: &Path,
    year: &str,
    dry_run: bool,
    limit: Option<usize>,
    store_base: &str,
) -> IngestReport {
    let mut report = IngestReport {
        root: root.display().to_string(),
        year: year.to_string(),
        dry_run,
        ..Default::default()
    };

    // SurrealDB errors on a SELECT from a never-defined table; define the tables
    // this run reads/writes (idempotent). `claimlist_row` is read for linking.
    if let Err(e) = db
        .query(
            "DEFINE TABLE IF NOT EXISTS file_resource SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS repair_case SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS has_evidence SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS claimlist_row SCHEMALESS;",
        )
        .await
        .and_then(|r| r.check())
    {
        warn!("[nas-ingest] table define failed: {e}");
        report.errors.push(format!("table define: {e}"));
        return report;
    }

    let year_dir = resolve_year_dir(root, year);
    let mut case_dirs: Vec<PathBuf> = match std::fs::read_dir(&year_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(e) => {
            warn!("[nas-ingest] read_dir {}: {e}", year_dir.display());
            report.errors.push(format!("read year dir {}: {e}", year_dir.display()));
            return report;
        }
    };
    case_dirs.sort();
    if let Some(n) = limit {
        case_dirs.truncate(n);
    }

    for case_dir in &case_dirs {
        ingest_case(db, instance_id, case_dir, year, dry_run, store_base, &mut report).await;
    }

    info!(
        "[nas-ingest-done] dry_run={} cases={} files={} new_blobs={} edges={} collisions={} videos={} zero_byte={} unclassified={} bytes_new={}",
        dry_run,
        report.cases_found,
        report.files_total,
        report.blobs_written,
        report.edges,
        report.collisions,
        report.videos_meta_only,
        report.zero_byte_skipped,
        report.unclassified_files,
        report.bytes_new,
    );
    report
}

/// Recursively collect every FILE under `case_dir`, as `(abs_path, rel_components)`.
fn collect_files(case_dir: &Path) -> Vec<(PathBuf, Vec<String>)> {
    let mut out = Vec::new();
    let mut stack = vec![(case_dir.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                warn!("[nas-ingest] read_dir {}: {e}", dir.display());
                continue;
            }
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let mut child_rel = rel.clone();
            child_rel.push(name);
            if path.is_dir() {
                stack.push((path, child_rel));
            } else if path.is_file() {
                out.push((path, child_rel));
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn ingest_case(
    db: &SurrealDb,
    instance_id: Option<&str>,
    case_dir: &Path,
    year: &str,
    dry_run: bool,
    store_base: &str,
    report: &mut IngestReport,
) {
    let folder_name = case_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let (case_ref, customer_label) = parse_case_identity(&folder_name);
    let scheme = naming_scheme(&folder_name);

    // Deterministic id = content hash of year+folder_name. Folder name (not
    // case_ref alone) keeps re-runs idempotent AND separates the documented
    // duplicate case-ref `GER2608-016`, used by two different customers.
    let id_uuid = content_hash_uuid(format!("{year}\u{1f}{folder_name}").as_bytes()).to_string();
    let case_rid = format!("repair_case:`{id_uuid}`");

    report.cases_found += 1;
    IngestReport::bump(&mut report.cases_by_scheme, scheme);

    // Link attempt: the ClaimList bridge (`claimlist_row`) carries both
    // `cs_de_no` and `zoho_ticket`, so a case_ref match yields the Zoho ticket.
    let digits = case_ref_digits(&case_ref);
    let linked_ticket: Option<String> = db
        .query("SELECT VALUE zoho_ticket FROM claimlist_row WHERE cs_de_no = $ref OR cs_de_no = $num LIMIT 1")
        .bind(("ref", case_ref.clone()))
        .bind(("num", digits))
        .await
        .ok()
        .and_then(|mut r| r.take::<Option<String>>(0).ok().flatten())
        .filter(|s| !s.trim().is_empty());
    if linked_ticket.is_some() {
        report.cases_linked += 1;
    } else {
        report.cases_unlinked += 1;
    }

    if !dry_run {
        upsert_case(
            db,
            instance_id,
            &case_rid,
            &case_ref,
            year,
            &customer_label,
            &folder_name,
            case_dir,
            scheme,
            linked_ticket.as_deref(),
            report,
        )
        .await;
    }

    for (path, rel) in collect_files(case_dir) {
        ingest_file(db, &case_rid, &path, &rel, dry_run, store_base, report).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn upsert_case(
    db: &SurrealDb,
    instance_id: Option<&str>,
    case_rid: &str,
    case_ref: &str,
    year: &str,
    customer_label: &str,
    folder_name: &str,
    case_dir: &Path,
    scheme: &str,
    linked_ticket: Option<&str>,
    report: &mut IngestReport,
) {
    // Bump THIS node's vclock so the new entity dominates a peer's copy (same
    // discipline as `ai::doc_classify::persist_class`). No mesh id in tests ⇒ a
    // fresh clock keyed off a stub id, harmless.
    let vc = if let Some(iid) = instance_id {
        let existing: Option<Value> = db
            .query("SELECT VALUE _vclock FROM type::record($rid_str) LIMIT 1")
            .bind(("rid_str", case_rid.to_string()))
            .await
            .ok()
            .and_then(|mut r| r.take::<Option<Value>>(0).ok().flatten());
        eck_core::sync::conflict::next_local_vclock(existing.as_ref(), iid)
    } else {
        eck_core::sync::conflict::next_local_vclock(None, "local")
    };

    let res = db
        .query(
            "UPSERT type::record($rid) MERGE { \
                 case_ref: $ref, \
                 year: $year, \
                 customer_label: $cust, \
                 folder_name: $folder, \
                 source_path: $src, \
                 naming_scheme: $scheme, \
                 linked_ticket: $ticket, \
                 linked_via: $via, \
                 ingested_at: time::now(), \
                 _vclock: $vc, \
                 updated_at: time::now() \
             }",
        )
        .bind(("rid", case_rid.to_string()))
        .bind(("ref", case_ref.to_string()))
        .bind(("year", year.to_string()))
        .bind(("cust", customer_label.to_string()))
        .bind(("folder", folder_name.to_string()))
        .bind(("src", case_dir.display().to_string()))
        .bind(("scheme", scheme.to_string()))
        .bind(("ticket", linked_ticket.map(|s| s.to_string())))
        .bind(("via", linked_ticket.map(|_| "claimlist_cs_de".to_string())))
        .bind(("vc", vc))
        .await;
    match res {
        Ok(r) => {
            if let Err(e) = r.check() {
                warn!("[nas-ingest] upsert case {case_ref}: {e}");
                report.errors.push(format!("upsert {case_ref}: {e}"));
            }
        }
        Err(e) => {
            warn!("[nas-ingest] upsert case {case_ref}: {e}");
            report.errors.push(format!("upsert {case_ref}: {e}"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn ingest_file(
    db: &SurrealDb,
    case_rid: &str,
    path: &Path,
    rel: &[String],
    dry_run: bool,
    store_base: &str,
    report: &mut IngestReport,
) {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let rel_path = rel.join("/");
    let evidence_type = evidence_type_for_relpath(rel);
    let (mime, ext_missing, is_video) = mime_for_filename(&name);

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            warn!("[nas-ingest] metadata {}: {e}", path.display());
            report.errors.push(format!("metadata {rel_path}: {e}"));
            return;
        }
    };
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());

    // Zero-byte files: skip blob AND row, just log/count (item 5).
    if size == 0 {
        report.zero_byte_skipped += 1;
        IngestReport::sample(&mut report.zero_byte_samples, rel_path);
        return;
    }

    report.files_total += 1;
    IngestReport::bump(&mut report.files_by_evidence_type, evidence_type);
    if evidence_type == "unclassified" {
        report.unclassified_files += 1;
        IngestReport::sample(&mut report.unclassified_samples, rel_path.clone());
    }

    // Videos: metadata + edge only, blob copy skipped (item 5). Surrogate CAS id
    // from the path (we deliberately never read the ~GB bytes), so a re-run is
    // idempotent per path.
    if is_video {
        report.videos_meta_only += 1;
        IngestReport::sample(&mut report.video_samples, rel_path.clone());
        let cas = content_hash_uuid(path.display().to_string().as_bytes()).to_string();
        report.edges += 1;
        if !dry_run {
            persist_file_and_edge(
                db, case_rid, &cas, "", &name, mime, size as i64, ext_missing, None, None,
                Some("video"), evidence_type, &rel_path, mtime.as_deref(), report,
            )
            .await;
        }
        return;
    }

    // Blob-bearing file: hash the content (needed for the CAS key + collision
    // report in BOTH modes).
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            warn!("[nas-ingest] read {}: {e}", path.display());
            report.errors.push(format!("read {rel_path}: {e}"));
            return;
        }
    };
    let cas = content_hash_uuid(&bytes).to_string();
    report.bytes_total += size;

    let collision = db
        .query("SELECT VALUE id FROM file_resource WHERE cas_uuid = $cas LIMIT 1")
        .bind(("cas", cas.clone()))
        .await
        .ok()
        .and_then(|mut r| r.take::<Option<Value>>(0).ok().flatten())
        .is_some();
    if collision {
        report.collisions += 1;
        IngestReport::sample(&mut report.collision_samples, rel_path.clone());
    } else {
        report.file_resources_new += 1;
        report.bytes_new += size;
    }

    report.edges += 1;

    if dry_run {
        return;
    }

    // Real run: write blob to CAS (idempotent — FileStore skips an existing
    // path), then the file_resource row (dedup by cas_uuid) + edge.
    let filestore = FileStore::new(store_base);
    let (hash, storage_path, avatar_b64) = match filestore.save(&bytes, &name, None, None).await {
        Ok(saved) => {
            if !collision {
                report.blobs_written += 1;
            }
            let avatar = saved.avatar_data.as_ref().map(|a| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(a)
            });
            (saved.sha256, Some(saved.storage_path), avatar)
        }
        Err(e) => {
            warn!("[nas-ingest] filestore.save {rel_path}: {e}");
            report.errors.push(format!("save {rel_path}: {e}"));
            (String::new(), None, None)
        }
    };
    persist_file_and_edge(
        db, case_rid, &cas, &hash, &name, mime, size as i64, ext_missing, storage_path.as_deref(),
        avatar_b64, None, evidence_type, &rel_path, mtime.as_deref(), report,
    )
    .await;
}

/// INSERT the `file_resource` row (dedup by `cas_uuid`, mirrors
/// `services::support::import_attachment`) and RELATE the typed evidence edge
/// (idempotent pre-check on `has_evidence`).
#[allow(clippy::too_many_arguments)]
async fn persist_file_and_edge(
    db: &SurrealDb,
    case_rid: &str,
    cas: &str,
    hash: &str,
    name: &str,
    mime: &str,
    size: i64,
    ext_missing: bool,
    storage_path: Option<&str>,
    avatar_b64: Option<String>,
    blob_skipped: Option<&str>,
    evidence_type: &str,
    rel_path: &str,
    mtime: Option<&str>,
    report: &mut IngestReport,
) {
    let insert = db
        .query(
            "LET $exists = (SELECT id FROM file_resource WHERE cas_uuid = $cas LIMIT 1); \
             IF array::len($exists) == 0 { \
                 INSERT INTO file_resource { \
                     cas_uuid: $cas, \
                     hash: $hash, \
                     original_name: $name, \
                     mime_type: $mime, \
                     size_bytes: $size, \
                     avatar_b64: $avatar, \
                     storage_path: $path, \
                     ext_missing: $extmissing, \
                     blob_skipped: $blobskip, \
                     context: 'nas_repair', \
                     created_at: time::now(), \
                     updated_at: time::now() \
                 }; \
             };",
        )
        .bind(("cas", cas.to_string()))
        .bind(("hash", hash.to_string()))
        .bind(("name", name.to_string()))
        .bind(("mime", mime.to_string()))
        .bind(("size", size))
        .bind(("avatar", avatar_b64))
        .bind(("path", storage_path.map(|s| s.to_string())))
        .bind(("extmissing", ext_missing))
        .bind(("blobskip", blob_skipped.map(|s| s.to_string())))
        .await;
    if let Err(e) = insert.and_then(|r| r.check()) {
        warn!("[nas-ingest] insert file_resource {rel_path}: {e}");
        report.errors.push(format!("insert {rel_path}: {e}"));
        return;
    }

    let edge = db
        .query(
            "LET $fid = (SELECT id FROM file_resource WHERE cas_uuid = $cas LIMIT 1)[0].id; \
             LET $cid = type::record($crid); \
             LET $edge = (SELECT id FROM has_evidence WHERE in = $cid AND out = $fid LIMIT 1); \
             IF $fid IS NOT NONE AND array::len($edge) == 0 { \
                 RELATE $cid -> has_evidence -> $fid SET \
                     evidence_type = $etype, \
                     source_rel_path = $rel, \
                     size = $size, \
                     mtime = $mtime, \
                     blob_skipped = $blobskip, \
                     created_at = time::now(); \
             };",
        )
        .bind(("cas", cas.to_string()))
        .bind(("crid", case_rid.to_string()))
        .bind(("etype", evidence_type.to_string()))
        .bind(("rel", rel_path.to_string()))
        .bind(("size", size))
        .bind(("mtime", mtime.map(|s| s.to_string())))
        .bind(("blobskip", blob_skipped.map(|s| s.to_string())))
        .await;
    if let Err(e) = edge.and_then(|r| r.check()) {
        warn!("[nas-ingest] relate has_evidence {rel_path}: {e}");
        report.errors.push(format!("edge {rel_path}: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_scheme_classifies_all_four() {
        assert_eq!(naming_scheme("CS-DE-2601040_ACME GmbH"), "cs_de");
        assert_eq!(naming_scheme("GER2608-016_CleverFit Kaltenkirchen"), "ger2608");
        assert_eq!(naming_scheme("GER26 (7)"), "ger26_placeholder");
        assert_eq!(naming_scheme("CS-DE-"), "anomalous"); // bare, no number
        assert_eq!(naming_scheme("something weird"), "anomalous");
        // ger2608 is CURRENT, never flagged anomalous.
        assert_ne!(naming_scheme("GER2608-007_X"), "anomalous");
    }

    #[test]
    fn parse_case_identity_splits_on_first_underscore() {
        assert_eq!(
            parse_case_identity("CS-DE-2601040_Health Coach in Düsseldorf"),
            ("CS-DE-2601040".to_string(), "Health Coach in Düsseldorf".to_string())
        );
        // Customer label may itself contain underscores — only the first splits.
        assert_eq!(
            parse_case_identity("GER2608-010_VW Werk_Hannover"),
            ("GER2608-010".to_string(), "VW Werk_Hannover".to_string())
        );
        // No underscore ⇒ empty customer.
        assert_eq!(parse_case_identity("GER26 (7)"), ("GER26 (7)".to_string(), String::new()));
        assert_eq!(parse_case_identity("CS-DE-"), ("CS-DE-".to_string(), String::new()));
    }

    #[test]
    fn case_ref_digits_strips_non_numeric() {
        assert_eq!(case_ref_digits("CS-DE-2601040"), "2601040");
        assert_eq!(case_ref_digits("GER2608-016"), "2608016");
        assert_eq!(case_ref_digits("GER26 (7)"), "267");
    }

    #[test]
    fn subfolder_numeric_prefix_matching() {
        // Full 01..12 taxonomy → the expected evidence types.
        assert_eq!(evidence_type_for_subfolder("01_Fotos"), "photo");
        assert_eq!(evidence_type_for_subfolder("02_Reparaturbeauftragung"), "ra");
        assert_eq!(evidence_type_for_subfolder("03_Datenbanksicherung"), "db_backup");
        assert_eq!(evidence_type_for_subfolder("04_Datenbank-Excel-Export"), "db_export");
        assert_eq!(evidence_type_for_subfolder("05_QC-Report"), "qc_report");
        assert_eq!(evidence_type_for_subfolder("06_Wartungs- und STK-Protokollierung"), "wartung_stk");
        assert_eq!(evidence_type_for_subfolder("07_Kostenvoranschlag oder Garantieaustausch"), "kv");
        assert_eq!(
            evidence_type_for_subfolder("08_Unterschriebener Kostenvoranschlag oder Garantieaustausch"),
            "kv_signed"
        );
        assert_eq!(evidence_type_for_subfolder("09_Rechnung oder Garantiefall"), "rechnung");
        assert_eq!(evidence_type_for_subfolder("10_E-Mails"), "email");
        assert_eq!(evidence_type_for_subfolder("11_Unterschriebene Vereinbarung zur Datenverarbeitung"), "dsgvo");
        assert_eq!(evidence_type_for_subfolder("12_Paketinhalt nach Amsterdam"), "paket_nl");
    }

    #[test]
    fn non_standard_photo_folders_map_to_photo() {
        assert_eq!(evidence_type_for_subfolder("Fotos und Videos"), "photo");
        assert_eq!(evidence_type_for_subfolder("Fotos"), "photo");
        assert_eq!(evidence_type_for_subfolder("2.Gerät_570"), "photo");
        // A single digit + dot must NOT be read as numbered taxonomy "2".
        assert_ne!(evidence_type_for_subfolder("2.Gerät_570"), "ra");
        // Genuinely unknown folder.
        assert_eq!(evidence_type_for_subfolder("Rechnungen alt"), "unclassified");
        // Out-of-range numeric prefix.
        assert_eq!(evidence_type_for_subfolder("13_Sonstiges"), "unclassified");
    }

    #[test]
    fn loose_file_prefix_classification() {
        assert_eq!(evidence_type_for_loose_file("PXL_20260824_114353470.jpg"), "photo");
        assert_eq!(evidence_type_for_loose_file("0.jpg"), "photo");
        assert_eq!(evidence_type_for_loose_file("walkthrough.mp4"), "photo");
        // uKV_ must win over KV_.
        assert_eq!(evidence_type_for_loose_file("uKV_12345.pdf"), "kv_signed");
        assert_eq!(evidence_type_for_loose_file("KV_12345.pdf"), "kv");
        assert_eq!(evidence_type_for_loose_file("RP_98765.pdf"), "qc_report");
        assert_eq!(evidence_type_for_loose_file("qcreport_20260204130619_G61700378"), "qc_report");
        assert_eq!(evidence_type_for_loose_file("mystery.bin"), "unclassified");
    }

    #[test]
    fn relpath_routes_loose_vs_subfolder() {
        // Directly at case root → loose classification.
        assert_eq!(evidence_type_for_relpath(&["KV_1.pdf".to_string()]), "kv");
        // Under a numbered subfolder → subfolder wins, even a KV_-named file.
        assert_eq!(
            evidence_type_for_relpath(&["01_Fotos".to_string(), "KV_1.pdf".to_string()]),
            "photo"
        );
        // Deep nesting inherits the FIRST-level subfolder.
        assert_eq!(
            evidence_type_for_relpath(&["05_QC-Report".to_string(), "sub".to_string(), "r.doc".to_string()]),
            "qc_report"
        );
        assert_eq!(evidence_type_for_relpath(&[]), "unclassified");
    }

    #[test]
    fn mime_handles_extensionless_qcreport_and_media() {
        assert_eq!(mime_for_filename("qcreport_20260204_G617"), ("application/msword", true, false));
        assert_eq!(mime_for_filename("0.jpg"), ("image/jpeg", false, false));
        assert_eq!(mime_for_filename("scan.pdf"), ("application/pdf", false, false));
        assert_eq!(mime_for_filename("walkthrough.mp4").2, true); // is_video
        assert_eq!(mime_for_filename("clip.mov").2, true);
        assert_eq!(mime_for_filename("backup.inb"), ("application/octet-stream", false, false));
    }

    // ── mem-Surreal execution + idempotency ─────────────────────────────────

    /// A unique scratch dir under the OS temp dir (no `tempfile` dep — mirrors
    /// `support::tests::temp_filestore`). Best-effort cleanup by the caller.
    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eck_nas_ingest_{tag}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_fake_tree() -> PathBuf {
        let dir = scratch_dir("tree");
        let year = dir.join("2026");
        // CS-DE case with a numbered subfolder + a loose file.
        let c1 = year.join("CS-DE-2601040_ACME GmbH");
        std::fs::create_dir_all(c1.join("01_Fotos")).unwrap();
        std::fs::write(c1.join("01_Fotos").join("0.jpg"), b"photo-bytes-A").unwrap();
        std::fs::create_dir_all(c1.join("05_QC-Report")).unwrap();
        std::fs::write(c1.join("05_QC-Report").join("qcreport_20260204_G617"), b"qc-doc-bytes").unwrap();
        std::fs::write(c1.join("KV_555.pdf"), b"kv-pdf-bytes").unwrap(); // loose root file
        std::fs::write(c1.join("empty.txt"), b"").unwrap(); // zero-byte → skipped
        // GER2608 current-scheme case: loose files only.
        let c2 = year.join("GER2608-016_CleverFit Kaltenkirchen");
        std::fs::create_dir_all(&c2).unwrap();
        std::fs::write(c2.join("PXL_1.jpg"), b"photo-bytes-B").unwrap();
        std::fs::write(c2.join("clip.mp4"), b"pretend-this-is-a-huge-video").unwrap(); // video → meta only
        dir
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }

    async fn mem_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();
        db
    }

    #[tokio::test]
    async fn dry_run_reports_without_writing() {
        let tree = write_fake_tree();
        let db = mem_db().await;
        let store = scratch_dir("store");
        let report = run_ingest(&db, Some("test-node"), &tree, "2026", true, None, store.to_str().unwrap()).await;

        assert_eq!(report.cases_found, 2);
        assert_eq!(report.cases_by_scheme.get("cs_de"), Some(&1));
        assert_eq!(report.cases_by_scheme.get("ger2608"), Some(&1));
        assert_eq!(report.zero_byte_skipped, 1);
        assert_eq!(report.videos_meta_only, 1);
        // 4 real files counted (photo, qc, kv, PXL); the video is separate but
        // also counts in files_total; zero-byte excluded.
        assert_eq!(report.files_total, 5);
        // photo = 0.jpg + PXL_1.jpg + clip.mp4 (the video is photo-class evidence,
        // counted here even though its blob is skipped).
        assert_eq!(report.files_by_evidence_type.get("photo"), Some(&3));
        assert_eq!(report.files_by_evidence_type.get("qc_report"), Some(&1));
        assert_eq!(report.files_by_evidence_type.get("kv"), Some(&1));
        assert!(report.errors.is_empty(), "dry run errors: {:?}", report.errors);

        // Nothing was written.
        let cases: Vec<Value> = db.query("SELECT * FROM repair_case").await.unwrap().take(0).unwrap();
        assert_eq!(cases.len(), 0, "dry run must not write repair_case rows");
        let frs: Vec<Value> = db.query("SELECT * FROM file_resource").await.unwrap().take(0).unwrap();
        assert_eq!(frs.len(), 0, "dry run must not write file_resource rows");
        cleanup(&tree);
        cleanup(&store);
    }

    #[tokio::test]
    async fn real_run_writes_then_is_idempotent() {
        let tree = write_fake_tree();
        let db = mem_db().await;
        // FileStore writes under `store_base` — a scratch dir, so no repo litter
        // and no global CWD mutation (safe in a parallel test binary).
        let store = scratch_dir("store");
        let sb = store.to_str().unwrap();

        let r1 = run_ingest(&db, Some("test-node"), &tree, "2026", false, None, sb).await;
        assert!(r1.errors.is_empty(), "run1 errors: {:?}", r1.errors);

        let count = |db: &SurrealDb, tbl: &'static str| {
            let db = db.clone();
            async move {
                let rows: Vec<Value> = db.query(format!("SELECT * FROM {tbl}")).await.unwrap().take(0).unwrap();
                rows.len()
            }
        };
        let cases1 = count(&db, "repair_case").await;
        let frs1 = count(&db, "file_resource").await;
        let edges1 = count(&db, "has_evidence").await;
        assert_eq!(cases1, 2, "two repair_case rows");
        // 4 blob files + 1 video meta row = 5 file_resource rows.
        assert_eq!(frs1, 5, "five file_resource rows (4 blobs + 1 video meta)");
        assert_eq!(edges1, 5, "five evidence edges");

        // Second identical run: no new rows (idempotent).
        let r2 = run_ingest(&db, Some("test-node"), &tree, "2026", false, None, sb).await;
        assert!(r2.errors.is_empty(), "run2 errors: {:?}", r2.errors);
        assert_eq!(count(&db, "repair_case").await, cases1, "no duplicate cases");
        assert_eq!(count(&db, "file_resource").await, frs1, "no duplicate file_resource");
        assert_eq!(count(&db, "has_evidence").await, edges1, "no duplicate edges");

        cleanup(&tree);
        cleanup(&store);
    }

    #[tokio::test]
    async fn limit_caps_case_count() {
        let tree = write_fake_tree();
        let db = mem_db().await;
        let store = scratch_dir("store");
        let report = run_ingest(&db, Some("test-node"), &tree, "2026", true, Some(1), store.to_str().unwrap()).await;
        assert_eq!(report.cases_found, 1, "limit=1 ingests only the first case");
        cleanup(&tree);
        cleanup(&store);
    }
}
