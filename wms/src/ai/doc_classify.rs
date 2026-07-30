//! Layer-2 attachment classification (proposal 51): ONE cheap-model
//! (flash-lite) pass per attachment blob, over the ALREADY-STORED extracted
//! text, producing a small structured "doc card" persisted on the byte-owning
//! `file_resource` row (same merkle/vclock discipline as
//! [`crate::ai::attachments::persist_extract`]).
//!
//! PII boundary (ABSOLUTE): the model never sees raw PII. The extracted text is
//! masked LOCALLY before the call — no AI is used for masking — reusing the same
//! chain as the subject/distill masker (`services::support::mask_subject`):
//! optional on-device NER person tokenization (`ECK_PII_NER=local`), the lexicon
//! person-name heuristic, then the deterministic `scrub_pii_regex` backstop
//! (emails / phones / IBAN / German street addresses). So a scanned repair form's
//! name/address is already tokenized (`Name_<hex>` / `Address_<hex>`) by the time
//! it reaches the cheap model, which is told to treat those tokens as opaque.
//!
//! Metered: every call rides the same `AiAuth` auth/model path and telemetry
//! (`ai_telemetry`) as the summarization worker, and the background sweep is
//! budget-halt gated and OFF by default (see
//! [`crate::services::scheduler::start_classify_sweep`]).

use eck_core::ai::AiAuth;
use eck_core::db::SurrealDb;
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use super::telemetry::log_telemetry;

/// Classification pipeline revision stamped onto every persisted card
/// (`file_resource.classify_ver`). BUMP THIS to re-arm re-classification
/// fleet-wide: the sweep re-picks any row whose `classify_ver` is below this.
pub const CLASSIFY_VER: i64 = 1;

/// The cheap model's input budget — the masked extracted text is capped at this
/// many bytes (char-boundary-safe) before the call.
const CLASSIFY_INPUT_CAP: usize = 8_000;

/// Minimum `extracted_quality.dictionary_ratio` for a row to be worth an AI
/// classification. Below this the sweep stamps [`DocCard::low_quality`] instead
/// (no AI call) so the row leaves the candidate set.
pub const MIN_DICTIONARY_RATIO: f64 = 0.5;

// ── Card ─────────────────────────────────────────────────────────────────────

/// Document class as an enum-as-string. Unknown model values fall through to
/// [`DocType::Other`] (`#[serde(other)]`), so a novel label never fails parsing.
/// [`DocType::UnclassifiableLowQuality`] is NOT offered to the model — it is the
/// sentinel the sweep stamps for a row whose extract is too garbled to classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocType {
    Reparaturauftrag,
    Kostenvoranschlag,
    Invoice,
    ShippingLabel,
    Datenverarbeitung,
    QcReport,
    PhotoScreen,
    UnclassifiableLowQuality,
    #[serde(other)]
    Other,
}

impl Default for DocType {
    fn default() -> Self {
        DocType::Other
    }
}

/// The persisted structured card (`file_resource.doc_class`). Every field is
/// `#[serde(default)]` so a partial model response still parses; `customer_token`
/// is a `Name_`/`Company_` pseudonym token (opaque, carried straight from the
/// masked text) when present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocCard {
    #[serde(default)]
    pub doc_type: DocType,
    #[serde(default)]
    pub is_blank: bool,
    #[serde(default)]
    pub signature_hint: bool,
    #[serde(default)]
    pub serials: Vec<String>,
    #[serde(default)]
    pub cs_de_numbers: Vec<String>,
    #[serde(default)]
    pub sdg_numbers: Vec<String>,
    #[serde(default)]
    pub dates: Vec<String>,
    #[serde(default)]
    pub amounts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_token: Option<String>,
    #[serde(default)]
    pub confidence: f32,
}

impl DocCard {
    /// The sentinel card the sweep stamps for a row whose extract is below
    /// [`MIN_DICTIONARY_RATIO`] — records the decision so the row leaves the
    /// candidate set without ever paying for a model call.
    pub fn low_quality() -> Self {
        DocCard {
            doc_type: DocType::UnclassifiableLowQuality,
            is_blank: false,
            signature_hint: false,
            serials: Vec::new(),
            cs_de_numbers: Vec::new(),
            sdg_numbers: Vec::new(),
            dates: Vec::new(),
            amounts: Vec::new(),
            customer_token: None,
            confidence: 0.0,
        }
    }

    /// Clamp the model's self-reported confidence into `0.0..=1.0`.
    fn normalize(&mut self) {
        if !self.confidence.is_finite() {
            self.confidence = 0.0;
        }
        self.confidence = self.confidence.clamp(0.0, 1.0);
    }
}

// ── Prompt ─────────────────────────────────────────────────────────────────

/// Strict-JSON classification instruction. Kept single-prompt + `responseMime
/// Type: application/json` — the exact shape the (proven) `extract_and_anonymize`
/// JSON path uses — rather than a `systemInstruction`/tools payload.
const CLASSIFY_INSTRUCTION: &str = r#"You classify ONE scanned German business document from a device repair / support workflow. The input is OCR text (possibly noisy). Personal data has ALREADY been replaced by opaque tokens such as Name_8E5F3A1B00000000, Email_A1B2C3D400000000, Phone_1234ABCD00000000, Address_DEADBEEF00000000, Company_00AA11BB22CC33DD. Treat every such token as an OPAQUE identifier: never expand, translate, or guess the hidden value — only copy a token verbatim if a field asks for it.

Return ONLY a single JSON object (no prose, no markdown fences) with EXACTLY these keys:
{
  "doc_type": one of "reparaturauftrag" (repair order / RMA form), "kostenvoranschlag" (cost estimate / quote), "invoice" (Rechnung), "shipping_label" (Versand-/Paketlabel), "datenverarbeitung" (data-processing / DSGVO consent form), "qc_report" (device QC / test report), "photo_screen" (a photo of a device screen or display), or "other",
  "is_blank": true if this is an empty/unfilled template with no customer-specific entries, else false,
  "signature_hint": true if a handwritten signature or a filled signature line appears present,
  "serials": [device serial numbers, verbatim],
  "cs_de_numbers": [CS-DE numbers, verbatim],
  "sdg_numbers": [SDG numbers, verbatim],
  "dates": [dates found, as written],
  "amounts": [monetary amounts with currency, as written],
  "customer_token": the single Name_ or Company_ token identifying the customer if the text carries one, else null,
  "confidence": your confidence as a number from 0.0 to 1.0
}

Rules: valid JSON only. Use [] for empty lists and null for an absent customer_token. If the type is unclear, use "other". Never invent values that are not visible in the text."#;

/// Extra line prepended on the retry when the first response would not parse.
const JSON_ONLY_NUDGE: &str =
    "Your previous answer was not valid JSON. Return ONLY the JSON object — no explanation, no markdown code fences.\n\n";

// ── Model resolution ──────────────────────────────────────────────────────

/// The cheap model for classification: `ECK_CLASSIFY_MODEL` override, else the
/// codebase's generation model (`GEMINI_GENERATION_MODEL`, the flash-lite id used
/// for the other cheap work — vision, translation), else the flash-lite default.
fn classify_model() -> String {
    std::env::var("ECK_CLASSIFY_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("GEMINI_GENERATION_MODEL").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "gemini-3.5-flash-lite".to_string())
}

// ── Local PII masking (no AI call) ────────────────────────────────────────

/// Mask a blob's extracted text before it is handed to the cheap model. Reuses
/// the subject/distill masker's chain (`services::support::mask_subject`):
///   1. optional on-device NER person tokenization (`ECK_PII_NER=local`),
///   2. the lexicon person-name heuristic (`mask_person_names_heuristic`),
///   3. the deterministic `scrub_pii_regex` backstop.
/// Steps 2–3 ALWAYS run, so even without the NER model nothing PII-shaped leaves
/// in plaintext. No AI is used here — Gemini never sees raw PII.
async fn mask_extracted_text(text: &str) -> String {
    let ner_masked = ner_mask_names(text).await;
    let (masked, _pairs) = eck_core::utils::anonymizer::mask_person_names_heuristic(&ner_masked);
    let (masked, _fps) = eck_core::utils::anonymizer::scrub_pii_regex(&masked);
    masked
}

/// On-device NER person-name masking, mirroring
/// `embeddings::local_extract_and_anonymize`: each PER span the German NER model
/// finds (gated by `plausible_person_name`) is replaced with its deterministic
/// `Name_` token. Best-effort — any failure returns the text unchanged and the
/// heuristic + regex backstop below still runs.
#[cfg(feature = "local-embed")]
async fn ner_mask_names(text: &str) -> String {
    if crate::ai::local_ner::ner_mode() != "local" {
        return text.to_string();
    }
    let mut work = text.to_string();
    match crate::ai::local_ner::extract_person_names(&work).await {
        Ok(names) => {
            for name in names {
                let name = name.trim();
                if name.chars().count() < 2 {
                    continue;
                }
                if !eck_core::utils::anonymizer::plausible_person_name(name) {
                    continue;
                }
                let token = eck_core::utils::anonymizer::obfuscate_pii(name, "Name");
                if let Ok(re) = regex::Regex::new(&format!("(?i){}", regex::escape(name))) {
                    work = re.replace_all(&work, token.as_str()).into_owned();
                }
            }
        }
        Err(e) => warn!("[DocClassify] local NER failed ({e}) — heuristic+regex backstop only"),
    }
    work
}

#[cfg(not(feature = "local-embed"))]
async fn ner_mask_names(text: &str) -> String {
    text.to_string()
}

/// Truncate to at most `max` bytes at a UTF-8 char boundary (floor) — pure, so
/// the char-boundary discipline can be unit-tested without a model call.
pub(crate) fn cap_to_boundary(s: &str, max: usize) -> &str {
    &s[..crate::ai::floor_char_boundary(s, max)]
}

// ── Parsing (pure) ─────────────────────────────────────────────────────────

/// Slice out the outermost `{ … }` object from a raw model response, tolerating
/// leading/trailing prose or markdown code fences.
fn extract_json_object(resp: &str) -> Option<&str> {
    let start = resp.find('{')?;
    let end = resp.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&resp[start..=end])
}

/// Parse one raw model response into a [`DocCard`], or `None` when no valid card
/// can be extracted. Pure.
pub(crate) fn parse_card(resp: &str) -> Option<DocCard> {
    let json = extract_json_object(resp)?;
    let mut card: DocCard = serde_json::from_str(json).ok()?;
    card.normalize();
    Some(card)
}

/// The retry decision as a PURE function: parse the first response; if it fails
/// and a `second` (retry) response exists, parse that; else give up (`None` = do
/// NOT stamp — the row stays a candidate for the next tick).
pub(crate) fn resolve_parse(first: &str, second: Option<&str>) -> Option<DocCard> {
    if let Some(card) = parse_card(first) {
        return Some(card);
    }
    second.and_then(parse_card)
}

// ── Model call ─────────────────────────────────────────────────────────────

/// One classification call. `nudge` prepends the JSON-only reminder for the
/// retry. Single-prompt + JSON response mime, no tools/grounding.
async fn call_model(
    http: &HttpClient,
    auth: &AiAuth,
    model: &str,
    masked_text: &str,
    nudge: bool,
) -> anyhow::Result<(String, Value)> {
    let prefix = if nudge { JSON_ONLY_NUDGE } else { "" };
    let prompt = format!(
        "{prefix}{CLASSIFY_INSTRUCTION}\n\nDocument text (OCR; PII already masked as opaque tokens):\n\n{masked_text}"
    );
    let payload = json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "temperature": 0.1,
            "maxOutputTokens": 1024,
        }
    });
    auth.generate_content(http, model, payload).await
}

/// Outcome of a single blob classification, for the sweep's batch accounting.
pub(crate) enum ClassifyOutcome {
    /// Card parsed and persisted.
    Classified,
    /// Auth/network/generate error — nothing stamped (retry next tick).
    ModelError,
    /// Double parse failure — nothing stamped (retry next tick).
    ParseError,
}

/// Classify ONE blob's already-stored extracted text and persist the card.
/// Masks locally, calls the cheap model, parses with one JSON-only retry, logs
/// telemetry per call, and on success persists the card with the same
/// merkle/vclock discipline as `persist_extract`. The caller (sweep) is
/// responsible for the low-dictionary-ratio skip; this always attempts the model.
pub(crate) async fn classify_one(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &AiAuth,
    cas_uuid: &str,
    extracted_text: &str,
) -> ClassifyOutcome {
    let model = classify_model();

    // Cap the RAW text first (bounds the NER windows + masking work), mask, then
    // cap again — tokens are longer than the spans they replace, so masking can
    // grow the text back over the budget.
    let raw = cap_to_boundary(extracted_text, CLASSIFY_INPUT_CAP);
    let masked = mask_extracted_text(raw).await;
    let masked = cap_to_boundary(&masked, CLASSIFY_INPUT_CAP).to_string();

    // First attempt.
    let first = match call_model(http, auth, &model, &masked, false).await {
        Ok((resp, usage)) => {
            log_telemetry(db, "doc_classify", &model, cas_uuid, &usage).await;
            resp
        }
        Err(e) => {
            warn!("[DocClassify] model call failed for {cas_uuid}: {e}");
            return ClassifyOutcome::ModelError;
        }
    };
    if let Some(card) = parse_card(&first) {
        persist_class(db, cas_uuid, &card, &model).await;
        return ClassifyOutcome::Classified;
    }

    // One retry with a JSON-only nudge.
    let second = match call_model(http, auth, &model, &masked, true).await {
        Ok((resp, usage)) => {
            log_telemetry(db, "doc_classify", &model, cas_uuid, &usage).await;
            resp
        }
        Err(e) => {
            warn!("[DocClassify] retry model call failed for {cas_uuid}: {e}");
            return ClassifyOutcome::ModelError;
        }
    };
    match resolve_parse(&first, Some(&second)) {
        Some(card) => {
            persist_class(db, cas_uuid, &card, &model).await;
            ClassifyOutcome::Classified
        }
        None => {
            warn!("[DocClassify] parse failed twice for {cas_uuid} — leaving candidate");
            ClassifyOutcome::ParseError
        }
    }
}

/// Stamp the low-quality sentinel card (no AI call) so a below-threshold row
/// leaves the candidate set.
pub(crate) async fn stamp_low_quality(db: &SurrealDb, cas_uuid: &str) {
    persist_class(db, cas_uuid, &DocCard::low_quality(), "").await;
}

// ── Persistence (persist_extract discipline) ──────────────────────────────

/// Persist a card on the byte-owning `file_resource` row (addressed by the
/// `cas_uuid` FIELD, like `persist_extract`). Fire-and-forget, log-on-error.
///
/// Mesh correctness (the `ai_summary` reference pattern, commit d4c8559):
/// `doc_class`/`classify_ver`/`classify_model`/`classified_at` are NOT in
/// `merkle::IGNORED_FIELDS`, so the live-watch bridge folds them into the content
/// hash automatically — this write only advances THIS node's `_vclock` so the
/// card strictly dominates a peer's card-less copy. `instance_id` comes from the
/// process-wide sync engine (via `attachments::mesh_instance_id`); unset in tests,
/// in which case there is nothing to sync and the write is skipped.
async fn persist_class(db: &SurrealDb, cas_uuid: &str, card: &DocCard, model: &str) {
    let Some(iid) = crate::ai::attachments::mesh_instance_id() else {
        return;
    };
    let card_val = match serde_json::to_value(card) {
        Ok(v) => v,
        Err(e) => {
            warn!("[DocClassify] serialize card for {cas_uuid}: {e}");
            return;
        }
    };

    let existing_vc: Option<Value> = db
        .query("SELECT VALUE _vclock FROM file_resource WHERE cas_uuid = $cid LIMIT 1")
        .bind(("cid", cas_uuid.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    let next_vc = eck_core::sync::conflict::next_local_vclock(existing_vc.as_ref(), &iid);

    // MERGE with literal/bound values only (no cross-field SET expression) — the
    // 2026-07-22 greedy-SET/WHERE gotcha does not apply, same shape as
    // `persist_extract`. `.check()` surfaces a statement-level error.
    let sql = "UPDATE file_resource MERGE { \
            doc_class: $card, \
            classify_ver: $ver, \
            classify_model: $model, \
            classified_at: time::now(), \
            _vclock: $vc, \
            updated_at: time::now() \
         } WHERE cas_uuid = $cid";
    let res = db
        .query(sql)
        .bind(("card", card_val))
        .bind(("ver", CLASSIFY_VER))
        .bind(("model", model.to_string()))
        .bind(("vc", next_vc))
        .bind(("cid", cas_uuid.to_string()))
        .await;
    match res {
        Ok(r) => {
            if let Err(e) = r.check() {
                warn!("[DocClassify] persist class {cas_uuid}: {e}");
            }
        }
        Err(e) => warn!("[DocClassify] persist class {cas_uuid}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_serde_round_trip() {
        let card = DocCard {
            doc_type: DocType::Reparaturauftrag,
            is_blank: false,
            signature_hint: true,
            serials: vec!["SN12345".to_string()],
            cs_de_numbers: vec!["CS-DE-77".to_string()],
            sdg_numbers: vec!["SDG-9".to_string()],
            dates: vec!["2026-07-28".to_string()],
            amounts: vec!["120,00 EUR".to_string()],
            customer_token: Some("Name_8E5F3A1B00000000".to_string()),
            confidence: 0.75, // exactly representable in f32 → stable round-trip
        };
        let s = serde_json::to_string(&card).unwrap();
        let back = parse_card(&s).expect("card must round-trip through parse_card");
        assert_eq!(card, back);
    }

    #[test]
    fn doc_type_snake_case_and_unknown_fallback() {
        // Known variants serialize snake_case.
        assert_eq!(serde_json::to_string(&DocType::QcReport).unwrap(), "\"qc_report\"");
        assert_eq!(serde_json::to_string(&DocType::ShippingLabel).unwrap(), "\"shipping_label\"");
        assert_eq!(
            serde_json::to_string(&DocType::UnclassifiableLowQuality).unwrap(),
            "\"unclassifiable_low_quality\""
        );
        // An unknown label deserializes to Other rather than failing.
        let unknown: DocType = serde_json::from_str("\"garantieantrag\"").unwrap();
        assert_eq!(unknown, DocType::Other);
    }

    #[test]
    fn parse_card_good_json() {
        let resp = r#"{"doc_type":"invoice","is_blank":false,"serials":["A1"],"amounts":["50 EUR"],"confidence":0.9}"#;
        let card = parse_card(resp).expect("valid JSON parses");
        assert_eq!(card.doc_type, DocType::Invoice);
        assert_eq!(card.serials, vec!["A1".to_string()]);
        assert!((card.confidence - 0.9).abs() < 1e-6);
        // Missing keys fall back to defaults.
        assert!(card.dates.is_empty());
        assert!(card.customer_token.is_none());
    }

    #[test]
    fn parse_card_tolerates_fences_and_prose() {
        // A model that wraps the object in a ```json fence + prose still parses.
        let resp = "Here is the classification:\n```json\n{ \"doc_type\": \"kostenvoranschlag\", \"confidence\": 1.5 }\n```\nDone.";
        let card = parse_card(resp).expect("object is extracted from surrounding text");
        assert_eq!(card.doc_type, DocType::Kostenvoranschlag);
        // Out-of-range confidence is clamped by normalize().
        assert_eq!(card.confidence, 1.0);
    }

    #[test]
    fn parse_card_garbage_is_none() {
        assert!(parse_card("not json at all").is_none());
        assert!(parse_card("").is_none());
        assert!(parse_card("[1,2,3]").is_none()); // array, not an object body
    }

    #[test]
    fn resolve_parse_retry_logic() {
        let good = r#"{"doc_type":"invoice"}"#;
        let bad = "sorry, I cannot comply";

        // First response good → parsed, no retry needed.
        assert_eq!(resolve_parse(good, None).unwrap().doc_type, DocType::Invoice);
        // First bad, no retry available → None.
        assert!(resolve_parse(bad, None).is_none());
        // First bad, retry good → parsed.
        assert_eq!(resolve_parse(bad, Some(good)).unwrap().doc_type, DocType::Invoice);
        // Both bad → None (do not stamp).
        assert!(resolve_parse(bad, Some(bad)).is_none());
    }

    #[test]
    fn low_quality_card_is_sentinel() {
        let c = DocCard::low_quality();
        assert_eq!(c.doc_type, DocType::UnclassifiableLowQuality);
        assert_eq!(c.confidence, 0.0);
        // Round-trips through the stored object shape.
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v.get("doc_type").and_then(|d| d.as_str()), Some("unclassifiable_low_quality"));
    }

    #[test]
    fn cap_to_boundary_is_char_safe() {
        // 'ö' (2 bytes) straddles the cap boundary — must step back, never panic.
        let s = format!("{}ö tail", "a".repeat(7_999));
        let capped = cap_to_boundary(&s, CLASSIFY_INPUT_CAP);
        assert!(capped.len() <= CLASSIFY_INPUT_CAP);
        assert_eq!(capped.len(), 7_999); // stepped back out of the multibyte char
        // Short strings pass through untouched.
        assert_eq!(cap_to_boundary("short", CLASSIFY_INPUT_CAP), "short");
    }
}
