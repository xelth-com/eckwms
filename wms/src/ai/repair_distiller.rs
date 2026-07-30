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
    // 0. Re-push any gate-passed lessons whose earlier xelixir POST failed (transient
    //    network/ADC/xelixir outage). Independent of GCP AI auth — runs even when the
    //    distiller LLM itself is unavailable, so a confirmed lesson is never lost.
    match retry_unpushed_lessons(state, http).await {
        Ok(n) if n > 0 => info!("[RepairDistiller] re-pushed {n} previously-unpushed lesson(s) to xelixir KB"),
        Err(e) => warn!("[RepairDistiller] re-push scan failed: {}", e),
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
            "SELECT record::id(id) AS id, status, meta, updated_at, repair_distill_attempts FROM document \
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
             diagnosis, action, remote_vs_workshop, parts, confidence \
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

        match push_to_xelixir_kb(
            http, &g("device_model"), &g("serial"), &g("symptom"), &g("error_code"),
            &g("diagnosis"), &g("action"), &g("remote_vs_workshop"), &parts,
            &g("confidence"), &g("ticket_number"),
        )
        .await
        {
            Ok(Some(fid)) => {
                let _ = state
                    .db
                    .query("UPDATE repair_lesson SET pushed_to_xelixir = true, xelixir_finding_id = $fid, updated_at = time::now() WHERE ticket_id = $tid")
                    .bind(("fid", fid))
                    .bind(("tid", ticket_id.clone()))
                    .await;
                repushed += 1;
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

    // Compact correspondence: direction + author type + Zoho summary (NOT the raw
    // HTML body / attachments) — short, lower-PII, enough to judge the resolution.
    let threads: Vec<Value> = state
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
        .take(0)?;

    if threads.is_empty() {
        // Threads not imported yet (scraper lag). Treat as transient so we do NOT
        // finalize the ticket — retry on later cycles (bounded by MAX_DISTILL_ATTEMPTS).
        return Err(anyhow::anyhow!("no threads imported yet for ticket {ticket_id}"));
    }

    let bundle = json!({
        "subject": subject,
        "status": status,
        "threads": threads,
    });
    let bundle_str = serde_json::to_string(&bundle).unwrap_or_default();
    let user_prompt = format!(
        "Distill the repair lesson from this support ticket. Strict JSON only.\n\n## Ticket\n```json\n{bundle_str}\n```"
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
    let (response_text, usage) = auth.generate_content(http, gen_model, payload).await?;
    log_telemetry(&state.db, "repair_distiller", gen_model, ticket_id, &usage).await;

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
    match push_to_xelixir_kb(
        http, &device_model, &serial, &symptom, &error_code, &diagnosis,
        &action, &remote_ws, &parts, &confidence, &ticket_number,
    )
    .await
    {
        Ok(Some(finding_id)) => {
            let _ = state
                .db
                .query("UPDATE repair_lesson SET pushed_to_xelixir = true, xelixir_finding_id = $fid, updated_at = time::now() WHERE ticket_id = $tid")
                .bind(("fid", finding_id.clone()))
                .bind(("tid", ticket_id.to_string()))
                .await;
            info!(
                "[RepairDistiller] ticket {} -> xelixir KB {} (model={}, '{}')",
                ticket_id, finding_id, device_model, symptom
            );
            Ok(true)
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

/// POST a `kb_record` to the xelixir KB MCP endpoint. Returns the new finding id,
/// or `None` if the bridge is not configured (XELIXIR_KB_URL/XELIXIR_KB_TOKEN).
#[allow(clippy::too_many_arguments)]
async fn push_to_xelixir_kb(
    http: &HttpClient,
    device_model: &str,
    serial: &str,
    symptom: &str,
    error_code: &str,
    diagnosis: &str,
    action: &str,
    remote_ws: &str,
    parts: &[String],
    confidence: &str,
    ticket_number: &str,
) -> anyhow::Result<Option<String>> {
    let url = match std::env::var("XELIXIR_KB_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => return Ok(None),
    };
    let token = std::env::var("XELIXIR_KB_TOKEN").unwrap_or_default();
    if token.is_empty() {
        return Ok(None);
    }

    let parts_str = if parts.is_empty() { String::new() } else { format!(" Parts: {}.", parts.join(", ")) };
    let ec_note = if error_code.is_empty() { String::new() } else { format!(" [error_code {error_code}]") };
    let text = format!(
        "FIELD REPAIR (confirmed){ec_note}: {symptom} -> diagnosis: {diagnosis}. Fix: {action} ({remote_ws}).{parts_str} \
         Confirmed-fixed in the field on a {device_model} (SN {serial}). Source: 9eck repair-lesson distiller, Zoho ticket {ticket_number}."
    );

    let mut anchors = vec![json!({"kind":"symptom","slug": symptom})];
    if !error_code.is_empty() {
        anchors.push(json!({"kind":"error_code","slug": error_code}));
    }

    let args = json!({
        "text": text,
        "kind": "repair_outcome",
        "model": device_model,
        "confidence": confidence,
        "author": "9eck-distiller",
        "device_uuid": serial,
        "tags": ["field-repair", format!("src:zoho:{ticket_number}")],
        "anchors": anchors,
    });
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "kb_record", "arguments": args }
    });

    let resp = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;
    let raw = resp.text().await?;

    // The MCP endpoint replies as SSE: lines prefixed with "data: ".
    for line in raw.lines() {
        let payload = line.strip_prefix("data: ").unwrap_or(line);
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            if let Some(err) = v.get("error") {
                return Err(anyhow::anyhow!("kb_record error: {err}"));
            }
            if let Some(text) = v.pointer("/result/content/0/text").and_then(|t| t.as_str()) {
                if let Ok(inner) = serde_json::from_str::<Value>(text) {
                    if let Some(fid) = inner.get("finding_id").and_then(|f| f.as_str()) {
                        return Ok(Some(fid.to_string()));
                    }
                }
                return Ok(Some(text.chars().take(60).collect()));
            }
        }
    }
    Err(anyhow::anyhow!("kb_record: no usable response ({})", raw.chars().take(200).collect::<String>()))
}
