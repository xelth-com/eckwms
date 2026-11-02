//! Examination-bundle WRITE path (MCP, Master tier only).
//!
//! Files an EN 62353 / service examination bundle (protocol PDF/HTML, tester
//! payloads, QC reports, photos, manifests) against a DEVICE and optionally a
//! ticket, records the examination as a `device_event`, and allocates protocol
//! numbers (`YYYY-NNNN`) on one authoritative writer node. Contract:
//! `docs/MESH_BUNDLE_TOOLS.md`; design ledger: xelixir
//! `.eck/collab/xelixir-9eck-stk-bundles-2026-10-28/SPEC.md`.
//!
//! Flow: `bundle_begin` (staging + device) → `bundle_upload_chunk` × N (any
//! order, duplicates ignored, sha256-verified on completion) →
//! `protocol_number_allocate` (writer node) → `bundle_commit` (validates, then
//! CAS + `file_resource` + `device_event` + provenance edges in one go).
//!
//! Tables: `device`, `device_event` (mesh-synced, deterministic ids);
//! `bundle_staging`, `protocol_reservation`, `protocol_counter`,
//! `mcp_write_audit` (node-local). Staging bytes live under
//! `<base>/data/bundle_staging/<bundle_id>/` and are GC'd 48 h after begin.
//!
//! Authorization is enforced HERE, at dispatch, for every transport (bearer
//! `/mcp`, signed `/mcp/signed`, relay) — discovery filtering is not
//! authorization. These tools are never part of the orchestrator's
//! `ONESHOT_TOOLS`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use eck_core::db::SurrealDb;
use eck_core::utils::filestore::{content_hash_uuid, sha256_hex};
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use super::McpTier;
use crate::AppState;

/// Staging lifetime: an open bundle not committed within this window is aborted
/// by the GC (boot + hourly) and its tmp bytes removed.
pub(crate) const STAGING_TTL_HOURS: i64 = 48;
/// Max raw bytes per chunk (the client keeps the serialized call < 1 MiB for the
/// relay; 640 KiB raw ≈ 854 KiB base64).
pub(crate) const MAX_CHUNK_RAW: usize = 640 * 1024;
/// Per-file and per-bundle quotas (resource-exhaustion guard).
pub(crate) const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CHUNKS: u64 = 1 + MAX_FILE_BYTES / 1024; // any sane chunking of a max file
/// Env flag that makes THIS node the authoritative protocol-number allocator.
pub(crate) const WRITER_ENV: &str = "ECK_PROTOCOL_WRITER";

pub(crate) const TOOL_NAMES: &[&str] = &[
    "bundle_begin",
    "bundle_upload_chunk",
    "bundle_commit",
    "bundle_abort",
    "protocol_number_allocate",
];

const ROLES: &[&str] = &[
    "protocol_pdf",
    "protocol_html",
    "protocol_pdf_rev",
    "protocol_html_rev",
    "tester_payload",
    "qc_report",
    "photo",
    "manifest",
    "anchor_receipt",
    "other",
];
const KINDS: &[&str] = &["STK", "MTK", "FUNKTIONSKONTROLLE", "REPAIR", "OTHER"];
const RESULTS: &[&str] = &["PASS", "PASS_WITH_NOTES", "FAIL", "NOT_ASSESSED"];
/// Results that establish a due date for the NEXT examination of that kind.
const PASSING: &[&str] = &["PASS", "PASS_WITH_NOTES"];
pub(crate) const DEFAULT_REQUIRED_ROLES: &[&str] =
    &["protocol_pdf", "protocol_html", "tester_payload", "manifest"];

// ─── caller identity / writer flag / lock ──────────────────────────────────

tokio::task_local! {
    /// Verified caller identity of the MCP request being served (cert subject +
    /// key prefix on the signed path, the bearer tier on `/mcp`). Set by the
    /// transport entry points around `handle_jsonrpc`.
    static MCP_CALLER: String;
}

/// Run `fut` with `caller` as the recorded identity for any write it performs.
pub(crate) async fn with_caller<F: Future>(caller: String, fut: F) -> F::Output {
    MCP_CALLER.scope(caller, fut).await
}

pub(crate) fn current_caller() -> String {
    MCP_CALLER
        .try_with(|c| c.clone())
        .unwrap_or_else(|_| "unknown".to_string())
}

pub(crate) fn writer_enabled() -> bool {
    std::env::var(WRITER_ENV).map(|v| v.trim() == "1").unwrap_or(false)
}

/// One process-wide lock serialises every bundle/numbering write: allocation
/// and commit are read-validate-write sequences, and a single node is the only
/// writer, so a mutex is the simplest airtight guard against interleaving.
fn write_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

// ─── errors ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct BErr {
    pub code: &'static str,
    pub msg: String,
}

fn err(code: &'static str, msg: impl Into<String>) -> BErr {
    BErr { code, msg: msg.into() }
}

fn dbe<E: std::fmt::Display>(e: E) -> BErr {
    err("db", e.to_string())
}

fn ioe<E: std::fmt::Display>(e: E) -> BErr {
    err("io", e.to_string())
}

type BResult<T> = Result<T, BErr>;

/// Everything a bundle operation needs, independent of `AppState` (testable on
/// a `mem://` store and a temp directory).
pub(crate) struct BundleEnv<'a> {
    pub db: &'a SurrealDb,
    /// Root under which `data/filestore` (CAS) and `data/bundle_staging` live.
    pub base_dir: String,
    pub caller: String,
    pub writer: bool,
}

// ─── small helpers ─────────────────────────────────────────────────────────

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn req_str<'a>(args: &'a Value, key: &str) -> BResult<&'a str> {
    match args.get(key).and_then(|v| v.as_str()).map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(err("invalid_argument", format!("missing required argument `{key}`"))),
    }
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn validate_bundle_id(s: &str) -> BResult<()> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(err(
            "invalid_argument",
            "bundle_id must be 1-128 chars of [A-Za-z0-9._-], starting alphanumeric",
        ))
    }
}

/// Normalized serial: trimmed, upper-case; conservative charset.
pub(crate) fn normalize_serial(s: &str) -> BResult<String> {
    let n = s.trim().to_uppercase();
    let ok = !n.is_empty()
        && n.len() <= 64
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if ok {
        Ok(n)
    } else {
        Err(err("invalid_argument", format!("invalid serial '{s}'")))
    }
}

fn murmur_key(s: &str) -> String {
    content_hash_uuid(s.as_bytes()).to_string()
}

pub(crate) fn device_key(serial_norm: &str) -> String {
    murmur_key(serial_norm)
}

pub(crate) fn event_key(serial_norm: &str, kind: &str, protocol_no: &str) -> String {
    murmur_key(&format!("{serial_norm}|{kind}|{protocol_no}"))
}

fn edge_key(from: &str, to_cas: &str, bundle_id: &str, role: &str, source_name: &str) -> String {
    murmur_key(&format!("{from}|{to_cas}|{bundle_id}|{role}|{source_name}"))
}

fn file_key(role: &str, name: &str) -> String {
    format!("{role}|{name}")
}

/// `"2026-0005"` → `(2026, 5)`.
pub(crate) fn parse_protocol_no(s: &str) -> Option<(i64, i64)> {
    let (y, n) = s.split_once('-')?;
    if y.len() != 4 || !(4..=6).contains(&n.len()) {
        return None;
    }
    if !y.chars().all(|c| c.is_ascii_digit()) || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (y, n) = (y.parse().ok()?, n.parse().ok()?);
    if n == 0 {
        return None;
    }
    Some((y, n))
}

pub(crate) fn fmt_protocol_no(year: i64, n: i64) -> String {
    format!("{year}-{n:04}")
}

/// Recursively key-sorted JSON — a stable byte form for hashing regardless of
/// serde_json's map ordering.
fn canonical(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let b: BTreeMap<&String, Value> = m.iter().map(|(k, v)| (k, sort(v))).collect();
                let mut out = Map::new();
                for (k, v) in b {
                    out.insert(k.clone(), v);
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    // BTreeMap iteration feeds the map in sorted order; to_string preserves it
    // whether or not serde_json's `preserve_order` is on.
    sort(v).to_string()
}

fn staging_dir(base: &str, bundle_id: &str) -> PathBuf {
    Path::new(base).join("data").join("bundle_staging").join(bundle_id)
}

fn rel_staging_path(bundle_id: &str, sub: &str, file: &str) -> String {
    format!("data/bundle_staging/{bundle_id}/{sub}/{file}")
}

async fn load_row(db: &SurrealDb, table: &str, key: &str) -> BResult<Option<Value>> {
    let rows: Vec<Value> = db
        .query("SELECT * OMIT id FROM type::record($tb, $id)")
        .bind(("tb", table.to_string()))
        .bind(("id", key.to_string()))
        .await
        .map_err(dbe)?
        .take(0)
        .map_err(dbe)?;
    Ok(rows.into_iter().next())
}

async fn save_staging(db: &SurrealDb, bundle_id: &str, doc: &Value) -> BResult<()> {
    db.query("UPSERT type::record('bundle_staging', $id) CONTENT $doc")
        .bind(("id", bundle_id.to_string()))
        .bind(("doc", doc.clone()))
        .await
        .map_err(dbe)?
        .check()
        .map_err(dbe)?;
    Ok(())
}

fn s_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn expired(st: &Value) -> bool {
    chrono::DateTime::parse_from_rfc3339(s_of(st, "expires_at"))
        .map(|t| t.with_timezone(&Utc) < Utc::now())
        .unwrap_or(false)
}

// ─── audit ─────────────────────────────────────────────────────────────────

async fn audit(env: &BundleEnv<'_>, tool: &str, bundle_id: &str, outcome: &str) {
    let r = env
        .db
        .query(
            "CREATE mcp_write_audit SET tool = $tool, bundle_id = $b, caller = $caller, \
             at = $at, outcome = $outcome, node_writer = $w",
        )
        .bind(("tool", tool.to_string()))
        .bind(("b", bundle_id.to_string()))
        .bind(("caller", env.caller.clone()))
        .bind(("at", now_rfc3339()))
        .bind(("outcome", outcome.to_string()))
        .bind(("w", env.writer))
        .await;
    if let Err(e) = r.and_then(|r| r.check()) {
        warn!("[bundles] mcp_write_audit insert failed: {e}");
    }
}

/// Run one bundle tool against `env` and append the audit record. The single
/// entry point for both the MCP wrapper and the tests.
pub(crate) async fn run_tool(env: &BundleEnv<'_>, name: &str, args: &Value) -> BResult<Value> {
    let r = match name {
        "bundle_begin" => bundle_begin(env, args).await,
        "bundle_upload_chunk" => bundle_upload_chunk(env, args).await,
        "bundle_commit" => bundle_commit(env, args).await,
        "bundle_abort" => bundle_abort(env, args).await,
        "protocol_number_allocate" => protocol_number_allocate(env, args).await,
        other => Err(err("unknown_tool", format!("not a bundle tool: {other}"))),
    };
    let outcome = match &r {
        Ok(_) => "ok".to_string(),
        Err(e) => format!("error {}: {}", e.code, e.msg),
    };
    audit(env, name, s_of(args, "bundle_id"), &outcome).await;
    r
}

/// MCP entry: Master-only gate (every transport), then [`run_tool`].
pub(crate) async fn call(state: &Arc<AppState>, name: &str, args: &Value, tier: McpTier) -> Value {
    let env = BundleEnv {
        db: &state.db,
        base_dir: ".".to_string(),
        caller: current_caller(),
        writer: writer_enabled(),
    };
    if tier != McpTier::Master {
        audit(&env, name, s_of(args, "bundle_id"), "error forbidden: master tier required").await;
        return error_value(format!("forbidden: {name} requires the master tier"));
    }
    match run_tool(&env, name, args).await {
        Ok(v) => json!({ "content": [{ "type": "text",
            "text": serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()) }] }),
        Err(e) => error_value(format!("{}: {}", e.code, e.msg)),
    }
}

fn error_value(msg: String) -> Value {
    json!({ "isError": true, "content": [{ "type": "text", "text": msg }] })
}

// ─── device / ticket ───────────────────────────────────────────────────────

/// Ensure the `device` row exists; returns `(device_id, created)`.
async fn ensure_device(
    env: &BundleEnv<'_>,
    serial_raw: &str,
    serial_norm: &str,
    manufacturer: Option<&str>,
    model: Option<String>,
    udi: Option<String>,
) -> BResult<(String, bool)> {
    let key = device_key(serial_norm);
    let id = format!("device:{key}");
    if load_row(env.db, "device", &key).await?.is_some() {
        return Ok((id, false));
    }
    env.db
        .query(
            "CREATE type::record('device', $k) SET serial = $serial, serial_norm = $sn, \
             manufacturer = $man, model = $model, udi = $udi, created_by = 'xelixir', \
             created_via = $caller, created_at = time::now(), updated_at = time::now()",
        )
        .bind(("k", key))
        .bind(("serial", serial_raw.trim().to_string()))
        .bind(("sn", serial_norm.to_string()))
        .bind(("man", manufacturer.map(|s| s.to_string())))
        .bind(("model", model))
        .bind(("udi", udi))
        .bind(("caller", env.caller.clone()))
        .await
        .map_err(dbe)?
        .check()
        .map_err(dbe)?;
    Ok((id, true))
}

/// Resolve a ticket reference to the `document` record KEY (Zoho id).
/// Accepts the public ticket number (digits, < 12 chars — resolved via
/// `meta.ticket_number`, like `list_ticket_attachments`), `document:<zoho_id>`,
/// or a bare Zoho id.
async fn resolve_ticket(db: &SurrealDb, t: &str) -> BResult<String> {
    let t = t.trim();
    let bare = t
        .strip_prefix("document:")
        .unwrap_or(t)
        .trim_matches(|c| c == '`' || c == '\u{27e8}' || c == '\u{27e9}');
    if !t.starts_with("document:") && !bare.is_empty() && bare.len() < 12 && bare.bytes().all(|b| b.is_ascii_digit()) {
        let by_no: Vec<Value> = db
            .query(
                "SELECT VALUE type::string(record::id(id)) FROM document \
                 WHERE type = 'support_ticket' AND <string> (meta.ticket_number ?? '') = $tn \
                 LIMIT 2",
            )
            .bind(("tn", bare.to_string()))
            .await
            .map_err(dbe)?
            .take(0)
            .map_err(dbe)?;
        return match by_no.as_slice() {
            [one] => Ok(one.as_str().unwrap_or_default().to_string()),
            [] => Err(err("ticket_not_found", format!("no ticket with number '{bare}'"))),
            _ => Err(err("ticket_ambiguous", format!("ticket number '{bare}' matches several documents — pass document:<zoho_id>"))),
        };
    }
    if bare.is_empty() {
        return Err(err("ticket_not_found", "empty ticket_id"));
    }
    if load_row(db, "document", bare).await?.is_some() {
        Ok(bare.to_string())
    } else {
        Err(err("ticket_not_found", format!("no ticket document '{t}'")))
    }
}

// ─── bundle_begin ──────────────────────────────────────────────────────────

async fn bundle_begin(env: &BundleEnv<'_>, args: &Value) -> BResult<Value> {
    let bundle_id = req_str(args, "bundle_id")?.to_string();
    validate_bundle_id(&bundle_id)?;
    let serial_raw = req_str(args, "serial")?.to_string();
    let serial_norm = normalize_serial(&serial_raw)?;
    match args.get("schema_version").and_then(|v| v.as_i64()) {
        None | Some(1) => {}
        Some(v) => return Err(err("invalid_argument", format!("unsupported schema_version {v} (expected 1)"))),
    }
    let manifest = match args.get("manifest") {
        Some(m) if m.is_object() => m.clone(),
        _ => return Err(err("invalid_argument", "`manifest` must be a JSON object")),
    };
    let manufacturer = opt_str(args, "manufacturer").filter(|s| !s.is_empty()).or_else(|| {
        let b = crate::ai::branding::tenant_brand();
        if b.is_empty() { None } else { Some(b.to_string()) }
    });
    let model = opt_str(args, "model");
    let udi = opt_str(args, "udi");
    let ticket_arg = args.get("ticket_id").and_then(|v| match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });

    let _g = write_lock().lock().await;
    let ticket_key = match &ticket_arg {
        Some(t) => Some(resolve_ticket(env.db, t).await?),
        None => None,
    };
    let ticket = ticket_key.as_ref().map(|k| format!("document:{k}"));
    let manifest_sha = sha256_hex(canonical(&manifest).as_bytes());
    let existing = load_row(env.db, "bundle_staging", &bundle_id).await?;
    if let Some(st) = &existing {
        match s_of(st, "state") {
            "aborted" => {
                return Err(err("aborted", format!("bundle '{bundle_id}' was aborted — start a new bundle_id")))
            }
            _ if s_of(st, "serial_norm") != serial_norm => {
                return Err(err(
                    "conflict",
                    format!("bundle '{bundle_id}' belongs to serial {}", s_of(st, "serial_norm")),
                ))
            }
            "committed" => {
                // Identical re-begin after commit → the receipt, no new staging.
                let same = st.get("ticket").and_then(|v| v.as_str()) == ticket.as_deref()
                    && s_of(st, "manifest_sha256") == manifest_sha;
                if !same {
                    return Err(err(
                        "conflict",
                        format!("bundle '{bundle_id}' is committed with a different ticket/manifest"),
                    ));
                }
                return Ok(json!({
                    "bundle_id": bundle_id,
                    "device_id": st.get("device_id").cloned().unwrap_or(Value::Null),
                    "device_created": false,
                    "ticket": st.get("ticket").cloned().unwrap_or(Value::Null),
                    "state": "committed",
                    "receipt": st.get("receipt").cloned().unwrap_or(Value::Null),
                }))
            }
            _ => {}
        }
    }

    let (device_id, device_created) =
        ensure_device(env, &serial_raw, &serial_norm, manufacturer.as_deref(), model.clone(), udi.clone()).await?;

    let expires_at = (Utc::now() + chrono::Duration::hours(STAGING_TTL_HOURS))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let (files, opened_at, opened_by) = match &existing {
        Some(st) => (
            st.get("files").cloned().unwrap_or_else(|| json!({})),
            st.get("opened_at").cloned().unwrap_or_else(|| json!(now_rfc3339())),
            st.get("opened_by").cloned().unwrap_or_else(|| json!(env.caller)),
        ),
        None => (json!({}), json!(now_rfc3339()), json!(env.caller)),
    };
    let doc = json!({
        "bundle_id": bundle_id,
        "state": "open",
        "serial": serial_raw,
        "serial_norm": serial_norm,
        "manufacturer": manufacturer,
        "model": model,
        "udi": udi,
        "device_id": device_id,
        "ticket": ticket,
        "ticket_key": ticket_key,
        "manifest": manifest,
        "manifest_sha256": manifest_sha,
        "schema_version": 1,
        "files": files,
        "opened_at": opened_at,
        "opened_by": opened_by,
        "expires_at": expires_at,
    });
    save_staging(env.db, &bundle_id, &doc).await?;
    Ok(json!({
        "bundle_id": bundle_id,
        "device_id": device_id,
        "device_created": device_created,
        "ticket": ticket,
        "expires_at": expires_at,
        "state": "open",
    }))
}

// ─── bundle_upload_chunk ───────────────────────────────────────────────────

fn valid_file_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 255
        && n != "."
        && n != ".."
        && !n.chars().any(|c| c == '/' || c == '\\' || c.is_control())
}

async fn bundle_upload_chunk(env: &BundleEnv<'_>, args: &Value) -> BResult<Value> {
    let bundle_id = req_str(args, "bundle_id")?.to_string();
    validate_bundle_id(&bundle_id)?;
    let role = req_str(args, "role")?.to_string();
    if !ROLES.contains(&role.as_str()) {
        return Err(err("invalid_argument", format!("unknown role '{role}' (one of {})", ROLES.join(", "))));
    }
    let file_name = req_str(args, "file_name")?.to_string();
    if !valid_file_name(&file_name) {
        return Err(err("invalid_argument", "file_name must be a bare file name (no path separators)"));
    }
    let mime = opt_str(args, "mime_type")
        .unwrap_or_else(|| crate::services::support::mime_from_name(&file_name).to_string());
    let size = args
        .get("size_bytes")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| err("invalid_argument", "missing/invalid `size_bytes`"))?;
    if size > MAX_FILE_BYTES {
        return Err(err("too_large", format!("file exceeds {MAX_FILE_BYTES} bytes")));
    }
    let sha = req_str(args, "sha256")?.to_ascii_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(err("invalid_argument", "`sha256` must be 64 hex chars"));
    }
    let chunks_total = args
        .get("chunks_total")
        .and_then(|v| v.as_u64())
        .filter(|n| (1..=MAX_CHUNKS).contains(n))
        .ok_or_else(|| err("invalid_argument", "missing/invalid `chunks_total`"))?;
    if chunks_total.saturating_mul(MAX_CHUNK_RAW as u64) < size {
        return Err(err("invalid_argument", "chunks_total too small for size_bytes at 640 KiB per chunk"));
    }
    let chunk_index = args
        .get("chunk_index")
        .and_then(|v| v.as_u64())
        .filter(|i| *i < chunks_total)
        .ok_or_else(|| err("invalid_argument", "missing/invalid `chunk_index` (0-based, < chunks_total)"))?;
    let content = B64
        .decode(s_of(args, "content_base64").trim())
        .map_err(|e| err("invalid_argument", format!("content_base64: {e}")))?;
    if content.len() > MAX_CHUNK_RAW {
        return Err(err("too_large", format!("chunk exceeds {MAX_CHUNK_RAW} raw bytes")));
    }

    let _g = write_lock().lock().await;
    let mut st = load_row(env.db, "bundle_staging", &bundle_id)
        .await?
        .ok_or_else(|| err("not_found", format!("no staging for bundle '{bundle_id}' — call bundle_begin")))?;
    if s_of(&st, "state") == "committed" {
        let known = st
            .get("files")
            .and_then(|f| f.as_object())
            .is_some_and(|m| m.values().any(|f| s_of(f, "sha256_declared") == sha));
        return if known {
            Ok(json!({
                "file_name": file_name, "role": role, "received": chunks_total,
                "chunks_total": chunks_total, "complete": true, "sha256_ok": true,
                "already_committed": true,
            }))
        } else {
            Err(err("closed", format!("bundle '{bundle_id}' is committed and has no file with sha256 {sha}")))
        };
    }
    if s_of(&st, "state") != "open" {
        return Err(err("closed", format!("bundle '{bundle_id}' is {}", s_of(&st, "state"))));
    }
    if expired(&st) {
        return Err(err("expired", format!("bundle '{bundle_id}' staging expired")));
    }

    let fk = file_key(&role, &file_name);
    let sub = sha256_hex(fk.as_bytes())[..24].to_string();
    let files = st
        .get_mut("files")
        .and_then(|f| f.as_object_mut())
        .ok_or_else(|| err("db", "staging row has no files map"))?;

    if let Some(e) = files.get(&fk) {
        let same = e.get("size_declared").and_then(|v| v.as_u64()) == Some(size)
            && s_of(e, "sha256_declared") == sha
            && e.get("chunks_total").and_then(|v| v.as_u64()) == Some(chunks_total);
        if !same {
            return Err(err(
                "conflict",
                format!("file '{file_name}' ({role}) was declared with a different size/sha256/chunks_total"),
            ));
        }
        if e.get("complete").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(json!({
                "file_name": file_name, "role": role, "received": chunks_total,
                "chunks_total": chunks_total, "complete": true, "sha256_ok": true, "duplicate": true,
            }));
        }
    } else {
        let declared: u64 = files
            .values()
            .filter_map(|e| e.get("size_declared").and_then(|v| v.as_u64()))
            .sum();
        if declared + size > MAX_BUNDLE_BYTES {
            return Err(err("too_large", format!("bundle exceeds {MAX_BUNDLE_BYTES} bytes")));
        }
        files.insert(
            fk.clone(),
            json!({
                "role": role, "file_name": file_name, "mime_type": mime,
                "size_declared": size, "sha256_declared": sha, "chunks_total": chunks_total,
                "chunks_received": [], "complete": false, "sha256_ok": false,
                "tmp_path": Value::Null,
            }),
        );
    }
    let entry = files.get_mut(&fk).expect("inserted above");

    let dir = staging_dir(&env.base_dir, &bundle_id).join(&sub);
    let mut received: BTreeSet<u64> = entry
        .get("chunks_received")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
        .unwrap_or_default();
    let duplicate = received.contains(&chunk_index);
    if !duplicate {
        tokio::fs::create_dir_all(&dir).await.map_err(ioe)?;
        let part = dir.join(format!("{chunk_index}.part"));
        let tmp = dir.join(format!("{chunk_index}.part.tmp"));
        tokio::fs::write(&tmp, &content).await.map_err(ioe)?;
        tokio::fs::rename(&tmp, &part).await.map_err(ioe)?;
        received.insert(chunk_index);
    }

    let mut complete = false;
    let mut sha_ok = false;
    if received.len() as u64 == chunks_total {
        let mut all = Vec::with_capacity(size as usize);
        for i in 0..chunks_total {
            let p = dir.join(format!("{i}.part"));
            all.extend_from_slice(&tokio::fs::read(&p).await.map_err(ioe)?);
        }
        let ok = all.len() as u64 == size && sha256_hex(&all) == sha;
        for i in 0..chunks_total {
            let _ = tokio::fs::remove_file(dir.join(format!("{i}.part"))).await;
        }
        if !ok {
            // Drop the file entry so the client can re-declare and re-upload it.
            let actual = sha256_hex(&all);
            let actual_len = all.len();
            if let Some(m) = st.get_mut("files").and_then(|f| f.as_object_mut()) {
                m.remove(&fk);
            }
            save_staging(env.db, &bundle_id, &st).await?;
            return Err(err(
                "sha256_mismatch",
                format!(
                    "'{file_name}': assembled {actual_len} bytes sha256 {actual} != declared {size} bytes {sha}; \
                     file dropped, re-upload all chunks"
                ),
            ));
        }
        tokio::fs::write(dir.join("assembled.bin"), &all).await.map_err(ioe)?;
        entry["tmp_path"] = json!(rel_staging_path(&bundle_id, &sub, "assembled.bin"));
        complete = true;
        sha_ok = true;
    }
    entry["chunks_received"] = json!(received.iter().collect::<Vec<_>>());
    entry["complete"] = json!(complete);
    entry["sha256_ok"] = json!(sha_ok);
    let n_received = received.len();
    save_staging(env.db, &bundle_id, &st).await?;
    Ok(json!({
        "file_name": file_name,
        "role": role,
        "received": n_received,
        "chunks_total": chunks_total,
        "complete": complete,
        "sha256_ok": sha_ok,
        "duplicate": duplicate,
    }))
}

// ─── bundle_commit ─────────────────────────────────────────────────────────

fn valid_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() && s.len() == 10
}

fn valid_month(s: &str) -> bool {
    s.len() == 7 && chrono::NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d").is_ok()
}

fn obj_or_null(v: Option<&Value>, key: &str) -> BResult<Value> {
    match v {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(o @ Value::Object(_)) => Ok(o.clone()),
        _ => Err(err("invalid_argument", format!("event.{key} must be an object or null"))),
    }
}

/// Validate + normalize the commit's event payload (the part a retry must
/// repeat byte-for-byte to be "identical").
fn normalize_event(ev: &Value) -> BResult<Value> {
    if !ev.is_object() {
        return Err(err("invalid_argument", "`event` must be an object"));
    }
    let kind = req_str(ev, "kind")?.to_ascii_uppercase();
    if !KINDS.contains(&kind.as_str()) {
        return Err(err("invalid_argument", format!("event.kind must be one of {}", KINDS.join(", "))));
    }
    let date = req_str(ev, "date")?.to_string();
    if !valid_date(&date) {
        return Err(err("invalid_argument", "event.date must be YYYY-MM-DD"));
    }
    let protocol_no = req_str(ev, "protocol_no")?.to_string();
    if parse_protocol_no(&protocol_no).is_none() {
        return Err(err("invalid_argument", "event.protocol_no must be YYYY-NNNN"));
    }
    let revision = match ev.get("revision") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_i64()
            .filter(|r| *r >= 0)
            .ok_or_else(|| err("invalid_argument", "event.revision must be a non-negative integer"))?,
    };
    let result = req_str(ev, "result")?.to_ascii_uppercase();
    if !RESULTS.contains(&result.as_str()) {
        return Err(err("invalid_argument", format!("event.result must be one of {}", RESULTS.join(", "))));
    }
    let next_due = match ev.get("next_due") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) if s.trim().is_empty() => Value::Null,
        Some(Value::String(s)) if valid_month(s.trim()) => json!(s.trim()),
        _ => return Err(err("invalid_argument", "event.next_due must be YYYY-MM or null")),
    };
    let examiner = req_str(ev, "examiner")?.to_string();
    Ok(json!({
        "kind": kind,
        "date": date,
        "protocol_no": protocol_no,
        "revision": revision,
        "result": result,
        "next_due": next_due,
        "examiner": examiner,
        "instrument": obj_or_null(ev.get("instrument"), "instrument")?,
        "standard": opt_str(ev, "standard"),
        "legal_basis": opt_str(ev, "legal_basis"),
        "scope": opt_str(ev, "scope"),
        "operator_snapshot": obj_or_null(ev.get("operator_snapshot"), "operator_snapshot")?,
        "notes": opt_str(ev, "notes"),
        "supersedes_protocol_no": opt_str(ev, "supersedes_protocol_no"),
    }))
}

async fn bundle_commit(env: &BundleEnv<'_>, args: &Value) -> BResult<Value> {
    let bundle_id = req_str(args, "bundle_id")?.to_string();
    validate_bundle_id(&bundle_id)?;
    let ev = normalize_event(args.get("event").unwrap_or(&Value::Null))?;
    let required: Vec<String> = match args.get("required_roles") {
        None | Some(Value::Null) => DEFAULT_REQUIRED_ROLES.iter().map(|s| s.to_string()).collect(),
        Some(Value::Array(a)) => {
            let mut v = Vec::new();
            for r in a {
                let r = r.as_str().unwrap_or("");
                if !ROLES.contains(&r) {
                    return Err(err("invalid_argument", format!("required_roles: unknown role '{r}'")));
                }
                v.push(r.to_string());
            }
            v
        }
        _ => return Err(err("invalid_argument", "required_roles must be an array of roles")),
    };
    let event_sha = sha256_hex(canonical(&ev).as_bytes());
    let protocol_no = s_of(&ev, "protocol_no").to_string();
    let kind = s_of(&ev, "kind").to_string();
    let (year, n) = parse_protocol_no(&protocol_no).expect("validated");

    let _g = write_lock().lock().await;
    let st = load_row(env.db, "bundle_staging", &bundle_id)
        .await?
        .ok_or_else(|| err("not_found", format!("no staging for bundle '{bundle_id}'")))?;
    match s_of(&st, "state") {
        "committed" => {
            return if s_of(&st, "event_sha256") == event_sha {
                Ok(st.get("receipt").cloned().unwrap_or(Value::Null))
            } else {
                Err(err(
                    "conflict",
                    format!("bundle '{bundle_id}' was already committed with a different event payload"),
                ))
            };
        }
        "open" => {}
        other => return Err(err("closed", format!("bundle '{bundle_id}' is {other}"))),
    }
    if expired(&st) {
        return Err(err("expired", format!("bundle '{bundle_id}' staging expired")));
    }
    let serial_norm = s_of(&st, "serial_norm").to_string();

    // Protocol number: held by THIS bundle (reserved), or a seeded legacy number
    // not yet bound to an event (migration of 2026-0001…0004).
    let holders: Vec<Value> = env
        .db
        .query("SELECT VALUE bundle_id FROM device_event WHERE protocol_no = $p")
        .bind(("p", protocol_no.clone()))
        .await
        .map_err(dbe)?
        .take(0)
        .map_err(dbe)?;
    if !holders.is_empty() {
        return Err(err(
            "protocol_no_taken",
            format!("protocol {protocol_no} is already recorded by bundle {}", holders[0]),
        ));
    }
    let res = load_row(env.db, "protocol_reservation", &protocol_no).await?;
    let legacy_adopt = res.as_ref().is_some_and(|r| s_of(r, "state") == "legacy");
    match &res {
        None => {
            return Err(err(
                "protocol_not_reserved",
                format!("protocol {protocol_no} is not reserved on this node — call protocol_number_allocate first"),
            ))
        }
        Some(r) => {
            let holder = s_of(r, "bundle_id");
            let state = s_of(r, "state");
            let mine = holder == bundle_id && (state == "reserved" || state == "committed");
            // Seeded legacy number (2026-0001…0004): the FIRST commit presenting
            // it adopts it (rebinds bundle_id, state → committed).
            let legacy = state == "legacy";
            if !(mine || legacy) {
                return Err(err(
                    "protocol_no_taken",
                    format!("protocol {protocol_no} is reserved by bundle {holder} (state {state})"),
                ));
            }
        }
    }

    // Files: all complete and verified; required roles present.
    let files: Vec<Value> = st
        .get("files")
        .and_then(|f| f.as_object())
        .map(|m| {
            let mut v: Vec<(String, Value)> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v.into_iter().map(|(_, v)| v).collect()
        })
        .unwrap_or_default();
    if files.is_empty() {
        return Err(err("incomplete", "bundle has no files"));
    }
    for f in &files {
        if f.get("complete").and_then(|v| v.as_bool()) != Some(true)
            || f.get("sha256_ok").and_then(|v| v.as_bool()) != Some(true)
        {
            return Err(err(
                "incomplete",
                format!("file '{}' ({}) is not completely uploaded", s_of(f, "file_name"), s_of(f, "role")),
            ));
        }
    }
    for r in &required {
        if !files.iter().any(|f| s_of(f, "role") == r) {
            return Err(err("missing_role", format!("required role '{r}' has no file")));
        }
    }

    // Manifest consistency: same protocol/serial when stated, and the same
    // sha256 for every uploaded file it names.
    let manifest = st.get("manifest").cloned().unwrap_or_else(|| json!({}));
    if let Some(p) = manifest.get("protocol_no").and_then(|v| v.as_str()) {
        if p != protocol_no {
            return Err(err("manifest_mismatch", format!("manifest protocol_no {p} != event {protocol_no}")));
        }
    }
    if let Some(d) = manifest.get("dut").and_then(|v| v.as_str()) {
        if !d.trim().eq_ignore_ascii_case(&serial_norm) {
            return Err(err("manifest_mismatch", format!("manifest dut {d} != serial {serial_norm}")));
        }
    }
    // Coverage is by sha256, not by name (amendment A2): a delivered manifest
    // may name `X.html` while the archived file is `X_rev0.html`; files outside
    // the manifest (photos, rev1) pass on their own verified sha256. Manifest
    // entries with no uploaded file are reported in the receipt.
    let uploaded: BTreeSet<String> = files
        .iter()
        .map(|f| s_of(f, "sha256_declared").to_ascii_lowercase())
        .collect();
    let mut manifest_missing: Vec<String> = Vec::new();
    let mut manifest_covered = 0usize;
    if let Some(mf) = manifest.get("files").and_then(|v| v.as_object()) {
        for (name, h) in mf {
            match h.as_str() {
                Some(h) if uploaded.contains(&h.to_ascii_lowercase()) => manifest_covered += 1,
                _ => manifest_missing.push(name.clone()),
            }
        }
    }
    manifest_missing.sort();

    // Supersedes link.
    let supersedes_key = match ev.get("supersedes_protocol_no").and_then(|v| v.as_str()) {
        Some(p) => {
            let k = event_key(&serial_norm, &kind, p);
            if load_row(env.db, "device_event", &k).await?.is_none() {
                return Err(err(
                    "supersedes_not_found",
                    format!("no {kind} event {p} for serial {serial_norm} to supersede"),
                ));
            }
            Some(k)
        }
        None => None,
    };

    // Read + re-verify the staged bytes, then land them in the CAS.
    let mut blobs: Vec<(Value, Vec<u8>)> = Vec::with_capacity(files.len());
    for f in &files {
        let tmp = s_of(f, "tmp_path");
        let bytes = tokio::fs::read(Path::new(&env.base_dir).join(tmp)).await.map_err(ioe)?;
        if sha256_hex(&bytes) != s_of(f, "sha256_declared") {
            return Err(err("sha256_mismatch", format!("staged '{}' no longer matches its sha256", s_of(f, "file_name"))));
        }
        blobs.push((f.clone(), bytes));
    }
    let manifest_sha = blobs
        .iter()
        .filter(|(f, _)| s_of(f, "role") == "manifest")
        .find(|(_, b)| serde_json::from_slice::<Value>(b).ok().as_ref() == Some(&manifest))
        .map(|(f, _)| s_of(f, "sha256_declared").to_string())
        .unwrap_or_else(|| sha256_hex(canonical(&manifest).as_bytes()));

    let ekey = event_key(&serial_norm, &kind, &protocol_no);
    let event_id = format!("device_event:{ekey}");
    let device_id = s_of(&st, "device_id").to_string();
    let ticket = st.get("ticket").and_then(|v| v.as_str()).map(str::to_string);
    let ticket_key = st.get("ticket_key").and_then(|v| v.as_str()).map(str::to_string);
    let now = now_rfc3339();

    let mut out_files = Vec::new();
    let mut edges: Vec<(String, (&str, String), Value)> = Vec::new(); // (edge key, from (table,key), file)
    for (f, bytes) in &blobs {
        let name = s_of(f, "file_name");
        let role = s_of(f, "role");
        let imported = crate::services::support::import_bytes(
            env.db,
            &env.base_dir,
            bytes,
            name,
            s_of(f, "mime_type"),
            &format!("bundle:{role}"),
        )
        .await
        .map_err(|e| err("cas", e.to_string()))?;
        let cas = imported.saved.cas_uuid.to_string();
        out_files.push(json!({
            "role": role, "name": name, "cas_uuid": cas,
            "sha256": imported.saved.sha256, "deduped": imported.deduped,
        }));
        let mut froms = vec![("device_event", ekey.clone())];
        if let Some(t) = &ticket_key {
            froms.push(("document", t.clone()));
        }
        for (tb, key) in froms {
            let k = edge_key(&format!("{tb}:{key}"), &cas, &bundle_id, role, name);
            edges.push((
                k,
                (tb, key),
                json!({ "cas": cas, "role": role, "name": name }),
            ));
        }
    }

    let mut receipt = json!({
        "event_id": event_id,
        "device_id": device_id,
        "protocol_no": protocol_no,
        "bundle_id": bundle_id,
        "files": out_files,
        "manifest_covered": manifest_covered,
        "manifest_missing": manifest_missing,
    });
    let receipt_sha = sha256_hex(canonical(&receipt).as_bytes());
    receipt["receipt_sha256"] = json!(receipt_sha);

    let mut event_doc = ev.clone();
    let o = event_doc.as_object_mut().expect("object");
    o.remove("supersedes_protocol_no");
    o.insert("event_id".into(), json!(event_id));
    o.insert("device_id".into(), json!(device_id));
    o.insert("serial_norm".into(), json!(serial_norm));
    o.insert("year".into(), json!(year));
    o.insert("n".into(), json!(n));
    o.insert("ticket_id".into(), json!(ticket));
    o.insert("bundle_id".into(), json!(bundle_id));
    o.insert("manifest_sha256".into(), json!(manifest_sha));
    o.insert("schema_version".into(), json!(1));
    o.insert(
        "supersedes_id".into(),
        json!(supersedes_key.as_ref().map(|k| format!("device_event:{k}"))),
    );
    o.insert("supersedes_protocol_no".into(), ev.get("supersedes_protocol_no").cloned().unwrap_or(Value::Null));
    o.insert("recorded_at".into(), json!(now));
    o.insert("recorded_by".into(), json!(env.caller));
    o.insert("event_sha256".into(), json!(event_sha));
    o.insert("receipt_sha256".into(), json!(receipt_sha));

    // One transaction: event + links, provenance edges, reservation, staging.
    let mut q = String::from(
        "BEGIN TRANSACTION; \
         CREATE type::record('device_event', $ek) CONTENT $ev; \
         UPDATE type::record('device_event', $ek) SET device = type::record('device', $dk), \
             created_at = time::now(), updated_at = time::now(); ",
    );
    if ticket.is_some() {
        q.push_str("UPDATE type::record('device_event', $ek) SET ticket = type::record('document', $tkey); ");
    }
    if supersedes_key.is_some() {
        q.push_str("UPDATE type::record('device_event', $ek) SET supersedes = type::record('device_event', $sk); ");
    }
    for (i, _) in edges.iter().enumerate() {
        q.push_str(&format!(
            "LET $f{i} = (SELECT VALUE id FROM file_resource WHERE cas_uuid = $e{i}.cas LIMIT 1)[0]; \
             IF array::len((SELECT VALUE id FROM type::record('has_attachment', $k{i}))) == 0 {{ \
                 INSERT RELATION INTO has_attachment {{ id: $k{i}, in: type::record($ft{i}, $fk{i}), out: $f{i}, \
                     label: $e{i}.role, role: $e{i}.role, bundle_id: $bid, source_name: $e{i}.name, \
                     uploaded_by: $caller, uploaded_at: $now, created_at: time::now() }}; \
             }}; "
        ));
    }
    q.push_str(
        "UPDATE type::record('protocol_reservation', $pn) SET state = 'committed', bundle_id = $bid, \
             committed_at = $now, committed_bundle_id = $bid, event_id = $evid, \
             legacy_adopted = $legacy OR (legacy_adopted ?? false); \
         UPDATE type::record('bundle_staging', $bid) SET state = 'committed', committed_at = $now, \
             event_sha256 = $esha, event_id = $evid, receipt = $receipt; \
         COMMIT TRANSACTION;",
    );
    let mut call = env
        .db
        .query(q)
        .bind(("ek", ekey.clone()))
        .bind(("ev", event_doc))
        .bind(("dk", device_key(&serial_norm)))
        .bind(("tkey", ticket_key.clone()))
        .bind(("legacy", legacy_adopt))
        .bind(("sk", supersedes_key.clone()))
        .bind(("pn", protocol_no.clone()))
        .bind(("bid", bundle_id.clone()))
        .bind(("evid", event_id.clone()))
        .bind(("esha", event_sha.clone()))
        .bind(("receipt", receipt.clone()))
        .bind(("caller", env.caller.clone()))
        .bind(("now", now.clone()));
    for (i, (k, (tb, key), e)) in edges.into_iter().enumerate() {
        call = call
            .bind((format!("k{i}"), k))
            .bind((format!("ft{i}"), tb.to_string()))
            .bind((format!("fk{i}"), key))
            .bind((format!("e{i}"), e));
    }
    call.await.map_err(dbe)?.check().map_err(dbe)?;

    let _ = tokio::fs::remove_dir_all(staging_dir(&env.base_dir, &bundle_id)).await;
    if legacy_adopt {
        audit(env, "bundle_commit", &bundle_id, &format!("legacy_adopted {protocol_no}")).await;
    }
    info!("[bundles] committed {bundle_id}: {event_id} protocol {protocol_no} ({} files)", blobs.len());
    Ok(receipt)
}

// ─── bundle_abort ──────────────────────────────────────────────────────────

async fn bundle_abort(env: &BundleEnv<'_>, args: &Value) -> BResult<Value> {
    let bundle_id = req_str(args, "bundle_id")?.to_string();
    validate_bundle_id(&bundle_id)?;
    let _g = write_lock().lock().await;
    let st = load_row(env.db, "bundle_staging", &bundle_id)
        .await?
        .ok_or_else(|| err("not_found", format!("no staging for bundle '{bundle_id}'")))?;
    match s_of(&st, "state") {
        "committed" => return Err(err("closed", format!("bundle '{bundle_id}' is already committed"))),
        "aborted" => {}
        _ => {
            env.db
                .query(
                    "UPDATE type::record('bundle_staging', $b) SET state = 'aborted', \
                     aborted_at = $now, abort_reason = 'client'",
                )
                .bind(("b", bundle_id.clone()))
                .bind(("now", now_rfc3339()))
                .await
                .map_err(dbe)?
                .check()
                .map_err(dbe)?;
        }
    }
    let _ = tokio::fs::remove_dir_all(staging_dir(&env.base_dir, &bundle_id)).await;
    // A reservation held by this bundle stays `reserved` forever (never recycled).
    Ok(json!({ "bundle_id": bundle_id, "state": "aborted" }))
}

// ─── protocol numbers ──────────────────────────────────────────────────────

async fn protocol_number_allocate(env: &BundleEnv<'_>, args: &Value) -> BResult<Value> {
    if !env.writer {
        return Err(err(
            "not_writer",
            format!("this node is not the authoritative protocol-number writer ({WRITER_ENV}=1 is unset)"),
        ));
    }
    let year = args
        .get("year")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
        .filter(|y| (2000..=2999).contains(y))
        .ok_or_else(|| err("invalid_argument", "missing/invalid `year`"))?;
    let bundle_id = req_str(args, "bundle_id")?.to_string();
    validate_bundle_id(&bundle_id)?;
    let kind = match opt_str(args, "kind") {
        Some(k) => {
            let k = k.to_ascii_uppercase();
            if !KINDS.contains(&k.as_str()) {
                return Err(err("invalid_argument", format!("kind must be one of {}", KINDS.join(", "))));
            }
            Some(k)
        }
        None => None,
    };

    let _g = write_lock().lock().await;
    let existing: Vec<Value> = env
        .db
        .query("SELECT * OMIT id FROM protocol_reservation WHERE bundle_id = $b AND year = $y LIMIT 1")
        .bind(("b", bundle_id.clone()))
        .bind(("y", year))
        .await
        .map_err(dbe)?
        .take(0)
        .map_err(dbe)?;
    if let Some(r) = existing.first() {
        return Ok(json!({
            "number": r.get("number").cloned().unwrap_or(Value::Null),
            "reserved_at": r.get("reserved_at").cloned().unwrap_or(Value::Null),
            "reused": true,
            "state": r.get("state").cloned().unwrap_or(Value::Null),
        }));
    }

    let floor = number_floor(env.db, year).await?;
    let n = floor + 1;
    let number = fmt_protocol_no(year, n);
    let reserved_at = now_rfc3339();
    env.db
        .query(
            "BEGIN TRANSACTION; \
             UPSERT type::record('protocol_counter', $ys) SET year = $y, \
                 n = math::max([(n ?? 0) + 1, $n]), updated_at = time::now(); \
             CREATE type::record('protocol_reservation', $num) SET year = $y, n = $n, number = $num, \
                 bundle_id = $b, kind = $kind, reserved_at = $at, reserved_by = $caller, state = 'reserved'; \
             COMMIT TRANSACTION;",
        )
        .bind(("ys", year.to_string()))
        .bind(("y", year))
        .bind(("n", n))
        .bind(("num", number.clone()))
        .bind(("b", bundle_id.clone()))
        .bind(("kind", kind))
        .bind(("at", reserved_at.clone()))
        .bind(("caller", env.caller.clone()))
        .await
        .map_err(dbe)?
        .check()
        .map_err(dbe)?;
    info!("[bundles] allocated protocol {number} for bundle {bundle_id}");
    Ok(json!({ "number": number, "reserved_at": reserved_at, "reused": false, "state": "reserved" }))
}

/// The highest number ever issued for `year`: max of the counter, every
/// reservation (incl. abandoned ones) and every recorded event — so a lost
/// counter can never cause a number to be re-issued.
async fn number_floor(db: &SurrealDb, year: i64) -> BResult<i64> {
    let mut resp = db
        .query(
            "SELECT VALUE n FROM type::record('protocol_counter', $ys); \
             SELECT VALUE n FROM protocol_reservation WHERE year = $y; \
             SELECT VALUE protocol_no FROM device_event WHERE string::starts_with(protocol_no ?? '', $prefix);",
        )
        .bind(("ys", year.to_string()))
        .bind(("y", year))
        .bind(("prefix", format!("{year}-")))
        .await
        .map_err(dbe)?;
    let counter: Vec<Value> = resp.take(0).map_err(dbe)?;
    let res: Vec<Value> = resp.take(1).map_err(dbe)?;
    let evs: Vec<Value> = resp.take(2).map_err(dbe)?;
    let mut floor = 0i64;
    for v in counter.iter().chain(res.iter()) {
        floor = floor.max(v.as_i64().unwrap_or(0));
    }
    for v in &evs {
        if let Some((y, n)) = v.as_str().and_then(parse_protocol_no) {
            if y == year {
                floor = floor.max(n);
            }
        }
    }
    Ok(floor)
}

/// Register the protocols issued before this tool existed (2026-0001…0004) as
/// `legacy` reservations so the first allocation yields 2026-0005; the first
/// `bundle_commit` presenting one adopts it. Idempotent;
/// runs at boot on the writer node only.
pub(crate) async fn seed_legacy(db: &SurrealDb) -> BResult<usize> {
    const YEAR: i64 = 2026;
    const LAST: i64 = 4;
    let _g = write_lock().lock().await;
    let mut created = 0;
    for n in 1..=LAST {
        let number = fmt_protocol_no(YEAR, n);
        if load_row(db, "protocol_reservation", &number).await?.is_some() {
            continue;
        }
        db.query(
            "CREATE type::record('protocol_reservation', $num) SET year = $y, n = $n, number = $num, \
             bundle_id = $b, kind = NONE, reserved_at = $at, reserved_by = 'seed', state = 'legacy', \
             seeded = true",
        )
        .bind(("num", number.clone()))
        .bind(("y", YEAR))
        .bind(("n", n))
        .bind(("b", format!("legacy-{number}")))
        .bind(("at", now_rfc3339()))
        .await
        .map_err(dbe)?
        .check()
        .map_err(dbe)?;
        created += 1;
    }
    db.query(
        "UPSERT type::record('protocol_counter', $ys) SET year = $y, \
         n = math::max([n ?? 0, $last]), updated_at = time::now()",
    )
    .bind(("ys", YEAR.to_string()))
    .bind(("y", YEAR))
    .bind(("last", LAST))
    .await
    .map_err(dbe)?
    .check()
    .map_err(dbe)?;
    Ok(created)
}

// ─── staging GC ────────────────────────────────────────────────────────────

/// Abort every OPEN staging past its `expires_at` and delete its tmp bytes.
pub(crate) async fn gc_expired(db: &SurrealDb, base_dir: &str) -> BResult<usize> {
    let _g = write_lock().lock().await;
    let rows: Vec<Value> = db
        .query("SELECT bundle_id, expires_at FROM bundle_staging WHERE state = 'open'")
        .await
        .map_err(dbe)?
        .take(0)
        .map_err(dbe)?;
    let mut n = 0;
    for r in rows.iter().filter(|r| expired(r)) {
        let b = s_of(r, "bundle_id").to_string();
        if validate_bundle_id(&b).is_err() {
            continue;
        }
        db.query(
            "UPDATE type::record('bundle_staging', $b) SET state = 'aborted', \
             aborted_at = $now, abort_reason = 'expired'",
        )
        .bind(("b", b.clone()))
        .bind(("now", now_rfc3339()))
        .await
        .map_err(dbe)?
        .check()
        .map_err(dbe)?;
        let _ = tokio::fs::remove_dir_all(staging_dir(base_dir, &b)).await;
        n += 1;
    }
    Ok(n)
}

/// Boot hook: seed legacy numbers (writer only), then GC now and every hour.
pub(crate) fn spawn_maintenance(db: SurrealDb) {
    tokio::spawn(async move {
        if writer_enabled() {
            match seed_legacy(&db).await {
                Ok(c) => info!("[bundles] protocol writer node; legacy seed created {c} reservation(s)"),
                Err(e) => warn!("[bundles] legacy seed failed: {}: {}", e.code, e.msg),
            }
        }
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match gc_expired(&db, ".").await {
                Ok(0) => {}
                Ok(n) => info!("[bundles] staging GC aborted {n} expired bundle(s)"),
                Err(e) => warn!("[bundles] staging GC failed: {}: {}", e.code, e.msg),
            }
        }
    });
}

// ─── device_history extension ──────────────────────────────────────────────

/// `{device, events[], next_due}` for one serial (read side of the bundle
/// path). PII in `operator_snapshot` is returned RAW — the MCP caller
/// tokenizes it.
pub(crate) async fn device_history_ext(db: &SurrealDb, serial: &str) -> BResult<Value> {
    let serial_norm = normalize_serial(serial).unwrap_or_else(|_| serial.trim().to_uppercase());
    let key = device_key(&serial_norm);
    let device = load_row(db, "device", &key).await?.map(|mut d| {
        d["device_id"] = json!(format!("device:{key}"));
        d
    });
    let mut events: Vec<Value> = db
        .query("SELECT * OMIT id, device, ticket, supersedes FROM device_event WHERE serial_norm = $s")
        .bind(("s", serial_norm.clone()))
        .await
        .map_err(dbe)?
        .take(0)
        .map_err(dbe)?;
    for e in events.iter_mut() {
        let ek = s_of(e, "event_id").trim_start_matches("device_event:").to_string();
        let mut atts: Vec<Value> = db
            .query(
                "SELECT role, source_name AS name, out.cas_uuid AS cas_uuid, out.size_bytes AS size \
                 FROM has_attachment WHERE in = type::record('device_event', $k)",
            )
            .bind(("k", ek))
            .await
            .map_err(dbe)?
            .take(0)
            .map_err(dbe)?;
        atts.sort_by(|a, b| (s_of(a, "role"), s_of(a, "name")).cmp(&(s_of(b, "role"), s_of(b, "name"))));
        e["attachments"] = json!(atts);
    }
    let rev = |e: &Value| e.get("revision").and_then(|v| v.as_i64()).unwrap_or(0);
    events.sort_by(|a, b| {
        (s_of(b, "date"), rev(b), s_of(b, "protocol_no")).cmp(&(s_of(a, "date"), rev(a), s_of(a, "protocol_no")))
    });
    let next_due = next_due_by_kind(&events);
    Ok(json!({ "device": device, "events": events, "next_due": next_due }))
}

/// kind → `next_due` of the latest non-superseded passing event of that kind
/// (events must be sorted newest first). A REPAIR never moves an STK date.
pub(crate) fn next_due_by_kind(events_desc: &[Value]) -> Value {
    let superseded: BTreeSet<&str> = events_desc
        .iter()
        .filter_map(|e| e.get("supersedes_id").and_then(|v| v.as_str()))
        .collect();
    let mut out = Map::new();
    for e in events_desc {
        let kind = s_of(e, "kind");
        if out.contains_key(kind)
            || superseded.contains(s_of(e, "event_id"))
            || !PASSING.contains(&s_of(e, "result"))
        {
            continue;
        }
        out.insert(kind.to_string(), e.get("next_due").cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

// ─── tool schemas ──────────────────────────────────────────────────────────

pub(crate) fn tool_schemas() -> Vec<Value> {
    let roles = json!(ROLES);
    vec![
        json!({
            "name": "bundle_begin",
            "description": "MASTER ONLY. Open (or refresh, idempotent while open) the staging area for an \
                examination bundle (EN 62353 STK/MTK/Funktionskontrolle/repair evidence) of one device. \
                Creates the `device` record for the serial if missing. Rejects an unknown ticket_id. \
                Staging expires 48 h after the last begin. See docs/MESH_BUNDLE_TOOLS.md.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string", "description": "Client idempotency key, [A-Za-z0-9._-]{1,128}." },
                    "serial": { "type": "string", "description": "Device serial number." },
                    "manufacturer": { "type": "string", "description": "Default: the node's ECK_TENANT_BRAND." },
                    "model": { "type": "string", "description": "e.g. 'Acme 770'." },
                    "udi": { "type": "string" },
                    "ticket_id": { "type": "string", "description": "Ticket document id (Zoho id) or ticket number." },
                    "manifest": { "type": "object", "description": "The bundle manifest JSON ({files:{name:sha256}, protocol_no, dut, …})." },
                    "schema_version": { "type": "integer", "description": "Must be 1." }
                },
                "required": ["bundle_id", "serial", "manifest"]
            }
        }),
        json!({
            "name": "bundle_upload_chunk",
            "description": "MASTER ONLY. Upload one chunk (≤ 640 KiB raw, base64) of one bundle file. \
                Chunks may arrive in any order; a repeated chunk is ignored. When all chunks are present the \
                node verifies size_bytes + sha256 and marks the file complete (a mismatch drops the file so it can be re-declared).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string" },
                    "role": { "type": "string", "enum": roles },
                    "file_name": { "type": "string" },
                    "mime_type": { "type": "string" },
                    "size_bytes": { "type": "integer" },
                    "sha256": { "type": "string", "description": "sha256 hex of the WHOLE file." },
                    "chunks_total": { "type": "integer" },
                    "chunk_index": { "type": "integer", "description": "0-based." },
                    "content_base64": { "type": "string" }
                },
                "required": ["bundle_id", "role", "file_name", "size_bytes", "sha256", "chunks_total", "chunk_index", "content_base64"]
            }
        }),
        json!({
            "name": "bundle_commit",
            "description": "MASTER ONLY. Validate the staged bundle (all files complete + sha256-verified, manifest \
                consistent, required roles present, protocol_no reserved for THIS bundle) and atomically file it: \
                CAS blobs, device_event, provenance has_attachment edges from the event and the ticket. An identical \
                retry returns the earlier receipt; a different event for the same bundle_id fails with `conflict`. \
                Corrections = new bundle with revision+1 and supersedes_protocol_no.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "bundle_id": { "type": "string" },
                    "event": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "enum": KINDS },
                            "date": { "type": "string", "description": "YYYY-MM-DD" },
                            "protocol_no": { "type": "string", "description": "YYYY-NNNN" },
                            "revision": { "type": "integer" },
                            "result": { "type": "string", "enum": RESULTS },
                            "next_due": { "type": ["string", "null"], "description": "YYYY-MM or null" },
                            "examiner": { "type": "string" },
                            "instrument": { "type": "object" },
                            "standard": { "type": "string" },
                            "legal_basis": { "type": "string" },
                            "scope": { "type": "string" },
                            "operator_snapshot": { "type": "object" },
                            "notes": { "type": "string" },
                            "supersedes_protocol_no": { "type": ["string", "null"] }
                        },
                        "required": ["kind", "date", "protocol_no", "result", "examiner"]
                    },
                    "required_roles": { "type": "array", "items": { "type": "string" },
                        "description": "Default: protocol_pdf, protocol_html, tester_payload, manifest." }
                },
                "required": ["bundle_id", "event"]
            }
        }),
        json!({
            "name": "bundle_abort",
            "description": "MASTER ONLY. Abort an open bundle staging and delete its tmp bytes. A protocol number \
                reserved for it stays reserved (numbers are never recycled).",
            "inputSchema": {
                "type": "object",
                "properties": { "bundle_id": { "type": "string" } },
                "required": ["bundle_id"]
            }
        }),
        json!({
            "name": "protocol_number_allocate",
            "description": "MASTER ONLY, authoritative writer node only (else `not_writer`). Reserve the next \
                protocol number YYYY-NNNN for a bundle; the same bundle_id always gets the same number back \
                (`reused: true`). Numbers are never recycled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "year": { "type": "integer" },
                    "bundle_id": { "type": "string" },
                    "kind": { "type": "string", "enum": KINDS }
                },
                "required": ["year", "bundle_id"]
            }
        }),
    ]
}

#[cfg(test)]
#[path = "bundles_tests.rs"]
mod tests;
