use axum::{
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
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
                   context, awaiting_input_schema, created_at, updated_at \
                   FROM ai_task"
        .to_string();

    if q.state.is_some() {
        sql.push_str(" WHERE state = $state");
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

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "status": "queued", "task_id": rid })),
    ))
}

// ─── CSV Enrichment ──────────────────────────────────────────────────────────

const ENRICH_SYSTEM_PROMPT: &str = r#"You are a data enrichment assistant for eckWMS (an RMA/repair WMS for InBody medical devices).

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
            .preamble(ENRICH_SYSTEM_PROMPT)
            .tool(SearchDatabaseTool { db: db.clone() })
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
