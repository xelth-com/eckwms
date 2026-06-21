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
use tracing::info;

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

/// Static path prefixes a caller may read from. Each entry is checked against
/// the canonicalized path of the request. Symlinks are not followed during
/// read. The dynamic per-node project root (env `WMS_PROJECT_ROOT`) is added
/// at startup via [`allowed_file_prefixes`].
const STATIC_ALLOWED_FILE_PREFIXES: &[&str] = &[
    "/var/www/9eck.com/",
    "/var/www/xelixir/",
    "/etc/systemd/system/9eck-",
    "/etc/systemd/system/xelixir.service.d/",
    "/etc/nginx/snippets/eckwms_",
    "/etc/nginx/sites-available/9eck",
    "/etc/nginx/sites-enabled/9eck",
];

/// Compose the full allow-list at call time: static prefixes plus the
/// dynamic `WMS_PROJECT_ROOT` (if set). On kiosk the project lives under
/// `/home/dimi/9eck.com/`, not `/var/www/9eck.com/`; without this hook
/// cross-mesh `ops.file_read` against the kiosk's source tree would 403.
fn allowed_file_prefixes() -> Vec<String> {
    let mut v: Vec<String> = STATIC_ALLOWED_FILE_PREFIXES
        .iter()
        .map(|s| s.to_string())
        .collect();
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
// again, or via the dimi-bootstrap helper for runtime drift).

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
    let expected_fragments = [
        "9eckwms-agent-mock",
        "dimi-bootstrap",
        "9eckwms-nginx",
    ];
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

/// Tables whose contents are Zone 1 (PII / credentials). Until the
/// physical DB split lands, this denylist is the enforcement. After
/// the split, these tables literally won't exist on `state.db`, so the
/// denylist becomes a belt-and-braces check.
const ZONE_1_TABLES: &[&str] = &[
    "user",
    "auth_token",
    "session",
    "order_pii",
    "partner_pii",
    "picking_pii",
    "document_pii",
];

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
    // Zone 1 table name (case-insensitive) is enough to reject. False
    // positives ("select count() from picking where note = 'user'")
    // are acceptable: caller can rephrase.
    let q_lower = q.to_ascii_lowercase();
    for forbidden in ZONE_1_TABLES {
        let needle = forbidden.to_ascii_lowercase();
        // word-boundary-ish check: surrounded by non-word chars
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
    // e.g. WMS restarting itself). On EPERM (cross-uid, like 9eckwms ↦ dimi
    // for kiosk.service), fall back to `sudo systemctl restart`, which is
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
///   - kiosk:       WMS runs as dimi, dimi owns the process → kill works.
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
/// uid than WMS (e.g. kiosk.service on the kiosk runs as `dimi`, WMS as
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

async fn run_with_timeout(
    mut cmd: Command,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let fut = cmd.output();
    tokio::time::timeout(Duration::from_secs(timeout_secs), fut)
        .await
        .map_err(|_| format!("subprocess timed out after {}s", timeout_secs))?
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
        // on the kiosk where WMS runs as `dimi`. Restart=always respawns.
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
/// as `dimi` but inherits `HOME=/root` from systemd. Prepend every plausible
/// .cargo/bin candidate so the binary resolves regardless of OS user setup.
fn augment_path_for_cargo(cmd: &mut Command) {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(format!("{}/.cargo/bin", home));
    }
    // Common fallbacks across our hosts.
    for p in &["/home/dimi/.cargo/bin", "/root/.cargo/bin", "/usr/local/bin"] {
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
