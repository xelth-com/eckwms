use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::obfuscate_pii;
use reqwest::Client as HttpClient;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use super::loop_guard::LoopGuard;
use super::telemetry::{log_telemetry, current_budget_level, BudgetLevel, THROTTLE_DELAY_SECS};
use tracing::{info, warn};

const BATCH_LIMIT: usize = 5;
const LOOP_INTERVAL_SECS: u64 = 10;
const RATE_LIMIT_MS: u64 = 500;
const MAX_RETRIES: i64 = 5;

const TICKET_PROMPT: &str = r#"You are an expert Level 3 Technical Support Engineer and Logistics Coordinator for InBody devices.
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
**Gerät / Modell:** (Extract device model or serial number, e.g., "InBody 770", "SN: 12345").
**Hauptproblem (Symptom):** (Briefly describe the technical failure in 1-2 sentences).
**Durchgeführte Schritte:** (Troubleshooting steps already taken).
**Lösung / Status:** (Current status, e.g., "RMA needed", "Waiting for customer", "Resolved")."#;

const INVOICE_PROMPT: &str = r#"You are an expert AI assistant for an ERP and Warehouse Management System.
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
pub async fn start_summarization_worker(db: SurrealDb, model: String) {
    // Delay to let the server finish startup
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    info!("[Summarization] Worker started ({LOOP_INTERVAL_SECS}s interval, model={model})");

    // Reset retryable summaries on startup — errored, skipped, and docs
    // the Observer paused after detecting a loop. Paused docs get their
    // retry counter cleared because the loop root cause (UPSERT CONTENT
    // wipe + volatile hash) has been fixed at the import layer.
    // Docs that failed MAX_RETRIES times stay as 'failed' to prevent infinite loops.
    let reset = db
        .query(&format!(
            "UPDATE document SET \
                summary_status = 'pending', \
                summary_retries = IF summary_status = 'paused_by_observer' THEN 0 ELSE summary_retries END, \
                summary_error = NONE \
             WHERE summary_status IN ['error', 'skipped', 'paused_by_observer'] \
             AND type IN ['support_ticket', 'invoice'] \
             AND (summary_retries IS NONE OR summary_retries < {MAX_RETRIES}) \
             RETURN NONE"
        ))
        .await;
    match reset {
        Ok(_) => info!("[Summarization] Reset retryable documents to pending (max {MAX_RETRIES} retries)"),
        Err(e) => warn!("[Summarization] Failed to reset docs: {e}"),
    }

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

    loop {
        interval.tick().await;

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

        if let Err(e) = process_pending(&db, &http, &auth, &model, &guard).await {
            warn!("[Summarization] cycle error: {e}");
        }
    }
}

async fn process_pending(db: &SurrealDb, http: &HttpClient, auth: &eck_core::ai::AiAuth, model: &str, guard: &LoopGuard) -> Result<(), anyhow::Error> {
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
    let docs: Vec<Value> = db
        .query(&format!(
            "SELECT record::id(id) AS id, type, status, meta, ai_summary, summary_status, ticket_id, summary_retries \
             FROM document \
             WHERE summary_status = 'pending' \
             AND type IN ['support_ticket', 'invoice'] \
             AND (summary_retries IS NONE OR summary_retries < {MAX_RETRIES}) \
             AND (summary_retries IS NONE OR summary_retries = 0 \
                  OR updated_at IS NONE \
                  OR time::now() > updated_at + type::duration(string::concat(math::pow(2, summary_retries ?? 0), 'm'))) \
             LIMIT {BATCH_LIMIT}"
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

        // Fetch heavy payload from document_raw (local shadow table)
        let raw_id = id.split(':').last().unwrap_or(&id).trim_matches('`').to_string();
        let raw_doc: Option<Value> = db
            .query("SELECT payload FROM document_raw WHERE record::id(id) = $id LIMIT 1")
            .bind(("id", raw_id.clone()))
            .await?
            .take(0)?;

        let (raw_text, pii_mask, prompt) = match doc_type {
            "support_ticket" => {
                // Merge: use raw payload if available, fall back to document fields
                let merged = if let Some(ref raw) = raw_doc {
                    let mut d = doc.clone();
                    if let Some(p) = raw.get("payload") {
                        d.as_object_mut().map(|o| o.insert("payload".to_string(), p.clone()));
                    }
                    d
                } else {
                    doc.clone()
                };
                let (text, mask) = build_ticket_text(db, &merged, raw_id.clone()).await?;
                (text, mask, TICKET_PROMPT)
            }
            "invoice" => {
                let merged = if let Some(ref raw) = raw_doc {
                    let mut d = doc.clone();
                    if let Some(p) = raw.get("payload") {
                        d.as_object_mut().map(|o| o.insert("payload".to_string(), p.clone()));
                    }
                    d
                } else {
                    doc.clone()
                };
                let (text, mask) = build_invoice_text(&merged);
                (text, mask, INVOICE_PROMPT)
            }
            _ => continue,
        };

        if raw_text.is_empty() {
            warn!("[Summarization] Skipping {id} ({doc_type}): empty text. raw_doc={}, raw_id={}", raw_doc.is_some(), &raw_id);
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
                let _ = pii_mask; // mask map not needed — tokens are deterministic
                let updated: Vec<Value> = db.query(
                    "UPDATE type::record($rid) SET ai_summary = $summary, summary_status = 'completed', \
                         summary_error = NONE, embedding_status = 'pending', \
                         embedding_retries = 0, embedding_error = NONE \
                     RETURN record::id(id) AS id",
                )
                .bind(("rid", format!("document:`{}`", id)))
                .bind(("summary", masked_summary.clone()))
                .await?
                .take(0)?;

                // Telemetry always logs — Gemini was called either way.
                if !usage.is_null() {
                    log_telemetry(db, "summarization", model, &id, &usage).await;
                }

                if updated.is_empty() {
                    // The success-path UPDATE silently matched 0 rows. This is the
                    // infinite-loop trap from 2026-04-21: Gemini was paid, but
                    // summary_status stayed 'pending', so the worker re-picked the
                    // same doc on the next tick. Force the retry counter forward
                    // via an ID-insensitive WHERE so exponential backoff can kick in.
                    warn!("[Summarization] success-path UPDATE matched 0 rows for {id} — forcing retry counter");
                    let forced: Vec<Value> = db.query(
                        "UPDATE document SET \
                             summary_status = 'error', \
                             summary_error = 'phantom update: type::record matched 0 rows', \
                             summary_retries = (summary_retries ?? 0) + 1, \
                             updated_at = time::now() \
                         WHERE record::id(id) = $id AND summary_status = 'pending' \
                         RETURN record::id(id) AS id",
                    )
                    .bind(("id", id.clone()))
                    .await?
                    .take(0)?;
                    if forced.is_empty() {
                        warn!("[Summarization] fallback UPDATE also matched 0 rows for {id} — record truly missing");
                    }
                } else {
                    count += 1;
                    guard.clear(&id);
                    info!("[Summarization] Summarized {id} ({doc_type}, {} chars)", masked_summary.len());
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

/// Build the raw text for summarization by combining ticket metadata and all thread contents.
/// PII (names, emails, phones, addresses) is replaced with numbered placeholders.
async fn build_ticket_text(
    db: &SurrealDb,
    ticket: &Value,
    ticket_id: String,
) -> Result<(String, PiiMask), anyhow::Error> {
    let mut parts = Vec::new();
    let mut pii = PiiMask::new();

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
        // Contact info — mask PII
        if let Some(contact) = t.get("contact") {
            let first = contact.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
            let last = contact.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
            let full_name = format!("{first} {last}").trim().to_string();
            if !full_name.is_empty() {
                let token = pii.mask("Name", &full_name);
                parts.push(format!("Contact: {token}"));
                // Also register individual name parts for thread content masking
                if !first.is_empty() && first.len() > 2 {
                    pii.mask("Name", first);
                }
                if !last.is_empty() && last.len() > 2 {
                    pii.mask("Name", last);
                }
            }
            if let Some(email) = contact.get("email").and_then(|v| v.as_str()) {
                let token = pii.mask("Email", email);
                parts.push(format!("Email: {token}"));
            }
            if let Some(phone) = contact.get("phone").and_then(|v| v.as_str()) {
                let token = pii.mask("Phone", phone);
                parts.push(format!("Phone: {token}"));
            }
            if let Some(acc) = contact.get("account") {
                if let Some(name) = acc.get("accountName").and_then(|v| v.as_str()) {
                    let token = pii.mask("Company", name);
                    parts.push(format!("Company: {token}"));
                }
            }
        }
        // Custom fields contain device/serial/address data
        if let Some(cf) = t.get("cf") {
            for (key, label, category) in [
                ("cf_serial_number", "Serial Number", None),
                ("cf_in_body_model", "Model", None),
                ("cf_company", "Company", Some("Company")),
                ("cf_street", "Address", Some("Address")),
                ("cf_city", "City", None),  // city/zip are OK to send
                ("cf_country_1", "Country", None),
            ] {
                if let Some(s) = cf.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() && s != "null" {
                        if let Some(cat) = category {
                            let token = pii.mask(cat, s);
                            parts.push(format!("{label}: {token}"));
                        } else {
                            parts.push(format!("{label}: {s}"));
                        }
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
            // Also collect PII from thread `from` field (often "Name <email>")
            if let Some(from) = payload.get("fromEmailAddress").and_then(|v| v.as_str()) {
                pii.mask("Email", from);
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
    let masked = pii.mask_text(&full);
    // Truncate to ~28000 chars to stay within Gemini context limits
    let result = if masked.len() > 28000 {
        format!("{}\n\n[... truncated ...]", &masked[..28000])
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
    let result = if full.len() > 28000 {
        format!("{}\n\n[... truncated ...]", &full[..28000])
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
    let user_msg = format!(
        "Analyze the following raw support ticket data and produce the structured summary:\n\n{raw_text}"
    );

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
fn strip_html(s: &str) -> String {
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
