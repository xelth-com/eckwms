//! Batch-mode summarization via Vertex AI batch prediction (~50% cheaper than
//! live `generateContent`). Runs INSIDE the existing single summarization worker
//! loop (`summarization::start_summarization_worker`) — one `batch_tick` per
//! 10s cycle, no extra tokio task, so there are no cross-task races on the DB.
//!
//! Flow per tick (managed/Vertex only — Studio always falls through to live):
//!   1. Service any in-flight/submitting `ai_batch_job` rows (one poll each).
//!   2. Orphan-guard: revert `document`s stuck in `summary_status='batched'` for
//!      >48h back to `'pending'` (backstop for a lost job row).
//!   3. If `ECK_SUMMARY_BATCH=1`, no job is already in flight, and the eligible
//!      pile is >= `ECK_SUMMARY_BATCH_MIN`, submit a batch of up to
//!      `ECK_SUMMARY_BATCH_MAX` docs. Otherwise fall through to the live path.
//!
//! ## Keying (how output rows map back to documents)
//! Batch jobs REQUIRE GCS in/out and the prediction rows come back in a DIFFERENT
//! order than submitted, so we need a stable key. PROVEN against a live trial
//! (2026-07-29): Vertex echoes the full submitted `request` verbatim in every
//! output row (only re-ordering object keys). We therefore key each row by the
//! sha256 of the CANONICAL (recursively key-sorted) serialization of its
//! `request` JSON — computed identically at submit time (stored in the job row's
//! `docs[]`) and at parse time (over the echoed `request`). The document id is
//! NEVER placed inside the prompt. Custom top-level fields were NOT used: the
//! trial carried none, so their passthrough is unproven and a rejected unknown
//! field would fail the whole job; the sha256 route is proven and cost-free.
//!
//! ## Request-shape divergences from the live payload (all REQUIRED / deliberate)
//!   (a) `contents[].role = "user"` is set EXPLICITLY. The live path relies on
//!       `AiAuth::generate_content_raw` injecting it; batch rows bypass that code.
//!   (b) `generationConfig.thinkingConfig.thinkingBudget = 0` is set explicitly —
//!       same reason (the live path injects it for Vertex, and without it the 3.5
//!       "thinking" models starve the JSON answer).
//!   (c) The `googleSearch` grounding tool is OMITTED. The live path keeps it, but
//!       grounding inside a batch job is unproven; batch summaries are therefore
//!       NOT address-grounded (the hourly geo sweep still geocodes those tickets).
//!
//! Everything else (system prompt, masked user text, temperature, maxOutputTokens)
//! is byte-identical to live via the shared `summarization` helpers, so the two
//! paths produce equivalent summaries and the completion write is shared verbatim.

use eck_core::ai::AiAuth;
use eck_core::db::SurrealDb;
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

use super::summarization;

const DEFAULT_BUCKET: &str = "eckwms-ai-batch";
const DEFAULT_MIN: usize = 10;
const DEFAULT_MAX: usize = 200;
const DEFAULT_MAX_AGE_HOURS: i64 = 24;
/// A doc claimed by a job but never resolved for this long is force-requeued —
/// backstop for a job row that was lost entirely (DB wipe, crash before create).
const ORPHAN_MAX_AGE: &str = "48h";
/// A `submitting` job row that never recorded a `job_name` this long after
/// creation is treated as a crashed submit and torn down.
const SUBMIT_STALE_AGE: &str = "10m";
/// Cadence for the maintenance block (poll in-flight jobs + orphan guard).
/// These scans are NOT per-tick work: jobs run ~7 min, the orphan threshold is
/// 48 h, and `orphan_guard`'s un-indexed UPDATE WHERE walks the whole `document`
/// table — at the 10 s tick rate that alone measurably loads the embedded
/// store (a plain full-scan count took ~9 s on the production-sized dev DB).
const MAINTENANCE_EVERY_SECS: u64 = 60;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct BatchConfig {
    enabled: bool,
    min: usize,
    max: usize,
    bucket: String,
    max_age_hours: i64,
}

impl BatchConfig {
    fn from_env() -> Self {
        Self::from_getter(|k| std::env::var(k).ok())
    }

    /// Pure parser (env access injected) so it is unit-testable without touching
    /// the process-global environment.
    fn from_getter<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let enabled = get("ECK_SUMMARY_BATCH")
            .map(|v| {
                let t = v.trim();
                t == "1" || t.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        let min = get("ECK_SUMMARY_BATCH_MIN")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_MIN);
        let max = get("ECK_SUMMARY_BATCH_MAX")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX)
            .max(1);
        let bucket = get("ECK_AI_BATCH_BUCKET")
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BUCKET.to_string());
        let max_age_hours = get("ECK_AI_BATCH_MAX_AGE_HOURS")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|h| *h > 0)
            .unwrap_or(DEFAULT_MAX_AGE_HOURS);
        BatchConfig { enabled, min, max, bucket, max_age_hours }
    }
}

// ── Job-state classification ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStateClass {
    /// Still working (PENDING/QUEUED/RUNNING/PAUSED/…) — keep polling.
    Running,
    /// Terminal success — read the output.
    Succeeded,
    /// Terminal failure (FAILED/CANCELLED/EXPIRED/CANCELLING) — revert docs.
    Failed,
    /// Unrecognised state string — treat like Running (keep polling) but log.
    Unknown,
}

fn classify_job_state(state: &str) -> JobStateClass {
    match state {
        "JOB_STATE_SUCCEEDED" => JobStateClass::Succeeded,
        "JOB_STATE_FAILED" | "JOB_STATE_CANCELLED" | "JOB_STATE_EXPIRED" | "JOB_STATE_CANCELLING" => {
            JobStateClass::Failed
        }
        "JOB_STATE_PENDING" | "JOB_STATE_QUEUED" | "JOB_STATE_RUNNING" | "JOB_STATE_PAUSED"
        | "JOB_STATE_UPDATING" | "JOB_STATE_PARTIALLY_SUCCEEDED" => JobStateClass::Running,
        _ => JobStateClass::Unknown,
    }
}

// ── Canonical request hashing (the keying primitive) ──────────────────────────

/// Deterministic JSON serialization: object keys sorted recursively, compact
/// separators. Leaf scalars go through `serde_json::to_string`, which is stable
/// (ryu float formatting), so the SAME structural value always yields the SAME
/// string regardless of the key order it arrived in. Rust-to-Rust consistent:
/// we hash our own submitted request and the echoed request with this exact fn.
fn canonical_json(v: &Value) -> String {
    let mut buf = String::new();
    write_canonical(v, &mut buf);
    buf
}

fn write_canonical(v: &Value, buf: &mut String) {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            buf.push('{');
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                buf.push_str(&serde_json::to_string(k).unwrap_or_default());
                buf.push(':');
                write_canonical(val, buf);
            }
            buf.push('}');
        }
        Value::Array(arr) => {
            buf.push('[');
            for (i, e) in arr.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                write_canonical(e, buf);
            }
            buf.push(']');
        }
        other => buf.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// sha256 (hex) of the canonical serialization of a batch-row `request`.
fn req_sha(request: &Value) -> String {
    let canon = canonical_json(request);
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    hex::encode(hasher.finalize())
}

// ── Request / response row helpers ────────────────────────────────────────────

/// Build one batch input row `{ "request": { … } }` for a document, plus its
/// `req_sha` key. Mirrors the live `summarize()` payload except for the three
/// documented divergences (explicit role, thinkingBudget=0, no googleSearch).
fn build_batch_row(system_prompt: &str, raw_text: &str) -> (Value, String) {
    let user_msg = summarization::summary_user_message(raw_text);
    let request = json!({
        "systemInstruction": { "parts": [{ "text": system_prompt }] },
        "contents": [{ "role": "user", "parts": [{ "text": user_msg }] }],
        "generationConfig": {
            "temperature": 0.2,
            "maxOutputTokens": 4096,
            "thinkingConfig": { "thinkingBudget": 0 }
        }
    });
    let sha = req_sha(&request);
    (json!({ "request": request }), sha)
}

/// First non-empty `text` part of the first candidate. Scans parts rather than
/// blindly taking `parts[0]` so a leading thought-only part (no `text`, just a
/// `thoughtSignature`) never yields an empty summary.
fn extract_response_text(response: &Value) -> Option<String> {
    let parts = response
        .get("candidates")?
        .get(0)?
        .get("content")?
        .get("parts")?
        .as_array()?;
    for p in parts {
        if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn extract_usage(response: &Value) -> Option<Value> {
    response
        .get("usageMetadata")
        .filter(|v| v.is_object())
        .cloned()
}

/// Sum a set of per-row `usageMetadata` objects into one aggregate for a single
/// authority usage report per completed job.
fn sum_usage(usages: &[Value]) -> Value {
    let mut prompt = 0i64;
    let mut candidates = 0i64;
    let mut total = 0i64;
    for u in usages {
        prompt += u.get("promptTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
        candidates += u.get("candidatesTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
        total += u.get("totalTokenCount").and_then(|v| v.as_i64()).unwrap_or(0);
    }
    json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": candidates,
        "totalTokenCount": total,
    })
}

/// `gs://<bucket>/<object-prefix>` → `<object-prefix>` (for GCS JSON-API calls,
/// which take the bare object path). `None` if the URI is for another bucket.
fn gs_uri_to_object_prefix(uri: &str, bucket: &str) -> Option<String> {
    uri.strip_prefix(&format!("gs://{bucket}/")).map(|s| s.to_string())
}

fn aiplatform_host(location: &str) -> String {
    if location == "global" {
        "aiplatform.googleapis.com".to_string()
    } else {
        format!("{location}-aiplatform.googleapis.com")
    }
}

// ── GCS JSON API (same minted bearer, cloud-platform scope) ───────────────────

async fn gcs_ensure_bucket(
    http: &HttpClient,
    bearer: &str,
    project: &str,
    bucket: &str,
) -> Result<(), anyhow::Error> {
    let url = format!("https://storage.googleapis.com/storage/v1/b?project={project}");
    let body = json!({
        "name": bucket,
        "location": "US",
        "storageClass": "STANDARD",
        "lifecycle": { "rule": [{ "action": { "type": "Delete" }, "condition": { "age": 7 } }] }
    });
    let res = http.post(&url).bearer_auth(bearer).json(&body).send().await?;
    let status = res.status();
    // 409 = already exists = fine (the shared bucket normally pre-exists).
    if status.is_success() || status.as_u16() == 409 {
        return Ok(());
    }
    let t = res.text().await.unwrap_or_default();
    anyhow::bail!("gcs bucket create failed ({status}): {t}");
}

async fn gcs_upload(
    http: &HttpClient,
    bearer: &str,
    bucket: &str,
    object: &str,
    bytes: Vec<u8>,
) -> Result<(), anyhow::Error> {
    let url = format!(
        "https://storage.googleapis.com/upload/storage/v1/b/{bucket}/o?uploadType=media&name={}",
        urlencoding::encode(object)
    );
    let res = http
        .post(&url)
        .bearer_auth(bearer)
        .header("Content-Type", "application/x-ndjson")
        .body(bytes)
        .send()
        .await?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        anyhow::bail!("gcs upload failed ({s}): {t}");
    }
    Ok(())
}

async fn gcs_read(
    http: &HttpClient,
    bearer: &str,
    bucket: &str,
    object: &str,
) -> Result<Vec<u8>, anyhow::Error> {
    let url = format!(
        "https://storage.googleapis.com/storage/v1/b/{bucket}/o/{}?alt=media",
        urlencoding::encode(object)
    );
    let res = http.get(&url).bearer_auth(bearer).send().await?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        anyhow::bail!("gcs read failed ({s}): {t}");
    }
    Ok(res.bytes().await?.to_vec())
}

/// Object names under `prefix`. One page (default 1000 items) — batch outputs are
/// a handful of shards, so pagination is not needed here.
async fn gcs_list(
    http: &HttpClient,
    bearer: &str,
    bucket: &str,
    prefix: &str,
) -> Result<Vec<String>, anyhow::Error> {
    let url = format!(
        "https://storage.googleapis.com/storage/v1/b/{bucket}/o?prefix={}",
        urlencoding::encode(prefix)
    );
    let res = http.get(&url).bearer_auth(bearer).send().await?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        anyhow::bail!("gcs list failed ({s}): {t}");
    }
    let j: Value = res.json().await?;
    Ok(j.get("items")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|it| it.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

async fn gcs_delete(
    http: &HttpClient,
    bearer: &str,
    bucket: &str,
    object: &str,
) -> Result<(), anyhow::Error> {
    let url = format!(
        "https://storage.googleapis.com/storage/v1/b/{bucket}/o/{}",
        urlencoding::encode(object)
    );
    let res = http.delete(&url).bearer_auth(bearer).send().await?;
    // 404 = already gone = fine.
    if res.status().is_success() || res.status().as_u16() == 404 {
        return Ok(());
    }
    let s = res.status();
    let t = res.text().await.unwrap_or_default();
    anyhow::bail!("gcs delete failed ({s}): {t}");
}

// ── Vertex batchPredictionJobs ────────────────────────────────────────────────

async fn vertex_submit_job(
    http: &HttpClient,
    bearer: &str,
    project: &str,
    location: &str,
    display_name: &str,
    model: &str,
    input_uri: &str,
    output_uri_prefix: &str,
) -> Result<(String, String), anyhow::Error> {
    let host = aiplatform_host(location);
    let url = format!("https://{host}/v1/projects/{project}/locations/{location}/batchPredictionJobs");
    let body = json!({
        "displayName": display_name,
        "model": format!("publishers/google/models/{model}"),
        "inputConfig": { "instancesFormat": "jsonl", "gcsSource": { "uris": [input_uri] } },
        "outputConfig": {
            "predictionsFormat": "jsonl",
            "gcsDestination": { "outputUriPrefix": output_uri_prefix }
        }
    });
    let res = http.post(&url).bearer_auth(bearer).json(&body).send().await?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        anyhow::bail!("vertex batch submit failed ({s}): {t}");
    }
    let j: Value = res.json().await?;
    let name = j
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("batch submit response missing `name`"))?
        .to_string();
    let state = j
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("JOB_STATE_PENDING")
        .to_string();
    Ok((name, state))
}

async fn vertex_get_job(
    http: &HttpClient,
    bearer: &str,
    location: &str,
    job_name: &str,
) -> Result<Value, anyhow::Error> {
    let host = aiplatform_host(location);
    let url = format!("https://{host}/v1/{job_name}");
    let res = http.get(&url).bearer_auth(bearer).send().await?;
    if !res.status().is_success() {
        let s = res.status();
        let t = res.text().await.unwrap_or_default();
        anyhow::bail!("vertex batch poll failed ({s}): {t}");
    }
    Ok(res.json().await?)
}

/// Best-effort cancel (age-cap path). Never fatal: a job that already went
/// terminal returns 400, which is fine — we tear the row down regardless.
async fn vertex_cancel_job(http: &HttpClient, bearer: &str, location: &str, job_name: &str) {
    let host = aiplatform_host(location);
    let url = format!("https://{host}/v1/{job_name}:cancel");
    match http.post(&url).bearer_auth(bearer).json(&json!({})).send().await {
        Ok(r) if r.status().is_success() => {}
        Ok(r) => warn!("[SummarizationBatch] cancel {job_name} returned {}", r.status()),
        Err(e) => warn!("[SummarizationBatch] cancel {job_name} error: {e}"),
    }
}

// ── DB helpers ────────────────────────────────────────────────────────────────

struct BatchDoc {
    doc_id: String,
    req_sha: String,
}

/// Count `document`s eligible for summarization RIGHT NOW (same WHERE as live).
/// BOUNDED eligibility probe: "are at least `min` docs eligible right now?".
/// Deliberately NOT a full `count() GROUP ALL` — that walks every `document`
/// row (heavy fields included) and took ~9 s on the production-sized dev store;
/// paying that every 10 s tick starves the embedded single-writer DB. A
/// LIMIT'd id-projection answers the only question the gate asks.
async fn min_eligible_reached(
    db: &SurrealDb,
    instance_id: &str,
    min: usize,
) -> Result<bool, anyhow::Error> {
    let where_clause = summarization::eligibility_where(instance_id);
    let rows: Vec<Value> = db
        .query(&format!(
            "SELECT record::id(id) AS id FROM document WHERE {where_clause} LIMIT {}",
            min.max(1)
        ))
        .await?
        .take(0)?;
    Ok(rows.len() >= min.max(1))
}

async fn active_job_exists(db: &SurrealDb) -> Result<bool, anyhow::Error> {
    let rows: Vec<Value> = db
        .query("SELECT count() AS n FROM ai_batch_job WHERE state IN ['submitting', 'in_flight'] GROUP ALL")
        .await?
        .take(0)?;
    let n = rows.first().and_then(|r| r.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);
    Ok(n > 0)
}

/// Backstop for docs stranded in `'batched'` (job row lost). SurrealDB v3: SET is
/// evaluated before WHERE, so the guard uses a total `updated_at` expression
/// (`type::is_datetime` gate) and an exact WHERE, checks statement errors, and
/// RETURNs the ids to log the count.
async fn orphan_guard(db: &SurrealDb) -> Result<(), anyhow::Error> {
    let reverted: Vec<Value> = db
        .query(&format!(
            "UPDATE document SET summary_status = 'pending', updated_at = time::now() \
             WHERE summary_status = 'batched' \
             AND type::is_datetime(updated_at) AND updated_at < time::now() - {ORPHAN_MAX_AGE} \
             RETURN record::id(id) AS id"
        ))
        .await?
        .check()?
        .take(0)?;
    if !reverted.is_empty() {
        warn!(
            "[SummarizationBatch] orphan guard requeued {} stranded 'batched' doc(s)",
            reverted.len()
        );
    }
    Ok(())
}

/// Revert claimed docs from `'batched'` back to `'pending'`. `bump_retry=false`
/// is an infra failure (submit crash) — no doc penalty; `true` is a doc/job
/// failure — increment `summary_retries` (feeds the exponential backoff) and
/// record `summary_error`. Only touches rows still in `'batched'` (exact WHERE).
async fn revert_docs(
    db: &SurrealDb,
    doc_ids: &[String],
    bump_retry: bool,
    error: Option<&str>,
) -> Result<(), anyhow::Error> {
    for id in doc_ids {
        if bump_retry {
            let _: Vec<Value> = db
                .query(
                    "UPDATE document SET summary_status = 'pending', \
                         summary_retries = (summary_retries ?? 0) + 1, \
                         summary_error = $err, updated_at = time::now() \
                     WHERE record::id(id) = $id AND summary_status = 'batched' \
                     RETURN record::id(id) AS id",
                )
                .bind(("id", id.clone()))
                .bind(("err", error.unwrap_or("batch revert").to_string()))
                .await?
                .check()?
                .take(0)?;
        } else {
            let _: Vec<Value> = db
                .query(
                    "UPDATE document SET summary_status = 'pending', summary_error = NONE, \
                         updated_at = time::now() \
                     WHERE record::id(id) = $id AND summary_status = 'batched' \
                     RETURN record::id(id) AS id",
                )
                .bind(("id", id.clone()))
                .await?
                .check()?
                .take(0)?;
        }
    }
    Ok(())
}

async fn mark_job_failed(db: &SurrealDb, row_id: &str, error: &str) -> Result<(), anyhow::Error> {
    db.query(
        "UPDATE ai_batch_job SET state = 'failed', error = $err, finished_at = time::now() \
         WHERE record::id(id) = $rid RETURN NONE",
    )
    .bind(("err", error.to_string()))
    .bind(("rid", row_id.to_string()))
    .await?
    .check()?;
    Ok(())
}

/// Masked subject per doc (for the fingerprint index at completion time).
async fn fetch_subjects(
    db: &SurrealDb,
    ids: &[String],
) -> Result<HashMap<String, String>, anyhow::Error> {
    let mut map = HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    let rows: Vec<Value> = db
        .query("SELECT record::id(id) AS id, meta.subject AS subject FROM document WHERE record::id(id) IN $ids")
        .bind(("ids", ids.to_vec()))
        .await?
        .take(0)?;
    for r in rows {
        if let Some(id) = r.get("id").and_then(|v| v.as_str()) {
            let subj = r.get("subject").and_then(|v| v.as_str()).unwrap_or("").to_string();
            map.insert(id.to_string(), subj);
        }
    }
    Ok(map)
}

// ── Submit ────────────────────────────────────────────────────────────────────

/// Select → build → claim → create-job-row → upload → submit, in a crash-safe
/// order. Returns `Ok(true)` when a job actually went in flight (the caller
/// skips the live path this tick), `Ok(false)` when nothing was submitted
/// (empty/duplicate/lost-race/infra-failure — the docs, if claimed, are reverted).
#[allow(clippy::too_many_arguments)]
async fn submit_batch(
    db: &SurrealDb,
    http: &HttpClient,
    bearer: &str,
    project: &str,
    location: &str,
    model: &str,
    instance_id: &str,
    cfg: &BatchConfig,
) -> Result<bool, anyhow::Error> {
    // 1. Select up to MAX eligible docs (same eligibility as live).
    let where_clause = summarization::eligibility_where(instance_id);
    let docs: Vec<Value> = db
        .query(&format!(
            "SELECT record::id(id) AS id, type, meta FROM document WHERE {where_clause} LIMIT {}",
            cfg.max
        ))
        .await?
        .take(0)?;
    if docs.is_empty() {
        return Ok(false);
    }

    // 2. Build masked texts + rows + keys. Skip unknown types and empty-text docs
    //    (same 'skipped' handling as live). Drop duplicate request hashes — an
    //    identical request would be an ambiguous key at completion.
    let mut rows: Vec<Value> = Vec::new();
    let mut pending: Vec<BatchDoc> = Vec::new();
    let mut sha_seen: HashSet<String> = HashSet::new();
    for doc in &docs {
        let id = match doc.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let (text, prompt) = match summarization::build_doc_masked_text(db, doc, &id).await? {
            Some(t) => t,
            None => continue, // unknown type
        };
        if text.is_empty() {
            db.query("UPDATE type::record($rid) SET summary_status = 'skipped', summary_retries = (summary_retries ?? 0) + 1, updated_at = time::now()")
                .bind(("rid", format!("document:`{}`", id)))
                .await?
                .check()?;
            continue;
        }
        let (row, sha) = build_batch_row(prompt, &text);
        if !sha_seen.insert(sha.clone()) {
            warn!("[SummarizationBatch] duplicate request hash for {id} — leaving pending (ambiguous key)");
            continue;
        }
        rows.push(row);
        pending.push(BatchDoc { doc_id: id, req_sha: sha });
    }
    // A 1-doc batch waits ~7 min for a single summary the live path does in
    // seconds — not worth it; let live handle a trivially small survivor set.
    if pending.len() < 2 {
        return Ok(false);
    }

    // 3. Claim each doc pending → batched; keep ONLY confirmed rows (a peer/live
    //    tick may have grabbed some). summary_status is merkle-IGNORED; the
    //    updated_at bump makes the 48h orphan guard measure time-since-claim.
    let mut confirmed: Vec<BatchDoc> = Vec::new();
    let mut confirmed_rows: Vec<Value> = Vec::new();
    for (bd, row) in pending.into_iter().zip(rows.into_iter()) {
        let claimed: Vec<Value> = db
            .query(
                "UPDATE document SET summary_status = 'batched', updated_at = time::now() \
                 WHERE record::id(id) = $id AND summary_status = 'pending' \
                 RETURN record::id(id) AS id",
            )
            .bind(("id", bd.doc_id.clone()))
            .await?
            .check()?
            .take(0)?;
        if !claimed.is_empty() {
            confirmed.push(bd);
            confirmed_rows.push(row);
        }
    }
    if confirmed.len() < 2 {
        // Lost the race for all but one — revert the stragglers (no penalty).
        let ids: Vec<String> = confirmed.iter().map(|d| d.doc_id.clone()).collect();
        let _ = revert_docs(db, &ids, false, None).await;
        return Ok(false);
    }

    // 4. Object layout + job row FIRST (state='submitting'), so a crash before
    //    the Vertex job exists still leaves a recoverable record.
    let token = uuid::Uuid::new_v4().to_string();
    let stem = format!("batch/{instance_id}/{token}");
    let input_object = format!("{stem}/input.jsonl");
    let output_prefix = format!("{stem}/out/");
    let input_uri = format!("gs://{}/{}", cfg.bucket, input_object);
    let output_uri = format!("gs://{}/{}", cfg.bucket, output_prefix);

    let docs_json: Vec<Value> = confirmed
        .iter()
        .map(|d| json!({ "doc_id": d.doc_id, "req_sha": d.req_sha }))
        .collect();
    let created: Vec<Value> = db
        .query(
            "CREATE ai_batch_job CONTENT { state: 'submitting', model: $model, docs: $docs, \
                 input_object: $input_object, output_prefix: $output_prefix, \
                 submitted_at: time::now(), usage_reported: false } \
             RETURN record::id(id) AS id",
        )
        .bind(("model", model.to_string()))
        .bind(("docs", Value::Array(docs_json)))
        .bind(("input_object", input_object.clone()))
        .bind(("output_prefix", output_prefix.clone()))
        .await?
        .check()?
        .take(0)?;
    let row_id = created
        .first()
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("failed to create ai_batch_job row"))?;

    // 5. Side effects: ensure bucket → upload input → create job. On ANY failure,
    //    revert claimed docs (NO retry bump — infra, not the docs' fault), delete
    //    the uploaded object, mark the job row failed.
    let jsonl = confirmed_rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let display = format!("eck-summary-{token}");
    let submit_result: Result<(String, String), anyhow::Error> = async {
        gcs_ensure_bucket(http, bearer, project, &cfg.bucket).await?;
        gcs_upload(http, bearer, &cfg.bucket, &input_object, jsonl.into_bytes()).await?;
        vertex_submit_job(http, bearer, project, location, &display, model, &input_uri, &output_uri).await
    }
    .await;

    match submit_result {
        Ok((job_name, state)) => {
            db.query(
                "UPDATE ai_batch_job SET job_name = $name, state = 'in_flight' \
                 WHERE record::id(id) = $rid RETURN NONE",
            )
            .bind(("name", job_name.clone()))
            .bind(("rid", row_id.clone()))
            .await?
            .check()?;
            info!(
                "[SummarizationBatch] submitted job {job_name} ({} docs, state={state}, model={model})",
                confirmed.len()
            );
            Ok(true)
        }
        Err(e) => {
            warn!(
                "[SummarizationBatch] submit failed ({e}) — reverting {} docs (no retry bump)",
                confirmed.len()
            );
            let ids: Vec<String> = confirmed.iter().map(|d| d.doc_id.clone()).collect();
            let _ = revert_docs(db, &ids, false, None).await;
            let _ = gcs_delete(http, bearer, &cfg.bucket, &input_object).await;
            let _ = mark_job_failed(db, &row_id, &format!("submit failed: {e}")).await;
            Ok(false)
        }
    }
}

// ── Service (poll + complete/fail) ────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn service_jobs(
    db: &SurrealDb,
    http: &HttpClient,
    bearer: &str,
    location: &str,
    model: &str,
    instance_id: &str,
    cfg: &BatchConfig,
) -> Result<(), anyhow::Error> {
    // `over_age` / `submit_stale` computed in the DB to avoid parsing datetimes
    // in Rust (max_age is a trusted integer from config).
    let jobs: Vec<Value> = db
        .query(&format!(
            "SELECT record::id(id) AS row_id, job_name, state, docs, input_object, output_prefix, usage_reported, \
                (type::is_datetime(submitted_at) AND submitted_at < time::now() - {age}h) AS over_age, \
                (type::is_datetime(submitted_at) AND submitted_at < time::now() - {stale}) AS submit_stale \
             FROM ai_batch_job WHERE state IN ['submitting', 'in_flight']",
            age = cfg.max_age_hours,
            stale = SUBMIT_STALE_AGE
        ))
        .await?
        .take(0)?;
    for job in &jobs {
        if let Err(e) = service_one_job(db, http, bearer, location, model, instance_id, cfg, job).await {
            let row_id = job.get("row_id").and_then(|v| v.as_str()).unwrap_or("?");
            warn!("[SummarizationBatch] servicing job {row_id} failed: {e}");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn service_one_job(
    db: &SurrealDb,
    http: &HttpClient,
    bearer: &str,
    location: &str,
    model: &str,
    instance_id: &str,
    cfg: &BatchConfig,
    job: &Value,
) -> Result<(), anyhow::Error> {
    let row_id = job.get("row_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let doc_ids: Vec<String> = job
        .get("docs")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.get("doc_id").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let job_name = job.get("job_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);

    // 'submitting' with no job_name = a submit that crashed before the job was
    // created. Tear it down once it is stale (docs revert with no penalty).
    let job_name = match job_name {
        Some(n) => n,
        None => {
            if job.get("submit_stale").and_then(|v| v.as_bool()).unwrap_or(false) {
                warn!("[SummarizationBatch] stale 'submitting' job {row_id} with no job_name — reverting {} docs", doc_ids.len());
                revert_docs(db, &doc_ids, false, None).await?;
                mark_job_failed(db, &row_id, "submit never recorded a job_name").await?;
            }
            return Ok(());
        }
    };

    let job_res = vertex_get_job(http, bearer, location, &job_name).await?;
    let vstate = job_res.get("state").and_then(|v| v.as_str()).unwrap_or("");
    match classify_job_state(vstate) {
        JobStateClass::Succeeded => {
            complete_job(db, http, bearer, &cfg.bucket, model, instance_id, &row_id, &job_res, job, &doc_ids).await?;
        }
        JobStateClass::Failed => {
            let err = job_res
                .get("error")
                .map(|e| e.to_string())
                .unwrap_or_else(|| vstate.to_string());
            warn!("[SummarizationBatch] job {job_name} terminal-failed ({vstate}) — reverting {} docs", doc_ids.len());
            revert_docs(db, &doc_ids, true, Some(&format!("batch {vstate}: {err}"))).await?;
            if let Some(obj) = job.get("input_object").and_then(|v| v.as_str()) {
                let _ = gcs_delete(http, bearer, &cfg.bucket, obj).await;
            }
            mark_job_failed(db, &row_id, &format!("{vstate}: {err}")).await?;
        }
        JobStateClass::Running | JobStateClass::Unknown => {
            if job.get("over_age").and_then(|v| v.as_bool()).unwrap_or(false) {
                warn!("[SummarizationBatch] job {job_name} exceeded max age {}h — cancelling + reverting", cfg.max_age_hours);
                vertex_cancel_job(http, bearer, location, &job_name).await;
                revert_docs(db, &doc_ids, true, Some("batch exceeded max age; cancelled")).await?;
                if let Some(obj) = job.get("input_object").and_then(|v| v.as_str()) {
                    let _ = gcs_delete(http, bearer, &cfg.bucket, obj).await;
                }
                mark_job_failed(db, &row_id, "exceeded max age; cancelled").await?;
            } else if matches!(classify_job_state(vstate), JobStateClass::Unknown) {
                warn!("[SummarizationBatch] job {job_name} unknown state '{vstate}' — will keep polling");
            }
        }
    }
    Ok(())
}

/// Read a SUCCEEDED job's output, map rows back to docs by `req_sha`, write each
/// summary via the shared `write_summary_completion`, meter once, clean up GCS.
#[allow(clippy::too_many_arguments)]
async fn complete_job(
    db: &SurrealDb,
    http: &HttpClient,
    bearer: &str,
    bucket: &str,
    model: &str,
    instance_id: &str,
    row_id: &str,
    job_res: &Value,
    job_row: &Value,
    all_doc_ids: &[String],
) -> Result<(), anyhow::Error> {
    // Idempotency: if a prior run already metered (usage_reported=true) but
    // crashed before marking 'done', don't re-summarize or re-bill — just finish.
    let already_reported = job_row.get("usage_reported").and_then(|v| v.as_bool()).unwrap_or(false);

    // req_sha → doc_id, from the job row we stored at submit time.
    let mut doc_map: HashMap<String, String> = HashMap::new();
    if let Some(arr) = job_row.get("docs").and_then(|v| v.as_array()) {
        for d in arr {
            if let (Some(sha), Some(id)) = (
                d.get("req_sha").and_then(|v| v.as_str()),
                d.get("doc_id").and_then(|v| v.as_str()),
            ) {
                doc_map.insert(sha.to_string(), id.to_string());
            }
        }
    }

    // Resolve the output directory: prefer the job's own reported dir; fall back
    // to the prefix we asked for.
    let out_prefix = job_res
        .get("outputInfo")
        .and_then(|o| o.get("gcsOutputDirectory"))
        .and_then(|v| v.as_str())
        .and_then(|uri| gs_uri_to_object_prefix(uri, bucket))
        .or_else(|| job_row.get("output_prefix").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();

    // Read ALL shards fully BEFORE any DB writes, so a read failure = a clean,
    // fully-repeatable retry (nothing written yet).
    let objects = gcs_list(http, bearer, bucket, &out_prefix).await?;
    let shards: Vec<String> = objects.iter().filter(|o| o.ends_with(".jsonl")).cloned().collect();
    let mut parsed_rows: Vec<Value> = Vec::new();
    for shard in &shards {
        let bytes = gcs_read(http, bearer, bucket, shard).await?;
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(v) => parsed_rows.push(v),
                Err(e) => warn!("[SummarizationBatch] job {row_id}: unparseable output line: {e}"),
            }
        }
    }

    let subjects = fetch_subjects(db, all_doc_ids).await?;

    let mut usages: Vec<Value> = Vec::new();
    let mut completed: HashSet<String> = HashSet::new();
    let mut errored: Vec<(String, String)> = Vec::new();
    for row in &parsed_rows {
        let request = match row.get("request") {
            Some(r) => r,
            None => {
                warn!("[SummarizationBatch] job {row_id}: output row missing `request` — cannot key");
                continue;
            }
        };
        let sha = req_sha(request);
        let doc_id = match doc_map.get(&sha) {
            Some(d) => d.clone(),
            None => {
                warn!("[SummarizationBatch] job {row_id}: output key {} not in job docs", &sha[..16.min(sha.len())]);
                continue;
            }
        };
        let response = row.get("response");
        let row_usage = response.and_then(extract_usage);
        if let Some(u) = &row_usage {
            usages.push(u.clone()); // aggregate ALL rows' usage
        }
        let row_status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let text_opt = response.and_then(extract_response_text).filter(|t| !t.is_empty());
        match (row_status.is_empty(), text_opt) {
            (true, Some(summary)) => {
                if already_reported {
                    // Prior run already wrote this; just remember it as done.
                    completed.insert(doc_id);
                    continue;
                }
                let subject = subjects.get(&doc_id).cloned().unwrap_or_default();
                let matched = summarization::write_summary_completion(db, &doc_id, &subject, &summary, instance_id).await?;
                if matched {
                    if let Some(u) = &row_usage {
                        super::telemetry::log_telemetry(db, "summarization_batch", model, &doc_id, u).await;
                    }
                    completed.insert(doc_id);
                }
            }
            _ => {
                let err = if !row_status.is_empty() {
                    format!("batch row status: {row_status}")
                } else {
                    "batch response had no text".to_string()
                };
                errored.push((doc_id, err));
            }
        }
    }

    // Docs claimed but absent from the output entirely → requeue with a penalty.
    let mut missing: Vec<String> = Vec::new();
    for id in all_doc_ids {
        if !completed.contains(id) && !errored.iter().any(|(d, _)| d == id) {
            missing.push(id.clone());
        }
    }

    if !already_reported {
        for (id, err) in &errored {
            revert_docs(db, std::slice::from_ref(id), true, Some(err)).await?;
        }
        if !missing.is_empty() {
            revert_docs(db, &missing, true, Some("doc missing from batch output")).await?;
        }
        // Aggregate ALL rows' usage → ONE authority report (model tagged `-batch`).
        if !usages.is_empty() {
            let summed = sum_usage(&usages);
            eck_core::ai::report_managed_usage(&format!("{model}-batch"), "summarize_batch", &summed);
        }
        db.query("UPDATE ai_batch_job SET usage_reported = true WHERE record::id(id) = $rid RETURN NONE")
            .bind(("rid", row_id.to_string()))
            .await?
            .check()?;
    }

    // Delete input + every output object (lifecycle rule is only the backstop).
    if let Some(obj) = job_row.get("input_object").and_then(|v| v.as_str()) {
        let _ = gcs_delete(http, bearer, bucket, obj).await;
    }
    if let Ok(all_out) = gcs_list(http, bearer, bucket, &out_prefix).await {
        for obj in all_out {
            let _ = gcs_delete(http, bearer, bucket, &obj).await;
        }
    }

    db.query("UPDATE ai_batch_job SET state = 'done', finished_at = time::now() WHERE record::id(id) = $rid RETURN NONE")
        .bind(("rid", row_id.to_string()))
        .await?
        .check()?;
    info!(
        "[SummarizationBatch] job {row_id} done: {} summarized, {} reverted, {} rows metered",
        completed.len(),
        errored.len() + missing.len(),
        usages.len()
    );
    Ok(())
}

// ── Tick entry ────────────────────────────────────────────────────────────────

/// Called once per summarization worker cycle. Returns `Ok(true)` when a batch
/// was submitted this tick (skip the live path — the claimed docs left the
/// pending pool); `Ok(false)` to fall through to the live per-doc path. Studio
/// auth, batch disabled, or a full pipeline all yield `false`.
/// Idempotently create the node-local job table. Without this, the very first
/// `SELECT FROM ai_batch_job` errors ("The table does not exist" — SurrealDB v3
/// treats a missing table as a statement error, NOT an empty result), which
/// killed every batch_tick before it could ever reach the submit that would
/// have created the table — a chicken-and-egg that kept batch mode dead.
async fn ensure_job_table(db: &SurrealDb) -> Result<(), anyhow::Error> {
    db.query("DEFINE TABLE IF NOT EXISTS ai_batch_job SCHEMALESS")
        .await?
        .check()?;
    Ok(())
}

/// True once per [`MAINTENANCE_EVERY_SECS`] window, process-wide. The first
/// call after startup fires immediately (recovers in-flight jobs from a
/// restart without waiting a minute).
fn maintenance_due() -> bool {
    use std::sync::Mutex;
    use std::time::Instant;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    match *last {
        Some(t) if t.elapsed().as_secs() < MAINTENANCE_EVERY_SECS => false,
        _ => {
            *last = Some(Instant::now());
            true
        }
    }
}

pub(crate) async fn batch_tick(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &AiAuth,
    model: &str,
    instance_id: &str,
) -> Result<bool, anyhow::Error> {
    // Batch is a Vertex/managed-only feature — Studio has no batch endpoint.
    let (bearer, project, location) = match auth {
        AiAuth::Vertex { bearer, project, location } => (bearer.as_str(), project.as_str(), location.as_str()),
        _ => return Ok(false),
    };
    let cfg = BatchConfig::from_env();

    // Maintenance (poll in-flight jobs, orphan-guard stranded docs) runs on its
    // own slower cadence — jobs take minutes and the orphan threshold is 48 h,
    // while orphan_guard's un-indexed UPDATE walks the whole document table.
    // It runs even with submissions disabled: a job started while enabled must
    // still drain after the flag is turned off.
    if maintenance_due() {
        ensure_job_table(db).await?;
        service_jobs(db, http, bearer, location, model, instance_id, &cfg).await?;
        orphan_guard(db).await?;
    }

    if !cfg.enabled {
        return Ok(false);
    }
    // One batch at a time: a job already in flight → live handles the trickle.
    // (ai_batch_job is tiny — this stays per-tick.)
    if active_job_exists(db).await? {
        return Ok(false);
    }
    // Bounded probe, not a full count — see min_eligible_reached.
    if !min_eligible_reached(db, instance_id, cfg.min).await? {
        return Ok(false);
    }
    submit_batch(db, http, bearer, project, location, model, instance_id, &cfg).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // The REAL trial artifacts downloaded from gs://eckwms-ai-batch/trial/ on
    // 2026-07-29 (2-row probe). Used to prove the keying end-to-end against
    // genuine Vertex batch output — the echoed `request` is verbatim-but-reordered.
    const TRIAL_INPUT: &str = r#"{"request": {"systemInstruction": {"parts": [{"text": "You summarize support tickets. Reply with 2 short sentences: what the customer wants and the current state. No preamble."}]}, "contents": [{"role": "user", "parts": [{"text": "Ticket 1: Customer reports the scale shows error E-12 after moving it to a new room. They ask whether a service visit is needed."}]}], "generationConfig": {"temperature": 0.2, "maxOutputTokens": 256}}}
{"request": {"systemInstruction": {"parts": [{"text": "You summarize support tickets. Reply with 2 short sentences: what the customer wants and the current state. No preamble."}]}, "contents": [{"role": "user", "parts": [{"text": "Ticket 2: Customer asks for a cost estimate to repair a broken display and whether they can bring the device in themselves to save shipping."}]}], "generationConfig": {"temperature": 0.2, "maxOutputTokens": 256}}}"#;

    // NOTE: prediction rows come back in REVERSED order (row 0 = Ticket 2).
    const TRIAL_PREDICTIONS: &str = r#"{"request":{"contents":[{"parts":[{"text":"Ticket 2: Customer asks for a cost estimate to repair a broken display and whether they can bring the device in themselves to save shipping."}],"role":"user"}],"generationConfig":{"maxOutputTokens":256,"temperature":0.2},"systemInstruction":{"parts":[{"text":"You summarize support tickets. Reply with 2 short sentences: what the customer wants and the current state. No preamble."}]}},"status":"","response":{"candidates":[{"content":{"parts":[{"text":"The customer wants a repair cost estimate for a broken display and the option for in-person drop-off. The request is currently open and awaiting a response.","thoughtSignature":"AY89a1+//2DVPjpp3VLkVlBZY6WGMUQ4StEmCvKtthpefJnGqRH36zJN4GuqY+IhHQE="}],"role":"model"},"finishReason":"STOP"}],"createTime":"2026-07-29T15:23:45.074204Z","modelVersion":"gemini-3.5-flash-lite","responseId":"ARtqatzDBKHw67MP-uLl8A0","usageMetadata":{"candidatesTokenCount":32,"candidatesTokensDetails":[{"modality":"TEXT","tokenCount":32}],"promptTokenCount":52,"promptTokensDetails":[{"modality":"TEXT","tokenCount":52}],"totalTokenCount":84,"trafficType":"ON_DEMAND"}},"processed_time":"2026-07-29T15:23:47.171831+00:00"}
{"request":{"contents":[{"parts":[{"text":"Ticket 1: Customer reports the scale shows error E-12 after moving it to a new room. They ask whether a service visit is needed."}],"role":"user"}],"generationConfig":{"maxOutputTokens":256,"temperature":0.2},"systemInstruction":{"parts":[{"text":"You summarize support tickets. Reply with 2 short sentences: what the customer wants and the current state. No preamble."}]}},"status":"","response":{"candidates":[{"content":{"parts":[{"text":"The customer wants to know if a service visit is required to fix error E-12 after moving the scale. The issue is currently unresolved and awaiting a response.","thoughtSignature":"AY89a1/9jGFYeno/3jViPPQNZcrW+iBunRVOpjFlRE8YFVP3LTqGZLm7mSVWTcjmsLo="}],"role":"model"},"finishReason":"STOP"}],"createTime":"2026-07-29T15:23:45.085026Z","modelVersion":"gemini-3.5-flash-lite","responseId":"ARtqaqKYBa61j8MPjJnGuAM","usageMetadata":{"candidatesTokenCount":33,"candidatesTokensDetails":[{"modality":"TEXT","tokenCount":33}],"promptTokenCount":55,"promptTokensDetails":[{"modality":"TEXT","tokenCount":55}],"totalTokenCount":88,"trafficType":"ON_DEMAND"}},"processed_time":"2026-07-29T15:23:46.180505+00:00"}"#;

    fn parse_lines(s: &str) -> Vec<Value> {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<Value>(l).expect("fixture line is valid JSON"))
            .collect()
    }

    #[test]
    fn config_defaults_when_env_absent() {
        let cfg = BatchConfig::from_getter(|_| None);
        assert!(!cfg.enabled);
        assert_eq!(cfg.min, DEFAULT_MIN);
        assert_eq!(cfg.max, DEFAULT_MAX);
        assert_eq!(cfg.bucket, DEFAULT_BUCKET);
        assert_eq!(cfg.max_age_hours, DEFAULT_MAX_AGE_HOURS);
    }

    #[test]
    fn config_parses_and_sanitises_env() {
        let map: HashMap<&str, &str> = [
            ("ECK_SUMMARY_BATCH", "1"),
            ("ECK_SUMMARY_BATCH_MIN", "5"),
            ("ECK_SUMMARY_BATCH_MAX", "50"),
            ("ECK_AI_BATCH_BUCKET", "my-bucket"),
            ("ECK_AI_BATCH_MAX_AGE_HOURS", "12"),
        ]
        .into_iter()
        .collect();
        let cfg = BatchConfig::from_getter(|k| map.get(k).map(|s| s.to_string()));
        assert!(cfg.enabled);
        assert_eq!(cfg.min, 5);
        assert_eq!(cfg.max, 50);
        assert_eq!(cfg.bucket, "my-bucket");
        assert_eq!(cfg.max_age_hours, 12);

        // Truthy variants + garbage/zero fall back to safe defaults.
        let t = BatchConfig::from_getter(|k| if k == "ECK_SUMMARY_BATCH" { Some("TRUE".into()) } else { None });
        assert!(t.enabled);
        let bad = BatchConfig::from_getter(|k| match k {
            "ECK_SUMMARY_BATCH" => Some("0".into()),
            "ECK_SUMMARY_BATCH_MAX" => Some("nonsense".into()),
            "ECK_AI_BATCH_MAX_AGE_HOURS" => Some("0".into()),
            _ => None,
        });
        assert!(!bad.enabled);
        assert_eq!(bad.max, DEFAULT_MAX);
        assert_eq!(bad.max_age_hours, DEFAULT_MAX_AGE_HOURS);
    }

    #[test]
    fn job_state_classification() {
        assert_eq!(classify_job_state("JOB_STATE_SUCCEEDED"), JobStateClass::Succeeded);
        assert_eq!(classify_job_state("JOB_STATE_RUNNING"), JobStateClass::Running);
        assert_eq!(classify_job_state("JOB_STATE_QUEUED"), JobStateClass::Running);
        assert_eq!(classify_job_state("JOB_STATE_FAILED"), JobStateClass::Failed);
        assert_eq!(classify_job_state("JOB_STATE_CANCELLED"), JobStateClass::Failed);
        assert_eq!(classify_job_state("JOB_STATE_EXPIRED"), JobStateClass::Failed);
        assert_eq!(classify_job_state("WAT"), JobStateClass::Unknown);
    }

    #[test]
    fn canonical_json_is_key_order_independent() {
        let a = json!({ "b": 1, "a": { "y": 2, "x": [3, 4] } });
        let b = json!({ "a": { "x": [3, 4], "y": 2 }, "b": 1 });
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(req_sha(&a), req_sha(&b));
        // Array order is significant (it carries meaning), so it must NOT match.
        let c = json!({ "a": { "x": [4, 3], "y": 2 }, "b": 1 });
        assert_ne!(req_sha(&a), req_sha(&c));
    }

    #[test]
    fn build_batch_row_shape_and_divergences() {
        let (row, sha) = build_batch_row("SYS PROMPT", "MASKED TEXT");
        let req = &row["request"];
        // (a) explicit role=user
        assert_eq!(req["contents"][0]["role"], json!("user"));
        // user message uses the shared live wrapper
        let user_text = req["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(user_text.contains("MASKED TEXT"));
        assert_eq!(user_text, summarization::summary_user_message("MASKED TEXT"));
        // (b) thinkingBudget=0 present
        assert_eq!(req["generationConfig"]["thinkingConfig"]["thinkingBudget"], json!(0));
        // live generation knobs preserved
        assert_eq!(req["generationConfig"]["temperature"], json!(0.2));
        assert_eq!(req["generationConfig"]["maxOutputTokens"], json!(4096));
        // system prompt carried
        assert_eq!(req["systemInstruction"]["parts"][0]["text"], json!("SYS PROMPT"));
        // (c) NO googleSearch tool in a batch row
        assert!(req.get("tools").is_none(), "batch rows must omit the googleSearch tool");
        // the returned sha is exactly req_sha of the inner request
        assert_eq!(sha, req_sha(req));
    }

    #[test]
    fn keying_maps_real_trial_rows_back_to_inputs() {
        // Simulate submit-time keying: sha(request) -> synthetic doc id, in input order.
        let inputs = parse_lines(TRIAL_INPUT);
        assert_eq!(inputs.len(), 2);
        let mut doc_map: HashMap<String, String> = HashMap::new();
        doc_map.insert(req_sha(&inputs[0]["request"]), "doc_ticket1".to_string());
        doc_map.insert(req_sha(&inputs[1]["request"]), "doc_ticket2".to_string());
        // Two distinct requests => two distinct keys (no accidental collision).
        assert_eq!(doc_map.len(), 2);

        // Parse-time: key each echoed prediction row back to its doc.
        let preds = parse_lines(TRIAL_PREDICTIONS);
        assert_eq!(preds.len(), 2);

        // Row 0 is Ticket 2 (order is reversed) — keying must still resolve it.
        let doc0 = doc_map.get(&req_sha(&preds[0]["request"])).expect("row 0 keyed");
        let doc1 = doc_map.get(&req_sha(&preds[1]["request"])).expect("row 1 keyed");
        assert_eq!(doc0, "doc_ticket2");
        assert_eq!(doc1, "doc_ticket1");

        // Text + usage extraction off the real response shape (text part also
        // carries a thoughtSignature — extractor must still find the text).
        let t0 = extract_response_text(&preds[0]["response"]).expect("text 0");
        assert!(t0.contains("repair cost estimate"));
        let u0 = extract_usage(&preds[0]["response"]).expect("usage 0");
        assert_eq!(u0["totalTokenCount"], json!(84));
    }

    #[test]
    fn usage_aggregation_sums_all_rows() {
        let preds = parse_lines(TRIAL_PREDICTIONS);
        let usages: Vec<Value> = preds
            .iter()
            .filter_map(|r| extract_usage(&r["response"]))
            .collect();
        assert_eq!(usages.len(), 2);
        let summed = sum_usage(&usages);
        // 52+55 prompt, 32+33 candidates, 84+88 total.
        assert_eq!(summed["promptTokenCount"], json!(107));
        assert_eq!(summed["candidatesTokenCount"], json!(65));
        assert_eq!(summed["totalTokenCount"], json!(172));
    }

    #[test]
    fn gs_uri_prefix_stripping() {
        assert_eq!(
            gs_uri_to_object_prefix("gs://eckwms-ai-batch/batch/x/out/pred/", "eckwms-ai-batch"),
            Some("batch/x/out/pred/".to_string())
        );
        assert_eq!(gs_uri_to_object_prefix("gs://other-bucket/x", "eckwms-ai-batch"), None);
    }

    #[test]
    fn extract_text_skips_thought_only_leading_part() {
        // A leading part with only a thoughtSignature must be skipped.
        let resp = json!({
            "candidates": [{
                "content": { "parts": [
                    { "thoughtSignature": "abc" },
                    { "text": "the real answer" }
                ]}
            }]
        });
        assert_eq!(extract_response_text(&resp).as_deref(), Some("the real answer"));
    }
}
