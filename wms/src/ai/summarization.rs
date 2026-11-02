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

/// The grounded Adressen slot. Gemini routes keep Google-Search grounding, so
/// the model MAY look up an incomplete address and note the source. Spliced into
/// `TICKET_PROMPT_TEMPLATE` via `{{ADDRESS_RULE}}` and swapped out on ungrounded
/// (OpenRouter) routes — see [`or_system_prompt`].
pub(crate) const ADDRESS_RULE_GROUNDED: &str = "**Adressen:** (Extract the physical street address, ZIP code, and city. IF the address is incomplete, USE GOOGLE SEARCH to look up the company name, email domain, or phone number and find their official physical address. IMPORTANT: Output the clean, raw address on the FIRST line. If you used Google Search to find it, add your explanation, warning, or thoughts on a NEW LINE below the address and enclose the entire explanation in parentheses).";

/// The ungrounded Adressen slot for web-less (OpenRouter) routes: no lookups, no
/// guessing, no "keine Recherche möglich" note — cheap models otherwise write a
/// missing-data apology INTO the stored summary. Replaces [`ADDRESS_RULE_GROUNDED`]
/// in [`or_system_prompt`].
const ADDRESS_RULE_UNGROUNDED: &str = "**Adressen:** (Extract the physical street address, ZIP code, and city exactly as they appear in the text. If the address is incomplete, output only what the text contains: never look anything up, never guess, and do not add any note about missing data, tools, or web access.)";

/// Shared PPRL token-semantics rule injected into every summariser/brain prompt
/// so the model treats tokens as opaque and never invents a "masking" apology.
/// One source of truth — also spliced into the orchestrator brain prompts.
pub(crate) const PPRL_TOKEN_RULE: &str = "PPRL tokens represent names, emails, phones, addresses, companies, IBANs, VAT IDs, or payment cards, as indicated by their prefixes (Name_…, Email_…, Phone_…, Address_…, Company_…, Iban_…, VatId_…, Card_…). Treat them as opaque identifiers: never decode, guess, alter, or invent them. When you keep a fact, copy its tokens exactly and preserve their associations. City, postal code, and country may appear separately in the clear; that is allowed, not missing address data or a privacy leak. Do not add commentary about masking or unavailable tools to your output.";

const TICKET_PROMPT_TEMPLATE: &str = r#"You are an expert Level 3 Technical Support Engineer and Logistics Coordinator for {{DEVICES}}.
Your task is to analyze a raw, noisy customer support email thread and extract the core technical facts AND all logistics/contact footprints.

CRITICAL INSTRUCTIONS:
- Ignore greetings and emotional complaints.
- Synthesize the entire thread into a single, cohesive summary.
- Output the result in German.
- {{PPRL_TOKEN_RULE}} You MUST preserve these tokens EXACTLY as they appear in your output — do NOT replace, translate, summarize, or remove them.

Extract the information into the following strict structure:

=== LOGISTIK & KONTAKTE ===
**Firma / Einrichtung:** (Extract company names, clinic names, or practices. If multiple, list them).
**Kontaktpersonen:** (List ALL distinct names, emails, and phone numbers found in the text and email signatures. This is crucial for matching future physical packages).
{{ADDRESS_RULE}}

=== TECHNISCHE DETAILS ===
**Gerät / Modell:** (Extract device model or serial number, e.g., "Model 770", "SN: 12345").
**Hauptproblem (Symptom):** (Briefly describe the technical failure in 1-2 sentences).
**Durchgeführte Schritte:** (Troubleshooting steps already taken).
**Lösung / Status:** (Current status, e.g., "RMA needed", "Waiting for customer", "Resolved").

=== PII SELF-CHECK (do this LAST) ===
The INPUT above was pseudonymized: real personal data of private individuals should appear ONLY as tokens (Name_…, Email_…, Phone_…, Address_…). If — and only if — you notice UNMASKED personal data of a real person in the INPUT (a real person's name, an email address, a phone number, or a postal address written in the clear instead of as a token), report it so it can be masked next time. Company, clinic, practice, dealer, and brand names in the clear are BY DESIGN — never report those. City, postal code, and country in the clear are allowed too — never report those. When (and only when) you have something to report, append as the VERY LAST line of your output a single line that starts with the exact marker ⟦PII_LEAKS⟧ immediately followed by a compact JSON array, e.g.:
⟦PII_LEAKS⟧[{"quote":"Hans Müller","kind":"PER"},{"quote":"hans@example.com","kind":"EMAIL"}]
"kind" is one of PER, EMAIL, PHONE, ADDR, OTHER. "quote" is the exact clear substring copied verbatim from the INPUT. If you noticed no unmasked personal data, do NOT output the marker or that line at all — omit it entirely."#;

/// The ticket summarization system prompt, with the tenant's device phrase
/// spliced in (env `ECK_TENANT_BRAND`; neutral "the company's devices" when
/// unset). Everything except that one phrase is byte-identical to the template.
pub(crate) fn ticket_prompt() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        TICKET_PROMPT_TEMPLATE
            .replace("{{DEVICES}}", &super::branding::devices_phrase())
            .replace("{{ADDRESS_RULE}}", ADDRESS_RULE_GROUNDED)
            .replace("{{PPRL_TOKEN_RULE}}", PPRL_TOKEN_RULE)
    })
}

/// The invoice summarization system prompt with the shared PPRL token rule
/// spliced in (env-free; everything except `{{PPRL_TOKEN_RULE}}` is the template).
pub(crate) fn invoice_prompt() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| INVOICE_PROMPT_TEMPLATE.replace("{{PPRL_TOKEN_RULE}}", PPRL_TOKEN_RULE))
}

const INVOICE_PROMPT_TEMPLATE: &str = r#"You are an expert AI assistant for an ERP and Warehouse Management System.
Your task is to analyze a raw invoice (Rechnung) document and extract the core logistical and product data.
- {{PPRL_TOKEN_RULE}} You MUST preserve these tokens EXACTLY as they appear.

Extract the information into the following strict structure in German:

=== KÄUFER & ADRESSEN ===
**Rechnungsadresse:** (Extract the billing company, name, and address)
**Lieferadresse:** (Extract the shipping/delivery address if different)
**Kontaktdaten:** (Email, phone numbers)

=== POSITIONEN & SERIENNUMMERN ===
**Gekaufte Artikel:** (List the models/products purchased)
**Seriennummern:** (Extract ALL serial numbers mentioned in the invoice. This is CRITICAL for warranty tracking).

=== PII SELF-CHECK (do this LAST) ===
The INPUT above was pseudonymized: real personal data of private individuals should appear ONLY as tokens (Name_…, Email_…, Phone_…, Address_…). If — and only if — you notice UNMASKED personal data of a real person in the INPUT (a real person's name, an email address, a phone number, or a postal address written in the clear instead of as a token), report it so it can be masked next time. Company, clinic, practice, dealer, and brand names in the clear are BY DESIGN — never report those. City, postal code, and country in the clear are allowed too — never report those. When (and only when) you have something to report, append as the VERY LAST line of your output a single line that starts with the exact marker ⟦PII_LEAKS⟧ immediately followed by a compact JSON array, e.g.:
⟦PII_LEAKS⟧[{"quote":"Hans Müller","kind":"PER"},{"quote":"hans@example.com","kind":"EMAIL"}]
"kind" is one of PER, EMAIL, PHONE, ADDR, OTHER. "quote" is the exact clear substring copied verbatim from the INPUT. If you noticed no unmasked personal data, do NOT output the marker or that line at all — omit it entirely."#;

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

/// Register one person-name span (whole + its >2-char parts) into `pii` so the
/// final `mask_text` tokenises it. Parts are registered like the structured
/// contact path (`build_ticket_text`) so a lone signature token or an inflected
/// mention that carries only one part still masks.
fn register_name_span(pii: &mut PiiMask, span: &str) {
    let span = span.trim();
    if span.chars().count() < 2 {
        return;
    }
    pii.mask("Name", span);
    if span.contains(' ') {
        for part in span.split_whitespace() {
            if part.len() > 2 {
                pii.mask("Name", part);
            }
        }
    }
}

/// Blank each `span` (case-insensitive) out of `text`, replacing it with a
/// single space. Used to hide NER-ORG (brand) regions from the lexicon
/// heuristic so a person-shaped brand ("John Harris Fitness") is never harvested
/// as a name. Positions are irrelevant — the heuristic returns span *strings*,
/// which stay verbatim substrings of the original text for every non-org name.
fn blank_spans(text: &str, spans: &[String]) -> String {
    let mut out = text.to_string();
    let mut spans: Vec<&String> = spans.iter().collect();
    spans.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for span in spans {
        if span.chars().count() < 2 {
            continue;
        }
        if let Ok(re) = regex::Regex::new(&format!("(?i){}", regex::escape(span))) {
            out = re.replace_all(&out, " ").into_owned();
        }
    }
    out
}

/// Egress person-name ladder over the ASSEMBLED outbound text (subject + all
/// thread prose), registering every third-party person name into `pii` so the
/// final `mask_text` tokenises it before the text reaches Gemini. This closes
/// the free-text NER gap: the structured from/to/cc/author path only catches
/// names that appear in a header, not a person named only inside a sentence or
/// signature line.
///
/// Two layers, mirroring the subject masker (`services::support::mask_subject`)
/// and the doc-classify chain (`ai::doc_classify::mask_extracted_text`):
///   (a) on-device NER PER spans — passed in as `ner = Some((persons, orgs))`
///       when `ECK_PII_NER=local` — each gated by `plausible_person_name` and
///       dropped when it lies inside an NER-ORG span (a person-shaped brand like
///       "Emil Krause Fitness" is a company and stays clear — policy + search);
///   (b) the lexicon/title heuristic (`mask_person_names_heuristic`), ALWAYS —
///       the model-less backstop — run over a copy with the NER-ORG spans
///       blanked so a brand is never harvested as a name.
/// Every span is registered whole AND split into its >2-char parts. Pure and
/// deterministic over `(text, ner)` so it is unit-testable with `ner = None`;
/// the single NER inference over the whole text is done once by the caller.
fn register_prose_person_names(pii: &mut PiiMask, text: &str, ner: Option<(&[String], &[String])>) {
    let orgs: Vec<String> = ner.map(|(_, o)| o.to_vec()).unwrap_or_default();
    let in_org = |name: &str| {
        let n = name.trim().to_lowercase();
        orgs.iter().any(|o| {
            let o = o.to_lowercase();
            o == n || o.contains(&n)
        })
    };

    // (a) NER PER spans — gated + org-excluded.
    if let Some((persons, _)) = ner {
        for name in persons {
            let name = name.trim();
            if name.chars().count() < 2 {
                continue;
            }
            if !eck_core::utils::anonymizer::plausible_person_name(name) {
                continue;
            }
            if in_org(name) {
                continue;
            }
            register_name_span(pii, name);
        }
    }

    // (b) Lexicon/title heuristic over a copy with NER-ORG spans blanked. We only
    // consume the emitted (token, span) pairs to learn WHICH spans to register —
    // the tokenisation itself happens in the final `mask_text` over the raw text.
    let scratch = blank_spans(text, &orgs);
    let (_masked, pairs) = eck_core::utils::anonymizer::mask_person_names_heuristic(&scratch);
    for (_token, span) in pairs {
        register_name_span(pii, &span);
    }
}

/// One on-device NER pass over the whole outbound text — `Some((persons, orgs))`
/// only when `ECK_PII_NER=local` and the model loads, else `None` (heuristic-only,
/// the exact pre-NER behavior). The model windows long input internally
/// (450-token windows, 50 overlap), so a single call covers the full ~28 KB
/// body; NER is NEVER run per-thread.
/// One PII-NER pass over the whole outbound text via the ungated routing facade
/// `crate::ai::pii_mode`. Returns `Some((persons, orgs))` when the configured
/// backend produced spans, else `None` (heuristic-only masking — the exact
/// pre-NER behavior). Only the `local` and `remote` modes carry an entity
/// backend here: `remote` takes the SAME branch as `local` did (spec
/// §5/§9 slice 1 — the "eck masker" is just a different execution site for the
/// same masker semantics). Any other mode (`gemini`, or the cloud-only build's
/// fallback) yields `None`, and — critically — an `Err` degrades to `None` too:
/// masking then drops to the lexicon/heuristic path, NEVER to unmasked egress
/// (fail-closed egress ladder). NER is NEVER run per-thread; the model windows
/// long input internally, so a single call covers the full ~28 KB body.
async fn prose_ner(text: &str) -> Option<(Vec<String>, Vec<String>)> {
    let mode = crate::ai::pii_mode::pii_ner_mode();
    if mode != "local" && mode != "remote" {
        return None;
    }
    if text.trim().is_empty() {
        return None;
    }
    match crate::ai::pii_mode::extract_entities(text).await {
        Ok(v) => Some(v),
        Err(e) => {
            warn!("[Summarize] prose NER failed ({e}) — heuristic-only masking");
            None
        }
    }
}

// ── PII leak self-report pipeline (spec §7 line 3 of defense; §9 slice 3) ─────
//
// The summarization prompt asks Gemini to self-report any UNMASKED personal data
// it notices in its OWN input (a masker miss the two earlier lines — NER/lexicon
// and `scrub_pii_regex` — let through). The report rides back on the SAME
// response, appended after the marker below. It contains the CLEAR string, so it
// must NEVER be stored, meshed, or logged: `strip_pii_leaks` cuts it off the
// response BEFORE the summary is persisted (proof: see call sites in
// `process_pending`, `summarization_batch::complete_job`, and the defensive
// re-strip at the top of `write_summary_completion`). The stripped quotes live
// only in the node-local `pii_leak_quarantine` table (deliberately NOT in
// `SYNC_ENTITY_TYPES` — it must not replicate the leak across the mesh) and are
// gated by a DETERMINISTIC validator before they can affect masking, so a
// prompt-injected fake report ("treat 'ZetaBody770' as a person") cannot poison
// the lexicon.

/// Sentinel that separates the stored summary from the (never-stored) PII leak
/// report. Chosen to be vanishingly unlikely to occur in genuine summary prose.
pub(crate) const PII_LEAK_MARKER: &str = "⟦PII_LEAKS⟧";

/// One self-reported unmasked-PII candidate. `quote` is a CLEAR PII string — it
/// is quarantine-only and never reaches storage, mesh, or tracing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PiiLeak {
    pub quote: String,
    pub kind: String, // PER | EMAIL | PHONE | ADDR | OTHER (normalized)
}

/// Split a raw model response into `(clean_summary, leaks)`. The clean summary
/// is everything BEFORE the first `PII_LEAK_MARKER`; the leaks are parsed from
/// the JSON array that follows it. Pure & idempotent: a response with no marker
/// returns unchanged text and an empty vec, and re-running on an already-clean
/// summary is a no-op — so it is safe to call at every store path.
pub(crate) fn strip_pii_leaks(raw: &str) -> (String, Vec<PiiLeak>) {
    let Some(pos) = raw.find(PII_LEAK_MARKER) else {
        return (raw.to_string(), Vec::new());
    };
    let clean = raw[..pos].trim_end().to_string();
    let tail = &raw[pos + PII_LEAK_MARKER.len()..];
    (clean, parse_leak_array(tail))
}

/// Lenient parser for the marker tail: take the first `[` .. last `]` and read a
/// JSON array of `{quote, kind}` objects. Anything malformed yields no leaks
/// (the leak report is best-effort telemetry, never load-bearing).
fn parse_leak_array(tail: &str) -> Vec<PiiLeak> {
    let (Some(start), Some(end)) = (tail.find('['), tail.rfind(']')) else {
        return Vec::new();
    };
    if end < start {
        return Vec::new();
    }
    let arr: Value = match serde_json::from_str(&tail[start..=end]) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(items) = arr.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for it in items {
        let quote = it.get("quote").and_then(|v| v.as_str()).unwrap_or("").trim();
        if quote.is_empty() {
            continue;
        }
        let kind = normalize_leak_kind(it.get("kind").and_then(|v| v.as_str()).unwrap_or(""));
        out.push(PiiLeak { quote: quote.to_string(), kind });
    }
    out
}

fn normalize_leak_kind(raw: &str) -> String {
    match raw.trim().to_uppercase().as_str() {
        "PER" | "EMAIL" | "PHONE" | "ADDR" => raw.trim().to_uppercase(),
        _ => "OTHER".to_string(),
    }
}

/// Deterministic validator verdict for one quarantined leak. The LLM never
/// writes masking config directly (prompt-injection defense) — a leak only
/// becomes actionable after passing THIS gate.
#[derive(Debug, PartialEq)]
enum LeakDecision {
    /// PER that passes the person-name gate → lexicon hotfix (status "applied").
    Apply,
    /// Shape-valid but not auto-actionable (non-PER, or OTHER) → status
    /// "pending", left for the Operator Inbox to confirm.
    Park(String),
    /// Fails the gate (brand/ORG/German phrase-name, or wrong shape) → "rejected".
    Reject(String),
}

fn email_shape(s: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$").unwrap()
    })
    .is_match(s.trim())
}

fn phone_shape(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    let ok_chars = s
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_whitespace() || "+()-/.".contains(c));
    digits >= 6 && digits <= 15 && ok_chars
}

fn addr_shape(s: &str) -> bool {
    let s = s.trim();
    // A postal address carries at least a house/ZIP number and enough letters
    // for a street/city name.
    s.chars().count() >= 6
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().filter(|c| c.is_alphabetic()).count() >= 3
}

/// Deterministic validation. PER reuses the existing `plausible_person_name`
/// gate (core anonymizer) — the same one built to stop German phrase-"names"
/// and, per-word, to reject brand-stoplist and org-marker tokens — so a
/// person-shaped brand or a "noun + verb" fragment is rejected without any LLM
/// involvement. EMAIL/PHONE/ADDR are shape-checked. Pure → unit-testable.
fn validate_leak(leak: &PiiLeak) -> LeakDecision {
    match leak.kind.as_str() {
        "PER" => {
            if eck_core::utils::anonymizer::plausible_person_name(&leak.quote) {
                LeakDecision::Apply
            } else {
                LeakDecision::Reject(
                    "not a plausible person name (brand/org marker or German phrase-name)".into(),
                )
            }
        }
        "EMAIL" => {
            if email_shape(&leak.quote) {
                LeakDecision::Park("email shape valid; awaiting operator confirmation".into())
            } else {
                LeakDecision::Reject("not email-shaped".into())
            }
        }
        "PHONE" => {
            if phone_shape(&leak.quote) {
                LeakDecision::Park("phone shape valid; awaiting operator confirmation".into())
            } else {
                LeakDecision::Reject("not phone-shaped".into())
            }
        }
        "ADDR" => {
            if addr_shape(&leak.quote) {
                LeakDecision::Park("address shape valid; awaiting operator confirmation".into())
            } else {
                LeakDecision::Reject("not address-shaped".into())
            }
        }
        // OTHER: no deterministic shape to trust; never auto-applied, parked for
        // a human. (Also the confirmed→synthetic-example seam — see below.)
        _ => LeakDecision::Park("kind OTHER; awaiting operator confirmation".into()),
    }
}

/// Persist stripped leaks into the node-local quarantine, validate each, and —
/// for a validated PER — apply the lexicon hotfix (the row itself, once
/// `status = 'applied'`, is what `register_lexicon_overlay` masks from the next
/// document on) and file an Operator Inbox item for human confirmation.
///
/// Best-effort: any DB error is logged and swallowed — a leak-report hiccup must
/// never fail or retry the summary write that already succeeded. `source_entity`
/// is the clear record id string (e.g. `document:abc`). Skipped entirely under
/// an effective-clear LLM policy, where PII is clear in the summary BY DECISION
/// and a "leak" report is meaningless.
pub(crate) async fn process_pii_leaks(db: &SurrealDb, source_entity: &str, leaks: Vec<PiiLeak>) {
    if leaks.is_empty() {
        return;
    }
    if crate::ai::pii_policy::effective_clear(crate::ai::pii_policy::Surface::Llm) {
        return;
    }
    for leak in leaks {
        // Dedup: one row per (source_entity, quote) — a doc summarized N times on
        // retry must not spawn N identical quarantine rows.
        let existing: Result<Option<Value>, _> = async {
            db.query(
                "SELECT record::id(id) AS id FROM pii_leak_quarantine \
                 WHERE source_entity = $se AND quote = $q LIMIT 1",
            )
            .bind(("se", source_entity.to_string()))
            .bind(("q", leak.quote.clone()))
            .await?
            .take(0)
        }
        .await;
        match existing {
            Ok(Some(_)) => continue, // already quarantined
            Ok(None) => {}
            Err(e) => {
                warn!("[PiiLeak] quarantine dedup query failed: {e}");
                continue;
            }
        }

        let decision = validate_leak(&leak);
        let (status, verdict) = match &decision {
            LeakDecision::Apply => ("applied", "validated PER; lexicon hotfix applied".to_string()),
            LeakDecision::Park(v) => ("pending", v.clone()),
            LeakDecision::Reject(v) => ("rejected", v.clone()),
        };

        let insert = db
            .query(
                "INSERT INTO pii_leak_quarantine { \
                    source_entity: $se, \
                    quote: $q, \
                    kind: $k, \
                    detected_at: time::now(), \
                    status: $status, \
                    validator_verdict: $verdict \
                 }",
            )
            .bind(("se", source_entity.to_string()))
            .bind(("q", leak.quote.clone()))
            .bind(("k", leak.kind.clone()))
            .bind(("status", status.to_string()))
            .bind(("verdict", verdict))
            .await;
        if let Err(e) = insert.and_then(|mut r| r.check()) {
            warn!("[PiiLeak] quarantine insert failed: {e}");
            continue;
        }

        // NEVER log the quote — only the shape/decision.
        match decision {
            LeakDecision::Apply => {
                info!("[PiiLeak] validated {} leak from {source_entity} → lexicon hotfix", leak.kind);
                file_operator_inbox_item(db, source_entity, &leak.kind, status).await;
            }
            LeakDecision::Park(_) => {
                info!("[PiiLeak] parked {} leak from {source_entity} for operator review", leak.kind);
                file_operator_inbox_item(db, source_entity, &leak.kind, status).await;
            }
            LeakDecision::Reject(_) => {
                info!("[PiiLeak] rejected {} leak from {source_entity} (validator)", leak.kind);
            }
        }

        // TODO(spec §7.1 confirmed→synthetic): OUT OF SCOPE here (needs a
        // locale gazetteer). When an operator flips a row to `status='confirmed'`,
        // a downstream job should emit a SYNTHETIC training example — the
        // sentence pattern with a substituted gazetteer name of the SAME shape —
        // so the real string never enters the training corpus. Seam lives here.
    }
}

/// Operator Inbox filing — STUB. The existing Operator Inbox (orchestrator
/// triage) is task-centric: it is the paused-`ai_task` dashboard fed by the
/// `ai_inbox` table, and each item resumes a specific ReAct task by `task_id`
/// (see `handlers::ai::reply_to_task`). A standalone "confirm this PII leak"
/// item has no `ai_task` to attach to, so wiring it through `ai_inbox` would
/// force a fake task. Instead the `pii_leak_quarantine` rows ARE the review
/// queue: an operator surface lists `status IN ['applied','pending']` and flips
/// a confirmed row to `status='confirmed'`. This stub records intent; the real
/// operator-facing surface is deferred to §9 slice 4.
async fn file_operator_inbox_item(_db: &SurrealDb, source_entity: &str, kind: &str, status: &str) {
    // TODO(spec §9 slice 4): surface pii_leak_quarantine (status applied/pending)
    // in the Operator Inbox for human confirm/reject. No quote in this log line.
    info!("[PiiLeak] operator-inbox item (stub): {kind} leak on {source_entity} status={status}");
}

/// Runtime-extensible lexicon overlay for the summarization egress masker.
///
/// The compile-time lexicon (`mask_person_names_heuristic` / `first_name_lexicon`
/// in core anonymizer) cannot learn new names at runtime. This overlay closes
/// that gap deterministically: every `pii_leak_quarantine` row with a VALIDATED
/// PER quote (`status='applied'`) is registered into `pii` as a `Name` span, so
/// the same string is tokenised from the NEXT document on — the immediate,
/// no-retrain "lexicon hotfix" of spec §7.1. Kept local to the summarization
/// path (the leak's own egress path); broader maskers (e.g. `mask_subject`) can
/// consult the same table later if needed. Best-effort: a query error just skips
/// the overlay (heuristic masking still runs — fail-open toward the earlier
/// masking lines, never toward unmasked egress, which the final `scrub_pii_regex`
/// still backstops).
async fn register_lexicon_overlay(db: &SurrealDb, pii: &mut PiiMask, text: &str) {
    let rows: Result<Vec<Value>, _> = async {
        db.query(
            "SELECT quote FROM pii_leak_quarantine WHERE status = 'applied' AND kind = 'PER'",
        )
        .await?
        .take(0)
    }
    .await;
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            warn!("[PiiLeak] lexicon overlay query failed: {e}");
            return;
        }
    };
    for row in &rows {
        if let Some(q) = row.get("quote").and_then(|v| v.as_str()) {
            if text.contains(q) {
                register_name_span(pii, q);
            }
        }
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
        .timeout(std::time::Duration::from_secs(180))
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

        // Route resolution (spec §2/§7 — `.eck/OPENROUTER_TIER.md`). In SHADOW
        // mode the LIVE path stays on the default auth (managed Vertex re-mints
        // transparently; studio returns the static key) and the OR route is
        // computed separately, per doc, for logging only — it never persists.
        // Otherwise `resolve_for_task` returns the OpenRouter auth when
        // `summarization` is in ECK_AI_TASK_ROUTES (fail-open to default on an
        // empty key), else the same default auth as before.
        let shadow = std::env::var("ECK_AI_SHADOW").map(|v| v.trim() == "1").unwrap_or(false);
        let resolved = if shadow {
            eck_core::ai::AiAuth::resolve(&http).await
        } else {
            eck_core::ai::AiAuth::resolve_for_task(&http, "summarization").await
        };
        let auth = match resolved {
            Ok(a) if a.is_configured() => a,
            Ok(_) => continue,
            Err(e) => {
                warn!("[Summarization] token resolve failed: {e}");
                continue;
            }
        };
        let route_is_or = auth.or_model().is_some();
        // Shadow OR auth — Some only when ECK_AI_SHADOW=1 AND a usable OR route
        // exists; the per-doc shadow computation runs against this.
        let shadow_or_auth = if shadow {
            match eck_core::ai::AiAuth::resolve_for_task(&http, "summarization").await {
                Ok(a) if a.or_model().is_some() && a.is_configured() => Some(a),
                _ => None,
            }
        } else {
            None
        };

        // Batch path (Vertex/managed only): service in-flight jobs, orphan-guard
        // stranded 'batched' docs, and — when the pending pile is large enough and
        // ECK_SUMMARY_BATCH=1 — submit a Vertex batch-prediction job (≈50% cheaper
        // than live calls). Returns Ok(true) when a batch was submitted this tick
        // (the claimed docs left the 'pending' pool, so skip the live path); Ok(false)
        // to fall through to the per-doc live path for whatever trickle remains.
        // Studio auth, batch disabled, or any error → falls through untouched. An
        // OR route short-circuits the whole batch path to live (spec §6): OR live
        // glm is ~7× cheaper than Vertex batch flash-lite, and glm's :batch tier
        // currently prices ABOVE its live tier.
        let handled_by_batch = if route_is_or {
            false
        } else {
            match super::summarization_batch::batch_tick(&db, &http, &auth, &model, &instance_id).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("[Summarization] batch tick error: {e}");
                    false
                }
            }
        };
        if handled_by_batch {
            continue;
        }

        if let Err(e) = process_pending(&db, &http, &auth, &model, &guard, &instance_id, shadow_or_auth.as_ref()).await {
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
            Some((text, invoice_prompt()))
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
    // Defensive last-line guard on EVERY store path: no summary may carry a PII
    // self-report into `ai_summary` (which meshes and exits via /mcp) or the
    // fingerprint index. Live and batch callers already strip + process the
    // leaks with their source-entity context; this idempotent re-strip (a
    // no-op on already-clean text) makes it structurally impossible for the
    // marker+quote to be persisted even if a future caller forgets. Leaks are
    // discarded here — already handled upstream.
    let (masked_summary, _stripped_leaks) = strip_pii_leaks(masked_summary);
    let masked_summary = masked_summary.as_str();
    // Token index of the masked fields — powers exact `search_database` lookup
    // by pseudonym token. Derived, merkle-IGNORED; peer nodes re-derive their
    // own copy via the main.rs self-heal when the summary arrives over mesh.
    let fps = eck_core::utils::anonymizer::extract_pii_tokens(&format!("{subject_masked}\n{masked_summary}"));
    // Channel-B fuzzy signatures for the Name/Address tokens in this summary
    // (`.eck/PPRL_FUZZY.md` §B). `meta` is not in scope here, so this resolves
    // from the process mint log (the names were just masked); tokens it cannot
    // resolve are filled in by the embedding worker's re-derive (this write nulls
    // `embedding`, re-queuing it) or the main.rs backfill sweep.
    let pii_fuzzy = crate::ai::pii_policy::derive_pii_fuzzy(&serde_json::json!({}), &fps);
    // A fresh summary normally re-queues the embedding — but NOT if embeddings
    // are parked 'unavailable' (backend has no embedding model): reviving those
    // into 'pending' just recreates the loop the Observer flags as "stuck".
    // Nulling `embedding` is the fleet-wide re-embed signal (the vector is part
    // of the content hash; status fields do NOT sync).
    let updated: Vec<Value> = db
        .query(
            "UPDATE type::record($rid) SET ai_summary = $summary, summary_status = 'completed', \
                 summary_error = NONE, \
                 pii_fingerprints = $fps, pii_fuzzy = $pf, \
                 embedding = NONE, embedding_model = NONE, \
                 embedding_status = IF embedding_status = 'unavailable' THEN 'unavailable' ELSE 'pending' END, \
                 embedding_retries = 0, embedding_error = NONE \
             RETURN record::id(id) AS id",
        )
        .bind(("rid", format!("document:`{}`", id)))
        .bind(("summary", masked_summary.to_string()))
        .bind(("fps", fps))
        .bind(("pf", pii_fuzzy))
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

async fn process_pending(db: &SurrealDb, http: &HttpClient, auth: &eck_core::ai::AiAuth, model: &str, guard: &LoopGuard, instance_id: &str, shadow_or_auth: Option<&eck_core::ai::AiAuth>) -> Result<(), anyhow::Error> {
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
    let mut index_rebuilt = false;
    for doc in &docs {
        let id = match doc.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        // STALE-INDEX GUARD (2026-09-10 kiosk): the planner answers the
        // `summary_status = 'pending'` predicate from `document_sumstatus_idx`
        // and does NOT re-check the row, so an index entry left behind by a
        // failed commit (SurrealKV "Memtable arena is full") returned a
        // 'completed' doc as pending every 10 s — one paid re-summary per
        // cycle, ~2-6 phantom summaries per hour for weeks. The row itself
        // carries the truth: refuse anything not actually pending and rebuild
        // the index once per cycle so the phantom disappears.
        let row_status = doc.get("summary_status").and_then(|v| v.as_str()).unwrap_or("");
        if row_status != "pending" {
            warn!(
                "[Summarization] stale index: {id} came back from document_sumstatus_idx as pending but the row says {row_status:?} — skipping{}",
                if index_rebuilt { "" } else { ", rebuilding document_sumstatus_idx" }
            );
            if !index_rebuilt {
                index_rebuilt = true;
                if let Err(e) = db.query("REBUILD INDEX IF EXISTS document_sumstatus_idx ON document").await {
                    warn!("[Summarization] index rebuild failed: {e}");
                }
            }
            continue;
        }

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

        // Shadow soak (spec §7 Stage A): with ECK_AI_SHADOW=1 and an OR route
        // set, compute the OR summary alongside the real one, log task/model/
        // latency/cost/guard-hits under [or-shadow], and DISCARD it — the normal
        // path below runs on the default auth exactly as if the route didn't
        // exist. Never persists, never touches summary_status.
        if let Some(or_auth) = shadow_or_auth {
            let or_model = or_auth.or_model().unwrap_or("");
            let t0 = std::time::Instant::now();
            match eck_core::ai::with_ai_task("bench", summarize(http, or_auth, model, prompt, &raw_text)).await {
                Ok((or_summary, or_usage)) => {
                    let latency_ms = t0.elapsed().as_millis();
                    let cost = or_usage.get("openrouterCostUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let cjk = contains_cjk(&or_summary);
                    info!(
                        "[or-shadow] task=summarization model={or_model} doc={id} latency_ms={latency_ms} cost_usd={cost:.6} cjk_bleed={cjk} chars={}",
                        or_summary.len()
                    );
                }
                Err(e) => warn!("[or-shadow] task=summarization model={or_model} doc={id} error: {e}"),
            }
        }

        // Time the LIVE call so an OR-routed (non-shadow) summarization emits a
        // greppable one-liner for the Stage B kiosk-flip watch (journald log-grep;
        // usageMetadata alone isn't greppable). Same no-content rule as pii_remote.
        let live_is_or = auth.or_model().is_some();
        let live_t0 = std::time::Instant::now();
        // OR-first with a Gemini fallback ON ERROR (spec: GLM first, Gemini
        // fallback). A routed (OR/glm) summarize that fails is retried ONCE on
        // the default auth before the doc is marked errored, so an OpenRouter
        // outage can't wedge the mesh's summarization. (No hedge here — this is
        // a background task; latency is fine, only a hard OR error escalates.)
        let mut served_is_or = live_is_or;
        let primary = eck_core::ai::with_ai_task("summarization", summarize_with_guards(http, auth, model, prompt, &raw_text, &id)).await;
        let outcome = match primary {
            Ok(v) => Ok(v),
            Err(e) if live_is_or => {
                warn!("[ai-fallback] task=summarization glm→gemini doc={id}: {e}");
                match eck_core::ai::AiAuth::resolve(http).await {
                    Ok(def) if def.is_configured() => {
                        served_is_or = false;
                        eck_core::ai::with_ai_task("summarization", summarize_with_guards(http, &def, model, prompt, &raw_text, &id)).await
                    }
                    _ => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        match outcome {
            Ok((raw_summary, usage)) => {
                if served_is_or {
                    let latency_ms = live_t0.elapsed().as_millis();
                    let provider = usage.get("openrouterProvider").and_then(|v| v.as_str()).unwrap_or("");
                    let cost = usage.get("openrouterCostUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let or_model = auth.or_model().unwrap_or("");
                    info!(
                        "[or-live] task=summarization model={or_model} provider={provider} doc={id} latency_ms={latency_ms} cost_usd={cost:.6}"
                    );
                }
                // Split the (never-stored) PII self-report off the response BEFORE
                // anything is logged, stored, or sent for address grounding. Every
                // use of `masked_summary` below sees the CLEAN text; the leak
                // quotes funnel into the local quarantine + deterministic
                // validator + lexicon hotfix and never leave this node.
                let (masked_summary, leaks) = strip_pii_leaks(&raw_summary);
                process_pii_leaks(db, &format!("document:{id}"), leaks).await;
                // Store the masked summary in DB — real PII never touches the database.
                // Unmask happens on-the-fly in the API handler using deterministic PPRL tokens.
                let subject_masked = doc
                    .get("meta")
                    .and_then(|m| m.get("subject"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Telemetry logs the model that ACTUALLY served the call: the OR
                // route model (glm) when the OR path served it, else the Gemini
                // model. `model` alone was the configured Gemini name even when an
                // OR route served the request (kiosk showed 237 mislabelled rows).
                if !usage.is_null() {
                    let served_model = if served_is_or {
                        auth.or_model().unwrap_or(model)
                    } else {
                        model
                    };
                    log_telemetry(db, "summarization", served_model, &id, &usage).await;
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

    // Rolling n-gram history for deduplication — seed with ticket metadata.
    // A single incrementally-updated set (never rebuilt per thread) plus a hard
    // char budget keep assembly O(kept chars). A monster ticket with thousands
    // of near-duplicate threads therefore can NOT materialize an O(threads²)
    // n-gram set and silently abort the process on heap exhaustion (seen at
    // ~doc 96 of a 4-concurrent OpenRouter soak). Dedup quality degrades
    // gracefully past the budget; normal tickets (a handful of threads) are
    // byte-for-byte unaffected.
    let mut deduper = RollingDeduper::new(&parts.join("\n\n"));
    let mut truncated = false;

    for (i, thread) in threads.iter().enumerate() {
        if i >= MAX_TICKET_THREADS {
            truncated = true;
            break;
        }
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
                // N-gram dedup against the rolling history. `push` returns None
                // once the assembly char budget is exhausted — stop BEFORE doing
                // any more dedup/allocation work on a monster ticket.
                match deduper.push(&plain) {
                    Some(deduped) if !deduped.is_empty() => parts.push(deduped),
                    Some(_) => {}
                    None => {
                        truncated = true;
                        break;
                    }
                }
            }
        }
    }
    if truncated {
        parts.push("[... additional threads truncated ...]".to_string());
    }

    let full = parts.join("\n\n");
    // Egress person-name ladder over the WHOLE outbound text: register third-party
    // names that appear only in free-text prose / signatures (not in a structured
    // from/to/cc/author header — those are handled above) so `mask_text` tokenises
    // them before the text reaches Gemini. ONE NER inference over `full` (the model
    // windows internally); the lexicon heuristic always runs as the model-less
    // backstop. Skipped under the clear LLM policy, where `mask_text` is skipped too.
    if !clear {
        let ner_spans = prose_ner(&full).await;
        let ner_ref = ner_spans.as_ref().map(|(p, o)| (p.as_slice(), o.as_slice()));
        register_prose_person_names(&mut pii, &full, ner_ref);
        // Runtime lexicon hotfix: register any previously-validated PER leak
        // (pii_leak_quarantine status='applied') that appears in this text, so a
        // name a past summary reported unmasked is tokenised from here on.
        register_lexicon_overlay(db, &mut pii, &full).await;
    }
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

/// True when `text` contains any CJK ideograph, Hiragana, or Katakana. The
/// OpenRouter glm model occasionally drops a Chinese fragment into an otherwise
/// German summary (the LLM judge missed it in the bench; this deterministic
/// check won't) — see `summarize_with_guards`.
pub(crate) fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c,
            '\u{4e00}'..='\u{9fff}'   // CJK Unified Ideographs
            | '\u{3040}'..='\u{309f}' // Hiragana
            | '\u{30a0}'..='\u{30ff}' // Katakana
        )
    })
}

/// True for a single CJK ideograph / Hiragana / Katakana char.
fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}'   // CJK Unified Ideographs
        | '\u{3040}'..='\u{309f}' // Hiragana
        | '\u{30a0}'..='\u{30ff}' // Katakana
    )
}

/// VISION-scoped CJK guard (`.eck/OPENROUTER_TIER.md` §5.1, scoped per
/// `VISION_BENCH.md` finding #2). The strict [`contains_cjk`] used for text
/// summaries would false-positive on legitimate image summaries: the tenant's
/// devices carry a physically-printed multilingual "No Pacemaker" warning (CJK) that the
/// model faithfully transcribes. So for image-summary output we do NOT reject on
/// the mere presence of CJK — we reject only genuine *bleed*, using a simple,
/// robust two-part heuristic:
///
///   1. Strip explicit citation/transcription spans — text inside straight
///      quotes `"…"`, German quotes `„…"`, guillemets `«…»`/`»…«`, and CJK
///      corner brackets `「…」`/`『…』`. Printed foreign-language labels legitimately
///      live inside these, so they don't count against the model.
///   2. On the REMAINING German prose, reject if CJK chars make up **> 15 %** of
///      the non-whitespace characters. A few inline CJK chars (a transcribed
///      label word) stay well under the threshold; a model that drops into a
///      full Chinese sentence mid-summary blows past it.
///
/// Returns `true` when the output should be treated as bleeding (rejected).
pub(crate) fn cjk_bleed_in_image_summary(text: &str) -> bool {
    // 1) Drop quoted / bracketed transcription spans. Non-nesting: any recognized
    //    opener turns citation-mode ON, its matching closer turns it OFF. Straight
    //    quote `"` toggles (open == close). Good enough to exempt printed-label
    //    citations without a full grammar.
    let mut prose = String::with_capacity(text.len());
    let mut in_cite = false;
    for c in text.chars() {
        match c {
            '"' => in_cite = !in_cite, // U+0022 straight quote toggles
            '„' | '«' | '「' | '『' if !in_cite => in_cite = true, // openers
            '"' | '»' | '」' | '』' if in_cite => in_cite = false, // closers
            _ if in_cite => { /* inside a citation span: skip */ }
            _ => prose.push(c),
        }
    }
    // 2) Fraction of CJK among non-whitespace prose chars.
    let mut non_ws = 0usize;
    let mut cjk = 0usize;
    for c in prose.chars() {
        if c.is_whitespace() {
            continue;
        }
        non_ws += 1;
        if is_cjk_char(c) {
            cjk += 1;
        }
    }
    if non_ws == 0 {
        return false;
    }
    (cjk as f64) / (non_ws as f64) > 0.15
}

/// Call the AI to summarize document text (`summarize`), then apply the
/// OpenRouter-only output guards from spec §5:
///   1. CJK-bleed: if an OR summary carries CJK, reject and retry once on the
///      SAME OR auth; if it still bleeds, fall back to the default auth
///      (`AiAuth::resolve`) for THIS document and log the guard trip.
/// Non-OR auth is returned verbatim (the guards are OR-specific — Gemini doesn't
/// bleed CJK). Returns `(summary_text, usageMetadata)`.
async fn summarize_with_guards(
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    model: &str,
    system_prompt: &str,
    raw_text: &str,
    id: &str,
) -> Result<(String, Value), anyhow::Error> {
    let (summary, usage) = summarize(http, auth, model, system_prompt, raw_text).await?;
    if auth.or_model().is_none() || !contains_cjk(&summary) {
        return Ok((summary, usage));
    }
    // CJK bleed on an OR result — one retry on the same OR auth.
    warn!("[or-guard] cjk_bleed doc={id} — retrying once on OR");
    let (retry, retry_usage) = summarize(http, auth, model, system_prompt, raw_text).await?;
    if !contains_cjk(&retry) {
        return Ok((retry, retry_usage));
    }
    // Still bleeding — fall back to the default auth for this document.
    warn!("[or-guard] cjk_bleed doc={id} persisted after retry — falling back to default auth");
    let default_auth = eck_core::ai::AiAuth::resolve(http).await?;
    summarize(http, &default_auth, model, system_prompt, raw_text).await
}

/// Call the AI via direct HTTP to summarize document text.
/// Returns (summary_text, usageMetadata) for telemetry logging.
///
/// For an OpenRouter route (spec §5.2/§6): the Google-Search grounding tool is
/// dropped (OR has no web access, and the shim bails on any `tools`), and one
/// system-prompt line is appended forbidding invented/"looked-up" addresses —
/// the ungrounded models otherwise roleplay a search and fabricate street
/// addresses. Gemini keeps its grounding tool unchanged.
/// Adapt a grounded system prompt for a web-less (OpenRouter) route: swap the
/// grounded Adressen rule for the ungrounded one (so ungrounded models don't
/// roleplay a lookup or write a "keine Recherche möglich" apology into the
/// summary), then append the no-web-access line. Pure/HTTP-free so it can be
/// unit-tested; Gemini routes never call this and keep their grounding tool.
pub(crate) fn or_system_prompt(system_prompt: &str) -> String {
    let ungrounded = system_prompt.replace(ADDRESS_RULE_GROUNDED, ADDRESS_RULE_UNGROUNDED);
    format!(
        "{ungrounded}\n\nYou have NO web access. Address candidates come only from the ticket text; never invent or 'look up' an address."
    )
}

async fn summarize(
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    model: &str,
    system_prompt: &str,
    raw_text: &str,
) -> Result<(String, Value), anyhow::Error> {
    let user_msg = summary_user_message(raw_text);
    let is_or = auth.or_model().is_some();

    let system_prompt_owned;
    let system_prompt = if is_or {
        system_prompt_owned = or_system_prompt(system_prompt);
        system_prompt_owned.as_str()
    } else {
        system_prompt
    };

    let mut payload = serde_json::json!({
        "systemInstruction": { "parts": [{ "text": system_prompt }] },
        "contents": [{ "parts": [{ "text": user_msg }] }],
        "generationConfig": {
            "temperature": 0.2,
            "maxOutputTokens": 4096,
        }
    });
    if !is_or {
        payload["tools"] = serde_json::json!([{ "googleSearch": {} }]);
    }

    auth.generate_content(http, model, payload).await
}

/// One READ-ONLY OpenRouter shadow computation for the admin soak
/// (`/api/admin/or-soak`, spec §7 Stage A). Builds the masked input EXACTLY as
/// the live path would (`build_doc_masked_text` — SELECTs only, no writes), then
/// runs ONLY the OR summarize call. Returns
/// `Ok(Some((summary, usageMetadata, masked_input_chars)))` — `masked_input_chars`
/// is `raw_text.chars().count()` of the PPRL-MASKED prompt text, so the A/B
/// collector can report input size without leaking the text — or `Ok(None)` when
/// the doc has no usable local text (unknown type / raw-unavailable / empty), the
/// caller counts those as skipped. NEVER writes: no persist, no vclock, no mesh,
/// no DB telemetry. `or_auth` MUST be the OpenRouter variant (its route model is
/// used; the `model` arg is ignored on the OR path). On this node the usage-report
/// seam is a no-op (no `ECK_VERTEX_USAGE_URL`), so not even a wallet debit occurs.
pub(crate) async fn or_soak_summarize(
    db: &SurrealDb,
    http: &HttpClient,
    or_auth: &eck_core::ai::AiAuth,
    doc: &Value,
    id: &str,
) -> Result<Option<(String, Value, usize)>, anyhow::Error> {
    let (raw_text, prompt) = match build_doc_masked_text(db, doc, id).await? {
        Some(t) => t,
        None => return Ok(None), // unknown type
    };
    if raw_text.trim().is_empty() {
        return Ok(None); // raw-unavailable / empty local source
    }
    let masked_input_chars = raw_text.chars().count();
    let (summary, usage) = eck_core::ai::with_ai_task("bench", summarize(http, or_auth, "", prompt, &raw_text)).await?;
    Ok(Some((summary, usage, masked_input_chars)))
}

/// N-gram (shingling) deduplication for email threads.
///
/// A thread body is split into 7-word shingles; a line is dropped when >60% of
/// its shingles already exist in the rolling history. Lines shorter than the
/// window size are always kept — they're typically short replies or signatures
/// that would otherwise false-positive. See `RollingDeduper` for the bounded
/// per-ticket driver.
const NGRAM_WINDOW: usize = 7;
const DUP_THRESHOLD: f64 = 0.60;

/// Assembly bounds. These cap the WORK done before the dedup n-gram set can grow,
/// so a pathological ticket (thousands of threads) can never materialize an
/// O(threads²) shingle set and abort the process on heap exhaustion.
///
/// - `MAX_ASSEMBLED_CHARS` — stop appending thread bodies once the kept text
///   reaches this budget (matches the downstream ~28k Gemini-context cap, so a
///   normal ticket that never approaches it is unaffected).
/// - `MAX_TICKET_THREADS` — hard ceiling on threads inspected, a belt-and-suspenders
///   bound for the fully-duplicate case where the char budget never advances.
/// - `MAX_HISTORY_NGRAMS` — cap on the rolling history set; past it, new shingles
///   stop being learned (dedup degrades gracefully, memory stays bounded).
const MAX_ASSEMBLED_CHARS: usize = 28_000;
const MAX_TICKET_THREADS: usize = 2_000;
const MAX_HISTORY_NGRAMS: usize = 60_000;

/// Rolling-window n-gram deduplicator for email threads.
///
/// Holds the accumulated history shingles in a SINGLE incrementally-updated set
/// (never rebuilt per thread — that was the O(threads²) soak bug) and enforces a
/// hard char budget, so assembling a ticket is O(total kept chars) regardless of
/// thread count.
struct RollingDeduper {
    history: HashSet<String>,
    assembled_chars: usize,
}

impl RollingDeduper {
    /// Seed the history with the ticket metadata block.
    fn new(seed: &str) -> Self {
        let mut history = HashSet::new();
        add_ngrams(&mut history, seed);
        Self {
            history,
            assembled_chars: seed.chars().count(),
        }
    }

    /// Dedup one thread body against the rolling history and fold the kept text
    /// back into it. Returns:
    /// - `None` once the char budget is exhausted (caller should stop),
    /// - `Some("")` when the whole body was a duplicate,
    /// - `Some(kept)` otherwise.
    fn push(&mut self, content: &str) -> Option<String> {
        if self.assembled_chars >= MAX_ASSEMBLED_CHARS {
            return None;
        }
        let deduped = dedup_lines(content, &self.history);
        if deduped.is_empty() {
            return Some(String::new());
        }
        self.assembled_chars += deduped.chars().count();
        // Learn the new shingles — but stop once the set is saturated so memory
        // stays bounded even under a flood of distinct-but-huge threads.
        if self.history.len() < MAX_HISTORY_NGRAMS {
            add_ngrams(&mut self.history, &deduped);
        }
        Some(deduped)
    }
}

/// Drop lines of `content` whose 7-word shingles already appear in `history`
/// above `DUP_THRESHOLD`. Lines shorter than the window are always kept.
fn dedup_lines(content: &str, history: &HashSet<String>) -> String {
    if history.is_empty() {
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
            if history.contains(&shingle) {
                hits += 1;
            }
        }
        if (hits as f64 / total as f64) < DUP_THRESHOLD {
            kept.push(line);
        }
    }
    kept.join("\n").trim().to_string()
}

/// Insert the lowercase 7-word shingles of `text` into `set`.
fn add_ngrams(set: &mut HashSet<String>, text: &str) {
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words.len() < NGRAM_WINDOW {
        return;
    }
    for w in words.windows(NGRAM_WINDOW) {
        set.insert(w.join(" "));
    }
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
    use super::{
        cjk_bleed_in_image_summary, contains_cjk, default_cf_key_map, email_shape,
        or_system_prompt, parse_cf_key_map, phone_shape, register_prose_person_names,
        strip_pii_leaks, ticket_prompt, validate_leak, LeakDecision, PiiLeak, PiiMask,
        RollingDeduper, ADDRESS_RULE_GROUNDED, ADDRESS_RULE_UNGROUNDED, MAX_ASSEMBLED_CHARS,
        PII_LEAK_MARKER, PPRL_TOKEN_RULE,
    };

    fn per(q: &str) -> PiiLeak { PiiLeak { quote: q.into(), kind: "PER".into() } }

    #[test]
    fn ticket_prompt_keeps_grounded_rule_and_token_rule() {
        let p = ticket_prompt();
        assert!(
            p.contains(ADDRESS_RULE_GROUNDED),
            "grounded Adressen rule missing from ticket prompt"
        );
        assert!(
            p.contains(PPRL_TOKEN_RULE),
            "shared PPRL token rule missing from ticket prompt"
        );
    }

    #[test]
    fn or_prompt_drops_grounding_adds_ungrounded_rule() {
        let or = or_system_prompt(ticket_prompt());
        assert!(
            !or.contains("GOOGLE SEARCH"),
            "OR prompt still tells the model to use Google Search:\n{or}"
        );
        assert!(
            !or.contains(ADDRESS_RULE_GROUNDED),
            "grounded rule survived into the OR prompt"
        );
        assert!(
            or.contains(ADDRESS_RULE_UNGROUNDED),
            "ungrounded Adressen rule missing from OR prompt"
        );
        assert!(
            or.contains("You have NO web access"),
            "no-web-access line dropped from OR prompt"
        );
    }

    // ── Vision-scoped CJK guard (VISION_BENCH finding #2) ───────────────────────

    /// (a) A legit image summary that transcribes the printed multilingual "No
    /// Pacemaker" label must NOT be rejected: the strict text-guard trips on any
    /// CJK, but the scoped image guard tolerates a few inline/quoted CJK chars.
    #[test]
    fn cjk_image_guard_passes_legit_label_transcription() {
        let s = "Foto eines ZetaBody-Analysators, Frontansicht. Auf dem Gehäuse ist ein \
                 mehrsprachiger Warnaufkleber „无心脏起搏器\" (kein Herzschrittmacher) sichtbar. \
                 Das Display zeigt einen Messfehler. Seriennummer nicht im Bild. UNLESBAR: keine.";
        // Strict guard (text path) would reject this…
        assert!(contains_cjk(s));
        // …but the scoped image guard must accept it (few CJK chars, and quoted).
        assert!(!cjk_bleed_in_image_summary(s));
    }

    /// (b) A genuinely bleeding output — a long unquoted Chinese run mid-summary —
    /// must be rejected (CJK fraction of the German prose exceeds 15 %).
    #[test]
    fn cjk_image_guard_rejects_genuine_bleed() {
        let s = "Foto eines Geräts. 这是一个身体成分分析仪它可以测量体脂率和肌肉量并且显示在屏幕上\
                 用户需要赤脚站立在电极上以获得准确的测量结果。 Ende.";
        assert!(cjk_bleed_in_image_summary(s));
    }

    /// Pure-German output is never a bleed.
    #[test]
    fn cjk_image_guard_passes_pure_german() {
        assert!(!cjk_bleed_in_image_summary(
            "Reines deutsches Reparatur-Summary ohne jegliche CJK-Zeichen. UNLESBAR: keine."
        ));
    }

    // ── Monster-ticket dedup is bounded (OpenRouter soak regression) ────────

    /// A ticket with thousands of distinct-but-repetitive threads must assemble
    /// in bounded time and memory. Before the fix, `build_ticket_text` rebuilt
    /// the full n-gram set per thread over an ever-growing history (O(threads²))
    /// and the process silently died (heap exhaustion) at ~doc 96 of a
    /// 4-concurrent soak. The rolling deduper caps the work by an assembly char
    /// budget, so this completes in well under 5s and yields a non-empty, capped
    /// body instead of aborting.
    #[test]
    fn monster_ticket_dedup_is_bounded() {
        let start = std::time::Instant::now();
        // Each thread shares a repeated boilerplate greeting/signature (the part
        // real dedup should collapse) plus a unique sentence (so the history set
        // WOULD grow unbounded under the old per-thread rebuild).
        let boiler = "Sehr geehrte Damen und Herren wir haben leider erneut ein Problem \
                      mit dem gelieferten Gerät festgestellt und bitten um Rueckmeldung. \
                      Mit freundlichen Gruessen das Support Team der Firma Muster GmbH.";
        let mut deduper = RollingDeduper::new("Subject: Sammelticket\n\nStatus: Open");
        let mut assembled = String::new();
        let mut stopped_on_budget = false;
        let mut processed = 0usize;
        for i in 0..5_000usize {
            let content = format!(
                "{boiler}\nEindeutige Nachricht Nummer {i} mit zusaetzlichem Freitext Inhalt hier."
            );
            match deduper.push(&content) {
                Some(kept) => {
                    if !kept.is_empty() {
                        assembled.push_str(&kept);
                        assembled.push('\n');
                    }
                }
                None => {
                    stopped_on_budget = true;
                    break;
                }
            }
            processed += 1;
        }
        let elapsed = start.elapsed();
        assert!(elapsed.as_secs() < 5, "assembly took too long: {elapsed:?}");
        assert!(!assembled.is_empty(), "expected a non-empty assembled body");
        // The char budget must have halted assembly well before all 5000 threads.
        assert!(stopped_on_budget, "expected the char budget to stop assembly");
        assert!(processed < 5_000, "budget should stop before every thread is read");
        // Bounded output: at most the budget plus one final (partial) thread body.
        assert!(
            assembled.chars().count() <= MAX_ASSEMBLED_CHARS + 4_000,
            "assembled body exceeded the bound: {} chars",
            assembled.chars().count()
        );
    }

    // ── OpenRouter CJK-bleed guard (spec §5.1) ──────────────────────────────

    /// The CJK detector fires on Chinese/Japanese script and stays quiet on
    /// German prose (incl. umlauts/ß) that an OR glm summary normally contains —
    /// so it only trips the reject-retry-fallback on a genuine bleed.
    #[test]
    fn cjk_guard_detects_bleed_but_not_german() {
        assert!(!contains_cjk("=== TECHNISCHE DETAILS ===\nGerät defekt, Messungen brechen ab. Größe/Maß."));
        assert!(!contains_cjk("Name_8E5F3A1B00000000 meldet einen Ausfall."));
        assert!(contains_cjk("Gerät defekt 设备故障 RMA nötig")); // Chinese fragment mid-German
        assert!(contains_cjk("概要")); // pure CJK
        assert!(contains_cjk("これはテストです")); // Hiragana/Katakana
    }

    // ── PII leak self-report: strip-before-store ────────────────────────────

    /// A model response carrying a PII self-report is split so the stored summary
    /// contains NEITHER the marker NOR the clear quote — only the leak vec does.
    #[test]
    fn strip_removes_marker_and_quote_from_stored_summary() {
        let raw = "Zusammenfassung: Gerät defekt, RMA nötig.\n\
                   ⟦PII_LEAKS⟧[{\"quote\":\"Hans Müller\",\"kind\":\"PER\"},\
                   {\"quote\":\"hans@example.com\",\"kind\":\"email\"}]";
        let (clean, leaks) = strip_pii_leaks(raw);
        assert_eq!(clean, "Zusammenfassung: Gerät defekt, RMA nötig.");
        assert!(!clean.contains("Hans Müller"), "quote leaked into summary: {clean}");
        assert!(!clean.contains("hans@example.com"), "email leaked into summary: {clean}");
        assert!(!clean.contains(PII_LEAK_MARKER), "marker leaked into summary: {clean}");
        assert_eq!(leaks.len(), 2);
        assert_eq!(leaks[0].quote, "Hans Müller");
        assert_eq!(leaks[0].kind, "PER");
        assert_eq!(leaks[1].kind, "EMAIL"); // normalized to uppercase
    }

    /// No marker → text passes through unchanged, and re-stripping an already
    /// clean summary is a no-op (idempotent — safe at the defensive store guard).
    #[test]
    fn strip_is_idempotent_and_noop_without_marker() {
        let plain = "Nur eine saubere Zusammenfassung ohne Leaks.";
        let (clean, leaks) = strip_pii_leaks(plain);
        assert_eq!(clean, plain);
        assert!(leaks.is_empty());
        let (again, none) = strip_pii_leaks(&clean);
        assert_eq!(again, plain);
        assert!(none.is_empty());
    }

    /// Malformed JSON after the marker still strips the summary clean (leaks are
    /// best-effort) — a store must never carry the quote regardless.
    #[test]
    fn strip_handles_malformed_leak_json() {
        let raw = "Summary body.\n⟦PII_LEAKS⟧ not-json-at-all Hans Müller";
        let (clean, leaks) = strip_pii_leaks(raw);
        assert_eq!(clean, "Summary body.");
        assert!(!clean.contains("Hans Müller"));
        assert!(leaks.is_empty());
    }

    // ── PII leak self-report: deterministic validator ───────────────────────

    #[test]
    fn validator_accepts_real_person_name() {
        assert_eq!(validate_leak(&per("Hans Müller")), LeakDecision::Apply);
    }

    #[test]
    fn validator_rejects_person_shaped_brand() {
        // "Fitness" is an org marker → the person-name gate rejects it, so a
        // prompt-injected "unmask this brand" report cannot poison the lexicon.
        assert!(matches!(validate_leak(&per("Anytime Fitness")), LeakDecision::Reject(_)));
    }

    #[test]
    fn validator_rejects_german_phrase_name() {
        // "Messungen brechen" — noun + lowercase verb — the classic false PER.
        assert!(matches!(validate_leak(&per("Messungen brechen")), LeakDecision::Reject(_)));
    }

    #[test]
    fn validator_email_shapes() {
        assert!(email_shape("hans@example.com"));
        assert!(!email_shape("not an email"));
        assert_eq!(
            validate_leak(&PiiLeak { quote: "hans@example.com".into(), kind: "EMAIL".into() }),
            LeakDecision::Park("email shape valid; awaiting operator confirmation".into())
        );
        assert!(matches!(
            validate_leak(&PiiLeak { quote: "nope".into(), kind: "EMAIL".into() }),
            LeakDecision::Reject(_)
        ));
    }

    #[test]
    fn validator_phone_shapes() {
        assert!(phone_shape("+49 30 1234567"));
        assert!(!phone_shape("call me"));
        assert!(!phone_shape("12345")); // too few digits
        assert!(matches!(
            validate_leak(&PiiLeak { quote: "+49 30 1234567".into(), kind: "PHONE".into() }),
            LeakDecision::Park(_)
        ));
        assert!(matches!(
            validate_leak(&PiiLeak { quote: "call me".into(), kind: "PHONE".into() }),
            LeakDecision::Reject(_)
        ));
    }

    /// (a) A third-party name that appears ONLY in free-text prose — not in any
    /// structured header — is caught by the lexicon/title heuristic and
    /// tokenised by the egress pass (NER = None, so this exercises the
    /// model-less backstop that ALWAYS runs).
    #[test]
    fn prose_lexicon_third_party_name_tokenised() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let mut pii = PiiMask::new();
        let text = "Frau Erika Musterfrau schrieb am Montag, dass das Gerät defekt ist.";
        register_prose_person_names(&mut pii, text, None);
        let masked = pii.mask_text(text);
        assert!(!masked.contains("Erika"), "first name must be tokenised: {masked}");
        assert!(!masked.contains("Musterfrau"), "surname must be tokenised: {masked}");
        assert!(masked.contains("Name_"), "a Name token must appear: {masked}");
    }

    /// (b) An org-marker-protected brand ("John Harris Fitness") stays clear —
    /// the heuristic's own org-marker veto keeps a person-shaped brand out of
    /// the Name slot even with NER = None.
    #[test]
    fn prose_org_marker_brand_stays_clear() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let mut pii = PiiMask::new();
        let text = "Das Studio John Harris Fitness meldet einen Ausfall.";
        register_prose_person_names(&mut pii, text, None);
        let masked = pii.mask_text(text);
        assert!(masked.contains("John Harris Fitness"), "brand must stay clear: {masked}");
        assert!(!masked.contains("Name_"), "no name should be registered: {masked}");
    }

    /// (c) NER-ORG protection is honored when spans are provided: a PER span
    /// equal-to/inside an ORG span is NOT registered (brand stays clear), while
    /// a genuine person named alongside it IS tokenised.
    #[test]
    fn prose_ner_org_protection_honored() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let mut pii = PiiMask::new();
        let text = "John Harris Fitness beschäftigt Julia Weber als Technikerin.";
        let persons = vec!["John Harris".to_string(), "Julia Weber".to_string()];
        let orgs = vec!["John Harris Fitness".to_string()];
        register_prose_person_names(&mut pii, text, Some((&persons, &orgs)));
        let masked = pii.mask_text(text);
        assert!(masked.contains("John Harris Fitness"), "NER-ORG brand must stay clear: {masked}");
        assert!(!masked.contains("Julia Weber"), "the real person must be tokenised: {masked}");
    }

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
