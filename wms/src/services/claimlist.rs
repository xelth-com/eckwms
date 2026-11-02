//! Legacy Bridge #1 — the tenant's "Claimlist Germany" weekly-report reader/exporter.
//!
//! During the transition off the old "Reparaturliste" flow, the tenant's weekly
//! reporting still runs off an online Excel workbook on their SharePoint
//! ("(EVERYONE) Claimlist Germany_weekly report_Updated 07.10.26 1.xlsx" since
//! 2026-10-30 — HQ re-creates the file now and then; the share URL is config, see
//! `CLAIMLIST_SHARE_URL` below — sheet `ClaimList`).
//! We do NOT write that file — a Claude-in-Excel agent sitting in the workbook
//! enters rows from our JSON export. This module is the READER half: it pulls
//! the workbook headless over MS Graph, snapshots the `ClaimList` rows into a
//! local (non-mesh) mirror, and diffs them against our `order` table so we can
//!   * detect `GERyymm-nnn` collisions (our optimistic number vs. theirs),
//!   * pick up rows the KR side entered manually, and
//!   * track the `AN` "inserted in weekly report" flag.
//! It also mints the next optimistic `GERyymm-nnn` for a finished repair.
//!
//! Config is read from the environment; the module is fully DORMANT (every
//! entry point returns `{skipped}`) unless `CLAIMLIST_TOKEN_FILE` is set:
//! ```text
//!   CLAIMLIST_TOKEN_FILE  path to the MS Graph token json (tenant_id/client_id/
//!                         share_url/refresh_token) — REQUIRED to activate.
//!   CLAIMLIST_SHARE_URL   optional override of the workbook share URL.
//! ```
//! See the claim-list spec under `.eck/` for the authoritative column map + conventions.

use base64::Engine;
use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::info;

const SHEET_NAME: &str = "ClaimList";

// The `ClaimList` sheet is resolved by HEADER TEXT, never by fixed index: the KR
// side inserts/appends columns without warning (2026-09-10: an empty grouping
// column A was inserted in front of everything and an `AP Fortschritt` formula
// appended — every fixed index was off by one and the snapshot went to garbage).
// See the claim-list spec under `.eck/` for the column semantics.
//
// key → the header text as the workbook spells it (matched normalised:
// trimmed, inner whitespace/newlines collapsed, case-insensitive, EXACT — so
// "Claim Number" never swallows "Claim Number Germany").
const HEADER_SPECS: &[(&str, &str)] = &[
    ("zoho", "Zoho Ticket"),
    ("country", "Country"),
    ("city", "Region (State)"), // header says region, holds the city
    ("claim_date", "Claim Date"),
    ("cs_de_no", "Claim Number Germany"),
    ("claim_no", "Claim Number"),
    ("customer", "Customer"),
    ("product_group", "Product Group"),
    ("product_name", "Product Name"),
    ("serial", "S/N"),
    ("replaced_item_code", "Replaced Item Code"),
    ("replaced_item_name", "Replaced Item Name"),
    ("original_sn", "Original Part S/N"),
    ("replacement_sn", "Replacement Part S/N"),
    ("warranty", "Warranty"),
    ("reported", "Inserted in the weekly report"),
];

/// The one column we cannot work without — it carries claim identity.
const REQUIRED_KEY: &str = "claim_no";

/// Normalise a header cell for matching: trim, collapse every run of
/// whitespace (spaces, tabs, the newlines the KR sheet carries in a few headers)
/// into a single space, lowercase. Pure — unit-tested.
pub fn normalize_header(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Resolved `key → 0-based column index` map for one workbook layout.
#[derive(Debug, Clone, Default)]
pub struct ColumnMap {
    cols: std::collections::HashMap<&'static str, usize>,
    /// Headers from `HEADER_SPECS` this workbook did not carry (informational).
    pub missing: Vec<&'static str>,
}

impl ColumnMap {
    /// Resolve every `HEADER_SPECS` entry against a header row. Errors — listing
    /// what was missing — when the required "Claim Number" column is absent,
    /// which is the signal that we are not looking at the sheet we think.
    /// Pure — unit-tested.
    pub fn from_header_row(headers: &[String]) -> Result<Self, String> {
        let norm: Vec<String> = headers.iter().map(|h| normalize_header(h)).collect();
        let mut cols = std::collections::HashMap::new();
        let mut missing: Vec<&'static str> = Vec::new();
        for (key, header) in HEADER_SPECS {
            let want = normalize_header(header);
            match norm.iter().position(|h| *h == want) {
                Some(i) => {
                    cols.insert(*key, i);
                }
                None => missing.push(*header),
            }
        }
        if !cols.contains_key(REQUIRED_KEY) {
            return Err(format!(
                "sheet '{}': header row not recognised — missing column(s): {}",
                SHEET_NAME,
                missing.join(", ")
            ));
        }
        Ok(Self { cols, missing })
    }

    /// 0-based index of a mapped key, if this workbook carries it.
    pub fn index(&self, key: &str) -> Option<usize> {
        self.cols.get(key).copied()
    }
}

// ─── config ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ClaimlistConfig {
    pub token_file: String,
    pub share_url_override: Option<String>,
}

impl ClaimlistConfig {
    /// Build from env. `None` when `CLAIMLIST_TOKEN_FILE` is unset/empty so the
    /// whole module stays a soft no-op on nodes that aren't the reader.
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Some(Self {
            token_file: get("CLAIMLIST_TOKEN_FILE")?,
            share_url_override: get("CLAIMLIST_SHARE_URL"),
        })
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ─── pure helpers (unit-tested) ────────────────────────────────────────────────

/// MS Graph `shares/{share_id}` addresses a shared item by `"u!"` + the
/// base64url (no padding) of the sharing URL. Pure — unit-tested.
pub fn share_id(share_url: &str) -> String {
    format!(
        "u!{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(share_url.as_bytes())
    )
}

/// Build the `GERyymm-` month prefix from a claim date. Accepts anything that
/// starts `YYYY-MM…` (RFC3339, `YYYY-MM-DD`, `YYYY-MM`). Pure — unit-tested.
pub fn month_prefix(claim_date: &str) -> Option<String> {
    let mut it = claim_date.trim().splitn(3, '-');
    let year = it.next()?;
    let month = it.next()?;
    if year.len() < 4 || month.len() < 2 {
        return None;
    }
    let yy = &year[year.len() - 2..];
    let mm = &month[..2];
    if !yy.chars().all(|c| c.is_ascii_digit()) || !mm.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("GER{}{}-", yy, mm))
}

/// Format an `nnn` counter under a `GERyymm-` prefix (zero-padded to 3).
/// Pure — unit-tested.
pub fn format_claim_no(prefix: &str, n: u32) -> String {
    format!("{}{:03}", prefix, n)
}

/// Parse the `nnn` counter out of a `GERyymm-nnn` string under the given prefix.
fn counter_for_prefix(prefix: &str, claim_no: &str) -> Option<u32> {
    claim_no.strip_prefix(prefix)?.parse::<u32>().ok()
}

/// Given a `GERyymm-` prefix and every claim_no already seen for that month
/// (from both our orders and the sheet), return the next free number as a
/// formatted `GERyymm-nnn`. Pure — unit-tested.
pub fn next_from_existing(prefix: &str, claim_nos: &[String]) -> String {
    let max = claim_nos
        .iter()
        .filter_map(|c| counter_for_prefix(prefix, c))
        .max()
        .unwrap_or(0);
    format_claim_no(prefix, max + 1)
}

/// Pull the CITY out of a free-text German postal address: the tail after the
/// 4–5 digit postcode ("Am Kiekenbusch 3, 21614 Buxtehude" → "Buxtehude"). Falls
/// back to the last comma-separated segment, then to nothing. Pure — unit-tested.
pub fn city_from_address(addr: &str) -> Option<String> {
    let clean = |s: &str| s.trim().trim_matches(|c| c == ',' || c == ';').trim().to_string();
    // Scan tokens for a bare postcode; everything after it on that line is the city.
    let flat = addr.replace(['\n', '\r'], ", ");
    let toks: Vec<&str> = flat.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        let t = t.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        // Drop a country prefix ("D-12345", "DE-12345") before the digit test.
        let t = t.rsplit('-').next().unwrap_or(t);
        if (4..=5).contains(&t.len()) && t.chars().all(|c| c.is_ascii_digit()) {
            let rest = clean(&toks[i + 1..].join(" "));
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    let last = flat.rsplit(',').map(clean).find(|s| !s.is_empty())?;
    if last.is_empty() {
        None
    } else {
        Some(last)
    }
}

/// Does a scanned barcode look like a device serial (rather than one of our own
/// `RCV-…` codes or a 19-char smart code)? Pure — unit-tested.
pub fn looks_like_serial(barcode: &str) -> bool {
    let b = barcode.trim();
    if b.len() < 6 || b.len() > 24 {
        return false;
    }
    if !b.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false; // RCV-YYYYMMDD-NNNN and friends carry dashes
    }
    if !b.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    // Our smart codes are exactly 19 chars behind a one-letter kind marker.
    if b.len() == 19 && matches!(b.chars().next(), Some('i' | 'b' | 'l' | 'p')) {
        return false;
    }
    true
}

// ─── MS Graph auth + fetch ─────────────────────────────────────────────────────

/// POST the stored refresh_token against the tenant token endpoint, ROTATE the
/// stored refresh_token in the token file (Graph issues a fresh one on every
/// refresh — dropping it would strand us at the next run), and return a live
/// access_token. Other keys in the token file are preserved.
pub async fn refresh_access_token(cfg: &ClaimlistConfig) -> Result<String, String> {
    let raw = std::fs::read_to_string(&cfg.token_file)
        .map_err(|e| format!("read token file {}: {}", cfg.token_file, e))?;
    let mut token: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse token file: {}", e))?;

    let tenant_id = token
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .ok_or("token file missing tenant_id")?
        .to_string();
    let client_id = token
        .get("client_id")
        .and_then(|v| v.as_str())
        .ok_or("token file missing client_id")?
        .to_string();
    let refresh_token = token
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or("token file missing refresh_token")?
        .to_string();

    let http = http_client();
    let url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        tenant_id
    );
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", client_id.as_str()),
        ("refresh_token", refresh_token.as_str()),
        (
            "scope",
            "https://graph.microsoft.com/Files.Read.All offline_access",
        ),
    ];
    let resp = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("token request send: {}", e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("token response parse (HTTP {}): {}", status, e))?;

    if !status.is_success() {
        let desc = body
            .get("error_description")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("error").and_then(|v| v.as_str()))
            .unwrap_or("unknown token error");
        return Err(format!("token refresh failed (HTTP {}): {}", status, desc));
    }

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("token response missing access_token")?
        .to_string();

    // Rotate the stored refresh_token (preserve every other key), write back.
    if let Some(new_rt) = body.get("refresh_token").and_then(|v| v.as_str()) {
        if let Some(obj) = token.as_object_mut() {
            obj.insert("refresh_token".to_string(), json!(new_rt));
            obj.insert(
                "obtained_at".to_string(),
                json!(chrono::Utc::now().to_rfc3339()),
            );
        }
        let pretty = serde_json::to_string_pretty(&token)
            .map_err(|e| format!("serialize rotated token: {}", e))?;
        std::fs::write(&cfg.token_file, pretty)
            .map_err(|e| format!("rewrite token file: {}", e))?;
    }

    Ok(access_token)
}

/// Resolve the workbook share URL (env override wins, else the token file).
fn resolve_share_url(cfg: &ClaimlistConfig) -> Result<String, String> {
    if let Some(u) = &cfg.share_url_override {
        return Ok(u.clone());
    }
    let raw = std::fs::read_to_string(&cfg.token_file)
        .map_err(|e| format!("read token file {}: {}", cfg.token_file, e))?;
    let token: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse token file: {}", e))?;
    token
        .get("share_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no share URL (set CLAIMLIST_SHARE_URL or share_url in token file)".into())
}

/// GET the `driveItem` metadata for the shared workbook — cheap poll surface.
/// Returns `{ name, size, lastModifiedDateTime }`.
pub async fn fetch_metadata(cfg: &ClaimlistConfig, access_token: &str) -> Result<Value, String> {
    let share_url = resolve_share_url(cfg)?;
    let url = format!(
        "https://graph.microsoft.com/v1.0/shares/{}/driveItem",
        share_id(&share_url)
    );
    let http = http_client();
    let resp = http
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("metadata send: {}", e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("metadata parse (HTTP {}): {}", status, e))?;
    if !status.is_success() {
        return Err(format!("metadata failed (HTTP {}): {}", status, body));
    }
    Ok(json!({
        "name": body.get("name"),
        "size": body.get("size"),
        "lastModifiedDateTime": body.get("lastModifiedDateTime"),
    }))
}

/// GET the workbook bytes (`driveItem/content`, ~3 MB xlsx).
pub async fn fetch_workbook(cfg: &ClaimlistConfig, access_token: &str) -> Result<Vec<u8>, String> {
    let share_url = resolve_share_url(cfg)?;
    let url = format!(
        "https://graph.microsoft.com/v1.0/shares/{}/driveItem/content",
        share_id(&share_url)
    );
    let http = http_client();
    let resp = http
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("content send: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("content failed (HTTP {}): {}", status, text));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("content read: {}", e))?;
    Ok(bytes.to_vec())
}

// ─── xlsx parse ────────────────────────────────────────────────────────────────

/// One parsed `ClaimList` row (only the columns we care about).
#[derive(Debug, Clone)]
struct ClaimRow {
    row_index: usize, // absolute sheet row (header = 1, first data row = 2)
    zoho_ticket: String,
    country: String,
    city: String,
    claim_date: String,
    cs_de_no: String,
    claim_no: String,
    customer: String,
    product_group: String,
    product_name: String,
    serial: String,
    replaced_item_code: String,
    replaced_item_name: String,
    original_sn: String,
    replacement_sn: String,
    warranty: String,
    reported: String,
}

/// Stringify a calamine cell. Floats that are whole numbers print without a
/// trailing `.0` (S/N, Zoho ticket ids read cleaner that way).
fn cell_string(cell: Option<&calamine::Data>) -> String {
    use calamine::Data;
    match cell {
        None | Some(Data::Empty) => String::new(),
        Some(Data::String(s)) => s.trim().to_string(),
        Some(Data::Int(i)) => i.to_string(),
        Some(Data::Float(f)) => {
            if f.fract() == 0.0 {
                format!("{}", *f as i64)
            } else {
                f.to_string()
            }
        }
        Some(Data::Bool(b)) => b.to_string(),
        Some(Data::DateTime(dt)) => dt.as_f64().to_string(),
        Some(Data::DateTimeIso(s)) => s.clone(),
        Some(Data::DurationIso(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Parse the `ClaimList` sheet out of raw xlsx bytes. Row 1 is the header row —
/// columns are resolved from its TEXT (see [`ColumnMap`]), never by position;
/// data starts at row 2 and rows with an empty claim number are skipped.
fn parse_workbook(bytes: &[u8]) -> Result<Vec<ClaimRow>, String> {
    use calamine::{open_workbook_from_rs, Reader, Xlsx};
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut wb: Xlsx<_> =
        open_workbook_from_rs(cursor).map_err(|e| format!("open xlsx: {}", e))?;
    let range = wb
        .worksheet_range(SHEET_NAME)
        .map_err(|e| format!("sheet '{}': {}", SHEET_NAME, e))?;

    let mut rows = range.rows();
    let header: Vec<String> = rows
        .next()
        .ok_or_else(|| format!("sheet '{}' is empty", SHEET_NAME))?
        .iter()
        .map(|c| cell_string(Some(c)))
        .collect();
    let map = ColumnMap::from_header_row(&header)?;
    if !map.missing.is_empty() {
        info!(
            "[claimlist] header layout resolved, absent columns: {}",
            map.missing.join(", ")
        );
    }

    let mut out = Vec::new();
    for (i, row) in rows.enumerate() {
        let get = |key: &str| -> String {
            map.index(key)
                .map(|c| cell_string(row.get(c)))
                .unwrap_or_default()
        };
        let claim_no = get("claim_no");
        if claim_no.is_empty() {
            continue; // no claim number → skip (owner rule: it drives identity)
        }
        out.push(ClaimRow {
            row_index: i + 2, // 1-based sheet row, header consumed above
            zoho_ticket: get("zoho"),
            country: get("country"),
            city: get("city"),
            claim_date: get("claim_date"),
            cs_de_no: get("cs_de_no"),
            claim_no,
            customer: get("customer"),
            product_group: get("product_group"),
            product_name: get("product_name"),
            serial: get("serial"),
            replaced_item_code: get("replaced_item_code"),
            replaced_item_name: get("replaced_item_name"),
            original_sn: get("original_sn"),
            replacement_sn: get("replacement_sn"),
            warranty: get("warranty"),
            reported: get("reported"),
        });
    }
    Ok(out)
}

// ─── persistence + main entry ──────────────────────────────────────────────────

/// Ensure the two local (non-mesh) bookkeeping tables exist. SurrealDB errors on
/// a SELECT from a never-defined table, so define on every run (idempotent).
async fn ensure_tables(db: &SurrealDb) -> Result<(), String> {
    db.query(
        "DEFINE TABLE IF NOT EXISTS claimlist_state SCHEMALESS; \
         DEFINE TABLE IF NOT EXISTS claimlist_row SCHEMALESS;",
    )
    .await
    .map_err(|e| format!("define claimlist tables: {}", e))?;
    Ok(())
}

/// The main reader entry. Self-guards on `CLAIMLIST_TOKEN_FILE`; refreshes the
/// Graph token, compares `lastModifiedDateTime` against the stored bookkeeping
/// row, and — only when the workbook changed — downloads, parses, snapshots the
/// rows into `claimlist_row`, and updates `claimlist_state`. Cheap (metadata
/// only) on the common unchanged path.
pub async fn check_and_ingest(db: &SurrealDb) -> Result<Value, String> {
    let Some(cfg) = ClaimlistConfig::from_env() else {
        return Ok(json!({ "skipped": "CLAIMLIST_TOKEN_FILE unset" }));
    };
    ensure_tables(db).await?;
    let now = chrono::Utc::now().to_rfc3339();

    let access_token = refresh_access_token(&cfg).await?;
    let meta = fetch_metadata(&cfg, &access_token).await?;
    let last_modified = meta
        .get("lastModifiedDateTime")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Previous bookkeeping row.
    let prev: Option<Value> = db
        .query("SELECT last_modified, row_count FROM claimlist_state:main")
        .await
        .map_err(|e| format!("read claimlist_state: {}", e))?
        .take(0)
        .map_err(|e| format!("read claimlist_state: {}", e))?;
    let prev_modified = prev
        .as_ref()
        .and_then(|v| v.get("last_modified"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if !last_modified.is_empty() && last_modified == prev_modified {
        // Unchanged — just touch last_checked.
        let _: Result<Option<Value>, _> = db
            .upsert(("claimlist_state", "main"))
            .merge(json!({ "last_checked": now, "last_error": Value::Null }))
            .await;
        return Ok(json!({
            "unchanged": true,
            "last_modified": last_modified,
            "row_count": prev.as_ref().and_then(|v| v.get("row_count")).cloned().unwrap_or(json!(0)),
        }));
    }

    // Changed (or first run) — download + parse + snapshot.
    let bytes = fetch_workbook(&cfg, &access_token).await?;
    let rows = match parse_workbook(&bytes) {
        Ok(r) => r,
        Err(e) => {
            let _: Result<Option<Value>, _> = db
                .upsert(("claimlist_state", "main"))
                .merge(json!({ "last_checked": now, "last_error": e }))
                .await;
            return Err(e);
        }
    };

    for r in &rows {
        // Deterministic id: claim_no + absolute sheet row (a claim spans one row
        // per replaced part, so claim_no alone is not unique).
        let id = format!("{}_{}", r.claim_no, r.row_index);
        let data = json!({
            "row_index": r.row_index,
            "zoho_ticket": r.zoho_ticket,
            "country": r.country,
            "city": r.city,
            "claim_date": r.claim_date,
            "cs_de_no": r.cs_de_no,
            "claim_no": r.claim_no,
            "customer": r.customer,
            "product_group": r.product_group,
            "product_name": r.product_name,
            "serial": r.serial,
            "replaced_item_code": r.replaced_item_code,
            "replaced_item_name": r.replaced_item_name,
            "original_sn": r.original_sn,
            "replacement_sn": r.replacement_sn,
            "warranty": r.warranty,
            "reported": r.reported,
            "fetched_at": now,
        });
        let _: Result<Option<Value>, _> = db
            .upsert(("claimlist_row", id.as_str()))
            .content(data)
            .await;
    }

    // Stale sweep: every row of THIS snapshot carries `fetched_at = now`, so
    // anything else is a ghost — a row the KR side deleted, or (2026-09-10) one
    // written under the old column layout whose id was built from the wrong
    // cell. Ghosts otherwise linger forever and lie in `diff_report`.
    let swept: Vec<Value> = db
        .query("DELETE claimlist_row WHERE fetched_at != $now RETURN BEFORE")
        .bind(("now", now.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    if !swept.is_empty() {
        info!("[claimlist] swept {} stale snapshot row(s)", swept.len());
    }

    let row_count = rows.len();
    let _: Result<Option<Value>, _> = db
        .upsert(("claimlist_state", "main"))
        .content(json!({
            "last_modified": last_modified,
            "last_checked": now,
            "last_error": Value::Null,
            "row_count": row_count,
            "name": meta.get("name"),
            "size": meta.get("size"),
        }))
        .await;

    info!("[claimlist] ingested {} rows (last_modified {})", row_count, last_modified);
    Ok(json!({
        "changed": true,
        "row_count": row_count,
        "last_modified": last_modified,
    }))
}

// ─── diff + numbering ──────────────────────────────────────────────────────────

/// Compare the snapshotted sheet against our `order` table.
///  * `collisions`   — our order carries a `claim_no` the sheet also uses, but
///                     on a DIFFERENT serial (case-insensitive; empty sheet
///                     serial = no conflict).
///  * `external_rows`— claim_nos in the sheet with no matching order (KR-side /
///                     manual entries).
///  * `reported`     — claim_nos the sheet flags `AN = o`.
pub async fn diff_report(db: &SurrealDb) -> Result<Value, String> {
    ensure_tables(db).await?;

    let sheet: Vec<Value> = db
        .query("SELECT claim_no, serial, reported FROM claimlist_row")
        .await
        .map_err(|e| format!("load claimlist_row: {}", e))?
        .take(0)
        .map_err(|e| format!("load claimlist_row: {}", e))?;

    let orders: Vec<Value> = db
        .query("SELECT claim_no, serial_number, uuid, record::id(id) AS oid FROM order WHERE claim_no != NONE")
        .await
        .map_err(|e| format!("load orders: {}", e))?
        .take(0)
        .map_err(|e| format!("load orders: {}", e))?;

    // Index the sheet by claim_no. Keep the first non-empty serial and OR the
    // reported flag across the (possibly multiple) rows of a claim.
    use std::collections::{HashMap, HashSet};
    let mut sheet_serial: HashMap<String, String> = HashMap::new();
    let mut sheet_claims: HashSet<String> = HashSet::new();
    let mut reported: HashSet<String> = HashSet::new();
    for s in &sheet {
        let cn = s.get("claim_no").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if cn.is_empty() {
            continue;
        }
        sheet_claims.insert(cn.clone());
        let serial = s.get("serial").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if !serial.is_empty() {
            sheet_serial.entry(cn.clone()).or_insert(serial);
        }
        if s.get("reported").and_then(|v| v.as_str()).unwrap_or("").trim().eq_ignore_ascii_case("o") {
            reported.insert(cn.clone());
        }
    }

    let mut collisions: Vec<Value> = Vec::new();
    let mut our_claims: HashSet<String> = HashSet::new();
    for o in &orders {
        let cn = o.get("claim_no").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if cn.is_empty() {
            continue;
        }
        our_claims.insert(cn.clone());
        let our_serial = o.get("serial_number").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if let Some(sheet_ser) = sheet_serial.get(&cn) {
            if !sheet_ser.is_empty() && !our_serial.eq_ignore_ascii_case(sheet_ser) {
                collisions.push(json!({
                    "claim_no": cn,
                    "our_serial": our_serial,
                    "sheet_serial": sheet_ser,
                    "order_id": o.get("oid").cloned().unwrap_or(Value::Null),
                    "order_uuid": o.get("uuid").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }

    let mut external_rows: Vec<String> = sheet_claims
        .iter()
        .filter(|c| !our_claims.contains(*c))
        .cloned()
        .collect();
    external_rows.sort();
    let mut reported: Vec<String> = reported.into_iter().collect();
    reported.sort();

    Ok(json!({
        "collisions": collisions,
        "external_rows": external_rows,
        "reported": reported,
        "sheet_claims": sheet_claims.len(),
        "our_claims": our_claims.len(),
    }))
}

/// Mint the next optimistic `GERyymm-nnn` for the month of `claim_date`. Takes
/// the max counter over BOTH our orders' `claim_no` AND the snapshotted sheet
/// rows for that month, +1. Falls back to the current UTC month if `claim_date`
/// is unparseable.
pub async fn next_claim_no(db: &SurrealDb, claim_date: &str) -> Result<String, String> {
    ensure_tables(db).await?;
    let prefix = month_prefix(claim_date)
        .or_else(|| month_prefix(&chrono::Utc::now().to_rfc3339()))
        .ok_or("could not derive month prefix")?;

    let ours: Vec<String> = db
        .query("SELECT VALUE claim_no FROM order WHERE string::starts_with(claim_no ?? '', $p)")
        .bind(("p", prefix.clone()))
        .await
        .map_err(|e| format!("scan orders: {}", e))?
        .take(0)
        .map_err(|e| format!("scan orders: {}", e))?;
    let sheet: Vec<String> = db
        .query("SELECT VALUE claim_no FROM claimlist_row WHERE string::starts_with(claim_no ?? '', $p)")
        .bind(("p", prefix.clone()))
        .await
        .map_err(|e| format!("scan claimlist_row: {}", e))?
        .take(0)
        .map_err(|e| format!("scan claimlist_row: {}", e))?;

    let mut all = ours;
    all.extend(sheet);
    Ok(next_from_existing(&prefix, &all))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_id_encodes_url_safe_no_pad() {
        // "hello" -> "aGVsbG8" (base64url, no padding); prefixed with "u!".
        assert_eq!(share_id("hello"), "u!aGVsbG8");
        let real = "https://example-my.sharepoint.com/:x:/g/personal/user123_example_com/IQ";
        let sid = share_id(real);
        assert!(sid.starts_with("u!"));
        // URL-safe alphabet only, and no '=' padding.
        assert!(!sid.contains('='));
        assert!(!sid.contains('+'));
        assert!(!sid.contains('/'));
    }

    #[test]
    fn month_prefix_from_various_date_shapes() {
        assert_eq!(month_prefix("2026-08-25").as_deref(), Some("GER2608-"));
        assert_eq!(month_prefix("2026-08-25T15:26:00Z").as_deref(), Some("GER2608-"));
        assert_eq!(month_prefix("2026-08").as_deref(), Some("GER2608-"));
        assert_eq!(month_prefix("garbage"), None);
        assert_eq!(month_prefix("26-8-1"), None); // too short to be safe
    }

    /// The layout as the workbook read BEFORE 2026-09-10: no leading grouping
    /// column, `Zoho Ticket` in A.
    fn old_layout() -> Vec<String> {
        [
            "Zoho Ticket", "Country", "Region (State)", "Claim Date",
            "Claim Number Germany", "Claim Number", "Customer", "Product Group",
            "Product Name", "S/N", "Manufacture Date", "Purchase Date",
            "Usage Days", "Defect Major", "Defect Minor", "Issue / Symptoms",
            "Resolution_Major", "Resolution Minor", "Resolution", "Completed Date",
            "Claim Process Days", "Received Date", "Repaired Date",
            "Days to Receive", "Days to Repair", "Replaced Item Code",
            "Replaced Item Name", "Original Part S/N
", "
Replacement Part S/N",
            "Warranty", "Initial Defect", "Medical Device Rpeort (MDR) ",
            "Manufacture Year", "Purchase Year", "Sales Year", "Usage Years",
            "Device Age at Sale (days)", "Measurement Count", "Week",
            "Inserted in the weekly report",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// The layout since 2026-09-10: an EMPTY grouping column A in front of
    /// everything (so every column moved right by one) and an appended
    /// `Fortschritt` formula column.
    fn new_layout() -> Vec<String> {
        let mut h = vec![String::new()];
        h.extend(old_layout());
        h.push("Fortschritt".to_string());
        h
    }

    #[test]
    fn city_parsed_from_address() {
        assert_eq!(
            city_from_address("Am Kiekenbusch 3, 21614 Buxtehude").as_deref(),
            Some("Buxtehude")
        );
        assert_eq!(
            city_from_address("Joy Fitness
Hauptstr. 5
60437 Frankfurt am Main").as_deref(),
            Some("Frankfurt am Main")
        );
        assert_eq!(
            city_from_address("Musterweg 1, D-12345 Berlin,").as_deref(),
            Some("Berlin")
        );
        // No postcode → last comma segment.
        assert_eq!(city_from_address("Some Gym, Eschborn").as_deref(), Some("Eschborn"));
        assert_eq!(city_from_address("   "), None);
    }

    #[test]
    fn serial_shaped_barcodes_only() {
        assert!(looks_like_serial("F91702145"));
        assert!(looks_like_serial("BPB1234567"));
        assert!(!looks_like_serial("RCV-20260907-9507")); // our own code
        assert!(!looks_like_serial("12345")); // too short
        assert!(!looks_like_serial("ABCDEFGH")); // no digit
        assert!(!looks_like_serial("b010203040506070809")); // 19-char smart code
        assert!(!looks_like_serial("i123456789012345678")); // 19-char smart code
        assert!(!looks_like_serial("F9170214512345678901234567")); // absurdly long
    }

    #[test]
    fn header_map_resolves_old_layout() {
        let m = ColumnMap::from_header_row(&old_layout()).unwrap();
        assert_eq!(m.index("zoho"), Some(0)); // A
        assert_eq!(m.index("claim_no"), Some(5)); // F
        assert_eq!(m.index("cs_de_no"), Some(4)); // E
        assert_eq!(m.index("customer"), Some(6)); // G
        assert_eq!(m.index("serial"), Some(9)); // J
        assert_eq!(m.index("warranty"), Some(29)); // AD
        assert_eq!(m.index("reported"), Some(39)); // AN
        assert!(m.missing.is_empty(), "missing: {:?}", m.missing);
    }

    #[test]
    fn header_map_resolves_new_shifted_layout() {
        let m = ColumnMap::from_header_row(&new_layout()).unwrap();
        // Everything is exactly one to the right of the old layout.
        assert_eq!(m.index("zoho"), Some(1)); // B
        assert_eq!(m.index("city"), Some(3)); // D
        assert_eq!(m.index("cs_de_no"), Some(5)); // F
        assert_eq!(m.index("claim_no"), Some(6)); // G  ← the regression
        assert_eq!(m.index("customer"), Some(7)); // H
        assert_eq!(m.index("serial"), Some(10)); // K
        assert_eq!(m.index("replaced_item_code"), Some(26)); // AA
        assert_eq!(m.index("warranty"), Some(30)); // AE
        assert_eq!(m.index("reported"), Some(40)); // AO
        assert!(m.missing.is_empty(), "missing: {:?}", m.missing);
    }

    #[test]
    fn header_matching_tolerates_whitespace_and_newlines_and_case() {
        assert_eq!(normalize_header("  Claim  Number 
"), "claim number");
        assert_eq!(normalize_header("Original Part S/N
"), "original part s/n");
        assert_eq!(normalize_header("
Replacement Part S/N"), "replacement part s/n");
        let headers: Vec<String> = vec![
            "
 CLAIM   number 
".into(),
            "original part s/n
".into(),
            "  
Replacement Part S/N ".into(),
        ];
        let m = ColumnMap::from_header_row(&headers).unwrap();
        assert_eq!(m.index("claim_no"), Some(0));
        assert_eq!(m.index("original_sn"), Some(1));
        assert_eq!(m.index("replacement_sn"), Some(2));
    }

    #[test]
    fn claim_number_germany_does_not_shadow_claim_number() {
        // Exact (normalised) match only — the CS-DE column must not be picked
        // up as the GER claim number, which is what a prefix match would do.
        let headers: Vec<String> = vec!["Claim Number Germany".into(), "Claim Number".into()];
        let m = ColumnMap::from_header_row(&headers).unwrap();
        assert_eq!(m.index("cs_de_no"), Some(0));
        assert_eq!(m.index("claim_no"), Some(1));
    }

    #[test]
    fn header_map_fails_loudly_without_claim_number() {
        let headers: Vec<String> = vec!["Country".into(), "Customer".into(), "S/N".into()];
        let err = ColumnMap::from_header_row(&headers).unwrap_err();
        assert!(err.contains("Claim Number"), "err: {}", err);
        assert!(err.contains("Zoho Ticket"), "err: {}", err);
    }

    #[test]
    fn next_claim_no_formatting_and_max() {
        let prefix = "GER2608-";
        // Empty month → 001.
        assert_eq!(next_from_existing(prefix, &[]), "GER2608-001");
        // Gaps + other-month noise ignored; max is 9 → next 010.
        let existing = vec![
            "GER2608-001".to_string(),
            "GER2608-009".to_string(),
            "GER2607-050".to_string(), // different month
            "GER2608-005".to_string(),
            "junk".to_string(),
        ];
        assert_eq!(next_from_existing(prefix, &existing), "GER2608-010");
        assert_eq!(format_claim_no(prefix, 7), "GER2608-007");
        assert_eq!(format_claim_no(prefix, 123), "GER2608-123");
    }
}
