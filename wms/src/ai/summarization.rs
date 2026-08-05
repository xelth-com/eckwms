use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::{obfuscate_pii, scrub_pii_regex};
use reqwest::Client as HttpClient;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use super::loop_guard::LoopGuard;
use super::telemetry::{log_telemetry, current_budget_level, BudgetLevel, THROTTLE_DELAY_SECS};
use tracing::{info, warn};

const BATCH_LIMIT: usize = 5;
const LOOP_INTERVAL_SECS: u64 = 10;
const RATE_LIMIT_MS: u64 = 500;
const MAX_RETRIES: i64 = 5;
/// Debounce for actively-written docs: skip candidates whose updated_at is
/// younger than this. Zoho ingest re-marks the parent ticket 'pending' and
/// bumps updated_at on EVERY imported thread, so without a settle window the
/// worker pays for a fresh Gemini summary per thread (Observer flags it as a
/// loop). Must exceed the per-ticket ingest burst incl. a 429 backoff (~3 min).
const SETTLE_MINS: i64 = 10;

const TICKET_PROMPT_TEMPLATE: &str = r#"You are an expert Level 3 Technical Support Engineer and Logistics Coordinator for {{DEVICES}}.
Your task is to analyze a raw, noisy customer support email thread and extract the core technical facts AND all logistics/contact footprints.

CRITICAL INSTRUCTIONS:
- Ignore greetings and emotional complaints.
- Synthesize the entire thread into a single, cohesive summary.
- Output the result in German.
- The text contains anonymized PPRL tokens like Name_8E5F3A1B00000000, Email_A1B2C3D400000000, Phone_1234ABCD00000000, Address_DEADBEEF00000000. You MUST preserve these tokens EXACTLY as they appear in your output — do NOT replace, translate, summarize, or remove them. They will be decoded after you respond.

Extract the information into the following strict structure:

=== LOGISTIK & KONTAKTE ===
**Firma / Einrichtung:** (Extract company names, clinic names, or practices. If multiple, list them).
**Kontaktpersonen:** (List ALL distinct names, emails, and phone numbers found in the text and email signatures. This is crucial for matching future physical packages).
**Adressen:** (Extract the physical street address, ZIP code, and city. IF the address is incomplete, USE GOOGLE SEARCH to look up the company name, email domain, or phone number and find their official physical address. IMPORTANT: Output the clean, raw address on the FIRST line. If you used Google Search to find it, add your explanation, warning, or thoughts on a NEW LINE below the address and enclose the entire explanation in parentheses).

=== TECHNISCHE DETAILS ===
**Gerät / Modell:** (Extract device model or serial number, e.g., "Model 770", "SN: 12345").
**Hauptproblem (Symptom):** (Briefly describe the technical failure in 1-2 sentences).
**Durchgeführte Schritte:** (Troubleshooting steps already taken).
**Lösung / Status:** (Current status, e.g., "RMA needed", "Waiting for customer", "Resolved")."#;

/// The ticket summarization system prompt, with the tenant's device phrase
/// spliced in (env `ECK_TENANT_BRAND`; neutral "the company's devices" when
/// unset). Everything except that one phrase is byte-identical to the template.
pub(crate) fn ticket_prompt() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| TICKET_PROMPT_TEMPLATE.replace("{{DEVICES}}", &super::branding::devices_phrase()))
}

pub(crate) const INVOICE_PROMPT: &str = r#"You are an expert AI assistant for an ERP and Warehouse Management System.
Your task is to analyze a raw invoice (Rechnung) document and extract the core logistical and product data.
- The text contains anonymized PPRL tokens like Name_8E5F3A1B00000000, Email_A1B2C3D400000000. You MUST preserve these tokens EXACTLY as they appear.

Extract the information into the following strict structure in German:

=== KÄUFER & ADRESSEN ===
**Rechnungsadresse:** (Extract the billing company, name, and address)
**Lieferadresse:** (Extract the shipping/delivery address if different)
**Kontaktdaten:** (Email, phone numbers)

=== POSITIONEN & SERIENNUMMERN ===
**Gekaufte Artikel:** (List the models/products purchased)
**Seriennummern:** (Extract ALL serial numbers mentioned in the invoice. This is CRITICAL for warranty tracking)."#;

// ── PII Masking ─────────────────────────────────────────────────────────────

/// Collects PII values and replaces them with PPRL SimHash tokens.
/// Uses the same `obfuscate_pii` as the embedding pipeline for consistency.
/// After AI responds, `unmask()` restores the real values.
struct PiiMask {
    /// Maps SimHash token → real value, e.g. "Name_8E5F3A1B00000000" → "Hans Müller"
    map: HashMap<String, String>,
}

impl PiiMask {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Register a PII value. Returns a deterministic SimHash token (e.g. `Name_CC0068898836CB06`).
    /// Same input always produces the same token (keyed by SYNC_SECRET).
    fn mask(&mut self, pii_type: &str, value: &str) -> String {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return String::new();
        }
        let token = obfuscate_pii(trimmed, pii_type);
        self.map.insert(token.clone(), trimmed.to_string());
        token
    }

    /// Replace all occurrences of a real PII value in text with its SimHash token.
    fn mask_text(&self, text: &str) -> String {
        let mut result = text.to_string();
        // Sort by value length descending to avoid partial replacements
        let mut entries: Vec<_> = self.map.iter().collect();
        entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        for (token, real_value) in entries {
            if !real_value.is_empty() {
                result = result.replace(real_value.as_str(), token);
            }
        }
        result
    }

    /// Restore real values in the AI-generated summary.
    fn unmask(&self, text: &str) -> String {
        let mut result = text.to_string();
        for (token, real_value) in &self.map {
            result = result.replace(token.as_str(), real_value);
        }
        result
    }
}

/// Spawns the background summarization worker that processes pending ticket documents.
pub async fn start_summarization_worker(db: SurrealDb, model: String, instance_id: String) {
    // Delay to let the server finish startup
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    info!("[Summarization] Worker started ({LOOP_INTERVAL_SECS}s interval, model={model})");

    // Reset retryable summaries on startup — errored, skipped, and docs
    // the Observer paused after detecting a loop. Paused docs get their
    // retry counter cleared because the loop root cause (UPSERT CONTENT
    // wipe + volatile hash) has been fixed at the import layer.
    // Docs that failed MAX_RETRIES times stay as 'failed' to prevent infinite loops.
    reset_retryable(&db).await;

    // Resurrect Observer-killed zombies (retries=99, status='failed'). These
    // were sacrificed during the 2026-04-21 Gemini loop mitigation; now that
    // the loop root cause is fixed, give them another MAX_RETRIES attempts.
    // If a true loop recurs, the Observer will kill them again.
    let resurrect = db
        .query(
            "UPDATE document SET \
                summary_status = 'pending', \
                summary_retries = 0, \
                summary_error = NONE \
             WHERE summary_status = 'failed' AND summary_retries = 99 \
             AND type IN ['support_ticket', 'invoice'] \
             RETURN NONE"
        )
        .await;
    if let Err(e) = resurrect {
        warn!("[Summarization] Failed to resurrect observer-killed zombies: {e}");
    }

    let http = HttpClient::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("failed to build summarization HTTP client");
    let guard = Arc::new(LoopGuard::new());
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(LOOP_INTERVAL_SECS));
    // Docs that exhausted retries on TRANSIENT failures (a 429/502 burst)
    // used to sit in 'error' until the next process restart. Re-run the
    // retryable-reset periodically so the pipeline self-heals in place.
    let mut last_reset = std::time::Instant::now();
    const RESET_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);
    // 'enriching' is a within-run state: the ingest scheduler finalizes each
    // ticket (enriching → pending) right after its threads land. A doc still
    // enriching after an hour of write silence means the run died mid-ticket
    // (crash, kill, scraper timeout) — requeue it so nothing gets stranded.
    let mut last_rescue = std::time::Instant::now();
    const RESCUE_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

    loop {
        interval.tick().await;
        eck_core::metrics::tick(eck_core::metrics::M::SummarizationCycle);
        if last_reset.elapsed() >= RESET_EVERY {
            reset_retryable(&db).await;
            last_reset = std::time::Instant::now();
        }
        if last_rescue.elapsed() >= RESCUE_EVERY {
            let rescued = db
                .query(
                    "UPDATE document SET summary_status = 'pending' \
                     WHERE summary_status = 'enriching' \
                     AND type::is_datetime(updated_at) AND updated_at < time::now() - 60m \
                     RETURN NONE",
                )
                .await;
            if let Err(e) = rescued {
                warn!("[Summarization] enriching-orphan rescue failed: {e}");
            }
            last_rescue = std::time::Instant::now();
        }

        // Resolve auth each cycle (managed mode re-mints the Vertex bearer
        // transparently before expiry; studio returns the static key).
        let auth = match eck_core::ai::AiAuth::resolve(&http).await {
            Ok(a) if a.is_configured() => a,
            Ok(_) => continue,
            Err(e) => {
                warn!("[Summarization] token resolve failed: {e}");
                continue;
            }
        };

        // Batch path (Vertex/managed only): service in-flight jobs, orphan-guard
        // stranded 'batched' docs, and — when the pending pile is large enough and
        // ECK_SUMMARY_BATCH=1 — submit a Vertex batch-prediction job (≈50% cheaper
        // than live calls). Returns Ok(true) when a batch was submitted this tick
        // (the claimed docs left the 'pending' pool, so skip the live path); Ok(false)
        // to fall through to the per-doc live path for whatever trickle remains.
        // Studio auth, batch disabled, or any error → falls through untouched.
        let handled_by_batch = match super::summarization_batch::batch_tick(
            &db, &http, &auth, &model, &instance_id,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("[Summarization] batch tick error: {e}");
                false
            }
        };
        if handled_by_batch {
            continue;
        }

        if let Err(e) = process_pending(&db, &http, &auth, &model, &guard, &instance_id).await {
            warn!("[Summarization] cycle error: {e}");
        }
    }
}

/// Requeue retryable documents (error/skipped) that still have retry budget.
/// Called at worker start and every ~6h so transient-failure victims (429/502
/// bursts) self-heal without a process restart.
///
/// `paused_by_observer` is deliberately NOT requeued (2026-07-25): the
/// Observer pauses a doc when it detects a LOOP — a code bug, not a transient.
/// Auto-unpausing every 6h turned the circuit breaker into a snooze button:
/// during the sig-param re-arm burn the Observer parked 353 docs, and this
/// sweep would have silently re-fed the whole set to Gemini the same evening.
/// Un-pausing is a HUMAN/one-shot-heal decision after the root cause is fixed.
async fn reset_retryable(db: &SurrealDb) {
    let reset = db
        .query(&format!(
            "UPDATE document SET \
                summary_status = 'pending', \
                summary_error = NONE \
             WHERE summary_status IN ['error', 'skipped'] \
             AND type IN ['support_ticket', 'invoice'] \
             AND (summary_retries IS NONE OR summary_retries < {MAX_RETRIES}) \
             RETURN NONE"
        ))
        .await;
    match reset {
        Ok(_) => info!("[Summarization] Reset retryable documents to pending (max {MAX_RETRIES} retries)"),
        Err(e) => warn!("[Summarization] Failed to reset docs: {e}"),
    }
}

/// The SurrealQL WHERE body that selects a summarization-eligible `document`.
/// Shared verbatim by the live selection (`process_pending`) and the batch
/// selection/count (`summarization_batch`) so a doc is picked identically by
/// both paths — same source-instance gate, type filter, settle window, retry
/// cap, and exponential backoff. `instance_id` is a trusted internal UUID,
/// interpolated the same way the live query always has.
pub(crate) fn eligibility_where(instance_id: &str) -> String {
    format!(
        "summary_status = 'pending' \
         AND (source_instance_id IS NONE OR source_instance_id = '{instance_id}') \
         AND type IN ['support_ticket', 'invoice'] \
         AND (updated_at IS NONE OR !type::is_datetime(updated_at) \
              OR updated_at < time::now() - {SETTLE_MINS}m) \
         AND (summary_retries IS NONE OR summary_retries < {MAX_RETRIES}) \
         AND (summary_retries IS NONE OR summary_retries = 0 \
              OR updated_at IS NONE \
              OR time::now() > updated_at + type::duration(string::concat(math::pow(2, summary_retries ?? 0), 'm')))"
    )
}

/// Merge the heavy `document_raw.payload` shadow onto a lightweight `document`
/// row so the text builders see the full Zoho object. Shared by the live and
/// batch paths. Falls back to the document's own fields when no raw shadow.
pub(crate) fn merge_payload(doc: &Value, raw_doc: &Option<Value>) -> Value {
    let mut d = doc.clone();
    if let Some(raw) = raw_doc {
        if let Some(p) = raw.get("payload") {
            if let Some(o) = d.as_object_mut() {
                o.insert("payload".to_string(), p.clone());
            }
        }
    }
    d
}

/// The user-message wrapper the live summarizer sends. Extracted so the batch
/// path builds a byte-identical prompt (keeps the two request shapes in step,
/// which the sha256 keying and the summary text both depend on).
pub(crate) fn summary_user_message(raw_text: &str) -> String {
    format!(
        "Analyze the following raw support ticket data and produce the structured summary:\n\n{raw_text}"
    )
}

/// Build the masked prompt text + system prompt for one document EXACTLY as the
/// live path does: fetch the `document_raw` shadow, merge its payload, then run
/// `build_ticket_text` / `build_invoice_text` (PII already tokenized). Returns
/// `Ok(Some((masked_text, system_prompt)))`, or `Ok(None)` for an unknown type.
/// The caller decides how to handle empty text. Shared by live and batch so the
/// text uploaded to GCS is exactly the text the live path would have sent.
pub(crate) async fn build_doc_masked_text(
    db: &SurrealDb,
    doc: &Value,
    id: &str,
) -> Result<Option<(String, &'static str)>, anyhow::Error> {
    let doc_type = doc.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
    // Fetch heavy payload from document_raw (local shadow table)
    let raw_id = id.split(':').last().unwrap_or(id).trim_matches('`').to_string();
    let raw_doc: Option<Value> = db
        .query("SELECT payload FROM document_raw WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", raw_id.clone()))
        .await?
        .take(0)?;

    let out = match doc_type {
        "support_ticket" => {
            let merged = merge_payload(doc, &raw_doc);
            let (text, _mask) = build_ticket_text(db, &merged, raw_id).await?;
            Some((text, ticket_prompt()))
        }
        "invoice" => {
            let merged = merge_payload(doc, &raw_doc);
            let (text, _mask) = build_invoice_text(&merged);
            Some((text, INVOICE_PROMPT))
        }
        _ => None,
    };
    Ok(out)
}

/// Completion write for a successfully-summarized `document`, shared VERBATIM by
/// the live per-doc path and the batch-completion path so they can never drift.
/// Writes `ai_summary` + `pii_fingerprints` + the embedding reset, handles the
/// phantom-0-rows trap (2026-04-21 infinite-loop guard), and bumps the local
/// vclock so the fresh summary strictly dominates a peer's summary-less copy.
/// `id` is the bare record id (no `document:` prefix). `subject_masked` feeds the
/// fingerprint index. Returns `Ok(true)` when the completion row matched,
/// `Ok(false)` on a phantom/missing record (caller must NOT count it as done).
pub(crate) async fn write_summary_completion(
    db: &SurrealDb,
    id: &str,
    subject_masked: &str,
    masked_summary: &str,
    instance_id: &str,
) -> Result<bool, anyhow::Error> {
    // Token index of the masked fields — powers exact `search_database` lookup
    // by pseudonym token. Derived, merkle-IGNORED; peer nodes re-derive their
    // own copy via the main.rs self-heal when the summary arrives over mesh.
    let fps = eck_core::utils::anonymizer::extract_pii_tokens(&format!("{subject_masked}\n{masked_summary}"));
    // A fresh summary normally re-queues the embedding — but NOT if embeddings
    // are parked 'unavailable' (backend has no embedding model): reviving those
    // into 'pending' just recreates the loop the Observer flags as "stuck".
    // Nulling `embedding` is the fleet-wide re-embed signal (the vector is part
    // of the content hash; status fields do NOT sync).
    let updated: Vec<Value> = db
        .query(
            "UPDATE type::record($rid) SET ai_summary = $summary, summary_status = 'completed', \
                 summary_error = NONE, \
                 pii_fingerprints = $fps, \
                 embedding = NONE, embedding_model = NONE, \
                 embedding_status = IF embedding_status = 'unavailable' THEN 'unavailable' ELSE 'pending' END, \
                 embedding_retries = 0, embedding_error = NONE \
             RETURN record::id(id) AS id",
        )
        .bind(("rid", format!("document:`{}`", id)))
        .bind(("summary", masked_summary.to_string()))
        .bind(("fps", fps))
        .await?
        .take(0)?;

    if updated.is_empty() {
        // The success-path UPDATE silently matched 0 rows. This is the
        // infinite-loop trap from 2026-04-21: the model was paid, but
        // summary_status stayed as-was, so the worker would re-pick the same
        // doc. Force the retry counter forward via an ID-insensitive WHERE so
        // exponential backoff can kick in. The WHERE covers both the live
        // ('pending') and batch ('batched') claim states.
        warn!("[Summarization] success-path UPDATE matched 0 rows for {id} — forcing retry counter");
        let forced: Vec<Value> = db
            .query(
                "UPDATE document SET \
                     summary_status = 'error', \
                     summary_error = 'phantom update: type::record matched 0 rows', \
                     summary_retries = (summary_retries ?? 0) + 1, \
                     updated_at = time::now() \
                 WHERE record::id(id) = $id AND summary_status IN ['pending', 'batched'] \
                 RETURN record::id(id) AS id",
            )
            .bind(("id", id.to_string()))
            .await?
            .take(0)?;
        if forced.is_empty() {
            warn!("[Summarization] fallback UPDATE also matched 0 rows for {id} — record truly missing");
        }
        return Ok(false);
    }

    // `ai_summary` is part of the content hash (merkle) — advance this node's
    // vclock so the summary strictly dominates a peer's summary-less copy (whose
    // 'skipped' write may carry a NEWER updated_at and would otherwise win LWW).
    if let Err(e) = eck_core::sync::conflict::bump_local_vclock_by_leaf(db, "document", id, instance_id).await {
        warn!("[Summarization] vclock bump failed for {id}: {e}");
    }
    Ok(true)
}

async fn process_pending(db: &SurrealDb, http: &HttpClient, auth: &eck_core::ai::AiAuth, model: &str, guard: &LoopGuard, instance_id: &str) -> Result<(), anyhow::Error> {
    // ── Circuit breaker check ──
    match current_budget_level() {
        BudgetLevel::Halt => return Ok(()), // complete stop
        BudgetLevel::Throttle => {
            tokio::time::sleep(std::time::Duration::from_secs(THROTTLE_DELAY_SECS)).await;
        }
        _ => {}
    }

    // Process both support_ticket and invoice document types.
    // Exponential backoff: docs with N retries wait 2^N minutes before next attempt (1m, 2m, 4m, 8m, 16m).
    // Hard cap at MAX_RETRIES — after that the doc stays as 'failed' permanently.
    // The WHERE body is shared with the batch path (`eligibility_where`) so the
    // two selections can never diverge on which docs are eligible.
    let docs: Vec<Value> = db
        .query(&format!(
            "SELECT record::id(id) AS id, type, status, meta, ai_summary, summary_status, ticket_id, summary_retries \
             FROM document \
             WHERE {} \
             LIMIT {BATCH_LIMIT}",
            eligibility_where(instance_id)
        ))
        .await?
        .take(0)?;

    if docs.is_empty() {
        return Ok(());
    }

    let mut count = 0u32;
    for doc in &docs {
        let id = match doc.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        // In-memory loop guard — independent of DB-side exponential backoff.
        // Protects against the case where summary_retries fails to increment
        // (e.g., silent UPDATE-matched-0-rows) and the worker would otherwise
        // re-pick the same doc indefinitely.
        if !guard.check_and_record(&id) {
            info!("[Summarization] loop_guard: skipping {id} (cooldown)");
            continue;
        }

        let doc_type = doc
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Build the masked prompt text EXACTLY as the batch path does (shared
        // helper: raw-payload merge + build_ticket_text/build_invoice_text).
        let (raw_text, prompt) = match build_doc_masked_text(db, doc, &id).await? {
            Some(t) => t,
            None => continue, // unknown type
        };

        if raw_text.is_empty() {
            warn!("[Summarization] Skipping {id} ({doc_type}): empty text");
            db.query("UPDATE type::record($rid) SET summary_status = 'skipped', summary_retries = (summary_retries ?? 0) + 1, updated_at = time::now()")
                .bind(("rid", format!("document:`{}`", id)))
                .await?
                .check()?;
            continue;
        }

        match summarize(http, auth, model, prompt, &raw_text).await {
            Ok((masked_summary, usage)) => {
                // Store the masked summary in DB — real PII never touches the database.
                // Unmask happens on-the-fly in the API handler using deterministic PPRL tokens.
                let subject_masked = doc
                    .get("meta")
                    .and_then(|m| m.get("subject"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Telemetry always logs — Gemini was called either way.
                if !usage.is_null() {
                    log_telemetry(db, "summarization", model, &id, &usage).await;
                }

                // Shared completion write (fingerprints + embedding reset +
                // phantom-0-rows guard + vclock bump) — identical to the batch path.
                let matched = write_summary_completion(db, &id, subject_masked, &masked_summary, instance_id).await?;
                if matched {
                    count += 1;
                    guard.clear(&id);
                    info!("[Summarization] Summarized {id} ({doc_type}, {} chars)", masked_summary.len());

                    // Resolve this ticket's map location now, at summary time —
                    // the single place a ticket gets an address: free zip/city,
                    // the summary's PLZ-Ort, phone Vorwahl, then AI grounding if
                    // the switch is on. Always leaves a terminal state, so it's
                    // never re-attempted. Tickets only (invoices have nothing to
                    // place).
                    if doc_type == "support_ticket" {
                        if let Some(meta) = doc.get("meta") {
                            super::address_discovery::resolve_ticket_address(
                                db, http, auth, model, &id, meta, &masked_summary,
                            ).await;
                        }
                    }
                }
            }
            Err(e) => {
                warn!("[Summarization] Failed to summarize {id}: {e}");
                db.query(
                    "UPDATE type::record($rid) SET summary_status = 'error', summary_error = $err, \
                     summary_retries = (summary_retries ?? 0) + 1, updated_at = time::now()",
                )
                .bind(("rid", format!("document:`{}`", id)))
                .bind(("err", e.to_string()))
                .await?
                .check()?;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(RATE_LIMIT_MS)).await;
    }

    if count > 0 {
        info!("[Summarization] {count} documents summarized");
    }

    Ok(())
}

/// Raw Zoho custom-field keys to read for each canonical summary display slot
/// (serial/model/company/street/city/country), from `ECK_SUMMARY_CF_KEYS` — a
/// JSON object mapping slot name to a list of raw CF keys to try in order
/// (first present, non-empty, non-"null" value wins), e.g.
/// `{"serial":["cf_serial_number"],"model":["cf_in_body_model"],...}`. The
/// display label and PII category per slot stay hardcoded (product logic) —
/// only which raw Zoho field feeds each slot is deployment data. Falls back to
/// the current hardcoded Zoho field names when unset or unparseable (logs one
/// warn) so summarization stays stable fleet-wide without a `.env` change.
/// Cached per process.
fn summary_cf_keys() -> &'static HashMap<String, Vec<String>> {
    static V: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    V.get_or_init(|| match std::env::var("ECK_SUMMARY_CF_KEYS") {
        Ok(raw) if !raw.trim().is_empty() => parse_cf_key_map(&raw).unwrap_or_else(|| {
            warn!(
                "[Summarize] ECK_SUMMARY_CF_KEYS is set but not valid JSON \
                 (expected an object of slot -> [raw CF keys]); using built-in Zoho field mapping"
            );
            default_cf_key_map()
        }),
        _ => default_cf_key_map(),
    })
}

/// The mapping this deployment used before `ECK_SUMMARY_CF_KEYS` existed —
/// also the fallback when the var is unset or fails to parse.
fn default_cf_key_map() -> HashMap<String, Vec<String>> {
    [
        ("serial", "cf_serial_number"),
        ("model", "cf_in_body_model"),
        ("company", "cf_company"),
        ("street", "cf_street"),
        ("city", "cf_city"),
        ("country", "cf_country_1"),
    ]
    .into_iter()
    .map(|(slot, key)| (slot.to_string(), vec![key.to_string()]))
    .collect()
}

/// Pure parser for `ECK_SUMMARY_CF_KEYS` — unit-testable without touching
/// process env (same pattern as `parse_summary_address_with`). `None` when
/// `raw` isn't a JSON object of `string -> [string, ...]`.
fn parse_cf_key_map(raw: &str) -> Option<HashMap<String, Vec<String>>> {
    let val: Value = serde_json::from_str(raw).ok()?;
    let obj = val.as_object()?;
    let mut out = HashMap::new();
    for (slot, keys) in obj {
        let arr = keys.as_array()?;
        let keys: Vec<String> = arr
            .iter()
            .map(|k| k.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()?;
        out.insert(slot.clone(), keys);
    }
    Some(out)
}

/// Build the raw text for summarization by combining ticket metadata and all thread contents.
/// PII (names, emails, phones, addresses) is replaced with numbered placeholders.
async fn build_ticket_text(
    db: &SurrealDb,
    ticket: &Value,
    ticket_id: String,
) -> Result<(String, PiiMask), anyhow::Error> {
    let mut parts = Vec::new();
    let mut pii = PiiMask::new();
    // Tiered PII policy (see ai::pii_policy): under an effective-clear LLM
    // policy the raw values go into the prompt AND into the stored summary —
    // a mesh-wide, deliberate customer decision. Everything below funnels
    // through `mask_val` + the final mask_text/scrub gate so one flag decides.
    let clear = crate::ai::pii_policy::effective_clear(crate::ai::pii_policy::Surface::Llm);

    // Ticket metadata — payload is the raw Zoho ticket object (subject, status, cf at top level)
    if let Some(t) = ticket.get("payload") {
        if let Some(s) = t.get("subject").and_then(|v| v.as_str()) {
            parts.push(format!("Subject: {s}"));
        }
        if let Some(s) = t.get("status").and_then(|v| v.as_str()) {
            parts.push(format!("Status: {s}"));
        }
        if let Some(s) = t.get("description").and_then(|v| v.as_str()) {
            let plain = strip_html(s);
            if !plain.is_empty() {
                parts.push(format!("Description: {plain}"));
            }
        }
        // Contact info — mask PII (or pass clear under the clear LLM policy;
        // registrations are skipped too since mask_text is skipped below).
        if let Some(contact) = t.get("contact") {
            let first = contact.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
            let last = contact.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
            let full_name = format!("{first} {last}").trim().to_string();
            if !full_name.is_empty() {
                let token = if clear { full_name.clone() } else { pii.mask("Name", &full_name) };
                parts.push(format!("Contact: {token}"));
                // Also register individual name parts for thread content masking
                if !clear && !first.is_empty() && first.len() > 2 {
                    pii.mask("Name", first);
                }
                if !clear && !last.is_empty() && last.len() > 2 {
                    pii.mask("Name", last);
                }
            }
            if let Some(email) = contact.get("email").and_then(|v| v.as_str()) {
                let token = if clear { email.to_string() } else { pii.mask("Email", email) };
                parts.push(format!("Email: {token}"));
            }
            if let Some(phone) = contact.get("phone").and_then(|v| v.as_str()) {
                let token = if clear { phone.to_string() } else { pii.mask("Phone", phone) };
                parts.push(format!("Phone: {token}"));
            }
            if let Some(acc) = contact.get("account") {
                if let Some(name) = acc.get("accountName").and_then(|v| v.as_str()) {
                    let token = if clear { name.to_string() } else { pii.mask("Company", name) };
                    parts.push(format!("Company: {token}"));
                }
            }
        }
        // Custom fields contain device/serial/address data. Which raw Zoho CF
        // key feeds each canonical slot is deployment data (ECK_SUMMARY_CF_KEYS);
        // the display label and PII category stay fixed product logic.
        if let Some(cf) = t.get("cf") {
            for (slot, label, category) in [
                ("serial", "Serial Number", None),
                ("model", "Model", None),
                ("company", "Company", Some("Company")),
                ("street", "Address", Some("Address")),
                ("city", "City", None),  // city/zip are OK to send
                ("country", "Country", None),
            ] {
                let Some(raw_keys) = summary_cf_keys().get(slot) else { continue };
                let value = raw_keys.iter().find_map(|key| {
                    cf.get(key)
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty() && *s != "null")
                });
                if let Some(s) = value {
                    match category {
                        Some(cat) if !clear => {
                            let token = pii.mask(cat, s);
                            parts.push(format!("{label}: {token}"));
                        }
                        _ => parts.push(format!("{label}: {s}")),
                    }
                }
            }
        }
    }

    // All threads for this ticket from document_raw (local heavy payloads)
    let threads: Vec<Value> = db
        .query("SELECT payload, updated_at AS created_at FROM document_raw WHERE type = 'support_thread' AND ticket_id = $tid ORDER BY created_at ASC")
        .bind(("tid", ticket_id.clone()))
        .await?
        .take(0)?;

    // N-gram history for rolling deduplication — seed with ticket metadata
    let mut history_text = parts.join("\n\n");

    for thread in &threads {
        if let Some(payload) = thread.get("payload") {
            // Every participant's signature name + email. Zoho's
            // from/to/cc are `"Display Name"<email>` lists, and `author` is a
            // structured {name,firstName,lastName,email}. Register each NAME and
            // EMAIL separately so mask_text tokenises them across the whole
            // thread — including the signature that repeats in every message
            // (the customer, our agents, AND third-party support staff who are
            // only ever a `to:` recipient, e.g. "erika musterfrau").
            if !clear {
                for field in ["fromEmailAddress", "to", "cc"] {
                    if let Some(s) = payload.get(field).and_then(|v| v.as_str()) {
                        for (name, email) in eck_core::utils::anonymizer::parse_named_addresses(s) {
                            // Only multi-token names ("First Last"): a short single
                            // token like "Info"/"Team" would corrupt real words via
                            // substring replace. The customer's own single-name is
                            // still covered by the structured-contact path.
                            if name.contains(' ') && name.len() >= 4 { pii.mask("Name", &name); }
                            if !email.is_empty() { pii.mask("Email", &email); }
                        }
                    }
                }
                if let Some(author) = payload.get("author") {
                    if let Some(v) = author.get("name").and_then(|v| v.as_str()) {
                        if v.contains(' ') && v.len() >= 4 { pii.mask("Name", v); }
                    }
                    if let Some(em) = author.get("email").and_then(|v| v.as_str()) {
                        pii.mask("Email", em);
                    }
                }
            }
            if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
                let plain = strip_html(content);
                if plain.is_empty() { continue; }
                // N-gram dedup: strip lines that are already present in history
                let deduped = deduplicate_email(&plain, &history_text);
                if !deduped.is_empty() {
                    parts.push(deduped.clone());
                    history_text.push_str("\n\n");
                    history_text.push_str(&deduped);
                }
            }
        }
    }

    let full = parts.join("\n\n");
    // Apply PII masking to the entire text (catches PII in thread bodies, signatures, etc.)
    // Under the clear LLM policy both passes are skipped — that IS the policy:
    // raw text to the model, clear summary at rest, mesh-wide by decision.
    let masked = if clear { full } else {
        let masked = pii.mask_text(&full);
        // GDPR backstop: the structured-field masker above only catches PII it pulled
        // from known fields. Free-text emails/phones/IBANs in thread bodies and
        // signatures (e.g. a third-party "office@…" address) would otherwise reach
        // Gemini in clear and get baked into the stored summary. Re-scrub the whole
        // text with the deterministic regex — same `obfuscate_pii` tokens, so reveal
        // can re-derive them. Nothing PII-shaped leaves in plaintext.
        let (masked, _fps) = scrub_pii_regex(&masked);
        masked
    };
    // Truncate to ~28000 bytes (char-boundary-safe) to stay within Gemini context limits
    let result = if masked.len() > 28000 {
        format!(
            "{}\n\n[... truncated ...]",
            &masked[..crate::ai::floor_char_boundary(&masked, 28000)]
        )
    } else {
        masked
    };
    Ok((result, pii))
}

/// Build raw text from an invoice document payload.
fn build_invoice_text(doc: &Value) -> (String, PiiMask) {
    let mut parts = Vec::new();
    let pii = PiiMask::new();

    if let Some(payload) = doc.get("payload") {
        // Try common invoice fields
        for (key, label) in [
            ("subject", "Betreff"),
            ("content", "Inhalt"),
            ("invoice_number", "Rechnungsnummer"),
            ("customer_name", "Kunde"),
            ("total_amount", "Betrag"),
        ] {
            if let Some(s) = payload.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    parts.push(format!("{label}: {s}"));
                }
            }
        }
        // If content is HTML, strip it
        if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
            let plain = strip_html(content);
            if !plain.is_empty() && !parts.iter().any(|p| p.starts_with("Inhalt:")) {
                parts.push(plain);
            }
        }
    }

    let full = parts.join("\n\n");
    // Same GDPR backstop as build_ticket_text: scrub free-text PII before
    // Gemini — skipped under the clear LLM policy (see ai::pii_policy).
    let full = if crate::ai::pii_policy::effective_clear(crate::ai::pii_policy::Surface::Llm) {
        full
    } else {
        let (full, _fps) = scrub_pii_regex(&full);
        full
    };
    let result = if full.len() > 28000 {
        format!(
            "{}\n\n[... truncated ...]",
            &full[..crate::ai::floor_char_boundary(&full, 28000)]
        )
    } else {
        full
    };
    (result, pii)
}

/// Call Gemini via direct HTTP to summarize document text.
/// Returns (summary_text, usageMetadata) for telemetry logging.
async fn summarize(
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    model: &str,
    system_prompt: &str,
    raw_text: &str,
) -> Result<(String, Value), anyhow::Error> {
    let user_msg = summary_user_message(raw_text);

    let payload = serde_json::json!({
        "systemInstruction": { "parts": [{ "text": system_prompt }] },
        "contents": [{ "parts": [{ "text": user_msg }] }],
        "tools": [{ "googleSearch": {} }],
        "generationConfig": {
            "temperature": 0.2,
            "maxOutputTokens": 4096,
        }
    });

    auth.generate_content(http, model, payload).await
}

/// N-gram (shingling) deduplication for email threads.
///
/// Splits `history` into 7-word shingles stored in a HashSet, then walks each
/// line of `content` and drops it when >60% of its shingles already exist in
/// history. Lines shorter than the window size are always kept — they're
/// typically short replies or signatures that would otherwise false-positive.
const NGRAM_WINDOW: usize = 7;
const DUP_THRESHOLD: f64 = 0.60;

fn deduplicate_email(content: &str, history: &str) -> String {
    let history_ngrams = build_ngram_set(history);
    if history_ngrams.is_empty() {
        return content.to_string();
    }

    let mut kept = Vec::new();
    for line in content.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        // Lines shorter than the window can't form a single shingle — always keep
        if words.len() < NGRAM_WINDOW {
            kept.push(line);
            continue;
        }
        let total = words.len() - NGRAM_WINDOW + 1;
        let mut hits = 0usize;
        for w in words.windows(NGRAM_WINDOW) {
            let shingle: String = w.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>().join(" ");
            if history_ngrams.contains(&shingle) {
                hits += 1;
            }
        }
        if (hits as f64 / total as f64) < DUP_THRESHOLD {
            kept.push(line);
        }
    }

    let result = kept.join("\n").trim().to_string();
    result
}

/// Build a set of lowercase 7-word shingles from text.
fn build_ngram_set(text: &str) -> HashSet<String> {
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_lowercase()).collect();
    let mut set = HashSet::new();
    if words.len() < NGRAM_WINDOW {
        return set;
    }
    for w in words.windows(NGRAM_WINDOW) {
        set.insert(w.join(" "));
    }
    set
}

/// HTML → plain text converter.
/// Inserts newlines at block-level boundaries (`<div>`, `<p>`, `<br>`, `<blockquote>`)
/// so that the N-gram deduplicator can work per-line.
pub(crate) fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();

    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let tag_lower = tag_buf.to_lowercase();
                // Extract tag name (strip attributes)
                let tag_name = tag_lower.split_whitespace().next().unwrap_or("");
                let tag_name = tag_name.trim_start_matches('/');
                if matches!(tag_name, "div" | "p" | "br" | "blockquote" | "tr" | "li" | "hr") {
                    out.push('\n');
                }
            }
            _ if in_tag => {
                tag_buf.push(c);
            }
            _ => out.push(c),
        }
    }

    // Collapse each line's internal whitespace, drop empty lines
    out.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{default_cf_key_map, parse_cf_key_map};

    #[test]
    fn parses_full_mapping() {
        let raw = r#"{"serial":["cf_serial_number"],"model":["cf_in_body_model"],
            "company":["cf_company"],"street":["cf_street"],"city":["cf_city"],
            "country":["cf_country_1"]}"#;
        let got = parse_cf_key_map(raw).expect("valid JSON parses");
        assert_eq!(got.get("serial"), Some(&vec!["cf_serial_number".to_string()]));
        assert_eq!(got.get("model"), Some(&vec!["cf_in_body_model".to_string()]));
    }

    #[test]
    fn parses_multiple_candidate_keys_per_slot() {
        let raw = r#"{"serial":["cf_serial_number","cf_serialnr"]}"#;
        let got = parse_cf_key_map(raw).expect("valid JSON parses");
        assert_eq!(
            got.get("serial"),
            Some(&vec!["cf_serial_number".to_string(), "cf_serialnr".to_string()])
        );
    }

    #[test]
    fn none_on_invalid_json() {
        assert_eq!(parse_cf_key_map("not json"), None);
    }

    #[test]
    fn none_on_wrong_shape() {
        // Values must be arrays of strings, not bare strings or numbers.
        assert_eq!(parse_cf_key_map(r#"{"serial":"cf_serial_number"}"#), None);
        assert_eq!(parse_cf_key_map(r#"{"serial":[1,2]}"#), None);
        assert_eq!(parse_cf_key_map(r#"["not","an","object"]"#), None);
    }

    #[test]
    fn default_map_reproduces_current_hardcoded_keys() {
        let got = default_cf_key_map();
        assert_eq!(got.get("serial"), Some(&vec!["cf_serial_number".to_string()]));
        assert_eq!(got.get("model"), Some(&vec!["cf_in_body_model".to_string()]));
        assert_eq!(got.get("company"), Some(&vec!["cf_company".to_string()]));
        assert_eq!(got.get("street"), Some(&vec!["cf_street".to_string()]));
        assert_eq!(got.get("city"), Some(&vec!["cf_city".to_string()]));
        assert_eq!(got.get("country"), Some(&vec!["cf_country_1".to_string()]));
    }
}
