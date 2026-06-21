use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{error, info, warn};

use super::telemetry::{evaluate_budget, BudgetLevel};
use crate::AppState;

// Phase 1 (deterministic loop detection) runs every 5 minutes — it's free SQL
// and needs to catch runaway loops fast. Phase 2 (AI-based analysis) still runs
// every hour since it costs Gemini tokens. Ratio = 12 Phase-1 ticks per Phase-2.
const PHASE1_INTERVAL_SECS: u64 = 300;
const PHASE2_EVERY_N_TICKS: u64 = 12;
// Per-entity threshold within the 15-minute rolling window.
const LOOP_THRESHOLD: i64 = 5;
const LOOP_WINDOW: &str = "15m";

const SYSTEM_PROMPT: &str = r#"You are the AI Security & Operations Observer for eckWMS.
Analyze the provided system logs (API telemetry, sync history, and document statuses) for the last 24 hours.
Look specifically for:
1. Infinite loops (e.g., the same document ID repeating excessively in telemetry).
2. Huge spikes in API token usage (e.g., >20k tokens for a single routine process).
3. Persistent crash loops in sync history.

If everything is normal, respond with {"status": "ok"}.
If you detect an anomaly, respond with ONLY a valid JSON object matching this schema:
{
  "status": "anomaly",
  "severity": "critical",
  "title": "Short descriptive title of the issue",
  "message": "Detailed explanation of the anomaly meant for the admin.",
  "tags": ["tag1", "tag2"]
}"#;

pub async fn start_observer_worker(state: Arc<AppState>) {
    // Initial delay
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    info!(
        "[Observer] AI System Observer started (Phase 1 every {}s, Phase 2 every {}s).",
        PHASE1_INTERVAL_SECS,
        PHASE1_INTERVAL_SECS * PHASE2_EVERY_N_TICKS,
    );

    let http = HttpClient::new();
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(PHASE1_INTERVAL_SECS));
    let mut tick: u64 = 0;

    loop {
        interval.tick().await;
        tick = tick.wrapping_add(1);

        // ── Phase 0: Evaluate token budget and update circuit breaker ──
        // Runs on every tick (5min) so throttle reacts fast to spending spikes.
        match evaluate_budget(&state.db).await {
            Ok((level, hourly, daily)) => {
                if level >= BudgetLevel::Warn {
                    let tier_name = format!("{level}");
                    let msg = format!(
                        "Token budget level: **{tier_name}**\n\n\
                         - Hourly: {}K tokens\n- Daily: {}K tokens\n\n\
                         {}",
                        hourly / 1000, daily / 1000,
                        match level {
                            BudgetLevel::Throttle => "AI workers rate-limited to 1 call/min.",
                            BudgetLevel::Halt => "All AI workers STOPPED.",
                            _ => "Monitoring closely.",
                        }
                    );
                    let severity = if level == BudgetLevel::Halt { "critical" } else { "warning" };
                    save_and_broadcast_alert(&state, severity, &format!("Token Budget: {tier_name}"), &msg, level == BudgetLevel::Halt).await;
                }
            }
            Err(e) => error!("[Observer] Budget evaluation failed: {}", e),
        }

        // ── Phase 1: Deterministic loop detection (no AI, no tokens) ──
        // Every tick (5min) — catches runaway loops within one 15min window.
        let hard_mitigated = match run_deterministic_checks(&state).await {
            Ok(m) => m,
            Err(e) => {
                error!("[Observer] Deterministic check failed: {}", e);
                false
            }
        };

        // ── Phase 2: AI-based analysis — costs Gemini tokens, hourly only ──
        let run_phase2 = tick % PHASE2_EVERY_N_TICKS == 0 && !hard_mitigated;
        if run_phase2 {
            if let Err(e) = run_ai_analysis(&state, &http).await {
                error!("[Observer] AI analysis cycle failed: {}", e);
            }
        }
    }
}

// ── Phase 1: Deterministic checks (free, instant, reliable) ────────────────

async fn run_deterministic_checks(state: &Arc<AppState>) -> anyhow::Result<bool> {
    let mut mitigated = false;

    // 1. Detect looping entity_ids in the recent telemetry window.
    let loops: Vec<Value> = state.db
        .query(&format!(
            "SELECT * FROM (\
                SELECT entity_id, module, count() AS calls, math::sum(total_tokens) AS tokens \
                FROM ai_telemetry \
                WHERE timestamp > time::now() - {LOOP_WINDOW} \
                GROUP BY entity_id, module\
             ) WHERE calls > {LOOP_THRESHOLD} \
             ORDER BY calls DESC \
             LIMIT 20"
        ))
        .await?
        .take(0)?;

    if !loops.is_empty() {
        let mut loop_details = Vec::new();
        let mut mitigated_ids = Vec::new();

        for entry in &loops {
            let entity = entry.get("entity_id").and_then(|v| v.as_str()).unwrap_or("?");
            let module = entry.get("module").and_then(|v| v.as_str()).unwrap_or("?");
            let calls = entry.get("calls").and_then(|v| v.as_i64()).unwrap_or(0);
            let tokens = entry.get("tokens").and_then(|v| v.as_i64()).unwrap_or(0);

            warn!(
                "[Observer] LOOP DETECTED: {entity} in {module} — {calls} calls, {tokens} tokens in {LOOP_WINDOW}"
            );
            loop_details.push(format!(
                "- `{entity}` ({module}): {calls} calls, {}K tokens",
                tokens / 1000
            ));

            // Auto-mitigate: break the loop by moving the offending entity
            // into a terminal state. Dispatch by entity type — ai_task loops
            // (flagged by the `orchestrator` module) must flip the task
            // itself; everything else is still a document-summarization /
            // embedding loop.
            let clean_id = entity.split(':').last().unwrap_or(entity).trim_matches('`');
            let is_ai_task = entity.starts_with("ai_task:") || module == "orchestrator";
            let result = if is_ai_task {
                state.db
                    .query(
                        "UPDATE ai_task SET \
                             state = 'failed', updated_at = time::now() \
                         WHERE record::id(id) = $id \
                         RETURN record::id(id) AS id"
                    )
                    .bind(("id", clean_id.to_string()))
                    .await
            } else {
                state.db
                    .query(
                        "UPDATE document SET \
                         summary_status = IF summary_status = 'pending' THEN 'failed' ELSE summary_status END, \
                         embedding_status = IF embedding_status = 'pending' THEN 'failed' ELSE embedding_status END, \
                         summary_retries = 99, embedding_retries = 99 \
                         WHERE record::id(id) = $id RETURN record::id(id) AS id"
                    )
                    .bind(("id", clean_id.to_string()))
                    .await
            };

            // Only report mitigation success if the UPDATE actually matched rows
            // (the pre-fix version blindly claimed victory on empty result sets,
            // which is how looping docs slipped past observer on 2026-04-21).
            match result {
                Ok(mut resp) => {
                    let updated: Vec<Value> = resp.take(0).unwrap_or_default();
                    if updated.is_empty() {
                        warn!("[Observer] Mitigation UPDATE matched 0 rows for {clean_id} — ID may be stored in an unexpected format");
                    } else {
                        mitigated_ids.push(clean_id.to_string());
                    }
                }
                Err(e) => warn!("[Observer] Failed to mitigate {clean_id}: {e}"),
            }
        }

        if !mitigated_ids.is_empty() {
            mitigated = true;
            let msg = format!(
                "Deterministic loop detection triggered.\n\n## Looping entities (last {LOOP_WINDOW})\n{}\n\n\
                 **Auto-mitigation:** {} document(s) set to 'failed' to break the loop.\n\
                 IDs: {}",
                loop_details.join("\n"),
                mitigated_ids.len(),
                mitigated_ids.join(", ")
            );

            warn!("[Observer] {}", msg);
            save_and_broadcast_alert(state, "critical", "Infinite Loop Auto-Mitigated", &msg, true).await;
        }
    }

    // 2. Detect stuck pending documents that have been pending for >2 hours with retries
    let stuck: Vec<Value> = state.db
        .query(
            "SELECT record::id(id) AS id, summary_status, embedding_status, summary_retries, embedding_retries \
             FROM document \
             WHERE (summary_status = 'pending' OR embedding_status = 'pending') \
             AND updated_at IS NOT NONE \
             AND updated_at < time::now() - 2h \
             AND ((summary_retries IS NOT NONE AND summary_retries >= 3) OR (embedding_retries IS NOT NONE AND embedding_retries >= 3)) \
             LIMIT 50"
        )
        .await?
        .take(0)?;

    if !stuck.is_empty() {
        warn!("[Observer] {} documents stuck in pending with high retry count — marking as failed", stuck.len());

        state.db
            .query(
                "UPDATE document SET \
                 summary_status = IF summary_status = 'pending' AND summary_retries >= 3 THEN 'failed' ELSE summary_status END, \
                 embedding_status = IF embedding_status = 'pending' AND embedding_retries >= 3 THEN 'failed' ELSE embedding_status END \
                 WHERE (summary_status = 'pending' OR embedding_status = 'pending') \
                 AND updated_at IS NOT NONE \
                 AND updated_at < time::now() - 2h \
                 AND ((summary_retries IS NOT NONE AND summary_retries >= 3) OR (embedding_retries IS NOT NONE AND embedding_retries >= 3)) \
                 RETURN NONE"
            )
            .await?;

        let msg = format!(
            "{} document(s) stuck in pending with >=3 retries for >2 hours. Marked as 'failed'.",
            stuck.len()
        );
        save_and_broadcast_alert(state, "warning", "Stuck Documents Auto-Cleaned", &msg, true).await;
        mitigated = true;
    }

    // 3. Telemetry hygiene: prune old telemetry records (keep last 7 days)
    let _: Result<Vec<Value>, _> = state.db
        .query("DELETE FROM ai_telemetry WHERE timestamp < time::now() - 7d RETURN NONE")
        .await
        .and_then(|mut r| r.take(0));

    // 4. Orchestrator hygiene: prune ai_thought older than 30d (GoBD retention
    //    window — hashes are sealed on Hedera for forensic audit if needed).
    let _: Result<Vec<Value>, _> = state.db
        .query("DELETE FROM ai_thought WHERE created_at < time::now() - 30d RETURN NONE")
        .await
        .and_then(|mut r| r.take(0));

    Ok(mitigated)
}

// ── Phase 2: AI-based analysis ─────────────────────────────────────────────

async fn run_ai_analysis(state: &Arc<AppState>, http: &HttpClient) -> anyhow::Result<()> {
    let auth = eck_core::ai::AiAuth::resolve(http).await?;
    if !auth.is_configured() {
        return Ok(());
    }

    let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
        .unwrap_or_else(|_| "gemini-3.1-flash-lite".to_string());

    // Gather recent logs — window matches Phase 2 cadence (hourly). A wider
    // 24h window caused stale-data alerts for already-mitigated loops after
    // the 2026-04-21 incident: Gemini re-flagged docs from that afternoon for
    // the entire next day until the window rolled past. Phase 1 covers the
    // short horizon; Phase 2 is second-opinion on what's happening *now*.
    let telemetry: Vec<Value> = state
        .db
        .query("SELECT * FROM ai_telemetry WHERE timestamp > time::now() - 1h LIMIT 100")
        .await?
        .take(0)?;
    let sync_errors: Vec<Value> = state
        .db
        .query("SELECT * FROM sync_history WHERE status = 'error' AND started_at > time::now() - 1h LIMIT 50")
        .await?
        .take(0)?;
    let pending_docs: Vec<Value> = state
        .db
        .query("SELECT record::id(id) as id, embedding_status, summary_status FROM document WHERE embedding_status = 'pending' OR summary_status = 'pending' LIMIT 50")
        .await?
        .take(0)?;

    // Skip if system is completely idle to save tokens
    if telemetry.is_empty() && sync_errors.is_empty() && pending_docs.is_empty() {
        return Ok(());
    }

    let report_data = json!({
        "telemetry_sample": telemetry,
        "sync_errors": sync_errors,
        "stuck_documents": pending_docs
    });

    let payload_str = serde_json::to_string(&report_data).unwrap_or_default();

    // Call Gemini (Studio key or managed Vertex, per AiAuth)
    let payload = json!({
        "systemInstruction": { "parts": [{ "text": SYSTEM_PROMPT }] },
        "contents": [{ "parts": [{ "text": format!("System Logs:\n{}", payload_str) }] }],
        "generationConfig": { "responseMimeType": "application/json", "temperature": 0.1 }
    });

    let (response_text, _usage) = auth.generate_content(http, &gen_model, payload).await?;

    let analysis: Value =
        serde_json::from_str(&response_text).unwrap_or_else(|_| json!({"status": "ok"}));

    // Handle Anomaly
    if analysis.get("status").and_then(|v| v.as_str()) == Some("anomaly") {
        let severity = analysis
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("warning");
        let title = analysis
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("System Anomaly Detected");
        let message = analysis
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Check system logs.");

        warn!("[Observer] AI anomaly detected: {} - {}", title, message);

        // Auto-mitigation must not fire on a healthy backlog. After a
        // bulk scraper import 900+ tickets sit in 'pending' legitimately
        // and the Gemini-rate-limited workers need tens of minutes to
        // grind through them. Gate mitigation on two conditions:
        //   (a) pending docs are *stale* (updated_at > 30m old) — fresh
        //       pendings are almost certainly being actively processed.
        //   (b) workers have made *zero* progress in the last 15m.
        // Only then pause, and pause ONLY the stale pendings.
        let progress_rows: Vec<Value> = state.db
            .query(
                "SELECT count() AS n FROM document \
                 WHERE (summary_status = 'completed' OR embedding_status = 'complete') \
                 AND updated_at > time::now() - 15m \
                 GROUP ALL",
            )
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        let recent_progress = progress_rows
            .first()
            .and_then(|r| r.get("n"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        let stale_rows: Vec<Value> = state.db
            .query(
                "SELECT count() AS n FROM document \
                 WHERE (summary_status = 'pending' OR embedding_status = 'pending') \
                 AND (updated_at IS NONE OR updated_at < time::now() - 30m) \
                 GROUP ALL",
            )
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        let stale_pending = stale_rows
            .first()
            .and_then(|r| r.get("n"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        let mitigated = severity == "critical" && recent_progress == 0 && stale_pending >= 10;
        if mitigated {
            let _ = state.db
                .query(
                    "UPDATE document SET \
                        embedding_status = IF embedding_status = 'pending' THEN 'paused_by_observer' ELSE embedding_status END, \
                        summary_status = IF summary_status = 'pending' THEN 'paused_by_observer' ELSE summary_status END \
                     WHERE (summary_status = 'pending' OR embedding_status = 'pending') \
                     AND (updated_at IS NONE OR updated_at < time::now() - 30m) \
                     RETURN NONE",
                )
                .await;
            warn!(
                "[Observer] AUTO-MITIGATION: Paused {} stale pending documents (no worker progress in 15m)",
                stale_pending
            );
        } else if severity == "critical" && pending_docs.len() >= 10 {
            info!(
                "[Observer] Skipping mitigation: {} pending docs but {} completed in last 15m (workers alive, backlog draining)",
                pending_docs.len(), recent_progress
            );
        }

        // Suppress stale-data alarms: Gemini often flags already-mitigated loops
        // that still appear inside the telemetry window. If mitigation didn't fire
        // AND workers are actively making progress, the system is healthy —
        // log for the ops channel but don't wake a human or poison the alert feed.
        let workers_alive = recent_progress > 0;
        let should_alert = mitigated || !workers_alive;
        if !should_alert {
            info!(
                "[Observer] Suppressing stale AI anomaly: {} completions in last 15m, {} stale pending — treating as healthy",
                recent_progress, stale_pending
            );
            return Ok(());
        }

        let full_msg = if mitigated {
            format!("{}\n\n[Auto-mitigation applied: {} stale pending documents paused by Observer]", message, stale_pending)
        } else {
            message.to_string()
        };

        save_and_broadcast_alert(state, severity, title, &full_msg, mitigated).await;

        // Send to xelth.com Universal Telemetry API
        let tags: Vec<String> = analysis
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|i| i.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let xelth_payload = json!({
            "app_id": "eckWMS",
            "app_version": env!("CARGO_PKG_VERSION"),
            "instance_id": state.instance_id,
            "event_type": "ai_anomaly",
            "severity": severity,
            "title": title,
            "details": { "raw_ai_analysis": analysis, "telemetry_count": telemetry.len() },
            "tags": tags
        });

        let _ = http
            .post("")
            .json(&xelth_payload)
            .send()
            .await;
    } else {
        info!("[Observer] AI analysis complete. System status: OK.");
    }

    Ok(())
}

// ── Shared helpers ─────────────────────────────────────────────────────────

async fn save_and_broadcast_alert(state: &Arc<AppState>, severity: &str, title: &str, message: &str, mitigated: bool) {
    let now = chrono::Utc::now().to_rfc3339();

    // Save to SurrealDB
    let alert = json!({
        "title": title,
        "message": message,
        "severity": severity,
        "status": "unread",
        "mitigated": mitigated,
        "created_at": &now,
        "reported_to_cloud": true
    });

    if let Err(e) = state.db.create::<Option<Value>>("system_alert").content(alert).await {
        error!("[Observer] Failed to save alert: {}", e);
    }

    // Broadcast to UI via WebSocket
    let ws_msg = json!({
        "type": "SYSTEM_ALERT",
        "title": title,
        "message": message,
        "severity": severity,
        "timestamp": &now
    });

    let _ = state.ws_tx.send(serde_json::to_string(&ws_msg).unwrap());
}
