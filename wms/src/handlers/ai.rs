use axum::{
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
    Extension, Json,
};
use eck_core::auth::Claims;
use rig::client::CompletionClient;
use rig::completion::{CompletionModel, Prompt};
use rig::providers::{gemini, openai};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::ai::tools::SearchDatabaseTool;
use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// GET /api/ai/usage — dashboard token-budget gauge + month-end forecast.
///
/// Combines the AUTHORITATIVE balance from the token authority (piggybacked on
/// each mint: remaining / monthly_grant / loan_outstanding / period_ends_at)
/// with the LOCAL burn rate from `ai_telemetry`, and computes whether the budget
/// will survive to the period reset. The authority snapshot is ~hourly-stale
/// (mint TTL), so `remaining` is age-corrected by subtracting tokens metered
/// locally since the snapshot. Falls back to the old local-only estimate in
/// studio mode / before the first managed mint (`source: "local"`).
pub async fn get_ai_usage(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let sum_tokens = |rows: &[Value], k: &str| -> i64 {
        rows.first()
            .and_then(|v| v.get(k))
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0)
    };

    // 24h spend (legacy gauge fields).
    let day: Vec<Value> = state.db
        .query("SELECT count() AS calls, math::sum(total_tokens) AS used \
                FROM ai_telemetry WHERE timestamp > time::now() - 1d GROUP ALL")
        .await.and_then(|mut r| r.take(0)).map_err(db_err)?;
    let used_24h = sum_tokens(&day, "used");
    let calls_24h = sum_tokens(&day, "calls");

    // 7-day burn rate → tokens/day, the basis of the forecast.
    let week: Vec<Value> = state.db
        .query("SELECT math::sum(total_tokens) AS used \
                FROM ai_telemetry WHERE timestamp > time::now() - 7d GROUP ALL")
        .await.and_then(|mut r| r.take(0)).map_err(db_err)?;
    let burn_per_day = (sum_tokens(&week, "used") as f64 / 7.0).max(0.0);

    let mut out = json!({
        "used_24h": used_24h,
        "calls_24h": calls_24h,
        "burn_per_day": burn_per_day.round() as i64,
    });

    // Wallet is per-MESH (shared). Pull a fresh authority snapshot when ours
    // is >60s old so every node's gauge shows the same figure; falls back to
    // the cached (mint/usage-piggybacked) snapshot when the authority is
    // unreachable, and to the local estimate when never minted.
    match eck_core::ai::refresh_quota_if_stale(60).await {
        Some(q) => {
            let now = chrono::Utc::now();
            // Age-correct the snapshot: subtract tokens metered locally since the
            // mint so `remaining` tracks reality between hourly snapshots.
            let age_secs = (now.timestamp() - q.fetched_at_unix).max(0);
            let since: Vec<Value> = state.db
                .query("SELECT math::sum(total_tokens) AS used \
                        FROM ai_telemetry WHERE timestamp > time::now() - type::duration($d) GROUP ALL")
                .bind(("d", format!("{}s", age_secs)))
                .await.and_then(|mut r| r.take(0)).map_err(db_err)?;
            let used_since = sum_tokens(&since, "used");
            let remaining_now = (q.tokens_remaining - used_since).max(0);
            // v3 plan engine: extra packs are drawn after the monthly grant, so
            // local burn overflowing the grant eats into the extra bucket.
            let total_available =
                (q.tokens_remaining + q.extra_balance.max(0) - used_since).max(0);
            let extra_now = total_available - remaining_now;

            let days_to_end = chrono::DateTime::parse_from_rfc3339(&q.period_ends_at)
                .ok()
                .map(|d| (d.timestamp() - now.timestamp()) as f64 / 86_400.0)
                .unwrap_or(0.0)
                .max(0.0);
            let projected_use = burn_per_day * days_to_end;
            let est_days_left = if burn_per_day > 1.0 {
                Some(total_available as f64 / burn_per_day)
            } else {
                None // negligible burn → effectively unlimited
            };
            // Green only when no loan is outstanding AND the budget (monthly +
            // extra pack) covers the projected spend to the period reset.
            let will_last =
                q.loan_outstanding == 0 && (total_available as f64) >= projected_use;
            let est_empty_at = est_days_left
                .filter(|d| !will_last) // only surface an exhaustion date when it matters
                .map(|d| (now + chrono::Duration::seconds((d * 86_400.0) as i64)).to_rfc3339())
                .unwrap_or_default();

            out["source"] = json!("authority");
            out["tokens_remaining"] = json!(remaining_now);
            out["monthly_grant"] = json!(q.monthly_grant);
            out["loan_outstanding"] = json!(q.loan_outstanding);
            // v3 plan-engine buckets — 0 until the authority runs the v3 engine.
            out["extra_balance"] = json!(extra_now);
            out["credit_used"] = json!(q.loan_outstanding);
            out["credit_limit"] = json!(q.credit_limit);
            out["period_ends_at"] = json!(q.period_ends_at);
            out["days_to_period_end"] = json!(days_to_end.round() as i64);
            out["will_last"] = json!(will_last);
            out["est_days_left"] = json!(est_days_left.map(|d| d.round() as i64).unwrap_or(-1));
            out["est_empty_at"] = json!(est_empty_at);
            // Legacy key kept = the per-client grant (was a hardcoded 500k).
            out["promo_cap"] = json!(q.monthly_grant);
        }
        None => {
            out["source"] = json!("local");
            out["promo_cap"] = json!(500_000);
        }
    }

    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct TaskQuery {
    pub state: Option<String>,
}

/// GET /api/ai/tasks — list AI tasks, optionally filtered by state.
///
/// Used by the Operator Inbox dashboard page. When `state` is provided,
/// filters to that single state (typically `awaiting_human`). Returns up
/// to 50 rows, newest first. `id` is extracted via `record::id(id)` so it
/// round-trips through `Vec<Value>` (otherwise the `Thing` enum silently
/// deserializes to `[]`).
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TaskQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let mut sql = "SELECT record::id(id) AS id, state, owner_instance_id, \
                   context, awaiting_input_schema, result, error, created_at, updated_at \
                   FROM ai_task"
        .to_string();

    if q.state.is_some() {
        // A closed ticket's triage is moot whatever state its task is in —
        // don't show operators stale questions/verdicts for tickets that are
        // already resolved (the 2026-08 corpus re-import seeded 267 of them).
        sql.push_str(" WHERE state = $state AND context.meta.status != 'Closed'");
    }
    sql.push_str(" ORDER BY updated_at DESC LIMIT 50");

    let mut stmt = state.db.query(&sql);
    if let Some(s) = q.state {
        stmt = stmt.bind(("state", s));
    }

    let tasks: Vec<Value> = stmt.await.map_err(db_err)?.take(0).map_err(db_err)?;
    Ok(Json(tasks))
}

/// POST /api/ai/tasks/:id/reply — human reply to a paused AI task.
///
/// Inserts an `ai_inbox` row pointing at the task. The orchestrator's
/// `LIVE SELECT * FROM ai_inbox` stream picks it up, flips the task from
/// `awaiting_human` to `resumed`, which re-triggers the task executor.
///
/// Idempotency: callers may send an explicit `Idempotency-Key` header.
/// When absent, we derive a content-hash key from the body so a double
/// click on the operator UI within the dedup window collapses to one
/// inbox row (the orchestrator would otherwise spin the ReAct loop twice
/// on the same reply, burning Gemini budget). The dedup window is short
/// on purpose — a deliberate re-reply with identical text after a few
/// minutes is still accepted.
pub async fn reply_to_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let rid = format!("ai_task:{}", task_id);

    let idem_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let body_str = serde_json::to_string(&body).unwrap_or_default();
            let mut hasher = Sha256::new();
            hasher.update(rid.as_bytes());
            hasher.update(b"|");
            hasher.update(body_str.as_bytes());
            format!("sha256:{}", hex::encode(hasher.finalize()))
        });

    let existing: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id FROM ai_inbox \
             WHERE task_id = $tid \
             AND idempotency_key = $ik \
             AND created_at > time::now() - 5m \
             LIMIT 1",
        )
        .bind(("tid", rid.clone()))
        .bind(("ik", idem_key.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    if !existing.is_empty() {
        debug!("[AI] reply_to_task dedup hit for {} key={}", rid, idem_key);
        return Ok((
            StatusCode::OK,
            Json(json!({ "status": "duplicate_ignored", "task_id": rid })),
        ));
    }

    state
        .db
        .query(
            "INSERT INTO ai_inbox { \
                task_id: $tid, \
                source: 'user', \
                content: $content, \
                idempotency_key: $ik, \
                created_at: time::now() \
            }",
        )
        .bind(("tid", rid.clone()))
        .bind(("content", body))
        .bind(("ik", idem_key))
        .await
        .map_err(db_err)?
        .check()
        .map_err(db_err)?;

    // Resume the parked task directly — do NOT depend on the orchestrator's
    // `LIVE SELECT ai_inbox` watcher (gated off by default; embedded SurrealDB live
    // streams busy-spin a core). The 30s orchestrator poll then claims the now-
    // `resumed` task. Mirrors what watch_inbox_live did.
    let _ = state
        .db
        .query(
            "UPDATE type::record($rid) SET state = 'resumed', updated_at = time::now() \
             WHERE state = 'awaiting_human'",
        )
        .bind(("rid", rid.clone()))
        .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "status": "queued", "task_id": rid })),
    ))
}

// ─── CSV Enrichment ──────────────────────────────────────────────────────────

const ENRICH_SYSTEM_PROMPT_TEMPLATE: &str = r#"You are a data enrichment assistant for eckWMS (an RMA/repair WMS for {{PRODUCT_DOMAIN}}).

You will receive ONE headerless CSV row as raw text. Columns are unknown — deduce them from content. Typical fragments include a city/location, a device model (e.g. 770, 270, H20N), and an issue note, in any order, with any separator.

Workflow:
1. Read the row and form a short search phrase. Skip separators, numbers that look like IDs only, and empty fields.
2. Call `search_database` with `table`='order' using the phrase to find matching RMA/repair records.
3. If no strong order match, call `search_database` with `table`='document' to look for a support ticket.
4. Return ONLY a single strict JSON object (no markdown, no prose) with these keys, using null when unknown:
   { "original_line": "<the raw row>",
     "matched_order_number": string|null,
     "matched_customer":    string|null,
     "device_model":        string|null,
     "issue_notes":         string|null,
     "confidence_score":    number between 0 and 1 }

Confidence scoring: 0.9+ only when both the order number AND customer name match; 0.6–0.8 when one strong field matches; below 0.4 if you're guessing from weak cues. Do not call more than 2 tools per row."#;

/// The CSV-enrichment system prompt with the tenant's product-domain phrase
/// spliced in (env `ECK_TENANT_BRAND`/`ECK_TENANT_VERTICAL`; neutral when unset).
fn enrich_system_prompt() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        ENRICH_SYSTEM_PROMPT_TEMPLATE
            .replace("{{PRODUCT_DOMAIN}}", &crate::ai::branding::product_domain_phrase())
    })
}

const ENRICH_MAX_LINES: usize = 200;

fn parse_enriched(raw: &str, original: &str) -> Value {
    // Gemini often wraps JSON in a ```json fence. Strip it defensively,
    // then try to locate the first `{...}` block. If parsing fails, return
    // a shell with the raw LLM text so the operator can still audit.
    let trimmed = raw.trim();
    let stripped = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = stripped.find('{');
    let end = stripped.rfind('}');
    if let (Some(s), Some(e)) = (start, end) {
        if e >= s {
            if let Ok(v) = serde_json::from_str::<Value>(&stripped[s..=e]) {
                return v;
            }
        }
    }
    json!({
        "original_line": original,
        "matched_order_number": null,
        "matched_customer": null,
        "device_model": null,
        "issue_notes": null,
        "confidence_score": 0.0,
        "parse_error": true,
        "raw_response": raw,
    })
}

/// POST /api/ai/enrich-csv — multipart CSV upload → AI-extracted rows.
///
/// WHY: Operators arrive with headerless CSVs dumped from 3rd-party
/// systems (couriers, field techs) that don't conform to any known schema.
/// Instead of asking them to reshape the file, we feed each raw line to a
/// rig-core Gemini agent with `search_database` wired in, and let the LLM
/// do the column inference + record matching in one pass.
///
/// Contract:
///   * Multipart field `file` (required) holds the `.csv` bytes.
///   * Empty lines are skipped; the first `ENRICH_MAX_LINES` non-empty rows
///     are processed sequentially (to stay under Gemini per-minute quotas).
///   * Response is `{ "results": [enriched_row, ...] }` — each element is
///     the strict JSON object the agent returned, or a `parse_error`
///     shell if it failed to produce valid JSON.
pub async fn enrich_csv(
    State(_state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let http = reqwest::Client::new();
    let auth = eck_core::ai::AiAuth::resolve(&http)
        .await
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, format!("AI token mint failed: {e}")))?;
    if !auth.is_configured() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("AI auth not configured ({} mode)", auth.mode()),
        ));
    }
    // Enrichment is a decision task (DB lookups + reasoning) → use Flash, not Lite.
    let model = std::env::var("GEMINI_ENRICH_MODEL")
        .or_else(|_| std::env::var("GEMINI_GENERATION_MODEL"))
        .unwrap_or_else(|_| "gemini-3.5-flash".to_string());

    let mut file_bytes: Option<Vec<u8>> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (StatusCode::BAD_REQUEST, format!("Multipart error: {}", e))
    })? {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let bytes = field.bytes().await.map_err(|e| {
                (StatusCode::BAD_REQUEST, format!("Read error: {}", e))
            })?;
            file_bytes = Some(bytes.to_vec());
            break;
        } else {
            let _ = field.bytes().await;
        }
    }

    let bytes = file_bytes
        .ok_or((StatusCode::BAD_REQUEST, "Missing 'file' field".into()))?;
    let text = String::from_utf8_lossy(&bytes);

    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}').to_string())
        .filter(|l| !l.is_empty())
        .take(ENRICH_MAX_LINES)
        .collect();

    if lines.is_empty() {
        return Ok(Json(json!({
            "results": [],
            "note": "CSV contained no non-empty rows",
        })));
    }

    // Build one rig-core client and reuse it across rows. `studio` uses the
    // gemini provider (AI Studio `?key=`); `managed` uses the openai provider
    // pointed at Vertex's OpenAI-compatible endpoint with a Bearer (§9, Path A).
    let results = match &auth {
        eck_core::ai::AiAuth::Studio { api_key } => {
            let client = gemini::Client::new(api_key)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("gemini client: {e}")))?;
            enrich_rows(client, &model, &lines, &_state.db).await
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
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("vertex client: {e}")))?;
            let vmodel = if model.starts_with("google/") { model.clone() } else { format!("google/{model}") };
            enrich_rows(client, &vmodel, &lines, &_state.db).await
        }
        // CSV enrichment resolves the default auth, never a routed OR task — this
        // arm exists only for exhaustiveness after the OpenRouter variant landed.
        eck_core::ai::AiAuth::OpenRouter { .. } => {
            return Err((
                StatusCode::BAD_REQUEST,
                "OpenRouter auth is not supported by CSV enrichment".to_string(),
            ));
        }
    };

    Ok(Json(json!({ "results": results })))
}

/// Run the CSV-enrichment agent over each row. Generic over the rig provider
/// client so `studio` (gemini) and `managed` (openai-on-Vertex) share one body.
async fn enrich_rows<C>(
    client: C,
    model: &str,
    lines: &[String],
    db: &eck_core::db::SurrealDb,
) -> Vec<Value>
where
    C: CompletionClient,
    C::CompletionModel: CompletionModel,
{
    let mut results: Vec<Value> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let agent = client
            .agent(model)
            .preamble(enrich_system_prompt())
            .tool(SearchDatabaseTool { db: db.clone(), fuzzy: None })
            .build();

        let user_prompt = format!("Raw CSV row:\n{}", line);

        // rig-core 0.33 defaults `max_turns=0`: the agent gets exactly one
        // LLM call and blows up with MaxTurnError the moment it tries to
        // emit a follow-up completion after a tool result. We want the
        // sequence {tool_call → tool_result → final JSON}, and optionally
        // a second search (order→document fallback), so 4 turns is safe.
        match agent.prompt(user_prompt).max_turns(4).await {
            Ok(response) => {
                debug!(
                    "[enrich_csv] row {}/{} -> {} chars",
                    i + 1,
                    lines.len(),
                    response.len()
                );
                results.push(parse_enriched(&response, line));
            }
            Err(e) => {
                warn!("[enrich_csv] row {} failed: {}", i + 1, e);
                results.push(json!({
                    "original_line": line,
                    "matched_order_number": null,
                    "matched_customer": null,
                    "device_model": null,
                    "issue_notes": null,
                    "confidence_score": 0.0,
                    "error": e.to_string(),
                }));
            }
        }
    }
    results
}

// ─── Brain Chat ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct ChatRequest {
    pub question: String,
    /// The VERBATIM tail of the conversation (full text, no per-turn truncation).
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    #[serde(default)]
    pub engine: Option<String>,
    /// The compacted running summary the client is carrying (from a prior
    /// `compaction` response), replayed as the first brain turn.
    #[serde(default)]
    pub summary: Option<String>,
    /// The transcript-telemetry session id (minted by the server on the first
    /// turn; the client persists it and echoes it back).
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Decoded chat request pieces shared by the blocking and streaming handlers.
struct ChatDecoded {
    question: String,
    tier: crate::mcp::McpTier,
    engine: crate::ai::orchestrator::BrainEngine,
    engine_label: &'static str,
    summary: Option<String>,
    history: Vec<(String, String)>,
    session_id: Option<String>,
    user: String,
}

/// Shared request decode: tier (admin=Master else Agent), product-default engine
/// (Glm unless the request asks for gemini), the verbatim history tail as
/// (role,text) pairs, the carried summary, and the session id.
fn chat_prelude(claims: &Claims, req: &ChatRequest) -> ApiResult<ChatDecoded> {
    let question = req.question.trim();
    if question.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "question must not be empty".into()));
    }

    // admin → Master (clear PII, every tool); a read-only kiosk/exhibition
    // observer → Observer (masked PII, NO side-effecting tools such as
    // printing — nobody is signed in, so the chat may read but never act);
    // everyone else signed in → Agent.
    let tier = match claims.role.as_str() {
        "admin" => crate::mcp::McpTier::Master,
        "observer" => crate::mcp::McpTier::Observer,
        _ => crate::mcp::McpTier::Agent,
    };

    let (engine, engine_label) = match req.engine.as_deref() {
        Some("gemini") => (crate::ai::orchestrator::BrainEngine::Gemini, "gemini"),
        _ => (crate::ai::orchestrator::BrainEngine::Glm, "glm"),
    };

    let history: Vec<(String, String)> = req
        .history
        .iter()
        .map(|t| (t.role.clone(), t.text.clone()))
        .collect();

    Ok(ChatDecoded {
        question: question.to_string(),
        tier,
        engine,
        engine_label,
        summary: req.summary.clone().filter(|s| !s.trim().is_empty()),
        history,
        session_id: req.session_id.clone(),
        user: claims.sub.clone(),
    })
}

/// Persist the session + turn telemetry, returning the (possibly minted)
/// session id. Best-effort — never fails the chat.
#[allow(clippy::too_many_arguments)]
async fn log_chat_turn(
    state: &Arc<AppState>,
    d: &ChatDecoded,
    client: &str,
    answer: &crate::ai::orchestrator::BrainAnswer,
    latency_ms: i64,
    compaction: Option<&crate::ai::orchestrator::ChatCompaction>,
) -> String {
    let tier_label = if d.tier.reveal_pii() { "master" } else { "agent" };
    let session_id = crate::ai::chat_log::ensure_session(
        &state.db,
        d.session_id.as_deref(),
        &d.user,
        tier_label,
        d.engine_label,
        client,
        &d.question,
    )
    .await;
    let idx = crate::ai::chat_log::next_idx(&state.db, &session_id).await;
    crate::ai::chat_log::record_turn(
        &state.db,
        &session_id,
        idx,
        &d.question,
        answer,
        latency_ms,
        compaction,
        None,
    )
    .await;
    session_id
}

/// Assemble the terminal response payload (shared by blocking + streaming done).
fn chat_done_payload(
    answer: &crate::ai::orchestrator::BrainAnswer,
    session_id: &str,
    compaction: Option<&crate::ai::orchestrator::ChatCompaction>,
    scrubbed_answer: &str,
) -> Value {
    let mut out = json!({
        "answer": scrubbed_answer,
        "model": answer.model,
        "usage": answer.usage.to_json(),
        "session_id": session_id,
    });
    if answer.hedged {
        out["hedged"] = json!(true);
    }
    if let Some(c) = compaction {
        out["compaction"] = json!({ "summary": c.summary, "dropped_turns": c.dropped_turns });
    }
    out
}

/// POST /api/ai/chat — free-form dashboard chat over the cache-friendly
/// multi-turn brain with rolling compaction. Per-request engine override
/// (glm/gemini); the dashboard defaults to `Glm` (product default per the owner).
pub async fn chat(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChatRequest>,
) -> ApiResult<Json<Value>> {
    let d = chat_prelude(&claims, &req)?;
    let started = std::time::Instant::now();

    let result = crate::ai::orchestrator::answer_chat(
        &state,
        &d.question,
        d.summary.clone(),
        d.history.clone(),
        d.tier,
        d.engine,
        None,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let latency_ms = started.elapsed().as_millis() as i64;

    let session_id = log_chat_turn(
        &state,
        &d,
        "dashboard",
        &result.answer,
        latency_ms,
        result.compaction.as_ref(),
    )
    .await;

    let scrubbed = if !d.tier.reveal_pii() {
        eck_core::utils::anonymizer::scrub_pii_regex(&result.answer.answer).0
    } else {
        result.answer.answer.clone()
    };
    let out = chat_done_payload(&result.answer, &session_id, result.compaction.as_ref(), &scrubbed);
    Ok(Json(out))
}

/// POST /api/ai/chat/stream — same body/tier/engine/history logic as `chat`,
/// but streams brain loop PROGRESS over SSE instead of blocking for the final
/// answer. True token streaming is out of scope (the brain is a multi-turn tool
/// loop); we surface `turn` (loop progress) and `tool` (each function call)
/// events, then a terminal `done` (final answer + model) or `error`. The blocking
/// `POST /api/ai/chat` stays as the non-streaming fallback.
pub async fn chat_stream(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChatRequest>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>> {
    use crate::ai::orchestrator::BrainEvent;
    use tokio_stream::StreamExt;

    let d = chat_prelude(&claims, &req)?;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<BrainEvent>();

    // Run the brain off-task; progress (turn/tool/hedge) arrives on `rx` as it
    // happens. The terminal `done` payload (answer + usage + session_id +
    // compaction) is assembled here once the run resolves, so the stream carries
    // the SAME shape as the blocking endpoint.
    let run_state = state.clone();
    let run_tx = tx.clone();
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let res = crate::ai::orchestrator::answer_chat(
            &run_state,
            &d.question,
            d.summary.clone(),
            d.history.clone(),
            d.tier,
            d.engine,
            Some(run_tx.clone()),
        )
        .await;
        match res {
            Ok(result) => {
                let latency_ms = started.elapsed().as_millis() as i64;
                let session_id = log_chat_turn(
                    &run_state,
                    &d,
                    "dashboard",
                    &result.answer,
                    latency_ms,
                    result.compaction.as_ref(),
                )
                .await;
                let scrubbed = if !d.tier.reveal_pii() {
                    eck_core::utils::anonymizer::scrub_pii_regex(&result.answer.answer).0
                } else {
                    result.answer.answer.clone()
                };
                let payload = chat_done_payload(
                    &result.answer,
                    &session_id,
                    result.compaction.as_ref(),
                    &scrubbed,
                );
                let _ = run_tx.send(BrainEvent::Done(payload));
            }
            Err(e) => {
                let _ = run_tx.send(BrainEvent::Error(e.to_string()));
            }
        }
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|ev| {
        let event = match ev {
            BrainEvent::Turn { turn, max } => Event::default()
                .event("turn")
                .json_data(json!({ "turn": turn, "max": max })),
            BrainEvent::Tool { name, args_preview } => Event::default()
                .event("tool")
                .json_data(json!({ "name": name, "args_preview": args_preview })),
            BrainEvent::Hedge { from, to, after_secs } => Event::default()
                .event("hedge")
                .json_data(json!({ "from": from, "to": to, "after_secs": after_secs })),
            BrainEvent::Done(payload) => Event::default().event("done").json_data(payload),
            BrainEvent::Error(msg) => Event::default()
                .event("error")
                .json_data(json!({ "error": msg })),
        };
        // json_data only fails if the value can't serialize — these are plain
        // objects, so fall back to a bare error event rather than propagate.
        Ok(event.unwrap_or_else(|_| Event::default().event("error").data("serialize error")))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))))
}

// ─── Bench: brain tool-loop benchmark (dev-gated, admin) ──────────────────────

#[derive(Deserialize)]
pub struct BenchBrainRequest {
    /// The public ticket number OR the `ai_task` record id to triage.
    pub ticket_id: String,
    /// Engine route: `"gemini"` / `"gemini:<model>"` (managed Vertex),
    /// `"openrouter:<model>"` (openrouter.ai via ECK_BENCH_OR_*),
    /// `"vertex-openai:<model>"` (a Vertex MaaS OpenAI model), or any other
    /// string (e.g. `"glm-5.3-flash"`) → the node's Z.ai/BYOK OpenRouter host.
    pub route: String,
    #[serde(default)]
    pub max_turns: Option<usize>,
    /// Replay the ticket as triage saw it AT INTAKE, not as it looks now: force
    /// `meta.status = "Open"` and drop the closure/assignee fields so the model
    /// can't read the outcome off the context (default true). Set false to feed
    /// the stored context verbatim.
    #[serde(default = "default_true")]
    pub intake_view: bool,
    /// Optional explicit context override (already intake-shaped by the caller);
    /// when present it is used verbatim instead of looking up the ai_task.
    #[serde(default)]
    pub context: Option<Value>,
    /// Override `ECK_BRAIN_IMAGE_PASSTHROUGH` for THIS run: when set, the actual
    /// attachment page image is shown to the answering model (in addition to the
    /// Gemini-vision text) after each `analyze_attachment_visual`/`ocr_attachment`
    /// call. Omitted → the node's env default.
    #[serde(default)]
    pub image_passthrough: Option<bool>,
}

fn default_true() -> bool {
    true
}

/// Reshape a stored `ai_task.context` into the INTAKE view: force
/// `meta.status = "Open"` and strip the fields that only exist post-resolution
/// (`assignee_*`, closure timestamps), so a bench replay does not leak the human
/// outcome to the model. Everything else (subject, device, serial, company,
/// description, masked identity fields) is preserved for `scrub_context_for_prompt`.
fn intake_shape_context(mut ctx: Value) -> Value {
    if let Some(meta) = ctx.get_mut("meta").and_then(|m| m.as_object_mut()) {
        meta.insert("status".to_string(), json!("Open"));
        for k in ["assignee_id", "assignee_name", "closed_time", "resolution"] {
            meta.remove(k);
        }
    }
    ctx
}

/// POST /api/ai/bench/brain — run ONE production brain triage of one ticket over
/// an explicit model route and return the full transcript telemetry.
///
/// DEV-GATED: returns 404 unless `ECK_BRAIN_BENCH=1`, so the endpoint does not
/// exist in normal deployments. Admin-only (router `require_admin` layer). It
/// reuses the EXACT orchestrator triage loop (`orchestrator::bench_run_triage`)
/// — same system prompt, `build_user_prompt`, and five tools real tasks use —
/// only pinning the engine and capturing per-turn/per-tool telemetry the wire
/// discards. See `.eck/BRAIN_LOOP_BENCH.md`.
pub async fn bench_brain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BenchBrainRequest>,
) -> ApiResult<Json<Value>> {
    if std::env::var("ECK_BRAIN_BENCH").ok().as_deref() != Some("1") {
        return Err((StatusCode::NOT_FOUND, "not found".into()));
    }
    let tid = req.ticket_id.trim().to_string();
    if tid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "ticket_id must not be empty".into()));
    }

    // Resolve the ticket's `ai_task.context` — match on the public ticket number
    // (context.meta.ticket_number) OR the ai_task record id. Newest task wins.
    let rows: Vec<Value> = state
        .db
        .query(
            "SELECT context, created_at FROM ai_task \
             WHERE context.meta.ticket_number = $tid OR record::id(id) = $tid \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(("tid", tid.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let looked_up = rows
        .into_iter()
        .next()
        .and_then(|r| r.get("context").cloned());

    // Explicit override wins; else the looked-up context, intake-shaped by default
    // so the model can't read the resolution off `meta.status`.
    let context = match req.context.clone() {
        Some(c) => c,
        None => {
            let c = looked_up.ok_or((
                StatusCode::NOT_FOUND,
                format!("no ai_task found for ticket_id '{tid}'"),
            ))?;
            if req.intake_view { intake_shape_context(c) } else { c }
        }
    };

    // Echo whether (masked) customer text is present in the context the model
    // will receive — a proxy for "did the description reach the prompt".
    let desc_chars = context
        .get("meta")
        .and_then(|m| m.get("description"))
        .and_then(|d| d.as_str())
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let ctx_status = context
        .get("meta")
        .and_then(|m| m.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();

    let max_turns = req.max_turns.unwrap_or(6).clamp(1, 12);
    // Per-run override of the node's ECK_BRAIN_IMAGE_PASSTHROUGH default.
    let image_passthrough = req
        .image_passthrough
        .unwrap_or_else(crate::ai::orchestrator::image_passthrough_env);
    let mut out =
        crate::ai::orchestrator::bench_run_triage(&state, context, &req.route, max_turns, image_passthrough)
            .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(o) = out.as_object_mut() {
        o.insert("context_description_chars".to_string(), json!(desc_chars));
        o.insert("context_status".to_string(), json!(ctx_status));
        o.insert("intake_view".to_string(), json!(req.intake_view));
    }
    Ok(Json(out))
}

// ─── Admin: chat transcript telemetry (require_admin) ─────────────────────────

#[derive(Deserialize)]
pub struct ChatSessionsQuery {
    pub limit: Option<usize>,
    pub user: Option<String>,
}

/// GET /api/admin/chat-sessions — list sessions with per-session aggregates.
pub async fn admin_chat_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ChatSessionsQuery>,
) -> ApiResult<Json<Value>> {
    let sessions =
        crate::ai::chat_log::list_sessions(&state.db, q.limit.unwrap_or(50), q.user.as_deref())
            .await
            .map_err(db_err)?;
    Ok(Json(json!({ "sessions": sessions })))
}

/// GET /api/admin/chat-sessions/:id — one session with its ordered turns.
pub async fn admin_chat_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    match crate::ai::chat_log::get_session(&state.db, &id).await.map_err(db_err)? {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, "session not found".into())),
    }
}

/// GET /api/admin/chat-sessions/:id/export.md — markdown transcript.
pub async fn admin_chat_session_export(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    match crate::ai::chat_log::export_markdown(&state.db, &id).await.map_err(db_err)? {
        Some(md) => Ok((
            [(axum::http::header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            md,
        )
            .into_response()),
        None => Err((StatusCode::NOT_FOUND, "session not found".into())),
    }
}

/// GET /api/admin/chat-stats — per-model aggregate stats.
pub async fn admin_chat_stats(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let stats = crate::ai::chat_log::stats(&state.db).await.map_err(db_err)?;
    Ok(Json(stats))
}

/// DELETE /api/admin/chat-sessions/:id — drop a session and its turns.
pub async fn admin_chat_session_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    crate::ai::chat_log::delete_session(&state.db, &id).await.map_err(db_err)?;
    Ok(Json(json!({ "status": "deleted", "session_id": id })))
}
