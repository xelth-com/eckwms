use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use super::embeddings::embed_query;
use super::telemetry::{current_budget_level, log_telemetry, BudgetLevel, THROTTLE_DELAY_SECS};
use crate::AppState;

const OPTIMIZER_INTERVAL_SECS: u64 = 3600; // hourly
const BATCH_LIMIT: usize = 5;
const RATE_LIMIT_MS: u64 = 1000;

const SYSTEM_PROMPT_TEMPLATE: &str = r#"You are the SOP (Standard Operating Procedure) Optimizer for eckWMS — a Rust-based WMS/ERP for {{PRODUCT_DOMAIN}}.
You analyze a single completed task that required human intervention and distill it into a reusable, generalized SOP the Orchestrator can apply to similar future tasks via retrieval.

STRICT OUTPUT RULES:
- Respond with ONLY a valid minified JSON object — no prose, no code fences, no explanation.
- Schema: {"title": "<short label, <=80 chars>", "trigger_context": "<abstract description of WHEN this SOP should fire, in <=400 chars>", "rule": "<the generalized procedure the Orchestrator should follow, <=1200 chars>", "outcome": "good" | "bad" | "unclear", "outcome_reason": "<one short sentence explaining the outcome judgment>"}
- The "rule" MUST be a generalized, reusable procedure — not a retelling of the specific incident.
- The "trigger_context" MUST describe the class of situation (e.g. "QC report is missing a serial number"), not the specific instance.
- The "outcome" judges whether the brain's own plan/result was validated by the human intervention: "good" if the human confirmed or simply used the brain's plan/output; "bad" if the human contradicted, corrected, or had to redo the brain's work; "unclear" if the bundle doesn't let you tell.

PII SAFETY — HARD REQUIREMENT:
- NEVER include specific names, email addresses, phone numbers, street addresses, customer identifiers, ticket numbers, order numbers, or device serial numbers in any field.
- Replace any concrete identifier with an abstract placeholder ("the customer", "the device", "the QC report").
- If the only way to describe the situation is via specific PII, return {"title":"SKIP","trigger_context":"SKIP","rule":"SKIP","outcome":"unclear","outcome_reason":"PII"} and nothing else.

If the interaction is not generalizable (one-off, trivial, or dominated by PII), also return the SKIP sentinel."#;

/// SOP optimizer system prompt with the tenant's product-domain phrase spliced
/// in (env `ECK_TENANT_BRAND`/`ECK_TENANT_VERTICAL`; neutral when unset).
fn system_prompt_text() -> &'static str {
    static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        SYSTEM_PROMPT_TEMPLATE.replace("{{PRODUCT_DOMAIN}}", &crate::ai::branding::product_domain_phrase())
    })
}

pub async fn start_optimizer_worker(state: Arc<AppState>) {
    // Stagger startup so the optimizer doesn't compete with observer/orchestrator
    // during the initial bring-up burst.
    tokio::time::sleep(Duration::from_secs(120)).await;
    info!(
        "[Optimizer] SOP Optimizer started ({}s interval)",
        OPTIMIZER_INTERVAL_SECS
    );

    let http = HttpClient::new();
    let mut interval = tokio::time::interval(Duration::from_secs(OPTIMIZER_INTERVAL_SECS));

    loop {
        interval.tick().await;

        if let Err(e) = run_hygiene(&state).await {
            warn!("[Optimizer] Hygiene pass failed: {}", e);
        }

        match current_budget_level() {
            BudgetLevel::Halt => {
                debug!("[Optimizer] Budget HALT — skipping extraction cycle");
                continue;
            }
            BudgetLevel::Throttle => {
                debug!("[Optimizer] Budget THROTTLE — sleeping before extraction");
                tokio::time::sleep(Duration::from_secs(THROTTLE_DELAY_SECS)).await;
            }
            _ => {}
        }

        if let Err(e) = run_extraction_cycle(&state, &http).await {
            error!("[Optimizer] Extraction cycle failed: {}", e);
        }
    }
}

// ── Hygiene: deprecate old unused SOPs ─────────────────────────────────────

async fn run_hygiene(state: &Arc<AppState>) -> anyhow::Result<()> {
    let _: Vec<Value> = state
        .db
        .query(
            "UPDATE ai_sop SET deprecated = true, updated_at = time::now() \
             WHERE usage_count < 3 \
             AND created_at < time::now() - 30d \
             AND deprecated = false \
             RETURN NONE",
        )
        .await?
        .take(0)
        .unwrap_or_default();

    // Failure-rate deprecation: once an SOP has been tried enough times to
    // have a stable signal (>=5 usages) and more than half of those runs
    // ended in `failure`, it is actively harming the orchestrator — retire
    // it. The `usage_count >= 5` floor makes the division safe.
    let _: Vec<Value> = state
        .db
        .query(
            "UPDATE ai_sop SET deprecated = true, updated_at = time::now() \
             WHERE deprecated = false \
             AND usage_count >= 5 \
             AND (failure_count * 2) > usage_count \
             RETURN NONE",
        )
        .await?
        .take(0)
        .unwrap_or_default();
    Ok(())
}

// ── Extraction: find completed tasks that had human intervention ───────────

async fn run_extraction_cycle(state: &Arc<AppState>, http: &HttpClient) -> anyhow::Result<()> {
    let auth = eck_core::ai::AiAuth::resolve(http).await?;
    if !auth.is_configured() {
        return Ok(());
    }

    let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
        .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string());

    // Find recent ai_inbox replies whose parent task is completed and not yet
    // analyzed. We scope to the last 24h to cap cost per cycle.
    let candidates: Vec<Value> = state
        .db
        .query(
            "SELECT task_id FROM ai_inbox \
             WHERE created_at > time::now() - 24h \
             GROUP BY task_id \
             LIMIT 200",
        )
        .await?
        .take(0)?;

    let mut processed = 0usize;

    for entry in candidates {
        if processed >= BATCH_LIMIT {
            break;
        }

        let task_rid = match entry.get("task_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        // Fetch task (must be completed and not sop_analyzed)
        let tasks: Vec<Value> = state
            .db
            .query(
                "SELECT record::id(id) AS id, state, context, sop_analyzed, result, error \
                 FROM type::record($rid)",
            )
            .bind(("rid", task_rid.clone()))
            .await?
            .take(0)?;

        let task = match tasks.into_iter().next() {
            Some(t) => t,
            None => continue,
        };

        if task.get("state").and_then(|v| v.as_str()) != Some("completed") {
            continue;
        }
        if task.get("sop_analyzed").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }

        // Hard pre-gate before spending any tokens: a task with no result (or
        // an explicit error) tells us nothing usable about the brain's plan.
        let result_is_empty = match task.get("result") {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Object(m)) => m.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            _ => false,
        };
        let has_error = !matches!(task.get("error"), None | Some(Value::Null));
        if result_is_empty || has_error {
            info!(
                "[Optimizer] Task {} has no usable result (empty={} error={}); marking unclear",
                task_rid, result_is_empty, has_error
            );
            let _ = state
                .db
                .query(
                    "UPDATE type::record($rid) SET sop_analyzed = true, \
                     sop_analyzed_at = time::now(), sop_outcome = 'unclear', \
                     sop_outcome_reason = 'no result'",
                )
                .bind(("rid", task_rid.clone()))
                .await;
            continue;
        }

        if let Err(e) =
            process_task(state, http, &auth, &gen_model, &task_rid, &task).await
        {
            warn!("[Optimizer] Failed to process {}: {}", task_rid, e);
            // Mark analyzed anyway so we don't loop on a broken task.
            let _ = state
                .db
                .query(
                    "UPDATE type::record($rid) SET sop_analyzed = true, \
                     sop_analyzed_at = time::now()",
                )
                .bind(("rid", task_rid.clone()))
                .await;
        }

        processed += 1;
        tokio::time::sleep(Duration::from_millis(RATE_LIMIT_MS)).await;
    }

    if processed > 0 {
        info!("[Optimizer] Processed {processed} completed task(s) this cycle");
    }
    Ok(())
}

async fn process_task(
    state: &Arc<AppState>,
    http: &HttpClient,
    auth: &eck_core::ai::AiAuth,
    gen_model: &str,
    task_rid: &str,
    task: &Value,
) -> anyhow::Result<()> {
    // Gather thought history and human replies
    let thoughts: Vec<Value> = state
        .db
        .query(
            "SELECT iteration, phase, payload, created_at FROM ai_thought \
             WHERE task_id = $tid ORDER BY created_at ASC",
        )
        .bind(("tid", task_rid.to_string()))
        .await?
        .take(0)?;

    let inbox: Vec<Value> = state
        .db
        .query(
            "SELECT source, content, created_at FROM ai_inbox \
             WHERE task_id = $tid ORDER BY created_at ASC",
        )
        .bind(("tid", task_rid.to_string()))
        .await?
        .take(0)?;

    if inbox.is_empty() {
        // No human intervention — nothing to generalize.
        let _ = state
            .db
            .query(
                "UPDATE type::record($rid) SET sop_analyzed = true, \
                 sop_analyzed_at = time::now()",
            )
            .bind(("rid", task_rid.to_string()))
            .await;
        return Ok(());
    }

    let bundle = json!({
        "task_id": task_rid,
        "context": task.get("context").cloned().unwrap_or(Value::Null),
        "thoughts": thoughts,
        "human_replies": inbox,
    });

    let bundle_str = serde_json::to_string(&bundle).unwrap_or_default();
    let user_prompt = format!(
        "Analyze the following completed task and distill one reusable SOP. \
         Respond with the strict JSON schema only.\n\n## Task bundle\n```json\n{bundle_str}\n```"
    );

    let payload = json!({
        "systemInstruction": { "parts": [{ "text": system_prompt_text() }] },
        "contents": [{ "parts": [{ "text": user_prompt }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "temperature": 0.1,
            "maxOutputTokens": 2048,
        }
    });
    let _ = &auth; // resolved above only to gate on is_configured()
    let gen =
        eck_core::ai::AiAuth::generate_for_task(http, "optimizer", gen_model, payload, None).await?;
    tracing::info!("[ai] task=optimizer path={} model={}", gen.path, gen.model);
    let (response_text, served_model, usage) = (gen.text, gen.model, gen.usage);

    log_telemetry(&state.db, "optimizer", &served_model, task_rid, &usage).await;

    let parsed: Value = serde_json::from_str(&response_text).map_err(|e| {
        anyhow::anyhow!("Optimizer LLM returned non-JSON: {e} (body: {response_text})")
    })?;

    let title = parsed.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let trigger_context = parsed
        .get("trigger_context")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let rule = parsed.get("rule").and_then(|v| v.as_str()).unwrap_or("");
    let outcome = parsed
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("unclear");
    let outcome_reason = parsed
        .get("outcome_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Mark the task as analyzed regardless of outcome — we don't want to burn
    // tokens on it again.
    let _ = state
        .db
        .query(
            "UPDATE type::record($rid) SET sop_analyzed = true, \
             sop_analyzed_at = time::now(), sop_outcome = $outcome, \
             sop_outcome_reason = $reason",
        )
        .bind(("rid", task_rid.to_string()))
        .bind(("outcome", outcome.to_string()))
        .bind(("reason", outcome_reason.to_string()))
        .await;

    // Best-effort seed of the learning-loop feedback table regardless of
    // whether we end up inserting an SOP.
    record_feedback(state, task_rid, outcome, outcome_reason).await;

    if title == "SKIP" || trigger_context == "SKIP" || rule == "SKIP" {
        debug!("[Optimizer] Task {} skipped by LLM (not generalizable)", task_rid);
        return Ok(());
    }
    if title.is_empty() || trigger_context.is_empty() || rule.is_empty() {
        warn!(
            "[Optimizer] Task {} produced empty SOP fields; skipping insert",
            task_rid
        );
        return Ok(());
    }

    // Good-outcome gate: only a confirmed-good brain plan is worth
    // generalizing into an SOP. A `bad` outcome means the human had to
    // correct the brain — distilling that into a "rule" would teach the
    // orchestrator the wrong lesson. `unclear` means we simply don't know.
    if outcome != "good" {
        info!(
            "[Optimizer] Task {} outcome={} ({}); not inserting SOP",
            task_rid, outcome, outcome_reason
        );
        return Ok(());
    }

    // Embed the abstract trigger_context. The LLM is prompted to exclude PII,
    // but `embed_query` also runs its own anonymization pass as a safety net.
    let embedding = match embed_query(trigger_context).await {
        Ok(v) => v,
        Err(e) => {
            warn!("[Optimizer] Failed to embed trigger_context for {task_rid}: {e}");
            return Ok(());
        }
    };

    // De-dup by embedding distance against existing (non-deprecated) SOPs.
    // Same HNSW-backed KNN syntax the orchestrator uses for retrieval.
    let dedup_rows: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, title, \
                vector::similarity::cosine(embedding, $emb) AS sim \
             FROM ai_sop WHERE deprecated = false AND embedding <|3, 100|> $emb \
             LIMIT 3",
        )
        .bind(("emb", embedding.clone()))
        .await?
        .take(0)?;

    let top = dedup_rows.first();
    let top_sim = top.and_then(|r| r.get("sim")).and_then(|v| v.as_f64());
    let threshold = sop_dedup_threshold();

    if dedup_verdict(top_sim, threshold) {
        if let Some(row) = top {
            let dup_id = row.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let dup_title = row.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let _ = state
                .db
                .query(
                    "UPDATE type::record($rid) SET duplicate_hits += 1, \
                     updated_at = time::now()",
                )
                .bind(("rid", dup_id.to_string()))
                .await;
            info!(
                "[Optimizer] SOP \"{}\" deduped against {} (sim={:.4})",
                dup_title,
                dup_id,
                top_sim.unwrap_or_default()
            );
        }
        return Ok(());
    }

    let inserted: Vec<Value> = state
        .db
        .query(
            "INSERT INTO ai_sop { \
                title: $t, \
                trigger_context: $tc, \
                rule: $r, \
                embedding: $emb, \
                success_count: 0, \
                failure_count: 0, \
                usage_count: 0, \
                duplicate_hits: 0, \
                deprecated: false, \
                source_task_id: $src, \
                created_at: time::now(), \
                updated_at: time::now() \
             } RETURN record::id(id) AS id",
        )
        .bind(("t", title.to_string()))
        .bind(("tc", trigger_context.to_string()))
        .bind(("r", rule.to_string()))
        .bind(("emb", embedding))
        .bind(("src", task_rid.to_string()))
        .await?
        .take(0)?;

    if let Some(sop) = inserted.first() {
        info!(
            "[Optimizer] Created SOP {} from task {} — \"{}\"",
            sop.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
            task_rid,
            title
        );
    }

    Ok(())
}

/// Best-effort seed row for the learning loop's `ai_feedback` table. The
/// table has no schema yet — a plain schemaless INSERT is fine.
async fn record_feedback(state: &Arc<AppState>, task_rid: &str, outcome: &str, reason: &str) {
    let res = state
        .db
        .query(
            "INSERT INTO ai_feedback { \
                task_id: $tid, \
                source: 'optimizer', \
                outcome: $outcome, \
                reason: $reason, \
                created_at: time::now() \
             } RETURN NONE",
        )
        .bind(("tid", task_rid.to_string()))
        .bind(("outcome", outcome.to_string()))
        .bind(("reason", reason.to_string()))
        .await;
    if let Err(e) = res {
        warn!("[Optimizer] Failed to record ai_feedback for {task_rid}: {e}");
    }
}

/// Env-configurable cosine-similarity threshold for SOP de-dup
/// (`ECK_SOP_DEDUP_COS`, default 0.92). Any unset/unparsable value falls
/// back to the default.
fn sop_dedup_threshold() -> f64 {
    parse_dedup_threshold(std::env::var("ECK_SOP_DEDUP_COS").ok().as_deref())
}

/// Pure parsing helper for `ECK_SOP_DEDUP_COS` — unset or unparsable falls
/// back to 0.92. Split out from `sop_dedup_threshold` so it's testable
/// without touching process env vars.
fn parse_dedup_threshold(raw: Option<&str>) -> f64 {
    raw.and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.92)
}

/// Pure de-dup decision: true means "treat as a duplicate, don't insert".
fn dedup_verdict(top_sim: Option<f64>, threshold: f64) -> bool {
    match top_sim {
        Some(sim) => sim >= threshold,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_verdict_none_is_not_duplicate() {
        assert!(!dedup_verdict(None, 0.92));
    }

    #[test]
    fn dedup_verdict_below_threshold_is_not_duplicate() {
        assert!(!dedup_verdict(Some(0.91), 0.92));
    }

    #[test]
    fn dedup_verdict_at_threshold_is_duplicate() {
        assert!(dedup_verdict(Some(0.92), 0.92));
    }

    #[test]
    fn dedup_verdict_above_threshold_is_duplicate() {
        assert!(dedup_verdict(Some(0.99), 0.92));
    }

    #[test]
    fn parse_dedup_threshold_unset_defaults() {
        assert_eq!(parse_dedup_threshold(None), 0.92);
    }

    #[test]
    fn parse_dedup_threshold_garbage_defaults() {
        assert_eq!(parse_dedup_threshold(Some("not-a-number")), 0.92);
        assert_eq!(parse_dedup_threshold(Some("")), 0.92);
    }

    #[test]
    fn parse_dedup_threshold_valid_value_parses() {
        assert_eq!(parse_dedup_threshold(Some("0.85")), 0.85);
    }

    #[test]
    fn bad_outcome_parses_with_no_insert_path() {
        // Simulates the LLM response parse in process_task: a "bad" outcome
        // must be distinguishable from "good" so the good-outcome gate can
        // route it away from the ai_sop insert.
        let parsed: Value = serde_json::from_str(
            r#"{"title":"Some SOP","trigger_context":"ctx","rule":"do X","outcome":"bad","outcome_reason":"human corrected the plan"}"#,
        )
        .unwrap();
        let outcome = parsed.get("outcome").and_then(|v| v.as_str()).unwrap_or("unclear");
        assert_eq!(outcome, "bad");
        assert_ne!(outcome, "good");
    }
}

