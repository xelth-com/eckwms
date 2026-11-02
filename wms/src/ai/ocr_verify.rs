//! Asynchronous OCR-serial verification (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §11).
//!
//! After a unit is saved with `serial_source = "ocr"` (a worker tapped the
//! serial off a label in the OCR review, §10; the receipt carries the full OCR
//! text as a note), a cheap TEXT model reviews the evidence on the server,
//! ASYNCHRONOUSLY. It may ADD device facts to the unit — it NEVER deletes,
//! renames, or overwrites anything. If it thinks the serial is wrong, or the
//! unit should be deleted, it only files an Operator Inbox item for a human.
//!
//! ## Trigger
//! `services::receiving` arms `receipt_unit.ocr_verify = {status:"pending"}` on
//! an OCR unit when no review exists yet or the serial changed (a vclock-
//! advancing, mesh-synced write).
//!
//! ## Pipeline (per unit, one model call)
//!   1. Gather inputs: serial + serial_source + code, the receipt's OCR/note
//!      text, and the `image_summary` of the unit's and receipt's photos.
//!   2. MASK the free text with the PPRL mask-before-LLM helpers (the receipt
//!      note carries customer names/addresses) BEFORE the call.
//!   3. One call, small max tokens, strict JSON out.
//!   4. VALIDATE every fact (anti-hallucination, pure + tested): kind
//!      whitelisted, value length-capped, confidence clamped, GROUNDED in the
//!      input text, mac/imei/date pass `ocr_hints`' validators, PII tokens
//!      dropped; a `proposed` serial must also be grounded.
//!   5. Persist ADDITIVELY: `ai_facts` (only kinds not already present, never
//!      `serial`) + `ocr_verify` status; `on` mode files ONE inbox item per
//!      `suggested`, idempotent by `input_hash`.
//!
//! ## Modes (`ECK_OCR_VERIFY = off|dry|on`)
//!   - `off`  — worker idle.
//!   - `dry`  — compute + store `ocr_verify` (+`proposed_facts`); apply NO
//!              `ai_facts`, file NO inbox item.
//!   - `on`   — apply `ai_facts`; file an inbox item for each `suggested`.
//! Unset: `dry` if this node is the AI author (image-summary/summarization
//! worker enabled) AND has AI credentials, else `off`. A node with no AI
//! credentials never runs it.

use eck_core::ai::{with_ai_task, AiAuth};
use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::{mask_person_names_heuristic, scrub_pii_regex};
use reqwest::Client as HttpClient;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::services::ocr_hints;
use crate::AppState;

// ── Tuning ───────────────────────────────────────────────────────────────────

const WORKER_INTERVAL_SECS: u64 = 30;
const WORKER_BATCH: usize = 5;
const WORKER_STARTUP_DELAY_SECS: u64 = 60;
const MAX_ATTEMPTS: i64 = 3;
const MAX_FACT_LEN: usize = 80;
const MAX_OUTPUT_TOKENS: i64 = 512;

/// The fact kinds the model may contribute (anti-hallucination whitelist).
const FACT_KINDS: &[&str] = &["model", "part_number", "mac", "imei", "date", "manufacturer"];

/// PPRL token prefixes — a fact whose value carries one is a masked PII leak and
/// is dropped (never surfaced, never stored).
const PII_PREFIXES: &[&str] =
    &["Name_", "Email_", "Phone_", "Address_", "Company_", "Iban_", "VatId_", "Card_"];

// ── Mode ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Off,
    Dry,
    On,
}

fn parse_mode(s: &str) -> Option<Mode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Some(Mode::Off),
        "dry" => Some(Mode::Dry),
        "on" => Some(Mode::On),
        _ => None,
    }
}

/// Effective mode (pure, tested). The env value wins when set & recognised;
/// unset/garbled falls back to `dry` for the AI author node, else `off`. A node
/// with no AI credentials is ALWAYS `off` — it could not make the call anyway.
pub fn resolve_mode(env: Option<&str>, has_credentials: bool, author_enabled: bool) -> Mode {
    if !has_credentials {
        return Mode::Off;
    }
    match env.and_then(parse_mode) {
        Some(m) => m,
        None => {
            if author_enabled {
                Mode::Dry
            } else {
                Mode::Off
            }
        }
    }
}

/// Is this node the AI author (where the image-summary/summarization worker runs)?
fn author_enabled() -> bool {
    let img = std::env::var("ECK_IMAGE_SUMMARY_WORKER").map(|v| v.trim() == "1").unwrap_or(false);
    let sum = std::env::var("ECK_SUMMARY_WORKER").map(|v| v.trim() != "0").unwrap_or(true);
    img || sum
}

// ── Inputs / review types ─────────────────────────────────────────────────────

/// The evidence the review reasons over. Free text (`ocr_text`, `photo_summaries`)
/// is ALREADY MASKED; `serial`/`code` are device identifiers (not PII) kept clear
/// so a serial/code fact can ground.
#[derive(Debug, Clone, Default)]
pub struct VerifyInputs {
    pub serial: Option<String>,
    pub serial_source: String,
    pub code: Option<String>,
    pub ocr_text: String,
    pub photo_summaries: Vec<String>,
}

impl VerifyInputs {
    /// Is there anything at all to review? (OCR note OR >=1 photo summary.)
    pub fn has_evidence(&self) -> bool {
        !self.ocr_text.trim().is_empty()
            || self.photo_summaries.iter().any(|s| !s.trim().is_empty())
    }

    /// The grounding haystack: everything the model saw, whitespace-normalised
    /// and lower-cased.
    fn haystack(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(s) = &self.serial {
            parts.push(s);
        }
        if let Some(c) = &self.code {
            parts.push(c);
        }
        parts.push(&self.ocr_text);
        for s in &self.photo_summaries {
            parts.push(s);
        }
        normalize_ws(&parts.join(" "))
    }
}

/// A validated, grounded fact ready to store under `ai_facts`.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedFact {
    pub kind: String,
    pub value: String,
    pub confidence: f64,
}

/// A human-facing suggestion the model raised. Additive only — v1 NEVER applies
/// it automatically (TECH_DEBT).
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// `rename_serial` | `delete_unit`.
    pub kind: String,
    pub proposed: Option<String>,
    pub reason: String,
}

/// The outcome of one review (pure).
#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    /// `ok` (nothing for a human) | `suggested` (serial suspect / delete raised).
    pub status: &'static str,
    pub facts: Vec<AcceptedFact>,
    pub dropped_ungrounded: usize,
    pub suggestion: Option<Suggestion>,
}

// ── Pure helpers ───────────────────────────────────────────────────────────────

/// Collapse all runs of whitespace to a single space, lower-case.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Does `value` occur in the grounding haystack (both whitespace-normalised,
/// case-insensitively)?
fn grounded(haystack: &str, value: &str) -> bool {
    let v = normalize_ws(value);
    !v.is_empty() && haystack.contains(&v)
}

/// Does `value` carry a PPRL PII token?
fn is_pii_token(value: &str) -> bool {
    PII_PREFIXES.iter().any(|p| value.contains(p))
}

/// Validate one raw fact against the anti-hallucination gate. `Some` = accepted.
fn validate_fact(haystack: &str, kind: &str, value: &str, confidence: f64) -> Option<AcceptedFact> {
    if !FACT_KINDS.contains(&kind) {
        return None;
    }
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_FACT_LEN {
        return None;
    }
    if is_pii_token(value) {
        return None;
    }
    if !grounded(haystack, value) {
        return None;
    }
    // Typed validators for the structured kinds.
    let typed_ok = match kind {
        "mac" => ocr_hints::is_mac(value),
        "imei" => ocr_hints::is_imei(value),
        "date" => ocr_hints::is_date(value),
        _ => true,
    };
    if !typed_ok {
        return None;
    }
    Some(AcceptedFact {
        kind: kind.to_string(),
        value: value.to_string(),
        confidence: confidence.clamp(0.0, 1.0),
    })
}

/// Parse the model's JSON answer, tolerating code fences / surrounding prose.
/// Returns `(facts, serial_verdict, proposed, reason, delete_reason)`.
#[allow(clippy::type_complexity)]
fn parse_answer(
    raw: &str,
) -> Option<(Vec<(String, String, f64)>, String, Option<String>, Option<String>, Option<String>)> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let (s, e) = (stripped.find('{')?, stripped.rfind('}')?);
    if e < s {
        return None;
    }
    let v: Value = serde_json::from_str(&stripped[s..=e]).ok()?;

    let mut facts = Vec::new();
    if let Some(arr) = v.get("facts").and_then(|f| f.as_array()) {
        for f in arr {
            let kind = f.get("kind").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let value = f.get("value").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let conf = f.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if !kind.is_empty() && !value.is_empty() {
                facts.push((kind, value, conf));
            }
        }
    }

    let sc = v.get("serial_check");
    let verdict = sc
        .and_then(|x| x.get("verdict"))
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_else(|| "ok".to_string());
    let proposed = sc
        .and_then(|x| x.get("proposed"))
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let reason = sc
        .and_then(|x| x.get("reason"))
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let delete_reason = v
        .get("delete_suggestion")
        .and_then(|x| x.get("reason"))
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some((facts, verdict, proposed, reason, delete_reason))
}

/// Review the evidence against one model answer. Pure — tests inject canned JSON.
/// `None` = the answer could not be parsed (caller counts a failed attempt).
pub fn review(inp: &VerifyInputs, model_json: &str) -> Option<Review> {
    let (raw_facts, verdict, proposed, reason, delete_reason) = parse_answer(model_json)?;
    let haystack = inp.haystack();

    // Validate + dedupe facts by kind (keep the highest confidence).
    let mut dropped_ungrounded = 0usize;
    let mut by_kind: Vec<AcceptedFact> = Vec::new();
    for (kind, value, conf) in raw_facts {
        match validate_fact(&haystack, &kind, &value, conf) {
            Some(f) => {
                if let Some(ex) = by_kind.iter_mut().find(|e| e.kind == f.kind) {
                    if f.confidence > ex.confidence {
                        *ex = f;
                    }
                } else {
                    by_kind.push(f);
                }
            }
            None => dropped_ungrounded += 1,
        }
    }

    // Suggestion. A suspect serial yields a rename hint; a `proposed` value must
    // itself be grounded, else it is dropped (never surface a hallucinated
    // correction). A suspect verdict WITHOUT a proposed value is still a valid
    // reason-only flag. Delete is surfaced only when no rename hint stands.
    let mut suggestion = None;
    if verdict == "suspect" {
        match &proposed {
            Some(p) if grounded(&haystack, p) => {
                suggestion = Some(Suggestion {
                    kind: "rename_serial".into(),
                    proposed: Some(p.clone()),
                    reason: reason.clone().unwrap_or_else(|| "serial suspect".into()),
                });
            }
            Some(_) => { /* ungrounded proposal → drop the rename hint */ }
            None => {
                suggestion = Some(Suggestion {
                    kind: "rename_serial".into(),
                    proposed: None,
                    reason: reason.clone().unwrap_or_else(|| "serial suspect".into()),
                });
            }
        }
    }
    if suggestion.is_none() {
        if let Some(r) = delete_reason {
            suggestion = Some(Suggestion { kind: "delete_unit".into(), proposed: None, reason: r });
        }
    }

    let status = if suggestion.is_some() { "suggested" } else { "ok" };
    Some(Review { status, facts: by_kind, dropped_ungrounded, suggestion })
}

/// One `ai_facts` entry value.
fn facts_entry(f: &AcceptedFact, at: &str) -> Value {
    json!({ "value": f.value, "confidence": f.confidence, "source": "ocr_verify", "at": at })
}

/// The additive facts delta: one entry per accepted kind NOT already present in
/// `existing`, never the key `serial`. Pure. `at` is an RFC3339 timestamp.
fn facts_delta(existing: &Value, accepted: &[AcceptedFact], at: &str) -> Value {
    let mut out = Map::new();
    for f in accepted {
        if f.kind == "serial" {
            continue;
        }
        let present = existing.get(&f.kind).map(|v| !v.is_null()).unwrap_or(false);
        if present || out.contains_key(&f.kind) {
            continue;
        }
        out.insert(f.kind.clone(), facts_entry(f, at));
    }
    Value::Object(out)
}

/// Merge `delta` onto `existing` (shallow; `existing` keys win only when `delta`
/// does not carry them — `delta` already excludes present kinds, so this never
/// overwrites a human-set or earlier fact).
fn merge_objects(existing: &Value, delta: &Value) -> Value {
    let mut base = existing.as_object().cloned().unwrap_or_default();
    if let Some(d) = delta.as_object() {
        for (k, v) in d {
            base.insert(k.clone(), v.clone());
        }
    }
    Value::Object(base)
}

/// The suggestion serialised for storage / the inbox context.
fn suggestion_json(s: &Suggestion) -> Value {
    let mut m = json!({ "kind": s.kind, "reason": s.reason });
    if let Some(p) = &s.proposed {
        m["proposed"] = json!(p);
    }
    m
}

/// sha256 of the masked inputs — the idempotency key (`ocr_verify.input_hash`).
fn input_hash(inp: &VerifyInputs) -> String {
    let mut h = Sha256::new();
    h.update(inp.serial.as_deref().unwrap_or("").as_bytes());
    h.update(b"\x1f");
    h.update(inp.serial_source.as_bytes());
    h.update(b"\x1f");
    h.update(inp.code.as_deref().unwrap_or("").as_bytes());
    h.update(b"\x1f");
    h.update(inp.ocr_text.as_bytes());
    for s in &inp.photo_summaries {
        h.update(b"\x1e");
        h.update(s.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Mask free text the PPRL way before it reaches the LLM (names heuristic +
/// regex scrub for email/phone/address/IBAN). Device identifiers go in clear.
fn mask_text(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let (names_masked, _) = mask_person_names_heuristic(text);
    let (scrubbed, _) = scrub_pii_regex(&names_masked);
    scrubbed
}

// ── Prompt ─────────────────────────────────────────────────────────────────────

fn system_prompt() -> String {
    format!(
        "You are a meticulous device-intake reviewer for a repair workshop. A warehouse \
worker read a device serial off a label by OCR; your job is to double-check it against the \
evidence and extract any extra device facts that are literally present. {rule}\n\n\
Rules:\n\
- Use ONLY what is written in the evidence. Never guess, infer, or look anything up.\n\
- Extract facts only when the value appears verbatim in the evidence.\n\
- If the serial looks wrong, set serial_check.verdict to \"suspect\" and, ONLY if a better \
serial is actually written in the evidence, put it in \"proposed\". Otherwise leave \"proposed\" out.\n\
- Suggest deletion only if the evidence clearly shows this is not a real unit.\n\n\
Answer with STRICT JSON ONLY, no prose, no code fence:\n\
{{\"facts\":[{{\"kind\":\"model|part_number|mac|imei|date|manufacturer\",\"value\":\"...\",\"confidence\":0.0}}],\
\"serial_check\":{{\"verdict\":\"ok|suspect\",\"proposed\":\"...\",\"reason\":\"...\"}},\
\"delete_suggestion\":{{\"reason\":\"...\"}}}}\n\
Omit \"proposed\", \"reason\", and \"delete_suggestion\" when they do not apply.",
        rule = crate::ai::summarization::PPRL_TOKEN_RULE
    )
}

fn user_message(inp: &VerifyInputs) -> String {
    let mut s = String::new();
    s.push_str("=== UNIT (OCR-captured) ===\n");
    s.push_str(&format!("serial: {}\n", inp.serial.as_deref().unwrap_or("(none)")));
    s.push_str(&format!("serial_source: {}\n", inp.serial_source));
    if let Some(c) = &inp.code {
        s.push_str(&format!("code: {c}\n"));
    }
    s.push_str("\n=== RECEIPT OCR / NOTE TEXT ===\n");
    s.push_str(if inp.ocr_text.trim().is_empty() { "(none)\n" } else { &inp.ocr_text });
    if !inp.ocr_text.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("\n=== PHOTO SUMMARIES ===\n");
    if inp.photo_summaries.iter().all(|p| p.trim().is_empty()) {
        s.push_str("(none)\n");
    } else {
        for (i, p) in inp.photo_summaries.iter().enumerate() {
            if !p.trim().is_empty() {
                s.push_str(&format!("[photo {}] {}\n", i + 1, p.trim()));
            }
        }
    }
    s
}

// ── Model call ─────────────────────────────────────────────────────────────────

fn text_model(auth: &AiAuth) -> String {
    auth.or_model().map(str::to_string).unwrap_or_else(|| {
        std::env::var("GEMINI_SUMMARY_MODEL")
            .or_else(|_| std::env::var("GEMINI_GENERATION_MODEL"))
            .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string())
    })
}

/// One text-model call (Gemini-shaped body; the OR shim translates it). Returns
/// `(text, usage)`.
async fn call_model(
    http: &HttpClient,
    auth: &AiAuth,
    model: &str,
    inp: &VerifyInputs,
) -> anyhow::Result<(String, Value)> {
    let body = json!({
        "systemInstruction": { "parts": [{ "text": system_prompt() }] },
        "contents": [{ "parts": [{ "text": user_message(inp) }] }],
        "generationConfig": { "temperature": 0.1, "maxOutputTokens": MAX_OUTPUT_TOKENS }
    });
    with_ai_task("ocr_verify", auth.generate_content(http, model, body)).await
}

// ── DB: gather / persist ─────────────────────────────────────────────────────

fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

/// One candidate unit awaiting review.
struct Candidate {
    unit_id: String,
    serial: Option<String>,
    serial_source: String,
    code: Option<String>,
    receipt_id: Option<String>,
    ai_facts: Value,
    attempts: i64,
}

fn s_opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Select up to `WORKER_BATCH` OCR units whose review is pending and under the
/// attempt cap.
async fn select_candidates(db: &SurrealDb) -> anyhow::Result<Vec<Candidate>> {
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS unit_id, serial, serial_source, code, barcode, \
                    receipt_id, ai_facts, ocr_verify.attempts AS attempts \
             FROM receipt_unit \
             WHERE serial_source = 'ocr' \
               AND ocr_verify.status = 'pending' \
               AND (ocr_verify.attempts IS NONE OR ocr_verify.attempts < $max) \
             LIMIT $limit",
        )
        .bind(("max", MAX_ATTEMPTS))
        .bind(("limit", WORKER_BATCH))
        .await?
        .take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let unit_id = s_opt(&r, "unit_id")?;
            Some(Candidate {
                unit_id,
                serial: s_opt(&r, "serial"),
                serial_source: s_opt(&r, "serial_source").unwrap_or_else(|| "ocr".into()),
                code: s_opt(&r, "code").or_else(|| s_opt(&r, "barcode")),
                receipt_id: s_opt(&r, "receipt_id"),
                ai_facts: r.get("ai_facts").cloned().unwrap_or(Value::Null),
                attempts: r.get("attempts").and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })
        .collect())
}

/// Load the receipt's OCR/note text + the image summaries of the unit's and
/// receipt's photos, MASK the free text, and assemble [`VerifyInputs`].
async fn gather_inputs(db: &SurrealDb, c: &Candidate) -> VerifyInputs {
    let (ocr_text_raw, _note) = match &c.receipt_id {
        Some(rcpt) => {
            let row: Option<Value> = db
                .query("SELECT ocr_text, note FROM type::record($rid) LIMIT 1")
                .bind(("rid", rid("receipt", rcpt)))
                .await
                .and_then(|mut r| r.take(0))
                .ok()
                .flatten();
            let ocr = row.as_ref().and_then(|v| s_opt(v, "ocr_text"));
            let note = row.as_ref().and_then(|v| s_opt(v, "note"));
            (ocr.or_else(|| note.clone()).unwrap_or_default(), note.unwrap_or_default())
        }
        None => (String::new(), String::new()),
    };

    // Image summaries of the unit's + receipt's photos.
    let rcpt = c.receipt_id.clone().unwrap_or_default();
    let summ_rows: Vec<Value> = db
        .query(
            "SELECT out.image_summary AS s FROM has_attachment \
             WHERE (in = type::record('receipt_unit', $uid) \
                    OR in = type::record('receipt', $rid)) \
               AND out.image_summary IS NOT NONE",
        )
        .bind(("uid", c.unit_id.clone()))
        .bind(("rid", rcpt))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let photo_summaries: Vec<String> = summ_rows
        .iter()
        .filter_map(|r| r.get("s").and_then(|v| v.as_str()))
        .map(mask_text)
        .filter(|s| !s.trim().is_empty())
        .collect();

    VerifyInputs {
        serial: c.serial.clone(),
        serial_source: c.serial_source.clone(),
        code: c.code.clone(),
        ocr_text: mask_text(&ocr_text_raw),
        photo_summaries,
    }
}

/// Next `_vclock` advancing THIS node's component.
async fn next_vclock(db: &SurrealDb, unit_id: &str, instance_id: &str) -> Value {
    let existing: Option<Value> = db
        .query("SELECT VALUE _vclock FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt_unit", unit_id)))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    eck_core::sync::conflict::next_local_vclock(existing.as_ref(), instance_id)
}

/// Build the stored `ocr_verify` object.
fn build_ocr_verify(
    status: &str,
    at: &str,
    input_hash: &str,
    attempts: i64,
    suggestion: Option<&Suggestion>,
    proposed_facts: Option<&Value>,
) -> Value {
    let mut m = json!({ "status": status, "at": at, "input_hash": input_hash, "attempts": attempts });
    if let Some(s) = suggestion {
        m["suggestion"] = suggestion_json(s);
    }
    if let Some(pf) = proposed_facts {
        if pf.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            m["proposed_facts"] = pf.clone();
        }
    }
    m
}

/// Write `ocr_verify` (and, on `on` mode, merged `ai_facts`) in one synced,
/// vclock-advancing MERGE.
async fn persist(
    db: &SurrealDb,
    instance_id: &str,
    unit_id: &str,
    ocr_verify: Value,
    ai_facts: Option<Value>,
) {
    let vclock = next_vclock(db, unit_id, instance_id).await;
    let (sql, facts_bind) = match &ai_facts {
        Some(_) => (
            "UPDATE type::record($rid) MERGE { ocr_verify: $ov, ai_facts: $af, \
                _vclock: $vc, updated_at: time::now() } RETURN NONE",
            true,
        ),
        None => (
            "UPDATE type::record($rid) MERGE { ocr_verify: $ov, \
                _vclock: $vc, updated_at: time::now() } RETURN NONE",
            false,
        ),
    };
    let mut q = db
        .query(sql)
        .bind(("rid", rid("receipt_unit", unit_id)))
        .bind(("ov", ocr_verify))
        .bind(("vc", vclock));
    if facts_bind {
        q = q.bind(("af", ai_facts.unwrap()));
    }
    if let Err(e) = q.await.and_then(|r| r.check()) {
        warn!("[ocr-verify] persist {unit_id}: {e}");
    }
}

// ── Per-unit processing ────────────────────────────────────────────────────────

/// Process one candidate end to end. `attempts_prior` is what the row already
/// carried; a transport/parse failure bumps it (→ `failed` at the cap).
async fn process_unit(
    db: &SurrealDb,
    http: &HttpClient,
    auth: &AiAuth,
    model: &str,
    mode: Mode,
    instance_id: &str,
    c: &Candidate,
) {
    let inp = gather_inputs(db, c).await;
    let at = chrono::Utc::now().to_rfc3339();
    let hash = input_hash(&inp);

    // No evidence → skipped (terminal, no model call).
    if !inp.has_evidence() {
        let ov = build_ocr_verify("skipped", &at, &hash, c.attempts, None, None);
        persist(db, instance_id, &c.unit_id, ov, None).await;
        record_event(db, instance_id, &c.unit_id, "skipped", 0).await;
        info!("[ocr-verify] unit={} skipped (no evidence)", c.unit_id);
        return;
    }

    let (raw, usage) = match call_model(http, auth, model, &inp).await {
        Ok(v) => v,
        Err(e) => {
            let attempts = c.attempts + 1;
            let status = if attempts >= MAX_ATTEMPTS { "failed" } else { "pending" };
            let ov = build_ocr_verify(status, &at, &hash, attempts, None, None);
            persist(db, instance_id, &c.unit_id, ov, None).await;
            warn!("[ocr-verify] unit={} model call failed (attempt {attempts}): {e}", c.unit_id);
            return;
        }
    };
    crate::ai::telemetry::log_telemetry(db, "ocr_verify", model, &c.unit_id, &usage).await;

    let review = match review(&inp, &raw) {
        Some(r) => r,
        None => {
            let attempts = c.attempts + 1;
            let status = if attempts >= MAX_ATTEMPTS { "failed" } else { "pending" };
            let ov = build_ocr_verify(status, &at, &hash, attempts, None, None);
            persist(db, instance_id, &c.unit_id, ov, None).await;
            warn!("[ocr-verify] unit={} unparseable model answer (attempt {attempts})", c.unit_id);
            return;
        }
    };

    let delta = facts_delta(&c.ai_facts, &review.facts, &at);
    let added = delta.as_object().map(|o| o.len()).unwrap_or(0);

    match mode {
        Mode::Dry => {
            let ov = build_ocr_verify(
                review.status,
                &at,
                &hash,
                c.attempts,
                review.suggestion.as_ref(),
                Some(&delta),
            );
            persist(db, instance_id, &c.unit_id, ov, None).await;
        }
        Mode::On => {
            let ai_facts = if added > 0 {
                Some(merge_objects(&c.ai_facts, &delta))
            } else {
                None
            };
            let ov = build_ocr_verify(review.status, &at, &hash, c.attempts, review.suggestion.as_ref(), None);
            persist(db, instance_id, &c.unit_id, ov, ai_facts).await;
            if review.status == "suggested" {
                if let Some(sg) = &review.suggestion {
                    file_inbox(db, instance_id, c, sg, &hash).await;
                }
            }
        }
        Mode::Off => return, // worker never selects in off mode
    }
    record_event(db, instance_id, &c.unit_id, review.status, added).await;
    info!(
        "[ocr-verify] unit={} status={} facts_added={} dropped_ungrounded={} mode={:?}",
        c.unit_id, review.status, added, review.dropped_ungrounded, mode
    );
}

/// File ONE PII-safe Operator Inbox item for a `suggested` review, idempotent by
/// unit id + `input_hash` (so a re-review of the same evidence never re-files).
async fn file_inbox(db: &SurrealDb, instance_id: &str, c: &Candidate, sg: &Suggestion, hash: &str) {
    // PII-safe context: ids, codes, serials, reason, evidence refs (file ids).
    let evidence: Vec<Value> = db
        .query(
            "SELECT record::id(out.id) AS file_id FROM has_attachment \
             WHERE (in = type::record('receipt_unit', $uid) \
                    OR in = type::record('receipt', $rid)) \
               AND out.image_summary IS NOT NONE LIMIT 20",
        )
        .bind(("uid", c.unit_id.clone()))
        .bind(("rid", c.receipt_id.clone().unwrap_or_default()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let evidence_ids: Vec<Value> =
        evidence.iter().filter_map(|r| r.get("file_id").cloned()).collect();

    let question = match sg.kind.as_str() {
        "rename_serial" => format!(
            "OCR-captured serial may be wrong on unit {}. {}",
            c.code.as_deref().unwrap_or(&c.unit_id),
            sg.reason
        ),
        "delete_unit" => format!("Unit {} may not be a real unit. {}", c.code.as_deref().unwrap_or(&c.unit_id), sg.reason),
        _ => sg.reason.clone(),
    };

    let context = json!({
        "unit_id": c.unit_id,
        "receipt_id": c.receipt_id,
        "code": c.code,
        "current_serial": c.serial,
        "proposed_serial": sg.proposed,
        "suggestion": sg.kind,
        "reason": sg.reason,
        "evidence_file_ids": evidence_ids,
        "input_hash": hash,
    });

    let filed = crate::ai::operator_inbox::file(
        db,
        instance_id,
        crate::ai::operator_inbox::InboxItem {
            source: "ocr_verify".into(),
            dedup_key: format!("ocr_verify:{}:{}", c.unit_id, hash),
            question,
            context,
        },
    )
    .await;
    if filed {
        info!("[ocr-verify] filed operator-inbox item for unit={} ({})", c.unit_id, sg.kind);
    }
}

async fn record_event(db: &SurrealDb, instance_id: &str, unit_id: &str, status: &str, added: usize) {
    crate::services::events::record(
        db,
        instance_id,
        crate::services::events::Event::new("ocr_verify", "unit", unit_id, format!("OCR verify: {status}")),
    )
    .await;
    let _ = added; // folded into the log line, not the event text (PII-safe)
}

// ── Worker ─────────────────────────────────────────────────────────────────────

/// Background OCR-verify worker. Effective mode = `ECK_OCR_VERIFY` when set,
/// else `dry` on the AI author node, else `off`; a node with no AI credentials
/// is always `off`. Returns immediately when off or on a cache node.
pub async fn start_ocr_verify_worker(state: Arc<AppState>) {
    let mode = resolve_mode(
        std::env::var("ECK_OCR_VERIFY").ok().as_deref(),
        AiAuth::is_enabled_in_env(),
        author_enabled(),
    );
    if mode == Mode::Off {
        info!("[ocr-verify] disabled (mode=off; set ECK_OCR_VERIFY=dry|on to enable)");
        return;
    }
    if state.node_role == "cache" {
        info!("[ocr-verify] skipped (node_role=cache)");
        return;
    }
    tokio::time::sleep(Duration::from_secs(WORKER_STARTUP_DELAY_SECS)).await;
    let http = match HttpClient::builder().timeout(Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(e) => {
            warn!("[ocr-verify] HTTP client build failed: {e}");
            return;
        }
    };
    info!("[ocr-verify] worker started (mode={mode:?}, batch={WORKER_BATCH}, interval={WORKER_INTERVAL_SECS}s)");

    let db = &state.db;
    let mut auth_warned = false;
    let mut interval = tokio::time::interval(Duration::from_secs(WORKER_INTERVAL_SECS));
    loop {
        interval.tick().await;
        if crate::ai::telemetry::current_budget_level() == crate::ai::telemetry::BudgetLevel::Halt {
            continue;
        }
        let candidates = match select_candidates(db).await {
            Ok(c) => c,
            Err(e) => {
                warn!("[ocr-verify] selection failed: {e}");
                continue;
            }
        };
        if candidates.is_empty() {
            continue;
        }
        let auth = match AiAuth::resolve_for_task(&http, "ocr_verify").await {
            Ok(a) if a.is_configured() => a,
            _ => {
                if !auth_warned {
                    warn!("[ocr-verify] AI auth not configured — worker idle");
                    auth_warned = true;
                }
                continue;
            }
        };
        let model = text_model(&auth);
        for c in candidates {
            process_unit(db, &http, &auth, &model, mode, &state.instance_id, &c).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp_with(ocr: &str, serial: Option<&str>) -> VerifyInputs {
        VerifyInputs {
            serial: serial.map(|s| s.to_string()),
            serial_source: "ocr".into(),
            code: None,
            ocr_text: ocr.to_string(),
            photo_summaries: vec![],
        }
    }

    #[test]
    fn mode_env_wins_then_defaults_then_credential_floor() {
        // Env set & recognised wins.
        assert_eq!(resolve_mode(Some("on"), true, false), Mode::On);
        assert_eq!(resolve_mode(Some("dry"), true, false), Mode::Dry);
        assert_eq!(resolve_mode(Some("off"), true, true), Mode::Off);
        // Unset / garbled → dry for the author, off otherwise.
        assert_eq!(resolve_mode(None, true, true), Mode::Dry);
        assert_eq!(resolve_mode(Some("garbage"), true, true), Mode::Dry);
        assert_eq!(resolve_mode(None, true, false), Mode::Off);
        // No credentials → always off, even with env=on.
        assert_eq!(resolve_mode(Some("on"), false, true), Mode::Off);
        assert_eq!(resolve_mode(None, false, true), Mode::Off);
    }

    #[test]
    fn parse_answer_tolerates_code_fences_and_prose() {
        let raw = "Here you go:\n```json\n{\"facts\":[{\"kind\":\"model\",\"value\":\"Acme 770\",\"confidence\":0.8}],\
                   \"serial_check\":{\"verdict\":\"ok\"}}\n```\nthanks";
        let (facts, verdict, proposed, _r, _d) = parse_answer(raw).expect("parsed");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].0, "model");
        assert_eq!(verdict, "ok");
        assert!(proposed.is_none());
    }

    #[test]
    fn parse_answer_garbage_is_none() {
        assert!(parse_answer("not json at all").is_none());
        assert!(parse_answer("").is_none());
    }

    #[test]
    fn validation_drops_ungrounded_pii_and_bad_luhn() {
        let inp = inp_with(
            "Model Acme 770 MAC 00:1B:44:11:3A:B7 IMEI 490154203237518 made 24.03.2023",
            Some("I81800562"),
        );
        let json = r#"{"facts":[
            {"kind":"model","value":"Acme 770","confidence":0.9},
            {"kind":"model","value":"GhostModel 999","confidence":0.9},
            {"kind":"mac","value":"00:1B:44:11:3A:B7","confidence":0.9},
            {"kind":"imei","value":"490154203237518","confidence":0.9},
            {"kind":"imei","value":"490154203237519","confidence":0.9},
            {"kind":"date","value":"24.03.2023","confidence":0.9},
            {"kind":"manufacturer","value":"Company_DEADBEEF","confidence":0.9},
            {"kind":"wat","value":"Acme 770","confidence":0.9}
        ],"serial_check":{"verdict":"ok"}}"#;
        let r = review(&inp, json).expect("review");
        let kinds: Vec<&str> = r.facts.iter().map(|f| f.kind.as_str()).collect();
        assert!(kinds.contains(&"model"));
        assert!(kinds.contains(&"mac"));
        assert!(kinds.contains(&"imei"));
        assert!(kinds.contains(&"date"));
        // GhostModel ungrounded, bad-Luhn imei dropped, Company_ token dropped,
        // "wat" off-whitelist dropped.
        assert!(!r.facts.iter().any(|f| f.value == "GhostModel 999"));
        assert!(!r.facts.iter().any(|f| f.kind == "manufacturer"));
        assert_eq!(r.facts.iter().filter(|f| f.kind == "imei").count(), 1);
        assert!(r.dropped_ungrounded >= 3);
        assert_eq!(r.status, "ok");
    }

    #[test]
    fn suspect_with_grounded_proposed_is_suggested() {
        let inp = inp_with("Serial on label reads I81800562 clearly", Some("181800562"));
        let json = r#"{"facts":[],"serial_check":{"verdict":"suspect","proposed":"I81800562","reason":"OCR drift"}}"#;
        let r = review(&inp, json).expect("review");
        assert_eq!(r.status, "suggested");
        let sg = r.suggestion.unwrap();
        assert_eq!(sg.kind, "rename_serial");
        assert_eq!(sg.proposed.as_deref(), Some("I81800562"));
    }

    #[test]
    fn suspect_with_ungrounded_proposed_drops_the_hint() {
        let inp = inp_with("nothing relevant here", Some("181800562"));
        let json = r#"{"facts":[],"serial_check":{"verdict":"suspect","proposed":"ZZZ99999","reason":"guess"}}"#;
        let r = review(&inp, json).expect("review");
        // Ungrounded proposal → no rename hint, nothing else → ok.
        assert_eq!(r.status, "ok");
        assert!(r.suggestion.is_none());
    }

    #[test]
    fn delete_suggestion_surfaces_when_no_rename() {
        let inp = inp_with("blank test page, not a device", Some("x"));
        let json = r#"{"facts":[],"serial_check":{"verdict":"ok"},"delete_suggestion":{"reason":"test page"}}"#;
        let r = review(&inp, json).expect("review");
        assert_eq!(r.status, "suggested");
        assert_eq!(r.suggestion.unwrap().kind, "delete_unit");
    }

    #[test]
    fn facts_delta_only_absent_kinds_never_serial() {
        let existing = json!({ "model": { "value": "Old", "confidence": 0.5, "source": "manual" } });
        let accepted = vec![
            AcceptedFact { kind: "model".into(), value: "New".into(), confidence: 0.9 },
            AcceptedFact { kind: "manufacturer".into(), value: "Acme".into(), confidence: 0.9 },
            AcceptedFact { kind: "serial".into(), value: "HACK".into(), confidence: 0.9 },
        ];
        let delta = facts_delta(&existing, &accepted, "2026-11-01T00:00:00Z");
        let obj = delta.as_object().unwrap();
        // model already present → NOT overwritten; serial never added.
        assert!(!obj.contains_key("model"));
        assert!(!obj.contains_key("serial"));
        assert!(obj.contains_key("manufacturer"));
        assert_eq!(obj["manufacturer"]["source"], json!("ocr_verify"));

        // Merge preserves the existing model entry and adds the new kind.
        let merged = merge_objects(&existing, &delta);
        assert_eq!(merged["model"]["value"], json!("Old"));
        assert_eq!(merged["manufacturer"]["value"], json!("Acme"));
    }

    #[test]
    fn input_hash_is_stable_and_sensitive() {
        let a = inp_with("same text", Some("I1"));
        let b = inp_with("same text", Some("I1"));
        let c = inp_with("same text", Some("I2"));
        assert_eq!(input_hash(&a), input_hash(&b));
        assert_ne!(input_hash(&a), input_hash(&c));
    }

    #[test]
    fn no_evidence_detected() {
        let empty = inp_with("   ", None);
        assert!(!empty.has_evidence());
        let with = inp_with("Model Acme", None);
        assert!(with.has_evidence());
    }
}
