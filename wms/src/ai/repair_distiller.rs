//! Repair-lesson distiller ("Pass C").
//!
//! Sibling of the SOP Optimizer (`optimizer.rs`), but for the FIELD-REPAIR side:
//! it reads resolved Zoho Desk support tickets (device_model + serial + symptom in
//! `meta`, correspondence in `document_raw` threads) and distills each into a
//! structured, PII-stripped repair lesson stored locally in `repair_lesson`.
//!
//! THE GATE (operator decision 2026-06-23): a lesson is pushed into the shared
//! xelixir KB (`kb_record`, the firmware/RE knowledge graph) ONLY when the repair
//! is CONFIRMED to have helped — i.e. the ticket is resolved/closed AND the LLM
//! classifies `outcome == "confirmed_fixed"`. Pending / unconfirmed / non-fixed
//! tickets are summarised locally only and never pollute the shared KB. This is
//! how the low-level RE knowledge (xelixir) and the field-repair knowledge (9eck)
//! meet in one query surface without dumping raw correspondence.
//!
//! Reuses the optimizer's plumbing: AiAuth + generate_content, budget gating,
//! telemetry, strict-JSON + SKIP sentinel, two-layer PII guard (LLM prompt +
//! device-side: device_model/serial are attached by Rust from `meta`, never by
//! the LLM, and names/emails/phones/addresses are forbidden in the prompt).
//!
//! CANDIDATE INTAKE (xelixir kb-curation CONTRACT §7, 2026-10-31): the LLM reads the
//! ticket's already-masked `ai_summary` when one exists (raw thread digests, run
//! through the regex + name-heuristic masker, are only the fallback); `kb_record`
//! carries `origin {system:"9eck", id:<ticket id>, revision:<ticket updated_at>}`
//! and its IntakeReply is stored (`xelixir_candidate_id`, `xelixir_finding_id`).
//! A daily write-back (`write_back_candidates`) copies a later promotion into
//! `xelixir_finding_id`; the advice-ledger sync (`advice_ledger.rs`) rides the
//! same worker on its own 6 h tick.

use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use super::embeddings::embed_query;
use super::telemetry::{current_budget_level, log_telemetry, BudgetLevel, THROTTLE_DELAY_SECS};
use crate::AppState;

const DISTILLER_INTERVAL_SECS: u64 = 86_400; // daily — support tickets are low-churn
const BATCH_LIMIT: usize = 8;
const RATE_LIMIT_MS: u64 = 1500;
const MAX_THREADS: usize = 30; // cap correspondence per ticket fed to the LLM
const MAX_DISTILL_ATTEMPTS: i64 = 3; // give up on a ticket only after this many FAILED cycles
const MAX_SUMMARY_CHARS: usize = 12_000; // cap the masked ai_summary fed to the LLM
const WRITE_BACK_LIMIT: i64 = 40; // kb_candidate reads per daily write-back pass

const SYSTEM_PROMPT_TEMPLATE: &str = r#"You are the Repair-Lesson Distiller for {{SERVICE_OP}}.
You read ONE customer support ticket (symptom + the email correspondence between the customer and a service technician) and distill the TECHNICAL repair lesson into strict JSON.

STRICT OUTPUT RULES:
- Respond with ONLY a valid minified JSON object — no prose, no code fences.
- Schema: {"symptom":"<generalized device symptom, <=160 chars>","error_code":"<a device error code (e.g. E2/S68D/L11I) if one is mentioned, else empty string>","diagnosis":"<the technical cause, <=400 chars>","action":"<what fixed it or the recommended fix, <=500 chars>","remote_vs_workshop":"remote|workshop|either|unknown","parts":["<replaced/needed parts, generic names>"],"outcome":"confirmed_fixed|pending|not_fixed|unknown","confidence":"high|med|low","skip":false}
- "outcome" = "confirmed_fixed" ONLY if the correspondence shows the fix was applied AND it resolved the problem. If the technician only ADVISED a fix, or the device was not yet sent in, or the thread ends unresolved → "pending". If a fix was tried and did NOT work → "not_fixed". Otherwise "unknown".
- Generalize: "symptom"/"diagnosis"/"action" describe the CLASS of problem, not this one customer.

GROUNDING — DO NOT FANTASIZE:
- Base every field ONLY on what the correspondence actually states. Never invent a cause, fix, part, or outcome the thread does not support.
- If the thread does not reveal the technical cause, leave "diagnosis" empty; if it does not reveal what was actually done, leave "action" empty.
- Set "outcome":"unknown" whenever the thread does not clearly show HOW THE CASE ENDED (no resolution shown, thread cut off, only an initial report, or a purely administrative close). Do NOT upgrade to "confirmed_fixed" on optimism — only when it is actually confirmed the problem was resolved.

PII SAFETY — HARD REQUIREMENT:
- NEVER include names, email addresses, phone numbers, street addresses, company names, customer numbers, or ticket numbers in ANY field. Do NOT include the device serial number (it is attached separately by the system).
- If the ticket is not a real device-repair case (sales, billing, non-technical, or dominated by PII), return {"skip":true} and nothing else."#;

/// The distiller system prompt with the tenant's service-operation phrase
/// spliced in (env `ECK_TENANT_BRAND`; neutral "a device-service operation"
/// when unset). Only that one phrase varies from the template.
fn system_prompt_text() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        SYSTEM_PROMPT_TEMPLATE.replace("{{SERVICE_OP}}", &crate::ai::branding::service_operation_phrase())
    })
}

pub async fn start_repair_distiller_worker(state: Arc<AppState>) {
    // Stagger startup behind the other AI workers' bring-up burst.
    tokio::time::sleep(Duration::from_secs(180)).await;
    info!(
        "[RepairDistiller] started ({}s interval, push-gate=confirmed_fixed)",
        DISTILLER_INTERVAL_SECS
    );

    let http = HttpClient::new();

    // Advice-ledger → xelixir KB sync: same worker, own tick (default 6 h), no LLM.
    if super::advice_ledger::enabled() {
        let (ls, lh) = (state.clone(), http.clone());
        tokio::spawn(async move { super::advice_ledger::run_loop(ls, lh).await });
    }

    let mut interval = tokio::time::interval(Duration::from_secs(DISTILLER_INTERVAL_SECS));

    loop {
        interval.tick().await;

        match current_budget_level() {
            BudgetLevel::Halt => {
                debug!("[RepairDistiller] Budget HALT — skipping cycle");
                continue;
            }
            BudgetLevel::Throttle => {
                debug!("[RepairDistiller] Budget THROTTLE — sleeping before cycle");
                tokio::time::sleep(Duration::from_secs(THROTTLE_DELAY_SECS)).await;
            }
            _ => {}
        }

        if let Err(e) = run_cycle(&state, &http).await {
            error!("[RepairDistiller] cycle failed: {}", e);
        }
    }
}

async fn run_cycle(state: &Arc<AppState>, http: &HttpClient) -> anyhow::Result<()> {
    // Define first: the re-push / write-back SELECTs below must not error on a
    // fresh node where no lesson has been stored yet (schemaless — new columns
    // such as xelixir_candidate_id / kb_state / kb_outcome / source_revision /
    // input_source need no migration).
    if let Err(e) = state.db.query("DEFINE TABLE IF NOT EXISTS repair_lesson SCHEMALESS;").await {
        warn!("[RepairDistiller] define repair_lesson failed: {}", e);
    }
    // 0. Re-push any gate-passed lessons whose earlier xelixir POST failed (transient
    //    network/ADC/xelixir outage). Independent of GCP AI auth — runs even when the
    //    distiller LLM itself is unavailable, so a confirmed lesson is never lost.
    match retry_unpushed_lessons(state, http).await {
        Ok(n) if n > 0 => info!("[RepairDistiller] re-pushed {n} previously-unpushed lesson(s) to xelixir KB"),
        Err(e) => warn!("[RepairDistiller] re-push scan failed: {}", e),
        _ => {}
    }
    // 0b. Finding-id write-back: candidates the operator has since promoted/linked.
    match write_back_candidates(state, http).await {
        Ok(n) if n > 0 => info!("[RepairDistiller] wrote back {n} candidate decision(s) from xelixir KB"),
        Err(e) => warn!("[RepairDistiller] candidate write-back failed: {}", e),
        _ => {}
    }

    let auth = eck_core::ai::AiAuth::resolve(http).await?;
    if !auth.is_configured() {
        return Ok(());
    }
    let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
        .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string());

    // Candidates: only RESOLVED/CLOSED tickets we haven't distilled yet.
    // "When does it make sense to take a lesson?" → once the case has actually
    // ENDED, so there is a real outcome to learn from instead of guessing about an
    // open ticket. An open ticket isn't a candidate; when it later closes the
    // scraper re-MERGEs its status (preserving repair_distilled), and it appears
    // here then. device_model must be known so the lesson is anchorable.
    let candidates: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, status, meta, ai_summary, updated_at, \
             (IF updated_at != NONE THEN type::string(updated_at) ELSE NONE END) AS revision, \
             repair_distill_attempts FROM document \
             WHERE type = 'support_ticket' \
             AND (repair_distilled IS NONE OR repair_distilled = false) \
             AND meta.device_model != NONE AND meta.device_model != '' \
             AND status != NONE AND status != '' \
             AND (string::contains(string::lowercase(status), 'closed') OR string::contains(string::lowercase(status), 'resolved')) \
             ORDER BY updated_at DESC LIMIT 200",
        )
        .await?
        .take(0)?;

    let mut processed = 0usize;
    let mut pushed = 0usize;

    for tk in candidates {
        if processed >= BATCH_LIMIT {
            break;
        }
        let ticket_id = match tk.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        match process_ticket(state, http, &auth, &gen_model, &ticket_id, &tk).await {
            Ok(did_push) => {
                if did_push {
                    pushed += 1;
                }
                // A real verdict (lesson stored, or a legitimate skip) → mark done.
                let _ = state
                    .db
                    .query("UPDATE document SET repair_distilled = true, repair_distilled_at = time::now() WHERE type = 'support_ticket' AND record::id(id) = $tid")
                    .bind(("tid", ticket_id.clone()))
                    .await;
            }
            Err(e) => {
                // Transient/infra failure (AI mint/ADC down, network, rate-limit, non-JSON).
                // Do NOT mark distilled — an outage must not silently consume tickets. Bump an
                // attempt counter and give up only after MAX_DISTILL_ATTEMPTS, so a genuinely
                // unprocessable ticket can't be retried forever either.
                let attempts = tk
                    .get("repair_distill_attempts")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    + 1;
                if attempts >= MAX_DISTILL_ATTEMPTS {
                    warn!("[RepairDistiller] ticket {} failed {} times ({}); giving up", ticket_id, attempts, e);
                    let _ = state
                        .db
                        .query("UPDATE document SET repair_distilled = true, repair_distilled_at = time::now(), repair_distill_attempts = $n WHERE type = 'support_ticket' AND record::id(id) = $tid")
                        .bind(("tid", ticket_id.clone()))
                        .bind(("n", attempts))
                        .await;
                } else {
                    warn!("[RepairDistiller] ticket {} failed (attempt {}/{}): {} — will retry next cycle", ticket_id, attempts, MAX_DISTILL_ATTEMPTS, e);
                    let _ = state
                        .db
                        .query("UPDATE document SET repair_distill_attempts = $n WHERE type = 'support_ticket' AND record::id(id) = $tid")
                        .bind(("tid", ticket_id.clone()))
                        .bind(("n", attempts))
                        .await;
                }
            }
        }

        processed += 1;
        tokio::time::sleep(Duration::from_millis(RATE_LIMIT_MS)).await;
    }

    if processed > 0 {
        info!("[RepairDistiller] distilled {processed} ticket(s), pushed {pushed} to xelixir KB");
    }
    Ok(())
}

/// Re-push lessons that passed the gate before but whose xelixir POST failed
/// (transient network/ADC/xelixir outage), so a confirmed lesson isn't lost to the
/// shared KB just because the bridge was momentarily down. No-op when push is disabled.
async fn retry_unpushed_lessons(state: &Arc<AppState>, http: &HttpClient) -> anyhow::Result<usize> {
    if std::env::var("XELIXIR_KB_URL").map(|u| u.is_empty()).unwrap_or(true) {
        return Ok(0); // bridge not configured → nothing to retry
    }
    let pending: Vec<Value> = state
        .db
        .query(
            "SELECT ticket_id, ticket_number, device_model, serial, symptom, error_code, \
             diagnosis, action, remote_vs_workshop, parts, confidence, source_revision \
             FROM repair_lesson \
             WHERE push_eligible = true AND pushed_to_xelixir = false LIMIT 20",
        )
        .await?
        .take(0)?;

    let mut repushed = 0usize;
    for l in pending {
        let g = |k: &str| l.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ticket_id = g("ticket_id");
        if ticket_id.is_empty() {
            continue;
        }
        let parts: Vec<String> = l
            .get("parts")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let revision = g("source_revision");
        let lesson = LessonPush {
            ticket_id: &ticket_id,
            ticket_number: &g("ticket_number"),
            revision: Some(revision.as_str()).filter(|r| !r.is_empty()),
            device_model: &g("device_model"),
            serial: &g("serial"),
            symptom: &g("symptom"),
            error_code: &g("error_code"),
            diagnosis: &g("diagnosis"),
            action: &g("action"),
            remote_ws: &g("remote_vs_workshop"),
            parts: &parts,
            confidence: &g("confidence"),
        };
        match push_to_xelixir_kb(http, &lesson).await {
            Ok(Some(reply)) => {
                if record_push_result(state, &ticket_id, &reply).await {
                    repushed += 1;
                }
            }
            Ok(None) => {} // not configured (already guarded above)
            Err(e) => warn!("[RepairDistiller] re-push failed for {}: {}", ticket_id, e),
        }
        tokio::time::sleep(Duration::from_millis(RATE_LIMIT_MS)).await;
    }
    Ok(repushed)
}

/// Returns Ok(true) if the lesson was pushed to the xelixir KB.
async fn process_ticket(
    state: &Arc<AppState>,
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    gen_model: &str,
    ticket_id: &str,
    ticket: &Value,
) -> anyhow::Result<bool> {
    let meta = ticket.get("meta").cloned().unwrap_or(json!({}));
    let device_model = meta.get("device_model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let serial = meta.get("serial_number").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mfg = meta.get("manufacturing_date").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let subject = meta.get("subject").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ticket_number = meta.get("ticket_number").and_then(|v| v.as_str()).unwrap_or(ticket_id).to_string();
    let status = ticket.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let revision = ticket.get("revision").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ai_summary = ticket.get("ai_summary").and_then(|v| v.as_str());

    // Prefer the ticket's MASKED ai_summary (CONTRACT §7). Only when it is absent
    // read the compact correspondence: direction + author type + Zoho summary (NOT
    // the raw HTML body / attachments), masked here before it leaves the process.
    let threads: Vec<Value> = if usable_summary(ai_summary).is_some() {
        Vec::new()
    } else {
        state
            .db
            .query(
                "SELECT payload.direction AS direction, payload.summary AS summary, \
                 payload.author.type AS author_type, payload.createdTime AS created, updated_at \
                 FROM document_raw WHERE type = 'support_thread' AND ticket_id = $tid \
                 ORDER BY updated_at ASC LIMIT $lim",
            )
            .bind(("tid", ticket_id.to_string()))
            .bind(("lim", MAX_THREADS as i64))
            .await?
            .take(0)?
    };

    let Some((bundle, source)) = build_distill_bundle(&subject, &status, ai_summary, &threads) else {
        // No summary and threads not imported yet (scraper lag). Treat as transient so
        // we do NOT finalize the ticket — retry later (bounded by MAX_DISTILL_ATTEMPTS).
        return Err(anyhow::anyhow!("no ai_summary and no threads imported yet for ticket {ticket_id}"));
    };
    debug!("[RepairDistiller] ticket {} input={:?}", ticket_id, source);
    let bundle_str = serde_json::to_string(&bundle).unwrap_or_default();
    let user_prompt = format!(
        "Distill the repair lesson from this support ticket. Strict JSON only. \
         The ticket comes either as a masked AI summary of its correspondence (`summary`) \
         or as a masked thread digest (`threads`); tokens like Name_<hex>/Email_<hex>/Company_<hex> \
         are pseudonyms — never copy them into any field.\n\n## Ticket\n```json\n{bundle_str}\n```"
    );

    let payload = json!({
        "systemInstruction": { "parts": [{ "text": system_prompt_text() }] },
        "contents": [{ "parts": [{ "text": user_prompt }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "temperature": 0.1,
            "maxOutputTokens": 1024,
        }
    });
    let _ = &auth; // resolved above only to gate on is_configured()
    let gen =
        eck_core::ai::AiAuth::generate_for_task(http, "distiller", gen_model, payload, None).await?;
    tracing::info!("[ai] task=distiller path={} model={}", gen.path, gen.model);
    let (response_text, served_model, usage) = (gen.text, gen.model, gen.usage);
    log_telemetry(&state.db, "repair_distiller", &served_model, ticket_id, &usage).await;

    let parsed: Value = serde_json::from_str(&response_text)
        .map_err(|e| anyhow::anyhow!("distiller LLM non-JSON: {e} (body: {response_text})"))?;

    if parsed.get("skip").and_then(|v| v.as_bool()) == Some(true) {
        debug!("[RepairDistiller] ticket {} skipped (not a device repair)", ticket_id);
        return Ok(false);
    }

    let symptom = parsed.get("symptom").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let error_code = parsed.get("error_code").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let diagnosis = parsed.get("diagnosis").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let action = parsed.get("action").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let remote_ws = parsed.get("remote_vs_workshop").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let outcome = parsed.get("outcome").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let confidence = parsed.get("confidence").and_then(|v| v.as_str()).unwrap_or("low").to_string();
    let parts: Vec<String> = parsed.get("parts").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if symptom.is_empty() || diagnosis.is_empty() {
        warn!("[RepairDistiller] ticket {} produced empty lesson; skipping", ticket_id);
        return Ok(false);
    }

    // DON'T FANTASIZE: if the LLM couldn't tell how the case ended, take NO lesson.
    // The ticket is already closed, so re-reading it later won't help — finalize
    // (mark distilled by the caller) without storing a speculative lesson.
    if outcome == "unknown" {
        info!("[RepairDistiller] ticket {} outcome=unknown (ending unclear) — taking no lesson", ticket_id);
        return Ok(false);
    }

    // Resolved-ticket signal (independent of the LLM's own judgement).
    let resolved = {
        let s = status.to_lowercase();
        s.contains("closed") || s.contains("resolved")
    };
    // THE GATE: push to xelixir only on a confirmed, resolved fix.
    let should_push = resolved && outcome == "confirmed_fixed";

    // Optional symptom embedding for local find_similar (best-effort).
    let embedding = embed_query(&symptom).await.ok();

    let _: Vec<Value> = state
        .db
        .query(
            "INSERT INTO repair_lesson { \
                ticket_id: $tid, ticket_number: $tn, device_model: $dm, serial: $sn, \
                manufacturing_date: $mfg, symptom: $sym, error_code: $ec, diagnosis: $diag, \
                action: $act, remote_vs_workshop: $rws, parts: $parts, outcome: $out, \
                confidence: $conf, embedding: $emb, push_eligible: $elig, pushed_to_xelixir: false, \
                source_revision: $rev, input_source: $src, \
                created_at: time::now(), updated_at: time::now() \
             }",
        )
        .bind(("tid", ticket_id.to_string()))
        .bind(("tn", ticket_number.clone()))
        .bind(("dm", device_model.clone()))
        .bind(("sn", serial.clone()))
        .bind(("mfg", mfg))
        .bind(("sym", symptom.clone()))
        .bind(("ec", error_code.clone()))
        .bind(("diag", diagnosis.clone()))
        .bind(("act", action.clone()))
        .bind(("rws", remote_ws.clone()))
        .bind(("parts", parts.clone()))
        .bind(("out", outcome.clone()))
        .bind(("conf", confidence.clone()))
        .bind(("emb", embedding))
        .bind(("elig", should_push))
        .bind(("rev", revision.clone()))
        .bind(("src", source.as_str()))
        .await?
        .take(0)
        .unwrap_or_default();

    if !should_push {
        info!(
            "[RepairDistiller] ticket {} stored LOCAL lesson (not pushed): model={} outcome={} resolved={} symptom=\"{}\"",
            ticket_id, device_model, outcome, resolved, symptom
        );
        return Ok(false);
    }

    // ── GATE PASSED: push the confirmed repair lesson to the xelixir KB ──
    let lesson = LessonPush {
        ticket_id,
        ticket_number: &ticket_number,
        revision: Some(revision.as_str()).filter(|r| !r.is_empty()),
        device_model: &device_model,
        serial: &serial,
        symptom: &symptom,
        error_code: &error_code,
        diagnosis: &diagnosis,
        action: &action,
        remote_ws: &remote_ws,
        parts: &parts,
        confidence: &confidence,
    };
    match push_to_xelixir_kb(http, &lesson).await {
        Ok(Some(reply)) => {
            let ok = record_push_result(state, ticket_id, &reply).await;
            info!(
                "[RepairDistiller] ticket {} -> xelixir KB outcome={} candidate={} finding={} (model={}, '{}')",
                ticket_id,
                reply.outcome.as_deref().unwrap_or("legacy"),
                reply.candidate_id.as_deref().unwrap_or("-"),
                reply.finding_id.as_deref().unwrap_or("-"),
                device_model,
                symptom
            );
            Ok(ok)
        }
        Ok(None) => {
            debug!("[RepairDistiller] xelixir push skipped (not configured) for {}", ticket_id);
            Ok(false)
        }
        Err(e) => {
            warn!("[RepairDistiller] xelixir push failed for {}: {}", ticket_id, e);
            Ok(false)
        }
    }
}

/// Which input the distiller LLM read for a ticket (stored as
/// `repair_lesson.input_source` for stats).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputSource {
    /// The ticket's already-masked `document.ai_summary` (preferred).
    AiSummary,
    /// Fallback: the Zoho thread digest, masked by [`mask_outbound`].
    Threads,
}

impl InputSource {
    /// Stable column value.
    fn as_str(self) -> &'static str {
        match self {
            InputSource::AiSummary => "ai_summary",
            InputSource::Threads => "threads",
        }
    }
}

/// The ticket's `ai_summary` when it is present and non-blank, else `None`.
fn usable_summary(ai_summary: Option<&str>) -> Option<&str> {
    ai_summary.map(str::trim).filter(|s| !s.is_empty())
}

/// Deterministic outbound PII mask for text that did NOT come through the
/// summarizer's egress ladder: the high-confidence regex scrub (email, phone,
/// IBAN, …) then the person-name heuristic. Both are idempotent over existing
/// `Name_/Email_…` tokens, so masking already-masked text is harmless.
fn mask_outbound(text: &str) -> String {
    let (scrubbed, _) = eck_core::utils::anonymizer::scrub_pii_regex(text);
    eck_core::utils::anonymizer::mask_person_names_heuristic(&scrubbed).0
}

/// Build the LLM input bundle for one ticket (CONTRACT §7): the masked
/// `ai_summary` when present and non-empty (threads are then ignored); else the
/// thread digest with every thread `summary` run through [`mask_outbound`].
/// The subject is masked on both paths. `None` = neither a summary nor any
/// thread (scraper lag → the caller retries later).
fn build_distill_bundle(
    subject: &str,
    status: &str,
    ai_summary: Option<&str>,
    threads: &[Value],
) -> Option<(Value, InputSource)> {
    let subject = mask_outbound(subject);
    if let Some(summary) = usable_summary(ai_summary) {
        let summary: String = summary.chars().take(MAX_SUMMARY_CHARS).collect();
        return Some((
            json!({ "subject": subject, "status": status, "summary": summary }),
            InputSource::AiSummary,
        ));
    }
    if threads.is_empty() {
        return None;
    }
    let masked: Vec<Value> = threads
        .iter()
        .map(|t| {
            let mut t = t.clone();
            if let Some(s) = t.get("summary").and_then(|v| v.as_str()).map(mask_outbound) {
                t["summary"] = json!(s);
            }
            t
        })
        .collect();
    Some((
        json!({ "subject": subject, "status": status, "threads": masked }),
        InputSource::Threads,
    ))
}

/// One gate-passed lesson as it is pushed to the xelixir KB (fresh distill or
/// re-push from `repair_lesson`).
struct LessonPush<'a> {
    /// `document` record id of the support ticket — the `origin.id`.
    ticket_id: &'a str,
    /// Zoho ticket number — the `src:zoho:<n>` tag.
    ticket_number: &'a str,
    /// Ticket `updated_at` at distill time — the `origin.revision` (absent on
    /// lessons stored before 2026-10-31).
    revision: Option<&'a str>,
    device_model: &'a str,
    serial: &'a str,
    symptom: &'a str,
    error_code: &'a str,
    diagnosis: &'a str,
    action: &'a str,
    remote_ws: &'a str,
    parts: &'a [String],
    confidence: &'a str,
}

/// The `kb_record` arguments for one lesson: the confirmed-fix text, anchors,
/// tags (`field-repair`, `src:zoho:<n>`) and `origin {system:"9eck", id, revision?}`
/// so a re-push of the same ticket revision is one delivery on the server.
fn kb_record_args(l: &LessonPush<'_>) -> Value {
    let parts_str = if l.parts.is_empty() { String::new() } else { format!(" Parts: {}.", l.parts.join(", ")) };
    let ec_note = if l.error_code.is_empty() { String::new() } else { format!(" [error_code {}]", l.error_code) };
    let text = format!(
        "FIELD REPAIR (confirmed){ec_note}: {} -> diagnosis: {}. Fix: {} ({}).{parts_str} \
         Confirmed-fixed in the field on a {} (SN {}). Source: 9eck repair-lesson distiller, Zoho ticket {}.",
        l.symptom, l.diagnosis, l.action, l.remote_ws, l.device_model, l.serial, l.ticket_number
    );

    let mut anchors = vec![json!({"kind":"symptom","slug": l.symptom})];
    if !l.error_code.is_empty() {
        anchors.push(json!({"kind":"error_code","slug": l.error_code}));
    }
    let mut origin = json!({ "system": "9eck", "id": l.ticket_id });
    if let Some(rev) = l.revision {
        origin["revision"] = json!(rev);
    }

    json!({
        "text": text,
        "kind": "repair_outcome",
        "model": l.device_model,
        "confidence": l.confidence,
        "author": "9eck-distiller",
        "device_uuid": l.serial,
        "tags": ["field-repair", format!("src:zoho:{}", l.ticket_number)],
        "anchors": anchors,
        "origin": origin,
    })
}

/// POST a `kb_record` to the xelixir KB MCP endpoint and parse the reply
/// (IntakeReply, or the pre-intake `{finding_id}` shape). Returns `None` if the
/// bridge is not configured (XELIXIR_KB_URL/XELIXIR_KB_TOKEN).
async fn push_to_xelixir_kb(
    http: &HttpClient,
    lesson: &LessonPush<'_>,
) -> anyhow::Result<Option<crate::ai::kb_client::IntakeOutcome>> {
    if !crate::ai::kb_client::configured() {
        return Ok(None);
    }
    let text = crate::ai::kb_client::call_tool(
        http, "kb_record", kb_record_args(lesson), Duration::from_secs(30),
    )
    .await?;
    let reply = crate::ai::kb_client::parse_intake_reply(&text).unwrap_or_default();
    if let Some(m) = reply.message.as_deref() {
        debug!("[RepairDistiller] kb_record says: {}", m);
    }
    Ok(Some(reply))
}

/// Store a `kb_record` reply on the lesson row. Every intake outcome
/// (`new_candidate | linked | corroborated_candidate | duplicate_delivery`) and
/// the legacy finding-only reply count as delivered → `pushed_to_xelixir = true`,
/// with `xelixir_candidate_id` / `xelixir_finding_id` set when the reply carries
/// them. A reply with neither id leaves the lesson unpushed (re-pushed next
/// cycle). Returns whether it counted as delivered.
async fn record_push_result(
    state: &Arc<AppState>,
    ticket_id: &str,
    reply: &crate::ai::kb_client::IntakeOutcome,
) -> bool {
    if !reply.delivered() {
        warn!("[RepairDistiller] kb_record reply for {} carried no candidate/finding id; will re-push", ticket_id);
        return false;
    }
    let state_tag = if reply.finding_id.is_some() { "linked" } else { "pending" };
    let _ = state
        .db
        .query(
            "UPDATE repair_lesson SET pushed_to_xelixir = true, \
             xelixir_candidate_id = $cid ?? xelixir_candidate_id, \
             xelixir_finding_id = $fid ?? xelixir_finding_id, \
             kb_outcome = $out, kb_state = $st, updated_at = time::now() WHERE ticket_id = $tid",
        )
        .bind(("cid", reply.candidate_id.clone()))
        .bind(("fid", reply.finding_id.clone()))
        .bind(("out", reply.outcome.clone()))
        .bind(("st", state_tag))
        .bind(("tid", ticket_id.to_string()))
        .await;
    true
}

/// What a `kb_candidate` read says about one of our candidates: its state and
/// the finding it ended up as (if any), plus the elder candidate it was counted
/// against when it was a corroboration.
#[derive(Debug, Clone, Default, PartialEq)]
struct CandidateStatus {
    state: String,
    linked_finding: Option<String>,
    linked_candidate: Option<String>,
}

/// Parse a `kb_candidate` reply text. `linked_finding` / `linked_candidate` may
/// be strings or record objects (`{tb, id}`); both read as `kb_<tb>:<id>`.
fn parse_candidate_status(text: &str) -> Option<CandidateStatus> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let link = |k: &str| -> Option<String> {
        match v.get(k)? {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Object(o) => {
                let tb = o.get("tb").or_else(|| o.get("table")).and_then(|x| x.as_str())?;
                let id = o.get("id").and_then(|x| x.as_str().map(String::from).or_else(|| Some(x.to_string())))?;
                Some(format!("{tb}:{id}"))
            }
            _ => None,
        }
    };
    Some(CandidateStatus {
        state: v.get("state").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        linked_finding: link("linked_finding"),
        linked_candidate: link("linked_candidate"),
    })
}

/// Read one candidate via the MCP tool `kb_candidate {candidate_id}`.
async fn fetch_candidate(http: &HttpClient, candidate_id: &str) -> anyhow::Result<CandidateStatus> {
    let text = crate::ai::kb_client::call_tool(
        http, "kb_candidate", json!({ "candidate_id": candidate_id }), Duration::from_secs(20),
    )
    .await?;
    parse_candidate_status(&text).ok_or_else(|| anyhow::anyhow!("kb_candidate: non-JSON reply"))
}

/// Daily finding-id write-back (CONTRACT §7): for lessons that hold a
/// `xelixir_candidate_id` but no `xelixir_finding_id`, read the candidate and
/// copy `linked_finding` into `xelixir_finding_id` when its state is
/// `linked | promoted`. A corroboration (`linked` to an elder candidate) follows
/// the elder one hop. The state is always stored in `kb_state` (for stats);
/// `rejected | expired` rows stop being polled. Returns the number of finding
/// ids written. No-op when the bridge is not configured.
async fn write_back_candidates(state: &Arc<AppState>, http: &HttpClient) -> anyhow::Result<usize> {
    if !crate::ai::kb_client::configured() {
        return Ok(0);
    }
    let rows: Vec<Value> = state
        .db
        .query(
            "SELECT ticket_id, xelixir_candidate_id FROM repair_lesson \
             WHERE xelixir_candidate_id != NONE AND xelixir_candidate_id != '' \
             AND (xelixir_finding_id IS NONE OR xelixir_finding_id = '') \
             AND (kb_state IS NONE OR kb_state NOT IN ['rejected', 'expired']) LIMIT $lim",
        )
        .bind(("lim", WRITE_BACK_LIMIT))
        .await?
        .take(0)?;

    let mut written = 0usize;
    for r in rows {
        let (Some(tid), Some(cid)) = (
            r.get("ticket_id").and_then(|v| v.as_str()),
            r.get("xelixir_candidate_id").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let mut st = match fetch_candidate(http, cid).await {
            Ok(s) => s,
            Err(e) => {
                warn!("[RepairDistiller] kb_candidate {} failed: {}", cid, e);
                continue;
            }
        };
        if st.linked_finding.is_none() {
            if let Some(elder) = st.linked_candidate.clone() {
                // Elder may belong to another tenant (not readable) — then wait.
                if let Ok(e) = fetch_candidate(http, &elder).await {
                    if matches!(e.state.as_str(), "linked" | "promoted") {
                        st.linked_finding = e.linked_finding;
                    }
                }
            }
        }
        let fid = st
            .linked_finding
            .clone()
            .filter(|_| matches!(st.state.as_str(), "linked" | "promoted"));
        let _ = state
            .db
            .query(
                "UPDATE repair_lesson SET kb_state = $st, xelixir_finding_id = $fid ?? xelixir_finding_id, \
                 updated_at = time::now() WHERE ticket_id = $tid",
            )
            .bind(("st", st.state.clone()))
            .bind(("fid", fid.clone()))
            .bind(("tid", tid.to_string()))
            .await;
        if fid.is_some() {
            written += 1;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_prefers_ai_summary() {
        let threads = vec![json!({"direction":"in","summary":"mail from max@example.com"})];
        let (b, src) = build_distill_bundle(
            "Waage zeigt E2", "Closed", Some("  Customer Name_1a2b reports E2; board swapped; fixed.  "), &threads,
        )
        .unwrap();
        assert_eq!(src, InputSource::AiSummary);
        assert_eq!(b["summary"], "Customer Name_1a2b reports E2; board swapped; fixed.");
        assert!(b.get("threads").is_none(), "threads must not be sent when a summary exists");
    }

    #[test]
    fn input_falls_back_to_masked_threads() {
        let threads = vec![json!({"direction":"in","summary":"Please call me at +49 170 1234567 or max.mustermann@example.com"})];
        for empty in [None, Some(""), Some("   \n ")] {
            let (b, src) = build_distill_bundle("E2", "Closed", empty, &threads).unwrap();
            assert_eq!(src, InputSource::Threads);
            let s = b["threads"][0]["summary"].as_str().unwrap();
            assert!(!s.contains("max.mustermann@example.com"), "email must be masked: {s}");
            assert!(!s.contains("1234567"), "phone must be masked: {s}");
            assert!(b.get("summary").is_none());
        }
        // Neither summary nor threads → None (transient, retry).
        assert!(build_distill_bundle("E2", "Closed", None, &[]).is_none());
    }

    #[test]
    fn record_args_carry_origin_and_tag() {
        let parts = vec!["right hand electrode".to_string()];
        let l = LessonPush {
            ticket_id: "53451000035655161", ticket_number: "31129", revision: Some("2026-10-30T09:12:00Z"),
            device_model: "Acme770", serial: "I821000195", symptom: "E2 on boot", error_code: "E2",
            diagnosis: "loose connector", action: "reseat", remote_ws: "workshop", parts: &parts, confidence: "high",
        };
        let a = kb_record_args(&l);
        assert_eq!(a["origin"], json!({"system":"9eck","id":"53451000035655161","revision":"2026-10-30T09:12:00Z"}));
        assert_eq!(a["tags"][1], "src:zoho:31129");
        assert_eq!(a["kind"], "repair_outcome");
        let l2 = LessonPush { revision: None, ..l };
        assert!(kb_record_args(&l2)["origin"].get("revision").is_none());
    }

    #[test]
    fn candidate_status_parses_string_and_record_links() {
        let s = parse_candidate_status(r#"{"id":"kb_candidate:aa","state":"promoted","linked_finding":"kb_finding:xyz"}"#).unwrap();
        assert_eq!(s.state, "promoted");
        assert_eq!(s.linked_finding.as_deref(), Some("kb_finding:xyz"));
        let s = parse_candidate_status(r#"{"state":"linked","linked_finding":null,"linked_candidate":{"tb":"kb_candidate","id":"bb"}}"#).unwrap();
        assert_eq!(s.linked_candidate.as_deref(), Some("kb_candidate:bb"));
        assert!(s.linked_finding.is_none());
        assert!(parse_candidate_status("nope").is_none());
    }
}
