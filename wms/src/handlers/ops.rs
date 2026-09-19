//! Extended ops vocabulary — `/X/ops/*` endpoints.
//!
//! All endpoints in this module are mounted behind
//! `middleware::service_token::require_service_token`, so by the time
//! these handlers run the caller has already proven they hold
//! `XELIXIR_SERVICE_TOKEN`.
//!
//! Design philosophy: each verb is its own endpoint. No polymorphic
//! command field. See `.eck/XELIXIR_OPS_VOCABULARY.md` for the full
//! contract and trust model.

use std::collections::HashMap;
use std::path::{Component, Path as StdPath, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{Mutex, Semaphore};
use tracing::{info, warn};

use crate::AppState;

// ─── allow-lists ───────────────────────────────────────────────────────────
// Hard-coded on purpose. Adding a new service or path requires a code
// change and review — that's the point of a service-level allow-list.

const ALLOWED_SERVICES: &[&str] = &[
    "9eck-wms",
    "9eck-relay",
    "9eck-pos",
    "9eck-compliance",
    "xelixir",
    // Display layer on kiosk devices — cage + chromium + bash respawn loop.
    // Restarting forces a fresh chromium with cleared in-memory state (handy
    // when the cage'd Chromium pins a stale SPA bundle and there's no
    // keyboard / devtools on the physical device). Not present on
    // antigravity, so restart_service "kiosk" there is just a no-op error
    // surface — that's fine, the allow-list is shared.
    "kiosk",
];

/// deployment default; override via ECK_OPS_FILE_PREFIXES
///
/// Path prefixes a caller may read from when `ECK_OPS_FILE_PREFIXES` is unset.
/// Each entry is checked against the canonicalized path of the request.
/// Symlinks are not followed during read. The dynamic per-node project root
/// (env `WMS_PROJECT_ROOT`) is added at startup via [`allowed_file_prefixes`].
/// This literal keeps the current fleet working out of the box; a different
/// deployment overrides it via the env below (comma-separated absolute
/// prefixes) rather than editing code.
const DEFAULT_ALLOWED_FILE_PREFIXES: &[&str] = &[
    "/var/www/9eck.com/",
    "/var/www/xelixir/",
    "/etc/systemd/system/9eck-",
    "/etc/systemd/system/xelixir.service.d/",
    "/etc/nginx/snippets/eckwms_",
    "/etc/nginx/sites-available/9eck",
    "/etc/nginx/sites-enabled/9eck",
];

/// Compose the full allow-list at call time: the configured base prefixes —
/// env `ECK_OPS_FILE_PREFIXES` (comma-separated absolute prefixes) when set,
/// else [`DEFAULT_ALLOWED_FILE_PREFIXES`] — UNION the dynamic `WMS_PROJECT_ROOT`
/// (if set). On some nodes the project lives under the deploy user's home
/// (e.g. `/home/<deploy-user>/9eck.com/`), not `/var/www/9eck.com/`; without
/// this hook cross-mesh `ops.file_read` against that source tree would 403.
fn allowed_file_prefixes() -> Vec<String> {
    let mut v: Vec<String> = match std::env::var("ECK_OPS_FILE_PREFIXES") {
        Ok(s) if !s.trim().is_empty() => s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        _ => DEFAULT_ALLOWED_FILE_PREFIXES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    if let Ok(root) = std::env::var("WMS_PROJECT_ROOT") {
        let mut p = root;
        if !p.ends_with('/') {
            p.push('/');
        }
        if !v.iter().any(|x| *x == p) {
            v.push(p);
        }
    }
    v
}

const MAX_FILE_READ_BYTES: u64 = 1_048_576; // 1 MB
const DEFAULT_JOURNAL_SINCE: &str = "15 min ago";
const MAX_JOURNAL_LINES: usize = 1000;
const SUBPROCESS_TIMEOUT_SECS: u64 = 5;

// ─── GET /X/ops/journal ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct JournalQuery {
    /// systemd service name. Defaults to `9eck-wms`. Must be in `ALLOWED_SERVICES`.
    pub service: Option<String>,
    /// systemd-compatible time spec (e.g., "10 min ago", "2026-05-20 10:00:00").
    pub since: Option<String>,
    /// Optional grep filter passed to journalctl --grep.
    pub grep: Option<String>,
}

pub async fn journal(Query(q): Query<JournalQuery>) -> (StatusCode, Json<Value>) {
    let service = q.service.as_deref().unwrap_or("9eck-wms");
    if !ALLOWED_SERVICES.contains(&service) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("service '{}' not in allow-list", service),
                "allowed": ALLOWED_SERVICES,
            })),
        );
    }
    let since = q.since.as_deref().unwrap_or(DEFAULT_JOURNAL_SINCE);

    let mut cmd = Command::new("journalctl");
    cmd.arg("-u").arg(service)
        .arg("--since").arg(since)
        .arg("--no-pager")
        .arg("--lines").arg(MAX_JOURNAL_LINES.to_string());
    if let Some(g) = q.grep.as_deref() {
        cmd.arg("--grep").arg(g);
    }

    let started = Instant::now();
    let out = match run_with_timeout(cmd, SUBPROCESS_TIMEOUT_SECS).await {
        Ok(o) => o,
        Err(e) => return subprocess_error(e),
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let lines = text.lines().count();
    info!(
        target: "ops_audit",
        verb = "journal", service, since, lines, duration_ms = started.elapsed().as_millis() as u64,
        "ops verb executed"
    );
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "service": service,
            "since": since,
            "lines": lines,
            "text": text,
        })),
    )
}

// ─── GET /X/ops/service_status ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ServiceStatusQuery {
    pub service: Option<String>,
}

pub async fn service_status(Query(q): Query<ServiceStatusQuery>) -> (StatusCode, Json<Value>) {
    let service = q.service.as_deref().unwrap_or("9eck-wms");
    if !ALLOWED_SERVICES.contains(&service) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("service '{}' not in allow-list", service),
            })),
        );
    }

    // Use `systemctl show` for structured output instead of `status`.
    let mut cmd = Command::new("systemctl");
    cmd.arg("show").arg(service).arg("--no-pager").arg("--property=ActiveState,SubState,MainPID,ActiveEnterTimestamp,NRestarts,ExecMainStartTimestamp,Result");

    let out = match run_with_timeout(cmd, SUBPROCESS_TIMEOUT_SECS).await {
        Ok(o) => o,
        Err(e) => return subprocess_error(e),
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let parsed: serde_json::Map<String, Value> = text
        .lines()
        .filter_map(|line| {
            let mut split = line.splitn(2, '=');
            let k = split.next()?.to_string();
            let v = split.next()?.to_string();
            Some((k, Value::String(v)))
        })
        .collect();
    info!(target: "ops_audit", verb = "service_status", service, "ops verb executed");
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "service": service,
            "fields": parsed,
        })),
    )
}

// ─── GET /X/ops/system_health ──────────────────────────────────────────────

pub async fn system_health() -> (StatusCode, Json<Value>) {
    async fn shell(arg: &str) -> String {
        let mut c = Command::new("sh");
        c.arg("-c").arg(arg);
        match run_with_timeout(c, SUBPROCESS_TIMEOUT_SECS).await {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(e) => format!("error: {}", e),
        }
    }

    let uptime = shell("uptime").await;
    let df = shell("df -h --output=source,size,used,avail,pcent,target -x tmpfs -x devtmpfs").await;
    let free = shell("free -m").await;
    let top_cpu = shell("ps -eo pid,user,pcpu,rss,comm --sort=-pcpu | head -6").await;

    info!(target: "ops_audit", verb = "system_health", "ops verb executed");
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "uptime": uptime.trim(),
            "df": df,
            "free": free,
            "top_cpu": top_cpu,
            "loop_metrics": crate::services::health_monitor::report_compact(),
        })),
    )
}

// ─── GET /X/ops/health_check ──────────────────────────────────────────────
//
// Structured drift report for a node's ops config. Tells you remotely
// (without shell) whether the host is wired up per `scripts/kiosk-bootstrap.sh`:
//
//   * which uid WMS is running as (and its group memberships)
//   * presence of expected /etc/sudoers.d/ fragments
//   * sudo capabilities of WMS uid (`sudo -nl` parsed)
//   * file permissions on .env / data/wms.db / data/wms_users.db
//   * which agent process (if any) is running and under which uid
//
// Every field is best-effort — a missing field reports `"unavailable"`
// rather than failing the whole verb. Operators read the JSON and
// decide what to fix (typically by running scripts/kiosk-bootstrap.sh
// again, or via the deploy user's bootstrap helper for runtime drift).

pub async fn health_check(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    use serde_json::Map;

    async fn shell(arg: &str) -> Option<String> {
        let mut c = Command::new("sh");
        c.arg("-c").arg(arg);
        match run_with_timeout(c, SUBPROCESS_TIMEOUT_SECS).await {
            Ok(o) => Some(String::from_utf8_lossy(&o.stdout).trim().to_string()),
            Err(_) => None,
        }
    }

    let mut report = Map::new();

    // WMS uid + groups
    let wms_uid = shell("id -u").await.unwrap_or_else(|| "unavailable".into());
    let wms_user = shell("id -un").await.unwrap_or_else(|| "unavailable".into());
    let wms_groups = shell("id -Gn").await.unwrap_or_else(|| "unavailable".into());
    report.insert(
        "wms_identity".into(),
        json!({
            "uid": wms_uid,
            "user": wms_user,
            "groups": wms_groups,
        }),
    );

    // Expected sudoers fragments — best-effort via ls (cannot read the
    // file contents under non-root, but presence-only is enough for drift).
    // The deploy user's passwordless-restart bootstrap fragment is named after
    // that user (env `ECK_DEPLOY_USER` → `<user>-bootstrap`); the fixed service
    // fragments are always checked.
    let mut expected_fragments: Vec<String> =
        vec!["9eckwms-agent-mock".to_string(), "9eckwms-nginx".to_string()];
    if let Ok(u) = std::env::var("ECK_DEPLOY_USER") {
        let u = u.trim();
        if !u.is_empty() {
            expected_fragments.push(format!("{u}-bootstrap"));
        }
    }
    let mut fragments = Map::new();
    for name in &expected_fragments {
        let path = format!("/etc/sudoers.d/{}", name);
        let exists = tokio::fs::metadata(&path).await.is_ok()
            || shell(&format!("test -e {} && echo y", path))
                .await
                .map(|s| s == "y")
                .unwrap_or(false);
        fragments.insert(name.to_string(), json!({ "exists": exists, "path": path }));
    }
    report.insert("sudoers_fragments".into(), Value::Object(fragments));

    // Sudo capabilities of THIS uid
    let sudo_l = shell("sudo -n -l 2>&1 | head -20")
        .await
        .unwrap_or_else(|| "unavailable".into());
    report.insert("sudo_capabilities".into(), json!(sudo_l));

    // File permissions on critical paths.
    let project_root = std::env::var("WMS_PROJECT_ROOT")
        .unwrap_or_else(|_| "/var/www/9eck.com".into());
    let critical_paths = [
        format!("{}/.env", project_root.trim_end_matches('/')),
        format!("{}/data/wms.db", project_root.trim_end_matches('/')),
        format!("{}/data/wms_users.db", project_root.trim_end_matches('/')),
    ];
    let mut perms = Map::new();
    for p in &critical_paths {
        let stat = shell(&format!("stat -c '%a %U:%G %n' {} 2>/dev/null || echo missing", p))
            .await
            .unwrap_or_else(|| "unavailable".into());
        perms.insert(p.clone(), json!(stat));
    }
    report.insert("critical_paths".into(), Value::Object(perms));

    // Agent process (if any) — read uid from /proc directly.
    let agent_proc = shell(
        r#"pgrep -fa agent_mock | head -3 | while read line; do
             pid=$(echo "$line" | awk '{print $1}')
             user=$(ps -o user= -p "$pid" 2>/dev/null)
             echo "$pid $user $line"
           done"#,
    )
    .await
    .unwrap_or_else(|| "unavailable".into());
    report.insert("agent_processes".into(), json!(agent_proc));

    // Expected systemd unit status — non-fatal; the unit might be named
    // differently per host (e.g. 9eck-wms vs 9eck-wms.service).
    let unit_active = shell("systemctl is-active 9eck-wms 2>/dev/null || systemctl is-active 9eckwms 2>/dev/null || echo unknown")
        .await
        .unwrap_or_else(|| "unavailable".into());
    report.insert("wms_unit_state".into(), json!(unit_active));

    // Drift summary — boolean rollup of "things that should be true".
    let mut drift = Vec::<String>::new();
    if !wms_groups.split_whitespace().any(|g| g == "systemd-journal") {
        drift.push("wms-uid-not-in-systemd-journal".into());
    }
    if !report
        .get("sudoers_fragments")
        .and_then(|v| v.get("9eckwms-agent-mock"))
        .and_then(|v| v.get("exists"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        drift.push("missing-sudoers-9eckwms-agent-mock".into());
    }
    if std::env::var("XELIXIR_AGENT_USER").is_err() {
        drift.push("XELIXIR_AGENT_USER-env-not-set".into());
    }
    report.insert("drift".into(), json!(drift));

    info!(
        target: "ops_audit",
        verb = "health_check", drift_count = drift.len(),
        instance = %state.instance_id,
        "ops verb executed"
    );
    (StatusCode::OK, Json(Value::Object(report)))
}

// ─── GET /X/ops/file_read ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct FileReadQuery {
    pub path: String,
}

pub async fn file_read(Query(q): Query<FileReadQuery>) -> (StatusCode, Json<Value>) {
    if !path_is_allowed(&q.path) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "path is not in the read allow-list",
                "allowed_prefixes": allowed_file_prefixes(),
            })),
        );
    }

    let p = StdPath::new(&q.path);
    // Use symlink_metadata so we can detect (and refuse) symlinks before
    // committing to a read — a symlink under the allow-list could point
    // anywhere on disk.
    let md = match tokio::fs::symlink_metadata(p).await {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "success": false, "error": e.to_string() })),
            );
        }
    };
    if md.file_type().is_symlink() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "symlinks are not followed" })),
        );
    }
    if !md.is_file() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "not a regular file" })),
        );
    }
    if md.len() > MAX_FILE_READ_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "success": false,
                "error": "file exceeds 1 MB read cap",
                "size": md.len(),
            })),
        );
    }

    match tokio::fs::read(p).await {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            info!(
                target: "ops_audit",
                verb = "file_read", path = %q.path, bytes = bytes.len(),
                "ops verb executed"
            );
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "path": q.path,
                    "bytes": bytes.len(),
                    "content": text,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

// ─── POST /X/ops/file_write ────────────────────────────────────────────────
// Atomic write via temp file + rename. Same allow-list prefix as file_read
// — and additionally requires that the existing path is either absent or
// already a regular file under an allowed prefix. Body: `{path, content,
// mode?}`. mode is octal string (e.g., "0600"); defaults to 0644.

#[derive(Deserialize)]
pub struct FileWriteRequest {
    pub path: String,
    pub content: String,
    pub mode: Option<String>,
}

pub async fn file_write(Json(body): Json<FileWriteRequest>) -> (StatusCode, Json<Value>) {
    if !path_is_allowed(&body.path) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "path is not in the write allow-list",
                "allowed_prefixes": allowed_file_prefixes(),
            })),
        );
    }
    if body.content.len() > MAX_FILE_READ_BYTES as usize {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "success": false,
                "error": "content exceeds 1 MB write cap",
            })),
        );
    }
    // Refuse to clobber non-regular files (symlinks, dirs, devices).
    if let Ok(md) = tokio::fs::symlink_metadata(&body.path).await {
        if !md.is_file() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "target exists and is not a regular file" })),
            );
        }
    }
    let parent = match StdPath::new(&body.path).parent() {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "path has no parent" })),
            );
        }
    };
    let tmp_path = parent.join(format!(
        ".file_write.{}.tmp",
        uuid::Uuid::new_v4()
    ));

    let mode = body.mode.as_deref().unwrap_or("0644");
    let mode_u32 = u32::from_str_radix(mode.trim_start_matches('0'), 8).unwrap_or(0o644);

    // Write to temp, then rename atomically.
    if let Err(e) = tokio::fs::write(&tmp_path, &body.content).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": format!("write tmp: {}", e) })),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode_u32);
        if let Err(e) = tokio::fs::set_permissions(&tmp_path, perms).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": format!("chmod: {}", e) })),
            );
        }
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &body.path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": format!("rename: {}", e) })),
        );
    }
    info!(
        target: "ops_audit",
        verb = "file_write", path = %body.path, bytes = body.content.len(),
        "ops verb executed"
    );
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "path": body.path,
            "bytes": body.content.len(),
        })),
    )
}

// ─── POST /X/ops/surrealql_read ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SurrealqlReadRequest {
    pub query: String,
}

/// Tables whose contents are Zone 1 (PII / credentials). This denylist is
/// LOAD-BEARING for `user`: since 2026-07-09 real accounts live in `state.db`
/// again (mesh-synced so one password works fleet-wide), and this check is
/// what keeps the xelixir ops plane from reading credential hashes. The other
/// names stay listed for when their sidecar tables land.
const ZONE_1_TABLES: &[&str] = &[
    "user",
    "auth_token",
    "session",
    "order_pii",
    "partner_pii",
    "picking_pii",
    "document_pii",
    // Shipping carries recipient PII (name/street/zip). The distilled shipment
    // record meshes but the ops plane must not read it; the raw scraper payload
    // (shipment_raw, local-only) is likewise off-limits, as is the legacy blob.
    "stock_picking_delivery",
    "shipment_raw",
    "shipment",
    // Node-local PII-leak quarantine: holds CLEAR unmasked-PII strings the LLM
    // self-reported (spec .eck/PII_REMOTE_MASKING.md §7). Local-only (never in
    // SYNC_ENTITY_TYPES); the ops/MCP surrealql_read plane must not read it.
    "pii_leak_quarantine",
    // Warehouse receiving: the receipt header carries client_name /
    // client_address off the parcel label (and the raw OCR of it), and the
    // units point back at it. Meshes, but the ops control plane can't read it.
    "receipt",
    "receipt_unit",
    // The PII vault (.eck/PII_VAULT.md §3.1). The rows are ciphertext, but the
    // ops plane has no business enumerating WHICH tokens a tenant holds (that
    // alone is a customer-count / linkage signal), and `fuzzy` is a search
    // property over real names. Clear only ever comes out through the audited
    // reveal path, never through surrealql_read.
    "pii_vault",
];

/// Whether the query mentions a Zone 1 table as a standalone word; returns the
/// first offending table name. Shared by the ops handler and the MCP
/// `surrealql_read` tool so both surfaces enforce the identical denylist.
/// False positives ("... where note = 'user'") are acceptable: rephrase.
pub(crate) fn query_references_zone1(q: &str) -> Option<&'static str> {
    let q_lower = q.to_ascii_lowercase();
    for forbidden in ZONE_1_TABLES {
        let needle = forbidden.to_ascii_lowercase();
        let mut idx = 0;
        while let Some(pos) = q_lower[idx..].find(&needle) {
            let abs = idx + pos;
            let before_ok = abs == 0
                || !q_lower.as_bytes()[abs - 1].is_ascii_alphanumeric()
                    && q_lower.as_bytes()[abs - 1] != b'_';
            let after = abs + needle.len();
            let after_ok = after == q_lower.len()
                || (!q_lower.as_bytes()[after].is_ascii_alphanumeric()
                    && q_lower.as_bytes()[after] != b'_');
            if before_ok && after_ok {
                return Some(forbidden);
            }
            idx = abs + needle.len();
        }
    }
    None
}

pub async fn surrealql_read(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SurrealqlReadRequest>,
) -> (StatusCode, Json<Value>) {
    let q = body.query.trim();
    if q.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "empty query" })),
        );
    }
    // First keyword must be in the read-only set.
    let first_kw = q
        .split_whitespace()
        .next()
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    const READ_ONLY: &[&str] = &["SELECT", "INFO", "LIVE", "RETURN"];
    if !READ_ONLY.contains(&first_kw.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!("verb '{}' not in read-only set", first_kw),
                "allowed": READ_ONLY,
            })),
        );
    }
    // Cheap denylist scan — any whitespace-delimited token matching a
    // Zone 1 table name (case-insensitive) is enough to reject.
    if let Some(forbidden) = query_references_zone1(q) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!("query references Zone 1 table '{}'", forbidden),
            })),
        );
    }

    let started = Instant::now();
    let res = state.db.query(q.to_string()).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(mut r) => {
            let value: Result<Vec<Value>, _> = r.take(0);
            info!(
                target: "ops_audit",
                verb = "surrealql_read", first_kw = %first_kw, duration_ms,
                "ops verb executed"
            );
            match value {
                Ok(v) => (
                    StatusCode::OK,
                    Json(json!({ "success": true, "result": v, "duration_ms": duration_ms })),
                ),
                Err(e) => (
                    StatusCode::OK,
                    Json(json!({
                        "success": true,
                        "result": null,
                        "warning": format!("decode failed: {}", e),
                        "duration_ms": duration_ms
                    })),
                ),
            }
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

// ─── POST /X/ops/surrealql_write ───────────────────────────────────────────
//
// Tier-2 write companion to surrealql_read. Same Zone-1 denylist, but the
// allow-keywords list is the mutating verbs (INSERT, UPDATE, UPSERT, DELETE,
// RELATE, DEFINE). Caller MUST pass `confirm_zone: "zone2"` in the body to
// prove they're aware they're hitting Zone-2 data. xelixir_router was already
// dispatching `ops.surrealql_write` envelopes to this URL — the handler just
// never landed (commit history shows the verb routed since the May 16 series).

#[derive(Deserialize)]
pub struct SurrealqlWriteRequest {
    pub query: String,
    /// Spam-guard: must be the literal string `"zone2"`. Forces the caller
    /// to be explicit that this is a Zone-2 write, not a read-by-mistake.
    /// (Zone 1 = PII — denied unconditionally below regardless of this.)
    #[serde(default)]
    pub confirm_zone: String,
}

pub async fn surrealql_write(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SurrealqlWriteRequest>,
) -> (StatusCode, Json<Value>) {
    let q = body.query.trim();
    if q.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "empty query" })),
        );
    }
    if body.confirm_zone != "zone2" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "missing `confirm_zone: \"zone2\"` — surrealql_write requires explicit Zone-2 confirmation"
            })),
        );
    }

    let first_kw = q
        .split_whitespace()
        .next()
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    const WRITE_OK: &[&str] = &[
        "INSERT", "UPDATE", "UPSERT", "DELETE", "RELATE", "DEFINE", "BEGIN",
    ];
    if !WRITE_OK.contains(&first_kw.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!("verb '{}' not in write allow-list", first_kw),
                "allowed": WRITE_OK,
            })),
        );
    }

    // Zone-1 PII denylist — same as surrealql_read. Word-boundary-ish match.
    let q_lower = q.to_ascii_lowercase();
    for forbidden in ZONE_1_TABLES {
        let needle = forbidden.to_ascii_lowercase();
        let mut idx = 0;
        while let Some(pos) = q_lower[idx..].find(&needle) {
            let abs = idx + pos;
            let before_ok = abs == 0
                || !q_lower.as_bytes()[abs - 1].is_ascii_alphanumeric()
                    && q_lower.as_bytes()[abs - 1] != b'_';
            let after = abs + needle.len();
            let after_ok = after == q_lower.len()
                || (!q_lower.as_bytes()[after].is_ascii_alphanumeric()
                    && q_lower.as_bytes()[after] != b'_');
            if before_ok && after_ok {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "success": false,
                        "error": format!("query references Zone 1 table '{}'", forbidden),
                    })),
                );
            }
            idx = abs + needle.len();
        }
    }

    let started = Instant::now();
    let res = state.db.query(q.to_string()).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(mut r) => {
            let value: Result<Vec<Value>, _> = r.take(0);
            info!(
                target: "ops_audit",
                verb = "surrealql_write", first_kw = %first_kw, duration_ms,
                "ops verb executed"
            );
            match value {
                Ok(v) => (
                    StatusCode::OK,
                    Json(json!({ "success": true, "result": v, "duration_ms": duration_ms })),
                ),
                Err(e) => (
                    StatusCode::OK,
                    Json(json!({
                        "success": true,
                        "result": null,
                        "warning": format!("decode failed: {}", e),
                        "duration_ms": duration_ms
                    })),
                ),
            }
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

// ─── POST /X/ops/restart_service ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct RestartServiceRequest {
    pub service: String,
}

pub async fn restart_service(Json(body): Json<RestartServiceRequest>) -> (StatusCode, Json<Value>) {
    if !ALLOWED_SERVICES.contains(&body.service.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("service '{}' not in allow-list", body.service),
            })),
        );
    }
    let started = Instant::now();
    // Try same-uid kill first (works when WMS owns the unit's main process —
    // e.g. WMS restarting itself). On EPERM (cross-uid, like 9eckwms ↦ the
    // deploy user for kiosk.service), fall back to `sudo systemctl restart`, which is
    // gated by /etc/sudoers.d/9eckwms-systemctl (narrow allowlist of
    // exactly these services — installed by scripts/kiosk-bootstrap.sh).
    let result = match restart_via_kill_main_pid(&body.service).await {
        Ok(()) => Ok(()),
        Err(e) if e.contains("Operation not permitted") || e.contains("not permitted") => {
            restart_via_sudo_systemctl(&body.service).await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => {
            info!(
                target: "ops_audit",
                verb = "restart_service",
                service = %body.service,
                duration_ms = started.elapsed().as_millis() as u64,
                "ops verb executed"
            );
            (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "service": body.service,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

/// Restart a systemd service WITHOUT `systemctl restart` (which needs
/// root / sudo / polkit). Strategy: look up the unit's MainPID and send
/// SIGTERM directly. The unit's `Restart=always` policy respawns it
/// within RestartSec. Works for any user that owns the unit's main
/// process — which is the case for our deployments:
///   - antigravity: WMS runs as root, all kills succeed.
///   - kiosk:       WMS runs as the deploy user, which owns the process → kill works.
async fn restart_via_kill_main_pid(service: &str) -> Result<(), String> {
    let mut show = Command::new("systemctl");
    show.arg("show").arg(service).arg("-p").arg("MainPID").arg("--value");
    let out = run_with_timeout(show, 5).await?;
    let pid_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let pid: i32 = pid_str
        .parse()
        .map_err(|_| format!("could not parse MainPID '{}'", pid_str))?;
    if pid <= 1 {
        return Err(format!("MainPID {} invalid for '{}'", pid, service));
    }
    let mut kill = Command::new("kill");
    kill.arg(pid.to_string());
    let out = run_with_timeout(kill, 5).await?;
    if !out.status.success() {
        return Err(format!(
            "kill {} failed: {}",
            pid,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Fallback for restarting a unit whose MainPID is owned by a different
/// uid than WMS (e.g. kiosk.service on the kiosk runs as the deploy user, WMS as
/// `9eckwms`). Uses `sudo systemctl restart` — gated by
/// /etc/sudoers.d/9eckwms-systemctl which lists exactly the allowed
/// services. WMS itself enforces ALLOWED_SERVICES first, so the call
/// chain is: HTTP → ALLOWED_SERVICES gate → sudoers gate → systemd.
async fn restart_via_sudo_systemctl(service: &str) -> Result<(), String> {
    let mut cmd = Command::new("sudo");
    cmd.arg("-n").arg("systemctl").arg("restart").arg(service);
    let out = run_with_timeout(cmd, 15).await?;
    if !out.status.success() {
        return Err(format!(
            "sudo systemctl restart {} failed: {}",
            service,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

// ─── helpers ───────────────────────────────────────────────────────────────

fn path_is_allowed(req_path: &str) -> bool {
    // Canonicalize-by-string: refuse any path containing `..` so we cannot
    // escape a prefix by symlink-or-relative trickery. Combined with the
    // symlink_metadata + allowed_file_prefixes() check this is conservative.
    let pb = PathBuf::from(req_path);
    for c in pb.components() {
        if let Component::ParentDir = c {
            return false;
        }
    }
    allowed_file_prefixes()
        .iter()
        .any(|p| req_path.starts_with(p))
}

/// Run `cmd` to completion under a wall-clock cap, capturing stdout+stderr.
///
/// Unix: the child is spawned as the **leader of a fresh process group**
/// (`process_group(0)` → pgid == child pid). On timeout we SIGKILL the whole
/// group by its negative pgid — not just the immediate child — then the leader,
/// then reap. This is what stops a timed-out `cargo_build` (30 min cap) from
/// leaving orphaned `cargo`/`rustc` grandchildren swap-thrashing the kiosk under
/// PID 1 (see xelixir `.eck/TECH_DEBT.md` "Remote-ops tooling friction"; mirrors
/// the fat agent's proven run_command pattern). The error string carries a
/// `[TIMEOUT after Ns]` marker so callers surface a clear killed-on-timeout
/// result instead of a bare "timed out".
#[cfg(unix)]
async fn run_with_timeout(
    mut cmd: Command,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    use tokio::io::AsyncReadExt;

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        // Fresh process group so the SIGKILL below reaps the whole subtree.
        .process_group(0)
        // Backstop: if the wait future is dropped for any other reason, the
        // immediate child is still SIGKILLed.
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
    // On unix the child's pid doubles as its pgid (process_group(0) above).
    let pid = child.id().map(|p| p as i32).unwrap_or(0);

    // Drain both pipes concurrently: a chatty child (cargo/rustc emit MBs of
    // output) would otherwise fill the ~64 KB pipe buffer and block on write,
    // masquerading as a hang and tripping the timeout for the wrong reason.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(ref mut p) = stdout_pipe {
            let _ = p.read_to_end(&mut buf).await;
        }
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(ref mut p) = stderr_pipe {
            let _ = p.read_to_end(&mut buf).await;
        }
        buf
    });

    match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(Ok(status)) => {
            let stdout = out_task.await.unwrap_or_default();
            let stderr = err_task.await.unwrap_or_default();
            Ok(std::process::Output { status, stdout, stderr })
        }
        Ok(Err(e)) => Err(format!("wait failed: {}", e)),
        Err(_) => {
            // Window elapsed. SIGKILL the whole process group (grandchildren
            // included), then the leader, then reap so nothing lingers.
            if pid > 0 {
                unsafe { libc::kill(-pid, libc::SIGKILL); }
                unsafe { libc::kill(pid, libc::SIGKILL); }
            }
            let _ = child.wait().await;
            // Reader tasks get EOF once the killed pipes close.
            let _ = out_task.await;
            let _ = err_task.await;
            Err(format!(
                "[TIMEOUT after {}s] subprocess killed (whole process group SIGKILLed)",
                timeout_secs
            ))
        }
    }
}

/// Non-unix fallback: no process-group semantics (`kill -pgid` is POSIX-only),
/// so this keeps the original single-child `output()` behaviour. WMS ships on
/// Linux kiosks; this arm exists only to keep Windows dev builds compiling.
#[cfg(not(unix))]
async fn run_with_timeout(
    mut cmd: Command,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let fut = cmd.output();
    tokio::time::timeout(Duration::from_secs(timeout_secs), fut)
        .await
        .map_err(|_| format!("[TIMEOUT after {}s] subprocess killed", timeout_secs))?
        .map_err(|e| format!("spawn failed: {}", e))
}

fn subprocess_error(e: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "success": false, "error": e })),
    )
}

// ─── POST /X/ops/nginx_test_reload ────────────────────────────────────────
// `nginx -t` to validate config; on success send SIGHUP to the master PID
// to graceful-reload. Avoids `systemctl reload nginx` so we don't need sudo
// — the master PID is owned by root on every host running nginx, so this
// only works when WMS itself runs as root (currently true on antigravity,
// the only host that runs nginx).

pub async fn nginx_test_reload() -> (StatusCode, Json<Value>) {
    // `sudo systemctl reload nginx`. Authorised via
    // /etc/sudoers.d/9eckwms-nginx — exactly this one command, no others.
    //
    // Note: a local `nginx -t` would be a nice safety net, but it requires
    // read access to all the TLS cert files referenced in the config,
    // which the non-root WMS uid (`9eckwms`) doesn't have. systemd's
    // reload itself refuses to apply invalid config and keeps the
    // running config intact, so the safety is preserved at the systemd
    // layer instead.
    let mut reload = Command::new("sudo");
    reload.arg("-n").arg("systemctl").arg("reload").arg("nginx");
    match run_with_timeout(reload, 10).await {
        Ok(o) if !o.status.success() => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "success": false,
                "error": "sudo systemctl reload nginx failed",
                "stderr": String::from_utf8_lossy(&o.stderr).to_string(),
            })),
        ),
        Err(e) => subprocess_error(e),
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "success": true })),
        ),
    }
}

// ─── POST /X/ops/package_install ──────────────────────────────────────────
// `apt-get install -y <pkg>` with a small allow-list. Long-running so it
// uses the same async task pattern as cargo_build / deploy.

const ALLOWED_PACKAGES: &[&str] = &[
    // Diagnostics / ops utilities — safe additions.
    "htop",
    "lsof",
    "strace",
    "tcpdump",
    "jq",
    "tree",
    "rsync",
    "grim",     // Wayland screenshot tool — useful on the kiosk
    "wf-recorder", // Wayland screen recorder
    "autossh",
];

#[derive(Deserialize)]
pub struct PackageInstallRequest {
    pub package: String,
}

pub async fn package_install(Json(body): Json<PackageInstallRequest>) -> (StatusCode, Json<Value>) {
    if !ALLOWED_PACKAGES.contains(&body.package.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("package '{}' not in allow-list", body.package),
                "allowed": ALLOWED_PACKAGES,
            })),
        );
    }
    let task = new_task("package_install");
    let task_id = task.id.clone();
    put_task(task).await;

    let pkg = body.package.clone();
    info!(target: "ops_audit", verb = "package_install", task_id = %task_id, package = %pkg, "ops verb dispatched");

    let id_for_spawn = task_id.clone();
    tokio::spawn(async move {
        let mut cmd = Command::new("apt-get");
        cmd.env("DEBIAN_FRONTEND", "noninteractive")
            .arg("install")
            .arg("-y")
            .arg(&body.package);
        let outcome = run_with_timeout(cmd, 300).await; // 5 min cap
        finalize_simple_task(&id_for_spawn, outcome).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "success": true, "task_id": task_id, "verb": "package_install" })),
    )
}

// ─── Tier-2: long-running ops (git_pull, cargo_build, deploy) ──────────────
//
// These verbs can take 30 s – 5 min. HTTP timeouts and the operator's
// patience both forbid blocking the request that long. Pattern:
//
//   POST  /X/ops/<verb>          → spawn → return { task_id }
//   GET   /X/ops/task/:task_id   → poll status, get final output when done
//
// Single-flight is enforced at the verb level via Semaphore(1) — only one
// build (or one deploy, which includes a build) at a time per host.

#[derive(Clone, Serialize)]
pub struct OpsTask {
    pub id: String,
    pub verb: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub state: String, // "running" | "completed" | "failed"
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub phase: Option<String>, // for multi-step verbs like `deploy`
}

type TaskRegistry = Mutex<HashMap<String, OpsTask>>;

fn registry() -> &'static Arc<TaskRegistry> {
    static R: OnceLock<Arc<TaskRegistry>> = OnceLock::new();
    R.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn build_semaphore() -> &'static Arc<Semaphore> {
    static S: OnceLock<Arc<Semaphore>> = OnceLock::new();
    S.get_or_init(|| Arc::new(Semaphore::new(1)))
}

fn new_task(verb: &str) -> OpsTask {
    OpsTask {
        id: uuid::Uuid::new_v4().to_string(),
        verb: verb.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        state: "running".to_string(),
        exit_code: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        phase: None,
    }
}

async fn put_task(t: OpsTask) {
    registry().lock().await.insert(t.id.clone(), t);
}

async fn update_task<F: FnOnce(&mut OpsTask)>(id: &str, f: F) {
    if let Some(t) = registry().lock().await.get_mut(id) {
        f(t);
    }
}

fn tail_text(bytes: &[u8], max_lines: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        s.to_string()
    } else {
        lines[lines.len() - max_lines..].join("\n")
    }
}

/// Project root for git/cargo operations. Defaults to /var/www/9eck.com on
/// production hosts; overridable via env for the kiosk's `~/9eck.com` checkout.
fn project_root() -> String {
    std::env::var("WMS_PROJECT_ROOT").unwrap_or_else(|_| "/var/www/9eck.com".into())
}

// ─── POST /X/ops/git_pull ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct GitPullRequest {
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default)]
    pub rebase: bool,
}

fn default_branch() -> String {
    "main".to_string()
}

pub async fn git_pull(Json(body): Json<GitPullRequest>) -> (StatusCode, Json<Value>) {
    let task = new_task("git_pull");
    let task_id = task.id.clone();
    put_task(task).await;

    let branch = body.branch.clone();
    let rebase = body.rebase;
    info!(target: "ops_audit", verb = "git_pull", task_id = %task_id, branch = %branch, rebase = rebase, "ops verb dispatched");

    let id_for_spawn = task_id.clone();
    tokio::spawn(async move {
        let root = project_root();
        let mut cmd = Command::new("git");
        cmd.current_dir(&root)
            .arg("pull")
            .arg(if body.rebase { "--rebase" } else { "--ff-only" })
            .arg("origin")
            .arg(&body.branch);
        let outcome = run_with_timeout(cmd, 60).await;
        finalize_simple_task(&id_for_spawn, outcome).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "success": true, "task_id": task_id, "verb": "git_pull" })),
    )
}

// ─── POST /X/ops/cargo_build ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CargoBuildRequest {
    /// Cargo package to build (e.g., "wms", "relay"). Default: "wms".
    #[serde(default = "default_crate")]
    pub crate_name: String,
    /// Release build? Default: true.
    #[serde(default = "default_true_bool")]
    pub release: bool,
}

fn default_crate() -> String {
    "wms".to_string()
}
fn default_true_bool() -> bool {
    true
}

pub async fn cargo_build(Json(body): Json<CargoBuildRequest>) -> (StatusCode, Json<Value>) {
    // Refuse to enqueue a second concurrent build instead of blocking the
    // caller's HTTP request behind the semaphore.
    if build_semaphore().available_permits() == 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": "another cargo_build is in progress; try again later"
            })),
        );
    }

    let task = new_task("cargo_build");
    let task_id = task.id.clone();
    put_task(task).await;

    let crate_name = body.crate_name.clone();
    let release = body.release;
    info!(target: "ops_audit", verb = "cargo_build", task_id = %task_id, crate_name = %crate_name, release = release, "ops verb dispatched");

    let id_for_spawn = task_id.clone();
    tokio::spawn(async move {
        let _permit = build_semaphore()
            .acquire()
            .await
            .expect("build semaphore closed");
        let root = project_root();
        let mut cmd = Command::new("cargo");
        augment_path_for_cargo(&mut cmd);
        cmd.current_dir(&root).arg("build");
        if body.release {
            cmd.arg("--release");
        }
        cmd.arg("-p").arg(&body.crate_name);
        let outcome = run_with_timeout(cmd, 1800).await; // 30 min cap (cold builds on kiosk are slow)
        finalize_simple_task(&id_for_spawn, outcome).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "success": true, "task_id": task_id, "verb": "cargo_build" })),
    )
}

// ─── POST /X/ops/deploy ────────────────────────────────────────────────────
// Orchestrator: git_pull → cargo_build → restart_service. Returns a single
// task_id; phase field on the OpsTask tracks which step is running.

#[derive(Deserialize)]
pub struct DeployRequest {
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(default = "default_crate")]
    pub crate_name: String,
    /// systemd unit to restart after the build. Must be in `ALLOWED_SERVICES`.
    #[serde(default = "default_restart_service")]
    pub service: String,
}

fn default_restart_service() -> String {
    "9eck-wms".to_string()
}

pub async fn deploy(Json(body): Json<DeployRequest>) -> (StatusCode, Json<Value>) {
    if !ALLOWED_SERVICES.contains(&body.service.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("service '{}' not in allow-list", body.service),
            })),
        );
    }
    if build_semaphore().available_permits() == 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": "a build / deploy is already in progress"
            })),
        );
    }

    let task = new_task("deploy");
    let task_id = task.id.clone();
    put_task(task).await;

    let branch = body.branch.clone();
    let crate_name = body.crate_name.clone();
    let service = body.service.clone();
    info!(target: "ops_audit", verb = "deploy", task_id = %task_id, branch = %branch, crate_name = %crate_name, service = %service, "ops verb dispatched");

    let id_for_spawn = task_id.clone();
    tokio::spawn(async move {
        let _permit = build_semaphore()
            .acquire()
            .await
            .expect("build semaphore closed");
        let root = project_root();

        // Phase 1: git pull
        update_task(&id_for_spawn, |t| t.phase = Some("git_pull".into())).await;
        let mut git = Command::new("git");
        git.current_dir(&root)
            .arg("pull")
            .arg("--ff-only")
            .arg("origin")
            .arg(&body.branch);
        match run_with_timeout(git, 60).await {
            Ok(o) if !o.status.success() => {
                finish_with_failure(&id_for_spawn, &o, "git pull failed").await;
                return;
            }
            Err(e) => {
                finish_with_error(&id_for_spawn, &format!("git pull: {}", e)).await;
                return;
            }
            Ok(o) => {
                update_task(&id_for_spawn, |t| {
                    t.stdout_tail = tail_text(&o.stdout, 50);
                })
                .await;
            }
        }

        // Phase 2: cargo build
        update_task(&id_for_spawn, |t| t.phase = Some("cargo_build".into())).await;
        let mut cargo = Command::new("cargo");
        augment_path_for_cargo(&mut cargo);
        cargo
            .current_dir(&root)
            .arg("build")
            .arg("--release")
            .arg("-p")
            .arg(&body.crate_name);
        match run_with_timeout(cargo, 1800).await {
            Ok(o) if !o.status.success() => {
                finish_with_failure(&id_for_spawn, &o, "cargo build failed").await;
                return;
            }
            Err(e) => {
                finish_with_error(&id_for_spawn, &format!("cargo build: {}", e)).await;
                return;
            }
            Ok(o) => {
                update_task(&id_for_spawn, |t| {
                    t.stdout_tail = tail_text(&o.stdout, 50);
                    t.stderr_tail = tail_text(&o.stderr, 50);
                })
                .await;
            }
        }

        // Phase 3: restart. Use kill-MainPID so we don't need sudo / polkit
        // on the kiosk where WMS runs as the deploy user. Restart=always respawns.
        //
        // CRITICAL: when the target service IS our own WMS, killing it
        // mid-task means the OUTER cross-mesh poller (waiting on
        // /X/ops/task/<id>) can't deliver the ack to the relay before
        // we die. That leaves the relay row unacked, the next WMS
        // process pulls the same task and restarts again — infinite
        // loop. So: first transition the OpsTask to `completed` (the
        // outer poller picks that up and acks the relay row), THEN
        // schedule the kill with a small delay.
        update_task(&id_for_spawn, |t| t.phase = Some("restart_service".into())).await;
        update_task(&id_for_spawn, |t| {
            t.phase = Some("done".into());
            t.state = "completed".into();
            t.exit_code = Some(0);
            t.finished_at = Some(chrono::Utc::now().to_rfc3339());
        })
        .await;

        let svc = body.service.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Err(e) = restart_via_kill_main_pid(&svc).await {
                tracing::warn!("delayed restart of {} failed: {}", svc, e);
            }
        });
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "success": true, "task_id": task_id, "verb": "deploy" })),
    )
}

// ─── GET /X/ops/task/:task_id ──────────────────────────────────────────────

pub async fn task_status(Path(task_id): Path<String>) -> (StatusCode, Json<Value>) {
    let reg = registry().lock().await;
    match reg.get(&task_id) {
        Some(t) => (StatusCode::OK, Json(serde_json::to_value(t).unwrap())),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "no such task_id" })),
        ),
    }
}

async fn finalize_simple_task(id: &str, outcome: Result<std::process::Output, String>) {
    match outcome {
        Ok(o) => {
            update_task(id, |t| {
                t.state = if o.status.success() {
                    "completed".into()
                } else {
                    "failed".into()
                };
                t.exit_code = o.status.code();
                t.stdout_tail = tail_text(&o.stdout, 50);
                t.stderr_tail = tail_text(&o.stderr, 50);
                t.finished_at = Some(chrono::Utc::now().to_rfc3339());
            })
            .await;
        }
        Err(e) => {
            update_task(id, |t| {
                t.state = "failed".into();
                t.stderr_tail = e;
                t.finished_at = Some(chrono::Utc::now().to_rfc3339());
            })
            .await;
        }
    }
}

async fn finish_with_failure(id: &str, o: &std::process::Output, reason: &str) {
    update_task(id, |t| {
        t.state = "failed".into();
        t.exit_code = o.status.code();
        t.stdout_tail = tail_text(&o.stdout, 50);
        t.stderr_tail = format!("{}\n--- stderr tail ---\n{}", reason, tail_text(&o.stderr, 50));
        t.finished_at = Some(chrono::Utc::now().to_rfc3339());
    })
    .await;
}

/// cargo is typically installed via rustup under `$HOME/.cargo/bin`, which
/// is NOT in the systemd-launched WMS process's `PATH` by default. We also
/// can't trust `$HOME` to point at the right place — on the kiosk, WMS runs
/// as the deploy user but inherits `HOME=/root` from systemd. Prepend every plausible
/// .cargo/bin candidate so the binary resolves regardless of OS user setup.
fn augment_path_for_cargo(cmd: &mut Command) {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(format!("{}/.cargo/bin", home));
    }
    // The deploy user's rustup home (env `ECK_DEPLOY_USER`) — needed when the
    // systemd unit runs as that user but inherits a different `HOME`.
    if let Ok(user) = std::env::var("ECK_DEPLOY_USER") {
        let user = user.trim();
        if !user.is_empty() {
            let s = format!("/home/{user}/.cargo/bin");
            if !candidates.contains(&s) {
                candidates.push(s);
            }
        }
    }
    // Common fallbacks across our hosts.
    for p in &["/root/.cargo/bin", "/usr/local/bin"] {
        let s = p.to_string();
        if !candidates.contains(&s) {
            candidates.push(s);
        }
    }
    let current = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", candidates.join(":"), current);
    cmd.env("PATH", new_path);
}

async fn finish_with_error(id: &str, msg: &str) {
    update_task(id, |t| {
        t.state = "failed".into();
        t.stderr_tail = msg.to_string();
        t.finished_at = Some(chrono::Utc::now().to_rfc3339());
    })
    .await;
}

// ─── POST /admin/or-soak ───────────────────────────────────────────────────
//
// Stage A of the OpenRouter cheap-tier rollout (`.eck/OPENROUTER_TIER.md` §7).
// Admin-gated (mounted in the require_admin router). Spawns a STRICTLY
// READ-ONLY background soak: for up to `limit` recent already-summarized docs
// it recomputes the summary via the OpenRouter shadow route and logs the result
// — it NEVER persists, bumps a vclock, meshes, or writes telemetry to the DB.
//
// CRITICAL (why read-only, not a real re-summarization): this dev node is NOT
// the summary author — the kiosk has been since 2026-08-26. A real
// re-summarization here would persist + vclock-bump + mesh-sync and re-arm a
// fleet-wide re-summarization burn (the 2026-08-26 incident cost 8.2M units).
// So the soak calls ONLY `or_soak_summarize` (build masked input via SELECTs +
// one OR chat call) and emits logs. On this node the usage-report seam is a
// no-op too (no `ECK_VERTEX_USAGE_URL`), so there isn't even a wallet debit.
//
// Safety: OR route forced directly (independent of ECK_AI_TASK_ROUTES /
// ECK_AI_SHADOW), concurrency 4, one retry per doc on a transient OR failure,
// and a hard cumulative-cost cap ($0.50) that aborts the whole run.

/// Hard ceiling on how many docs a single soak may touch (defense against a
/// fat `limit`). Requested `limit` is clamped to this.
const OR_SOAK_HARD_CAP: usize = 500;
/// Base-URL-aware default OR model for BOTH soaks, used when the request omits
/// `model` (an explicit body `model` always wins). On a Z.ai-direct / bigmodel
/// base URL the OpenRouter-prefixed `z-ai/glm-5.3-flash` 404s — that dialect
/// wants the UNprefixed name; on OpenRouter proper the prefix is required. Pure
/// (takes the base URL string), so it is unit-tested without env.
fn default_soak_model(or_base_url: &str) -> &'static str {
    let b = or_base_url.to_ascii_lowercase();
    if b.contains("z.ai") || b.contains("bigmodel") {
        "glm-5.3-flash"
    } else {
        "z-ai/glm-5.3-flash"
    }
}
/// Abort the whole run once cumulative OR cost crosses this (USD).
const OR_SOAK_COST_ABORT_USD: f64 = 0.50;
/// Parallel OR calls in flight.
const OR_SOAK_CONCURRENCY: usize = 4;

#[derive(Deserialize)]
pub struct OrSoakRequest {
    /// How many recent summarized docs to shadow (clamped to `OR_SOAK_HARD_CAP`).
    #[serde(default = "default_or_soak_limit")]
    pub limit: usize,
    /// OpenRouter model override; defaults via `default_soak_model` (base-URL-aware).
    #[serde(default)]
    pub model: Option<String>,
}

fn default_or_soak_limit() -> usize {
    200
}

/// The or-soak selection SQL, extracted so the mem-Surreal test executes the
/// EXACT query the handler runs (the shape that catches the v3 "ORDER BY field
/// must be projected" parse error). `updated_at` IS in the projection.
fn or_soak_select_sql(limit: usize) -> String {
    format!(
        "SELECT record::id(id) AS id, type, updated_at FROM document \n         WHERE ai_summary IS NOT NONE AND ai_summary != '' \n         AND type IN ['support_ticket', 'invoice'] \n         ORDER BY updated_at DESC LIMIT {limit}"
    )
}

/// The img-soak selection SQL, extracted for the same executed-query test.
/// `created_at` MUST be in the projection or SurrealDB v3 rejects the ORDER BY.
fn img_soak_select_sql(limit: usize) -> String {
    format!(
        "SELECT cas_uuid, created_at FROM file_resource \n         WHERE replaced_by IS NONE AND storage_path IS NOT NONE \n         AND mime_type IN ['image/jpeg', 'image/png', 'image/webp', 'image/avif'] \n         AND (optimization_level IS NONE OR optimization_level >= 0) \n         ORDER BY created_at DESC LIMIT {limit}"
    )
}

pub async fn or_soak(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OrSoakRequest>,
) -> (StatusCode, Json<Value>) {
    let limit = body.limit.clamp(1, OR_SOAK_HARD_CAP);
    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            default_soak_model(&std::env::var("ECK_OR_BASE_URL").unwrap_or_default()).to_string()
        });

    // Force the OR auth directly — do NOT depend on the global route/shadow env.
    let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "OPENROUTER_API_KEY is empty — cannot run the OR soak",
            })),
        );
    }

    let db = state.db.clone();
    info!(
        target: "ops_audit",
        verb = "or_soak", limit, model = %model,
        "ops verb dispatched (read-only OR shadow soak)"
    );

    let model_for_spawn = model.clone();
    tokio::spawn(async move {
        run_or_soak(db, key, model_for_spawn, limit).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "success": true,
            "verb": "or-soak",
            "limit": limit,
            "model": model,
            "note": "read-only OR shadow soak started; watch logs for [or-shadow] per doc and the final [or-soak-done] line",
        })),
    )
}

/// The background soak body. Read-only: selection SELECT + per-doc masked-input
/// build (SELECTs) + one OR chat call; logs only.
async fn run_or_soak(db: eck_core::db::SurrealDb, api_key: String, model: String, limit: usize) {
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("[or-soak] failed to build HTTP client: {e}");
            return;
        }
    };
    let or_auth = Arc::new(eck_core::ai::AiAuth::OpenRouter {
        api_key,
        model: model.clone(),
        // Shadow soak uses the node-local OPENROUTER_API_KEY directly (BYOK).
        proxied: false,
        serve_node: None,
        base_url: None,
    });

    // Selection: recent docs that already carry a summary (the re-summarize
    // sweep of §7). Raw-availability is enforced per-doc by `or_soak_summarize`
    // returning None (skipped) — the same input-build the live path uses.
    let docs: Vec<Value> = match db
        .query(or_soak_select_sql(limit))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(d) => d,
        Err(e) => {
            warn!("[or-soak] selection query failed: {e}");
            return;
        }
    };
    let requested = docs.len();
    info!("[or-soak] starting: requested={requested} model={model} concurrency={OR_SOAK_CONCURRENCY}");

    let sem = Arc::new(Semaphore::new(OR_SOAK_CONCURRENCY));
    let processed = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let cjk_hits = Arc::new(AtomicUsize::new(0));
    let empty_hits = Arc::new(AtomicUsize::new(0));
    let cost_micro = Arc::new(AtomicU64::new(0)); // Σ cost_usd × 1e6
    let latency_sum_ms = Arc::new(AtomicU64::new(0));
    let aborted = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for doc in docs {
        if aborted.load(Ordering::Relaxed) {
            break;
        }
        let id = match doc.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break, // semaphore closed
        };
        // Re-check after acquiring (an in-flight task may have tripped the cap).
        if aborted.load(Ordering::Relaxed) {
            drop(permit);
            break;
        }

        let db = db.clone();
        let http = http.clone();
        let or_auth = or_auth.clone();
        let (processed, skipped, errors) = (processed.clone(), skipped.clone(), errors.clone());
        let (cjk_hits, empty_hits) = (cjk_hits.clone(), empty_hits.clone());
        let (cost_micro, latency_sum_ms, aborted) =
            (cost_micro.clone(), latency_sum_ms.clone(), aborted.clone());

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            if aborted.load(Ordering::Relaxed) {
                return;
            }
            let started = Instant::now();
            // One retry on a transient OR failure. An empty-content-with-
            // reasoning error is a GUARD trip, not transient — never retried.
            let mut res = crate::ai::summarization::or_soak_summarize(&db, &http, &or_auth, &doc, &id).await;
            if let Err(ref e) = res {
                if !e.to_string().contains("empty content") {
                    res = crate::ai::summarization::or_soak_summarize(&db, &http, &or_auth, &doc, &id).await;
                }
            }

            match res {
                Ok(None) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some((summary, usage))) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    let cost = usage
                        .get("openrouterCostUsd")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let provider = usage
                        .get("openrouterProvider")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let cjk = crate::ai::summarization::contains_cjk(&summary);
                    let or_model = or_auth.or_model().unwrap_or("");
                    processed.fetch_add(1, Ordering::Relaxed);
                    latency_sum_ms.fetch_add(latency_ms, Ordering::Relaxed);
                    if cjk {
                        cjk_hits.fetch_add(1, Ordering::Relaxed);
                    }
                    // The existing [or-shadow] line, per doc (provider added for
                    // the soak so a grep shows which upstream served each call).
                    info!(
                        "[or-shadow] task=summarization model={or_model} provider={provider} doc={id} latency_ms={latency_ms} cost_usd={cost:.6} cjk_bleed={cjk} chars={}",
                        summary.len()
                    );
                    // Accumulate cost and trip the safety cap if crossed.
                    let micro = (cost.max(0.0) * 1_000_000.0) as u64;
                    let total_micro = cost_micro.fetch_add(micro, Ordering::Relaxed) + micro;
                    if total_micro as f64 / 1e6 > OR_SOAK_COST_ABORT_USD
                        && !aborted.swap(true, Ordering::Relaxed)
                    {
                        warn!(
                            "[or-soak-abort] cumulative cost ${:.4} exceeded ${:.2} safety cap — aborting run",
                            total_micro as f64 / 1e6,
                            OR_SOAK_COST_ABORT_USD
                        );
                    }
                }
                Err(e) => {
                    // Empty-content-with-reasoning is the §3.2 guard trip; count
                    // it separately from genuine call errors.
                    if e.to_string().contains("empty content") {
                        empty_hits.fetch_add(1, Ordering::Relaxed);
                    } else {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                    warn!("[or-soak] doc={id} error: {e}");
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let processed_n = processed.load(Ordering::Relaxed);
    let errors_n = errors.load(Ordering::Relaxed);
    let cjk_n = cjk_hits.load(Ordering::Relaxed);
    let empty_n = empty_hits.load(Ordering::Relaxed);
    let skipped_n = skipped.load(Ordering::Relaxed);
    let total_cost = cost_micro.load(Ordering::Relaxed) as f64 / 1e6;
    let avg_latency = if processed_n > 0 {
        latency_sum_ms.load(Ordering::Relaxed) / processed_n as u64
    } else {
        0
    };
    if aborted.load(Ordering::Relaxed) {
        warn!("[or-soak] run ended early via the ${:.2} cost cap", OR_SOAK_COST_ABORT_USD);
    }
    info!("[or-soak] skipped(raw-unavailable/unknown)={skipped_n}");
    info!(
        "[or-soak-done] requested={requested} processed={processed_n} errors={errors_n} cjk_hits={cjk_n} empty_hits={empty_n} total_cost_usd={total_cost:.4} avg_latency_ms={avg_latency}"
    );
}

// ─── POST /admin/img-soak ───────────────────────────────────────────────────
//
// Vision-wave sibling of or-soak (.eck/OPENROUTER_TIER.md §13). Admin-gated,
// STRICTLY READ-ONLY: shadow-runs the two-pass image pipeline
// (`ai::image_summary::run_image_pipeline`) on up to `limit` recent image
// attachments and logs the result — it NEVER persists, bumps a vclock, meshes,
// or writes DB telemetry. Forces the OR (glm) vision route directly from the
// node-local OPENROUTER_API_KEY (independent of ECK_AI_TASK_ROUTES /
// ECK_AI_IMAGE_POLICY), so it exercises the paid tier the wave will flip to.
// Same $0.50 cumulative-cost abort as or-soak; hard cap 200 attachments.

const IMG_SOAK_HARD_CAP: usize = 200;
const IMG_SOAK_CONCURRENCY: usize = 2; // Z.ai latency spikes under concurrency (bench)

#[derive(Deserialize)]
pub struct ImgSoakRequest {
    #[serde(default = "default_img_soak_limit")]
    pub limit: usize,
    #[serde(default)]
    pub model: Option<String>,
}

fn default_img_soak_limit() -> usize {
    50
}

pub async fn img_soak(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ImgSoakRequest>,
) -> (StatusCode, Json<Value>) {
    let limit = body.limit.clamp(1, IMG_SOAK_HARD_CAP);
    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            default_soak_model(&std::env::var("ECK_OR_BASE_URL").unwrap_or_default()).to_string()
        });

    let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "OPENROUTER_API_KEY is empty — cannot run the image soak",
            })),
        );
    }

    let db = state.db.clone();
    info!(
        target: "ops_audit",
        verb = "img_soak", limit, model = %model,
        "ops verb dispatched (read-only image vision shadow soak)"
    );

    let model_for_spawn = model.clone();
    tokio::spawn(async move {
        run_img_soak(db, key, model_for_spawn, limit).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "success": true,
            "verb": "img-soak",
            "limit": limit,
            "model": model,
            "note": "read-only image vision shadow soak started; watch logs for [img-shadow] per attachment and the final [img-soak-done] line",
        })),
    )
}

/// The background image soak body. Read-only: selection SELECT + per-attachment
/// two-pass pipeline (no persist). Logs only.
async fn run_img_soak(db: eck_core::db::SurrealDb, api_key: String, model: String, limit: usize) {
    let http = match reqwest::Client::builder().timeout(Duration::from_secs(90)).build() {
        Ok(c) => c,
        Err(e) => {
            warn!("[img-soak] failed to build HTTP client: {e}");
            return;
        }
    };
    let or_auth = Arc::new(eck_core::ai::AiAuth::OpenRouter {
        api_key,
        model: model.clone(),
        proxied: false,
        serve_node: None,
        base_url: None,
    });

    // Live (byte-owning) image rows, newest first. Same decodable-mime + non-
    // failed filter as the worker; the pipeline returns None for anything it
    // cannot decode (counted as skipped).
    let rows: Vec<Value> = match db
        .query(img_soak_select_sql(limit))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(d) => d,
        Err(e) => {
            warn!("[img-soak] selection query failed: {e}");
            return;
        }
    };
    let requested = rows.len();
    info!("[img-soak] starting: requested={requested} model={model} concurrency={IMG_SOAK_CONCURRENCY}");

    let sem = Arc::new(Semaphore::new(IMG_SOAK_CONCURRENCY));
    let processed = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let zoom_hits = Arc::new(AtomicUsize::new(0));
    let cost_micro = Arc::new(AtomicU64::new(0));
    let latency_sum_ms = Arc::new(AtomicU64::new(0));
    let aborted = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for row in rows {
        if aborted.load(Ordering::Relaxed) {
            break;
        }
        let cas = match row.get("cas_uuid").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };
        if aborted.load(Ordering::Relaxed) {
            drop(permit);
            break;
        }
        let db = db.clone();
        let http = http.clone();
        let or_auth = or_auth.clone();
        let (processed, skipped, errors, zoom_hits) =
            (processed.clone(), skipped.clone(), errors.clone(), zoom_hits.clone());
        let (cost_micro, latency_sum_ms, aborted) =
            (cost_micro.clone(), latency_sum_ms.clone(), aborted.clone());

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            if aborted.load(Ordering::Relaxed) {
                return;
            }
            let started = Instant::now();
            match crate::ai::image_summary::run_image_pipeline(&db, &http, &or_auth, &cas).await {
                Ok(None) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some(res)) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    processed.fetch_add(1, Ordering::Relaxed);
                    latency_sum_ms.fetch_add(latency_ms, Ordering::Relaxed);
                    if res.zoom_used {
                        zoom_hits.fetch_add(1, Ordering::Relaxed);
                    }
                    info!(
                        "[img-shadow] cas={cas} model={} tier_prompt_tokens={} zoom={} calls={} cost_usd={:.6} latency_ms={latency_ms} chars={}",
                        res.model, res.tier_prompt_tokens, res.zoom_used, res.calls, res.cost_usd, res.summary.len()
                    );
                    let micro = (res.cost_usd.max(0.0) * 1_000_000.0) as u64;
                    let total_micro = cost_micro.fetch_add(micro, Ordering::Relaxed) + micro;
                    if total_micro as f64 / 1e6 > OR_SOAK_COST_ABORT_USD
                        && !aborted.swap(true, Ordering::Relaxed)
                    {
                        warn!(
                            "[img-soak-abort] cumulative cost ${:.4} exceeded ${:.2} safety cap — aborting run",
                            total_micro as f64 / 1e6,
                            OR_SOAK_COST_ABORT_USD
                        );
                    }
                }
                Err(e) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    warn!("[img-soak] cas={cas} error: {e}");
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let processed_n = processed.load(Ordering::Relaxed);
    let errors_n = errors.load(Ordering::Relaxed);
    let zoom_n = zoom_hits.load(Ordering::Relaxed);
    let skipped_n = skipped.load(Ordering::Relaxed);
    let total_cost = cost_micro.load(Ordering::Relaxed) as f64 / 1e6;
    let avg_latency = if processed_n > 0 {
        latency_sum_ms.load(Ordering::Relaxed) / processed_n as u64
    } else {
        0
    };
    if aborted.load(Ordering::Relaxed) {
        warn!("[img-soak] run ended early via the ${:.2} cost cap", OR_SOAK_COST_ABORT_USD);
    }
    info!("[img-soak] skipped(undecodable/not-found)={skipped_n}");
    info!(
        "[img-soak-done] requested={requested} processed={processed_n} errors={errors_n} zoom_hits={zoom_n} total_cost_usd={total_cost:.4} avg_latency_ms={avg_latency}"
    );
}

// ── NAS repair-tree ingester (.eck/REPAIR_LIFECYCLE.md §7) ──────────────────
//
// Admin-gated. Walks the year folder under `ECK_NAS_INGEST_ROOT` and lands each
// file into CAS + a `repair_case`/`has_evidence` graph. DRY-RUN IS THE DEFAULT
// (`dry_run` absent ⇒ true): it hashes + reports and writes nothing. The endpoint
// 404s when `ECK_NAS_INGEST_ROOT` is unset (kiosk-lane only, like the scrapers).

#[derive(Deserialize)]
pub struct NasIngestRequest {
    /// Year folder to ingest (e.g. "2026"). Resolved under `ECK_NAS_INGEST_ROOT`.
    #[serde(default = "default_nas_year")]
    pub year: String,
    /// Defaults TRUE — a real run must opt in explicitly with `"dry_run": false`.
    #[serde(default = "default_true")]
    pub dry_run: bool,
    /// Optional cap on the number of case folders processed (soak / smoke runs).
    #[serde(default)]
    pub limit: Option<usize>,
}

fn default_nas_year() -> String {
    "2026".to_string()
}
fn default_true() -> bool {
    true
}

/// `POST /api/admin/nas-ingest` — see module comment. Runs synchronously and
/// returns the JSON report; use `limit` on the real NAS to bound a first run.
pub async fn nas_ingest(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NasIngestRequest>,
) -> (StatusCode, Json<Value>) {
    let root = match std::env::var("ECK_NAS_INGEST_ROOT") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "success": false,
                    "error": "ECK_NAS_INGEST_ROOT is unset — NAS ingester disabled on this node",
                })),
            );
        }
    };

    info!(
        target: "ops_audit",
        verb = "nas_ingest", year = %body.year, dry_run = body.dry_run,
        limit = ?body.limit, "ops verb dispatched (NAS repair-tree ingest)"
    );

    let iid = state.sync_engine.instance_id().to_string();
    let report = crate::services::nas_ingest::run_ingest(
        &state.db,
        Some(&iid),
        StdPath::new(&root),
        &body.year,
        body.dry_run,
        body.limit,
        ".",
    )
    .await;

    let payload = serde_json::to_value(&report).unwrap_or_else(|_| json!({}));
    (StatusCode::OK, Json(json!({ "success": true, "report": payload })))
}

#[cfg(test)]
mod soak_tests {
    use super::{default_soak_model, img_soak_select_sql, or_soak_select_sql};

    /// The base-URL-aware default model: OR-prefixed for OpenRouter proper,
    /// UNprefixed for the Z.ai-direct / bigmodel dialect (the prefixed name 404s
    /// there). Empty/unknown base URL keeps the OR-prefixed default.
    #[test]
    fn default_soak_model_is_base_url_aware() {
        assert_eq!(default_soak_model(""), "z-ai/glm-5.3-flash");
        assert_eq!(default_soak_model("https://openrouter.ai/api/v1"), "z-ai/glm-5.3-flash");
        assert_eq!(default_soak_model("https://api.z.ai/api/paas/v4"), "glm-5.3-flash");
        assert_eq!(default_soak_model("https://open.bigmodel.cn/api/paas/v4"), "glm-5.3-flash");
        // Case-insensitive host match.
        assert_eq!(default_soak_model("HTTPS://API.Z.AI/v1"), "glm-5.3-flash");
    }

    /// Both soak selection queries must actually PARSE + EXECUTE on SurrealDB v3.
    /// This is the shape that catches the "ORDER BY field must be projected"
    /// parse error (the img-soak bug): a `.take(0)` here fails if the query does
    /// not parse, regardless of whether any rows exist.
    #[tokio::test]
    async fn soak_selection_queries_execute_on_mem_surreal() {
        let db = surrealdb::Surreal::new::<surrealdb::engine::local::Mem>(())
            .await
            .unwrap();
        db.use_ns("test").use_db("test").await.unwrap();

        // Seed one matching row per table (also creates the tables; the mem
        // engine errors on a SELECT from a table that never existed). The ORDER
        // BY on a PROJECTED field must then parse + sort + return the row — a
        // non-projected ORDER BY field is a v3 parse error, which is exactly what
        // this executed query guards against.
        db.query("CREATE document CONTENT { type: 'support_ticket', ai_summary: 'x', updated_at: time::now() }")
            .await
            .unwrap()
            .check()
            .unwrap();
        db.query("CREATE file_resource CONTENT { cas_uuid: 'c1', storage_path: 'p', mime_type: 'image/jpeg', created_at: time::now() }")
            .await
            .unwrap()
            .check()
            .unwrap();

        let docs: Vec<serde_json::Value> =
            db.query(or_soak_select_sql(200)).await.unwrap().take(0).unwrap();
        assert_eq!(docs.len(), 1, "or-soak selection parses and returns the seeded ticket");

        let imgs: Vec<serde_json::Value> =
            db.query(img_soak_select_sql(50)).await.unwrap().take(0).unwrap();
        assert_eq!(imgs.len(), 1, "img-soak selection parses and returns the seeded image");
        assert_eq!(imgs[0].get("cas_uuid").and_then(|v| v.as_str()), Some("c1"));
    }
}
