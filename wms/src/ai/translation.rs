//! On-demand background translation of user-facing content (starting with
//! support-ticket AI summaries) into the requesting user's language.
//!
//! Thick-backend / thin-frontend: the detail handler serves the best available
//! text NOW (a fresh translation if one exists, else the original + a pending
//! flag) and NEVER blocks on Gemini. Missing/stale translations are produced by
//! this in-process queue and, on completion, announced over the dashboard
//! WebSocket (`translation_ready`) so open clients re-fetch.
//!
//! Guardrails are MANDATORY — we had a runaway-Gemini incident (~191 calls on a
//! single doc, 2026-04-21):
//!   * SINGLE-FLIGHT per `(source, field, lang)` — a `HashSet` of in-flight keys
//!     behind a mutex drops duplicate enqueues.
//!   * DAILY CAP — `TRANSLATE_DAILY_CAP` (default 500) hard-stops the spend; over
//!     cap we log a warning at most once per hour and refuse to enqueue.
//! On error the job is NOT retried (at most one Gemini call per enqueue).

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eck_core::db::SurrealDb;
use eck_core::models::translation as model;
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{info, warn};

const DEFAULT_DAILY_CAP: u32 = 500;
const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";
/// Small gap between Gemini calls so a burst of enqueues doesn't hammer the API.
const RATE_LIMIT_MS: u64 = 300;
const MAX_OUTPUT_TOKENS: u32 = 4096;

struct Job {
    /// Source thing string, e.g. ``document:`12345` ``.
    source: String,
    field: String,
    lang: String,
    /// Source text (already PPRL-masked upstream — translating it leaks nothing).
    text: String,
    source_hash: String,
}

struct Queue {
    tx: mpsc::UnboundedSender<Job>,
    /// In-flight `(source,field,lang)` keys — single-flight dedup.
    inflight: Arc<Mutex<HashSet<String>>>,
    cap: u32,
    /// Epoch-day the counter belongs to (UTC); rolls the count at midnight.
    day: AtomicI64,
    count: AtomicU32,
    /// Last time we logged the "cap reached" warning (rate-limited to 1/hour).
    last_cap_warn: Mutex<Option<Instant>>,
    /// DB handle + our instance_id, so `enqueue` can write the mesh-synced CLAIM
    /// row ("I'm translating X") the instant a job is accepted — before Gemini —
    /// so peers see it and don't duplicate the call.
    db: SurrealDb,
    instance_id: String,
}

static QUEUE: OnceLock<Queue> = OnceLock::new();

fn today_epoch_days() -> i64 {
    chrono::Utc::now().timestamp() / 86_400
}

fn job_key(source: &str, field: &str, lang: &str) -> String {
    format!("{source}\u{1f}{field}\u{1f}{lang}")
}

/// Wire up the queue and spawn its single consumer. Call once at startup AFTER
/// `ws_tx` exists, gated on AI being enabled. A second call is a no-op.
pub fn init(db: SurrealDb, ws_tx: broadcast::Sender<String>, instance_id: String) {
    let cap = std::env::var("TRANSLATE_DAILY_CAP")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_DAILY_CAP);
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let inflight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let queue = Queue {
        tx,
        inflight: inflight.clone(),
        cap,
        day: AtomicI64::new(today_epoch_days()),
        count: AtomicU32::new(0),
        last_cap_warn: Mutex::new(None),
        db: db.clone(),
        instance_id: instance_id.clone(),
    };
    if QUEUE.set(queue).is_err() {
        return; // already initialised
    }

    let model_name = std::env::var("GEMINI_TRANSLATE_MODEL")
        .unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    info!("[Translate] queue started (daily_cap={cap}, model={model_name})");

    tokio::spawn(async move {
        let http = HttpClient::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build translate HTTP client");
        while let Some(job) = rx.recv().await {
            let key = job_key(&job.source, &job.field, &job.lang);
            process(&db, &http, &ws_tx, &instance_id, &model_name, &job).await;
            // Release the single-flight slot whether we succeeded or not — a
            // failed job is intentionally NOT retried here (the client re-fetches
            // and, on a still-missing translation, re-enqueues on the next view).
            inflight.lock().await.remove(&key);
            tokio::time::sleep(Duration::from_millis(RATE_LIMIT_MS)).await;
        }
    });
}

/// Enqueue a background translation, honouring single-flight + daily cap. No-op
/// if the queue isn't initialised (AI disabled / tests). Never blocks the caller.
pub fn enqueue(source: &str, field: &str, lang: &str, text: &str, source_hash: &str) {
    let Some(q) = QUEUE.get() else { return };
    let key = job_key(source, field, lang);

    // Single-flight. `try_lock` keeps the request path non-blocking; a contended
    // lock means another enqueue is mid-decision — safe to skip (the client
    // re-fetches and re-enqueues later if still needed).
    let mut guard = match q.inflight.try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if guard.contains(&key) {
        return;
    }

    // Daily cap — reset at UTC midnight. Done inside the single-flight lock so the
    // count updates are serialised (no atomic race). At/over cap: warn ≤ 1/hour.
    let today = today_epoch_days();
    if q.day.load(Ordering::Relaxed) != today {
        q.day.store(today, Ordering::Relaxed);
        q.count.store(0, Ordering::Relaxed);
    }
    let count = q.count.load(Ordering::Relaxed);
    if !model::under_daily_cap(count, q.cap) {
        if let Ok(mut w) = q.last_cap_warn.try_lock() {
            let now = Instant::now();
            let due = w.map(|t| now.duration_since(t) >= Duration::from_secs(3600)).unwrap_or(true);
            if due {
                *w = Some(now);
                warn!("[Translate] daily cap {} reached — dropping enqueue for {source} -> {lang}", q.cap);
            }
        }
        return;
    }
    q.count.store(count + 1, Ordering::Relaxed);
    guard.insert(key.clone());
    drop(guard);

    let job = Job {
        source: source.to_string(),
        field: field.to_string(),
        lang: lang.to_string(),
        text: text.to_string(),
        source_hash: source_hash.to_string(),
    };
    if q.tx.send(job).is_err() {
        // Consumer gone — release the slot so a later attempt isn't wedged.
        if let Ok(mut g) = q.inflight.try_lock() {
            g.remove(&key);
        }
        return;
    }

    // CLAIM the work in the mesh-synced row NOW — "собираюсь перевести". We do this
    // the instant the job is accepted (before Gemini) so the `status=claimed` row
    // merkle-syncs to peers ASAP and they back off instead of duplicating the call.
    // Spawned (enqueue is sync + non-blocking); we're inside the Tokio runtime
    // because QUEUE is only ever Some after `init` ran on it.
    let db = q.db.clone();
    let inst = q.instance_id.clone();
    let (s, f, l, h) = (
        source.to_string(),
        field.to_string(),
        lang.to_string(),
        source_hash.to_string(),
    );
    tokio::spawn(async move {
        write_claim(&db, &inst, &s, &f, &l, &h).await;
    });
}

/// UPSERT the deterministic row as a fresh CLAIM (`status=claimed`, empty text)
/// with a `_vclock` stamp, so it dominates on peers and dedups the Gemini call.
/// Skips the write if a `done` row for the SAME source text already exists — a
/// (rare) result that arrived via sync in the window since the resolver's read
/// must not be clobbered back to an empty claim.
async fn write_claim(
    db: &SurrealDb,
    instance_id: &str,
    source: &str,
    field: &str,
    lang: &str,
    source_hash: &str,
) {
    let id = model::translation_id(source, field, lang);
    let trid = format!("translation:`{}`", id);

    let existing: Option<Value> = db
        .query("SELECT status, source_hash, _vclock FROM type::record($trid) LIMIT 1")
        .bind(("trid", trid.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    if let Some(row) = &existing {
        let st = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let sh = row.get("source_hash").and_then(|v| v.as_str()).unwrap_or("");
        if st == "done" && sh == source_hash {
            return; // a finished translation already exists — don't claim over it
        }
    }
    let vc = eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|r| r.get("_vclock")),
        instance_id,
    );

    let res = db
        .query(
            "UPSERT type::record($trid) CONTENT { \
                source: $source, field: $field, lang: $lang, text: '', \
                status: 'claimed', claimed_by: $inst, claimed_at: time::now(), \
                model: '', source_hash: $hash, created_at: time::now(), _vclock: $vc \
             }",
        )
        .bind(("trid", trid.clone()))
        .bind(("source", source.to_string()))
        .bind(("field", field.to_string()))
        .bind(("lang", lang.to_string()))
        .bind(("inst", instance_id.to_string()))
        .bind(("hash", source_hash.to_string()))
        .bind(("vc", vc))
        .await;
    match res {
        Ok(_) => info!("[Translate] claimed {} {} -> {}", field, source, lang),
        Err(e) => warn!("[Translate] claim write failed for {trid}: {e}"),
    }
}

async fn process(
    db: &SurrealDb,
    http: &HttpClient,
    ws_tx: &broadcast::Sender<String>,
    instance_id: &str,
    model_name: &str,
    job: &Job,
) {
    let id = model::translation_id(&job.source, &job.field, &job.lang);
    let trid = format!("translation:`{}`", id);

    // RACE RE-CHECK — close the enqueue-vs-sync race. Between us accepting this job
    // and getting here, a peer's claim/result may have merkle-synced in. Re-read
    // the row: if another instance holds a FRESH claim, or it's already `done` for
    // this exact source text, drop silently (someone else owns/finished it) — no
    // duplicate Gemini call.
    let pre: Option<Value> = db
        .query(
            "SELECT status, claimed_by, source_hash, \
                    type::string(claimed_at) AS claimed_at \
             FROM type::record($trid) LIMIT 1",
        )
        .bind(("trid", trid.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    if let Some(row) = &pre {
        let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let sh = row.get("source_hash").and_then(|v| v.as_str()).unwrap_or("");
        if status == "done" && sh == job.source_hash {
            info!("[Translate] {} -> {} already done (raced) — skipping", job.source, job.lang);
            return;
        }
        if status == "claimed" && sh == job.source_hash {
            let by = row.get("claimed_by").and_then(|v| v.as_str()).unwrap_or("");
            let fresh = row
                .get("claimed_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| {
                    (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds()
                        < model::CLAIM_TTL_SECS
                })
                .unwrap_or(false);
            if by != instance_id && !by.is_empty() && fresh {
                info!("[Translate] {} -> {} freshly claimed by {} (raced) — skipping", job.source, job.lang, by);
                return;
            }
        }
    }

    let auth = match eck_core::ai::AiAuth::resolve(http).await {
        Ok(a) if a.is_configured() => a,
        Ok(_) => {
            warn!("[Translate] AI not configured — skipping {}", job.source);
            return;
        }
        Err(e) => {
            warn!("[Translate] auth resolve failed: {e}");
            return;
        }
    };

    let payload = json!({
        "contents": [{ "parts": [{ "text": build_prompt(&job.lang, &job.text) }] }],
        "generationConfig": { "temperature": 0.1, "maxOutputTokens": MAX_OUTPUT_TOKENS },
    });

    let (translated, usage) = match auth.generate_content(http, model_name, payload).await {
        Ok(r) => r,
        Err(e) => {
            // No retry — at most one Gemini call per enqueue (runaway guardrail).
            // Mark the row `failed` so the claim can't wedge the mesh: peers see a
            // failed (not a perpetually-fresh claim) and retry after RETRY_AFTER.
            warn!("[Translate] {} {} -> {} failed: {e}", job.field, job.source, job.lang);
            write_failed(db, instance_id, &trid, job, &format!("{e}")).await;
            return;
        }
    };
    let translated = translated.trim().to_string();
    if translated.is_empty() {
        warn!("[Translate] empty translation for {} -> {}", job.source, job.lang);
        write_failed(db, instance_id, &trid, job, "empty translation").await;
        return;
    }

    // Advancing `_vclock` so the row DOMINATES on peers instead of being dropped
    // as "local wins/equal" — same idiom as the support/exact backfills. (This
    // read also sees our own `claimed` row's clock, so `done` lands strictly After
    // the claim and wins fleet-wide.)
    let cur: Vec<Value> = db
        .query("SELECT VALUE _vclock FROM type::record($trid) LIMIT 1")
        .bind(("trid", trid.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let vc = eck_core::sync::conflict::next_local_vclock(cur.first(), instance_id);

    // UPSERT the deterministic row as `done` — "перевёл" — then (idempotently)
    // attach the provenance edge source -> has_translation -> translation.
    // DELETE-before-RELATE keeps exactly one edge across retranslations.
    let stored = db
        .query(
            "UPSERT type::record($trid) CONTENT { \
                source: $source, field: $field, lang: $lang, text: $text, \
                model: $model, source_hash: $hash, status: 'done', \
                translated_by: $inst, created_at: time::now(), _vclock: $vc \
             }; \
             DELETE has_translation WHERE in = type::record($src) AND out = type::record($trid); \
             RELATE (type::record($src))->has_translation->(type::record($trid));",
        )
        .bind(("trid", trid.clone()))
        .bind(("src", job.source.clone()))
        .bind(("source", job.source.clone()))
        .bind(("field", job.field.clone()))
        .bind(("lang", job.lang.clone()))
        .bind(("text", translated.clone()))
        .bind(("model", model_name.to_string()))
        .bind(("hash", job.source_hash.clone()))
        .bind(("inst", instance_id.to_string()))
        .bind(("vc", vc))
        .await;
    if let Err(e) = stored {
        warn!("[Translate] store failed for {trid}: {e}");
        return;
    }

    if !usage.is_null() {
        super::telemetry::log_telemetry(db, "translation", model_name, &trid, &usage).await;
    }

    // Announce completion so open dashboards re-fetch the ticket with ?lang=.
    let evt = json!({
        "type": "translation_ready",
        "source": job.source,
        "field": job.field,
        "lang": job.lang,
    });
    let _ = ws_tx.send(evt.to_string());
    info!("[Translate] stored {} -> {} ({} chars)", job.source, job.lang, translated.len());
}

/// Mark the row `failed` (empty text, `failed_at`/`error` stamped) with a fresh
/// `_vclock` — so a Gemini failure releases the mesh claim instead of leaving a
/// forever-fresh `claimed` row that wedges the triple across every node. The
/// `decide()` RETRY_AFTER gate then keeps this from becoming a hot retry loop.
async fn write_failed(db: &SurrealDb, instance_id: &str, trid: &str, job: &Job, err: &str) {
    let cur: Vec<Value> = db
        .query("SELECT VALUE _vclock FROM type::record($trid) LIMIT 1")
        .bind(("trid", trid.to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let vc = eck_core::sync::conflict::next_local_vclock(cur.first(), instance_id);
    // Keep the reason short — it merkle-syncs with the row.
    let short: String = err.chars().take(200).collect();
    let res = db
        .query(
            "UPSERT type::record($trid) CONTENT { \
                source: $source, field: $field, lang: $lang, text: '', model: '', \
                status: 'failed', source_hash: $hash, failed_at: time::now(), \
                error: $err, created_at: time::now(), _vclock: $vc \
             }",
        )
        .bind(("trid", trid.to_string()))
        .bind(("source", job.source.clone()))
        .bind(("field", job.field.clone()))
        .bind(("lang", job.lang.clone()))
        .bind(("hash", job.source_hash.clone()))
        .bind(("err", short))
        .bind(("vc", vc))
        .await;
    if let Err(e) = res {
        warn!("[Translate] failed-marker write failed for {trid}: {e}");
    }
}

/// Friendly language name for the prompt; falls back to the raw ISO 639-1 code.
/// Shared with the i18n label-translation job (see `handlers::i18n`).
pub(crate) fn lang_display(code: &str) -> String {
    match code.to_lowercase().as_str() {
        "en" => "English",
        "de" => "German",
        "ko" => "Korean",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "pl" => "Polish",
        "tr" => "Turkish",
        "zh" => "Chinese",
        "ja" => "Japanese",
        "ar" => "Arabic",
        other => return format!("the language with ISO 639-1 code '{other}'"),
    }
    .to_string()
}

fn build_prompt(lang: &str, text: &str) -> String {
    format!(
        "Translate the text below into {target}.\n\
         Rules:\n\
         - Preserve line breaks, markdown, and whitespace EXACTLY.\n\
         - Do NOT translate or alter technical identifiers: product/model names, \
           serial numbers, codes, URLs, email addresses, and any token of the form \
           Word_HEX (e.g. Name_8E5F3A1B00000000, Address_DEADBEEF00000000) — copy \
           them verbatim.\n\
         - Output ONLY the translation. No preamble, no notes, no quoting.\n\n\
         Text:\n{text}",
        target = lang_display(lang),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_names_language_and_preserves_tokens_rule() {
        let p = build_prompt("ko", "Name_ABCD summary");
        assert!(p.contains("Korean"));
        assert!(p.contains("Word_HEX"));
        assert!(p.contains("Name_ABCD summary"));
    }

    #[test]
    fn prompt_falls_back_to_iso_code() {
        assert!(build_prompt("xx", "t").contains("ISO 639-1 code 'xx'"));
    }

    #[test]
    fn job_key_is_field_separated() {
        // Distinct triples must not collide onto one single-flight key.
        assert_ne!(job_key("a", "b", "c"), job_key("a", "bc", ""));
        assert_eq!(job_key("s", "summary", "ko"), job_key("s", "summary", "ko"));
    }

    #[test]
    fn enqueue_without_init_is_noop() {
        // QUEUE is uninitialised in the unit-test binary — must not panic.
        enqueue("document:`1`", "summary", "ko", "text", "hash");
    }
}
