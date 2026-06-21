use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rig::agent::AgentBuilder;
use rig::client::CompletionClient;
use rig::completion::{CompletionModel, Prompt};
use rig::providers::{gemini, openai};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use surrealdb::Notification;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::ai::embeddings::embed_query;
use crate::ai::telemetry::{current_budget_level, log_telemetry, BudgetLevel};
use crate::ai::tools::{AnalyzeQcReportTool, AskHumanTool, ListTicketAttachmentsTool};
use crate::AppState;
use eck_core::sync::hedera::submit_hash_if_configured;
use eck_core::utils::anonymizer::obfuscate_pii;
use eck_core::utils::filestore::FileStore;

/// Phase 2 — Central Brain orchestrator.
///
/// Event-sourced ReAct loop: LIVE SELECT on `ai_task` + `ai_inbox` wakes the
/// worker only when state actually changes. A 30s polling fallback recovers
/// dropped streams and stale (crashed-worker) claims. Claim transitions are
/// atomic — two orchestrators on the same DB cannot execute the same task twice.
///
/// The per-task execution body runs a `rig-core` Gemini agent with the
/// `ask_human` and `analyze_qc_report` tools, then writes an `ai_thought`
/// row (SHA-256 hashed + optionally Hedera-sealed for GoBD audit). Tasks
/// that called `ask_human` park in `awaiting_human` and are NOT marked
/// `completed` — they resume when an `ai_inbox` row arrives via the HTTP
/// `POST /api/ai/tasks/:id/reply` endpoint.
pub async fn start_orchestrator(state: Arc<AppState>) {
    tokio::time::sleep(Duration::from_secs(25)).await;

    let worker_id = format!("orch-{}", Uuid::new_v4());
    info!("[Orchestrator] Central Brain starting (worker_id={})", worker_id);

    {
        let s = state.clone();
        let wid = worker_id.clone();
        tokio::spawn(async move { watch_tasks_live(s, wid).await; });
    }
    {
        let s = state.clone();
        let wid = worker_id.clone();
        tokio::spawn(async move { watch_inbox_live(s, wid).await; });
    }

    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        if let Err(e) = poll_ready_tasks(&state, &worker_id).await {
            warn!("[Orchestrator] Poll cycle error: {}", e);
        }
    }
}

// ── Real-Time: ai_task LIVE SELECT ─────────────────────────────────────────

async fn watch_tasks_live(state: Arc<AppState>, worker_id: String) {
    info!("[Orchestrator] LIVE SELECT on ai_task");
    loop {
        match state.db.query("LIVE SELECT * FROM ai_task").await {
            Ok(mut response) => match response.stream::<Notification<Value>>(0) {
                Ok(mut stream) => {
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(notification) => {
                                let action = notification.action.to_string();
                                if action != "Create" && action != "Update" {
                                    continue;
                                }
                                let state_str = notification
                                    .data
                                    .get("state")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if state_str != "ready" && state_str != "resumed" {
                                    continue;
                                }
                                // Defense in depth: skip the downstream call
                                // during HALT so we don't burn a DB claim
                                // round-trip when we wouldn't execute anyway.
                                if current_budget_level() == BudgetLevel::Halt {
                                    debug!("[Orchestrator] Budget HALT — dropping live event");
                                    continue;
                                }
                                debug!(
                                    "[Orchestrator] Live task event ({}, state={}), triggering poll",
                                    action, state_str
                                );
                                if let Err(e) = poll_ready_tasks(&state, &worker_id).await {
                                    warn!("[Orchestrator] Live-triggered poll failed: {}", e);
                                }
                            }
                            Err(e) => warn!("[Orchestrator] Task live stream error: {}", e),
                        }
                    }
                    warn!("[Orchestrator] Task live stream ended; reconnecting in 10s");
                }
                Err(e) => warn!("[Orchestrator] Task stream init failed: {}", e),
            },
            Err(e) => warn!("[Orchestrator] Task LIVE query failed: {}", e),
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

// ── Real-Time: ai_inbox LIVE SELECT ────────────────────────────────────────

async fn watch_inbox_live(state: Arc<AppState>, _worker_id: String) {
    info!("[Orchestrator] LIVE SELECT on ai_inbox");
    loop {
        match state.db.query("LIVE SELECT * FROM ai_inbox").await {
            Ok(mut response) => match response.stream::<Notification<Value>>(0) {
                Ok(mut stream) => {
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(notification) => {
                                if notification.action.to_string() != "Create" {
                                    continue;
                                }
                                let task_id_full = match notification
                                    .data
                                    .get("task_id")
                                    .and_then(|v| v.as_str())
                                {
                                    Some(s) if !s.is_empty() => s.to_string(),
                                    _ => continue,
                                };
                                let res = state
                                    .db
                                    .query(
                                        "UPDATE type::record($rid) \
                                         SET state = 'resumed', updated_at = time::now() \
                                         WHERE state = 'awaiting_human'",
                                    )
                                    .bind(("rid", task_id_full.clone()))
                                    .await;
                                match res {
                                    Ok(_) => debug!(
                                        "[Orchestrator] Resumed task {} from inbox event",
                                        task_id_full
                                    ),
                                    Err(e) => warn!(
                                        "[Orchestrator] Failed to resume {}: {}",
                                        task_id_full, e
                                    ),
                                }
                            }
                            Err(e) => warn!("[Orchestrator] Inbox live stream error: {}", e),
                        }
                    }
                    warn!("[Orchestrator] Inbox live stream ended; reconnecting in 10s");
                }
                Err(e) => warn!("[Orchestrator] Inbox stream init failed: {}", e),
            },
            Err(e) => warn!("[Orchestrator] Inbox LIVE query failed: {}", e),
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

// ── Fallback polling + dispatch ────────────────────────────────────────────

async fn poll_ready_tasks(state: &Arc<AppState>, worker_id: &str) -> anyhow::Result<()> {
    // Honor the global token circuit breaker set by the Observer. HALT → skip
    // this tick entirely; THROTTLE → sleep 60s (same cadence as other AI
    // workers via telemetry::THROTTLE_DELAY_SECS) before claiming.
    match current_budget_level() {
        BudgetLevel::Halt => {
            debug!("[Orchestrator] Budget HALT — skipping poll cycle");
            return Ok(());
        }
        BudgetLevel::Throttle => {
            debug!("[Orchestrator] Budget THROTTLE — sleeping 60s before claim");
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        _ => {}
    }

    let tasks: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id FROM ai_task \
             WHERE state IN ['ready', 'resumed'] \
             AND (worker_id IS NONE OR claimed_at IS NONE OR claimed_at < time::now() - 5m) \
             LIMIT 5",
        )
        .await?
        .take(0)?;

    for task in tasks {
        if let Some(task_id) = task.get("id").and_then(|v| v.as_str()) {
            if let Err(e) = try_claim_and_execute(state.clone(), task_id.to_string(), worker_id.to_string()).await {
                error!("[Orchestrator] Exec error for {}: {}", task_id, e);
            }
        }
    }
    Ok(())
}

// ── Atomic claim + ReAct executor ─────────────────────────────────────────

async fn try_claim_and_execute(
    state: Arc<AppState>,
    task_id: String,
    worker_id: String,
) -> anyhow::Result<()> {
    let db = &state.db;
    let rid = format!("ai_task:{}", task_id);

    let claimed: Vec<Value> = db
        .query(
            "UPDATE type::record($rid) \
             SET state = 'running', worker_id = $wid, claimed_at = time::now() \
             WHERE state IN ['ready', 'resumed'] \
             AND (worker_id IS NONE OR claimed_at IS NONE OR claimed_at < time::now() - 5m) \
             RETURN record::id(id) AS id",
        )
        .bind(("rid", rid.clone()))
        .bind(("wid", worker_id.clone()))
        .await?
        .take(0)?;

    if claimed.is_empty() {
        return Ok(());
    }

    debug!("[Orchestrator] Claimed task {} as {}", task_id, worker_id);

    // ── Fetch the full task (context + any prior awaiting question) ──────
    let task_rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, context, awaiting_input_schema \
             FROM type::record($rid)",
        )
        .bind(("rid", rid.clone()))
        .await?
        .take(0)?;

    let task = match task_rows.into_iter().next() {
        Some(t) => t,
        None => {
            warn!("[Orchestrator] Task {} disappeared after claim", task_id);
            return Ok(());
        }
    };

    // ── Fetch inbox messages for this task (human replies accumulated) ───
    let inbox: Vec<Value> = db
        .query(
            "SELECT source, content, created_at FROM ai_inbox \
             WHERE task_id = $tid ORDER BY created_at ASC",
        )
        .bind(("tid", rid.clone()))
        .await?
        .take(0)?;

    // ── Run the agent ────────────────────────────────────────────────────
    let http = reqwest::Client::new();
    let auth = match eck_core::ai::AiAuth::resolve(&http).await {
        Ok(a) => a,
        Err(e) => {
            fail_task(&state, &rid, &format!("AI token mint failed: {e}")).await;
            return Ok(());
        }
    };
    if !auth.is_configured() {
        fail_task(
            &state,
            &rid,
            "AI auth not configured — cannot run orchestrator agent",
        )
        .await;
        return Ok(());
    }

    // The orchestrator makes the hard calls (tool routing, triage) — give it the
    // bigger Flash model rather than the Lite workhorse the background jobs use.
    let model = std::env::var("GEMINI_ORCHESTRATOR_MODEL")
        .or_else(|_| std::env::var("GEMINI_GENERATION_MODEL"))
        .unwrap_or_else(|_| "gemini-3.5-flash".to_string());

    // ── RAG: Retrieve relevant SOPs ──────────────────────────────────────
    // Build a query string from task.context.subject + task.context.description,
    // embed it, then HNSW-search `ai_sop` for the closest non-deprecated rules.
    // Failures here are non-fatal — we degrade to zero-SOP execution.
    let query_text = {
        let subject = task
            .get("context")
            .and_then(|c| c.get("subject"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = task
            .get("context")
            .and_then(|c| c.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("{subject} {description}").trim().to_string()
    };

    let mut sop_context = String::new();
    let mut used_sop_ids: Vec<String> = Vec::new();

    if !query_text.is_empty() {
        match embed_query(&query_text).await {
            Ok(query_emb) => {
                let sop_rows_res = db
                    .query(
                        "SELECT record::id(id) AS id, title, rule \
                         FROM ai_sop \
                         WHERE deprecated = false AND embedding <|3, 100|> $q \
                         LIMIT 3",
                    )
                    .bind(("q", query_emb))
                    .await
                    .and_then(|mut r| r.take::<Vec<Value>>(0));

                match sop_rows_res {
                    Ok(rows) => {
                        for row in rows {
                            if let (Some(id), Some(title), Some(rule)) = (
                                row.get("id").and_then(|v| v.as_str()),
                                row.get("title").and_then(|v| v.as_str()),
                                row.get("rule").and_then(|v| v.as_str()),
                            ) {
                                used_sop_ids.push(id.to_string());
                                sop_context.push_str(&format!("- [{title}]: {rule}\n"));
                            }
                        }
                    }
                    Err(e) => warn!(
                        "[Orchestrator] Failed to fetch SOPs for {}: {}",
                        rid, e
                    ),
                }
            }
            Err(e) => warn!(
                "[Orchestrator] Failed to embed query for SOP retrieval on {}: {}",
                rid, e
            ),
        }
    }

    let user_prompt = build_user_prompt(&task, &inbox);
    let mut system_prompt = SYSTEM_PROMPT.to_string();
    if !sop_context.is_empty() {
        system_prompt.push_str(
            "\n\n## COMPANY STANDARD OPERATING PROCEDURES (SOP)\n\
             Apply these rules if relevant to the current task:\n",
        );
        system_prompt.push_str(&sop_context);
    }

    let agent_result = run_agent(
        &auth,
        &model,
        &system_prompt,
        &user_prompt,
        db.clone(),
        rid.clone(),
        state.ws_tx.clone(),
    )
    .await;

    let (response_text, exec_error) = match agent_result {
        Ok(text) => (text, None),
        Err(e) => {
            let msg = e.to_string();
            warn!("[Orchestrator] Agent execution failed for {}: {}", rid, msg);
            (String::new(), Some(msg))
        }
    };

    // ── Token telemetry (heuristic: ~4 chars per token) ──────────────────
    // rig-core's Prompt trait doesn't surface usage metadata, so we estimate
    // from char lengths — same approach as summarization.rs. The `estimated`
    // flag in the usage payload lets downstream budget analysis downweight
    // these if needed. Observer aggregates via math::sum(total_tokens).
    let prompt_tokens = ((system_prompt.len() + user_prompt.len()) / 4) as i64;
    let candidates_tokens = (response_text.len() / 4) as i64;
    let usage = json!({
        "promptTokenCount": prompt_tokens,
        "candidatesTokenCount": candidates_tokens,
        "totalTokenCount": prompt_tokens + candidates_tokens,
        "estimated": true,
    });
    log_telemetry(db, "orchestrator", &model, &rid, &usage).await;

    // ── Persist thought with Hedera seal ─────────────────────────────────
    let payload = json!({
        "response": response_text,
        "error": exec_error,
        "inbox_len": inbox.len(),
    });

    write_thought(&state, &rid, 1, "execute", &payload).await;

    // ── Decide final task state ──────────────────────────────────────────
    // If the agent called `ask_human`, the task is already in `awaiting_human`
    // and we must NOT overwrite that. We re-read the state post-agent.
    let post_state: Vec<Value> = db
        .query("SELECT state FROM type::record($rid)")
        .bind(("rid", rid.clone()))
        .await?
        .take(0)?;
    let post_state_str = post_state
        .first()
        .and_then(|v| v.get("state"))
        .and_then(|v| v.as_str())
        .unwrap_or("running")
        .to_string();

    if post_state_str == "awaiting_human" {
        info!("[Orchestrator] Task {} parked awaiting human reply", task_id);
        return Ok(());
    }

    let final_state = if exec_error.is_some() { "failed" } else { "completed" };
    db.query(
        "UPDATE type::record($rid) SET state = $s, updated_at = time::now()",
    )
    .bind(("rid", rid.clone()))
    .bind(("s", final_state.to_string()))
    .await?
    .check()?;

    // ── SOP feedback (RLHF-lite) ─────────────────────────────────────────
    // Closes the Phase 5 → 6 loop: every SOP that fed into this run gets
    // usage_count++ and either success_count++ or failure_count++ depending
    // on the terminal task state. The Optimizer's hygiene pass later
    // deprecates SOPs whose usage stays low.
    if !used_sop_ids.is_empty() {
        let is_success = final_state != "failed";
        let update_sql = if is_success {
            "UPDATE type::record($rid) SET usage_count += 1, success_count += 1, updated_at = time::now()"
        } else {
            "UPDATE type::record($rid) SET usage_count += 1, failure_count += 1, updated_at = time::now()"
        };
        for sop_id in &used_sop_ids {
            let sop_rid = format!("ai_sop:{}", sop_id);
            if let Err(e) = db.query(update_sql).bind(("rid", sop_rid)).await {
                warn!(
                    "[Orchestrator] Failed to update SOP metrics for {}: {}",
                    sop_id, e
                );
            }
        }
    }

    info!(
        "[Orchestrator] Task {} finished with state={} (SOPs used: {})",
        task_id, final_state, used_sop_ids.len()
    );
    Ok(())
}

// ── Agent runner ──────────────────────────────────────────────────────────

const SYSTEM_PROMPT: &str = r#"You are the Central Brain (orchestrator) for eckWMS — a Rust-based WMS/ERP for InBody medical devices.

You are executing a single task end-to-end. You have the following tools:

- `list_ticket_attachments(ticket_id)` — List files already attached to the ticket (returns CAS UUIDs + names + mime types). Always call this FIRST if the task hints at QC reports, photos, or documents. Most QC reports have already been pulled from Zoho and are sitting in our file store — you just need to look them up.
- `analyze_qc_report(file_ids)` — Extract digital firmware, analog firmware, and serial number from one or more QC report files (identified by their CAS UUIDs). Feed it the CAS UUIDs you got from `list_ticket_attachments`.
- `ask_human(question)` — Pause execution and ask the operator a specific question. The operator will reply asynchronously; your execution will resume later. Call this ONLY when (a) the ticket context is too thin to act on AND (b) `list_ticket_attachments` came back empty. After calling `ask_human`, your turn ends — do not call any more tools.

TRIAGE RULES:
- If the ticket's `meta.description` is empty AND the subject is a reply (starts with Re:/Fwd:/Aw:) AND `list_ticket_attachments` returns nothing — there is no customer problem to solve. Respond with a single sentence like "No actionable content — reply thread without new request." and stop. Do not call `ask_human`.
- Do not ask for CAS UUIDs. You have `list_ticket_attachments` — use it.
- Think step by step. Be concise. If the task is solvable with the context already provided, just answer."#;

async fn run_agent(
    auth: &eck_core::ai::AiAuth,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    db: eck_core::db::SurrealDb,
    task_rid: String,
    ws_tx: tokio::sync::broadcast::Sender<String>,
) -> anyhow::Result<String> {
    let filestore = Arc::new(FileStore::new("."));

    // The agent (ReAct loop + tools) is identical across modes — only the
    // provider client it's built from changes. `studio` uses rig's gemini
    // provider (AI Studio `?key=`); `managed` uses rig's openai provider
    // pointed at Vertex's OpenAI-compatible endpoint with a Bearer token.
    // See .eck/AI_DUAL_PROVIDER_VERTEX.md §9 (Path A).
    match auth {
        eck_core::ai::AiAuth::Studio { api_key } => {
            let client = gemini::Client::new(api_key)?;
            drive_orchestrator(client.agent(model), system_prompt, user_prompt, db, task_rid, ws_tx, filestore).await
        }
        eck_core::ai::AiAuth::Vertex { bearer, project, location } => {
            let host = if location == "global" {
                "aiplatform.googleapis.com".to_string()
            } else {
                format!("{location}-aiplatform.googleapis.com")
            };
            let base = format!(
                "https://{host}/v1/projects/{project}/locations/{location}/endpoints/openapi"
            );
            let client = openai::CompletionsClient::builder()
                .api_key(bearer.as_str())
                .base_url(&base)
                .build()
                .map_err(|e| anyhow::anyhow!("Vertex openai client build failed: {e}"))?;
            // Vertex OpenAI-compat needs the publisher-qualified model id.
            let vmodel = if model.starts_with("google/") {
                model.to_string()
            } else {
                format!("google/{model}")
            };
            drive_orchestrator(client.agent(&vmodel), system_prompt, user_prompt, db, task_rid, ws_tx, filestore).await
        }
    }
}

/// Attach the orchestrator's tools and run the ReAct loop. Generic over the
/// completion model so `studio` (gemini) and `managed` (openai-on-Vertex) share
/// one body.
async fn drive_orchestrator<M>(
    builder: AgentBuilder<M>,
    system_prompt: &str,
    user_prompt: &str,
    db: eck_core::db::SurrealDb,
    task_rid: String,
    ws_tx: tokio::sync::broadcast::Sender<String>,
    filestore: Arc<FileStore>,
) -> anyhow::Result<String>
where
    M: CompletionModel,
{
    let agent = builder
        .preamble(system_prompt)
        .tool(AskHumanTool {
            db: db.clone(),
            task_rid: task_rid.clone(),
            ws_tx,
        })
        .tool(ListTicketAttachmentsTool { db: db.clone() })
        .tool(AnalyzeQcReportTool { db, filestore })
        .build();

    // rig-core 0.33 defaults `max_turns=0` — first tool result kills the
    // request with MaxTurnError. Give the orchestrator enough headroom for
    // list_attachments → analyze_qc_report → final answer, plus an
    // occasional ask_human branch.
    let response: String = agent.prompt(user_prompt).max_turns(6).await?;
    Ok(response)
}

/// Mask raw PII fields in `context.meta` before serializing the task
/// context into the LLM user prompt. Mirrors the embedding pipeline:
/// customer/email/phone/address become stable SimHash tokens, coarse geo
/// (city/zip) and business metadata (subject, device, serial, ticket
/// number, status) stay in the clear because the orchestrator needs them
/// to reason. The ticket `description` field frequently contains
/// free-form PII that we cannot statically locate — it is stripped
/// entirely; the model already has the original via
/// `list_attachments` / `analyze_qc_report` when it genuinely needs to.
fn scrub_context_for_prompt(ctx: &Value) -> Value {
    let mut out = ctx.clone();
    let Some(meta) = out.get_mut("meta").and_then(|m| m.as_object_mut()) else {
        return out;
    };

    let mask_str = |m: &mut serde_json::Map<String, Value>, key: &str, pii_type: &str| {
        if let Some(Value::String(s)) = m.get(key) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                let token = obfuscate_pii(trimmed, pii_type);
                m.insert(key.to_string(), Value::String(token));
            }
        }
    };

    mask_str(meta, "customer", "Name");
    mask_str(meta, "email", "Email");
    mask_str(meta, "phone", "Phone");
    mask_str(meta, "address", "Address");

    // `description` is free-form PII-prone customer text. Drop it from the
    // prompt entirely; tools re-fetch the source when needed.
    meta.remove("description");

    out
}

fn build_user_prompt(task: &Value, inbox: &[Value]) -> String {
    let ctx_str = task
        .get("context")
        .map(|c| serde_json::to_string_pretty(&scrub_context_for_prompt(c)).unwrap_or_default())
        .unwrap_or_else(|| "{}".into());

    let mut inbox_str = String::new();
    if !inbox.is_empty() {
        inbox_str.push_str("\n\n## Human replies (chronological)\n");
        for msg in inbox {
            let source = msg.get("source").and_then(|v| v.as_str()).unwrap_or("user");
            let content_str = msg
                .get("content")
                .map(|c| match c.as_str() {
                    Some(s) => s.to_string(),
                    None => serde_json::to_string(c).unwrap_or_default(),
                })
                .unwrap_or_default();
            inbox_str.push_str(&format!("- [{source}]: {content_str}\n"));
        }
    }

    format!("## Task context\n```json\n{ctx_str}\n```{inbox_str}\n\nProceed.")
}

// ── Thought persistence with Hedera seal ──────────────────────────────────

async fn write_thought(
    state: &Arc<AppState>,
    task_rid: &str,
    iteration: i64,
    phase: &str,
    payload: &Value,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let payload_str = serde_json::to_string(payload).unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(task_rid.as_bytes());
    hasher.update(phase.as_bytes());
    hasher.update(payload_str.as_bytes());
    hasher.update(now.as_bytes());
    let hash = hex::encode(hasher.finalize());

    let receipt = submit_hash_if_configured(state.hedera.as_ref(), &hash).await;
    let seq = receipt.as_ref().map(|r| r.sequence_number as i64);
    let ts = receipt.as_ref().map(|r| r.consensus_timestamp.clone());

    let res = state
        .db
        .query(
            "INSERT INTO ai_thought { \
                task_id: $tid, \
                iteration: $it, \
                phase: $phase, \
                payload: $payload, \
                content_hash: $h, \
                hedera_sequence: $seq, \
                hedera_timestamp: $ts, \
                created_at: time::now() \
            }",
        )
        .bind(("tid", task_rid.to_string()))
        .bind(("it", iteration))
        .bind(("phase", phase.to_string()))
        .bind(("payload", payload.clone()))
        .bind(("h", hash))
        .bind(("seq", seq))
        .bind(("ts", ts))
        .await;

    if let Err(e) = res.and_then(|mut r| r.take::<Vec<Value>>(0).map(|_| ())) {
        warn!("[Orchestrator] Failed to persist ai_thought for {}: {}", task_rid, e);
    }
}

async fn fail_task(state: &Arc<AppState>, task_rid: &str, reason: &str) {
    warn!("[Orchestrator] Failing task {}: {}", task_rid, reason);
    let _ = state
        .db
        .query("UPDATE type::record($rid) SET state = 'failed', updated_at = time::now()")
        .bind(("rid", task_rid.to_string()))
        .await;
    write_thought(
        state,
        task_rid,
        0,
        "fail",
        &json!({ "error": reason }),
    )
    .await;
}
