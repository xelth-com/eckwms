//! Node-local Brain-Chat transcript telemetry (owner: "we will take real
//! conversations and analyze them by hand").
//!
//! Two schemaless, **node-local** tables — deliberately NOT in
//! `SYNC_ENTITY_TYPES`, so they never mesh: master-tier answers contain clear
//! customer names, and these rows stay on the node that served the chat,
//! reachable only through the admin API / Master MCP surface.
//!
//! * `chat_session` — one per conversation: `{ created_at, user, tier, engine,
//!   client, title }`.
//! * `chat_turn` — one per question/answer round: the Q/A plus the per-run
//!   metrics (path, hedged, retries, brain_turns, tools, token counts incl.
//!   cache hits, cost, latency) and any `compaction` that fired that turn.
//!
//! The markdown export ([`export_markdown`]) is what the owner's analyst reads;
//! the same text backs the `chat_transcript` MCP tool.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::ai::orchestrator::{BrainAnswer, ChatCompaction};
use eck_core::db::SurrealDb;

/// Mint a session id when the client didn't carry one, and upsert the
/// `chat_session` row (first-seen wins for the immutable header fields; `title`
/// is set from the first question, <=80 chars). Returns the session id to hand
/// back to the client so it persists it alongside the history.
pub async fn ensure_session(
    db: &SurrealDb,
    session_id: Option<&str>,
    user: &str,
    tier: &str,
    engine: &str,
    client: &str,
    first_question: &str,
) -> String {
    let sid = session_id
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("cs-{}", Uuid::new_v4()));

    let title: String = first_question.chars().take(80).collect();
    // Create only if absent (idempotent): a repeat turn keeps the original
    // header + title. Two-arg `type::record(table, id)` (SurrealDB v3) so a
    // hyphenated id (cs-<uuid>) is treated as a STRING id — the single-arg
    // `type::record("chat_session:cs-…")` form truncates the id at the hyphen.
    let res = db
        .query(
            "LET $existing = (SELECT id FROM type::record('chat_session', $id)); \
             IF array::len($existing) == 0 THEN \
                 CREATE type::record('chat_session', $id) SET \
                     created_at = time::now(), user = $user, tier = $tier, \
                     engine = $engine, client = $client, title = $title \
             END",
        )
        .bind(("id", sid.clone()))
        .bind(("user", user.to_string()))
        .bind(("tier", tier.to_string()))
        .bind(("engine", engine.to_string()))
        .bind(("client", client.to_string()))
        .bind(("title", title))
        .await
        .and_then(|mut r| r.check());
    if let Err(e) = res {
        tracing::warn!("[chat-log] ensure_session failed for {sid}: {e}");
    }
    sid
}

/// Persist one chat turn. Best-effort: a telemetry write must never fail the
/// user's chat, so errors are logged and swallowed.
#[allow(clippy::too_many_arguments)]
pub async fn record_turn(
    db: &SurrealDb,
    session_id: &str,
    idx: i64,
    question: &str,
    answer: &BrainAnswer,
    latency_ms: i64,
    compaction: Option<&ChatCompaction>,
    error: Option<&str>,
) {
    let u = &answer.usage;
    let tools: Vec<Value> = u
        .tools
        .iter()
        .map(|(name, args)| json!({ "name": name, "args_preview": args }))
        .collect();
    let compaction_json = compaction.map(|c| {
        json!({
            "summary": c.summary,
            "dropped_turns": c.dropped_turns,
            "chunk_text_len": c.chunk_text_len,
        })
    });

    let res = db
        .query(
            "CREATE chat_turn SET \
                 session = type::record('chat_session', $sid), idx = $idx, ts = time::now(), \
                 question = $question, answer = $answer, model = $model, \
                 path = $path, hedged = $hedged, retries = $retries, \
                 brain_turns = $brain_turns, tools_called = $tools, \
                 prompt_tokens = $prompt, cached_tokens = $cached, \
                 completion_tokens = $completion, cost_usd = $cost, \
                 cache_ratio = $ratio, latency_ms = $latency, \
                 compaction = $compaction, error = $error",
        )
        .bind(("sid", session_id.to_string()))
        .bind(("idx", idx))
        .bind(("question", question.to_string()))
        .bind(("answer", answer.answer.clone()))
        .bind(("model", answer.model.clone()))
        .bind(("path", answer.path.to_string()))
        .bind(("hedged", answer.hedged))
        .bind(("retries", u.retries as i64))
        .bind(("brain_turns", u.turns as i64))
        .bind(("tools", tools))
        .bind(("prompt", u.prompt_tokens))
        .bind(("cached", u.cached_tokens))
        .bind(("completion", u.completion_tokens))
        .bind(("cost", u.cost_usd))
        .bind(("ratio", (u.cache_ratio() * 1000.0).round() / 1000.0))
        .bind(("latency", latency_ms))
        .bind(("compaction", compaction_json))
        .bind(("error", error.map(|s| s.to_string())))
        .await
        .and_then(|mut r| r.check());
    if let Err(e) = res {
        tracing::warn!("[chat-log] record_turn failed for {session_id}: {e}");
    }
}

/// The next 0-based turn index for a session (count of existing turns).
pub async fn next_idx(db: &SurrealDb, session_id: &str) -> i64 {
    let rows: Result<Vec<Value>, _> = db
        .query("SELECT count() AS n FROM chat_turn WHERE session = type::record('chat_session', $sid) GROUP ALL")
        .bind(("sid", session_id.to_string()))
        .await
        .and_then(|mut r| r.take(0));
    rows.ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|v| v.get("n").and_then(|n| n.as_i64()))
        .unwrap_or(0)
}

/// List sessions with per-session aggregates (turns, avg cache ratio, total
/// cost, compactions), newest first. Optional `user` filter.
pub async fn list_sessions(
    db: &SurrealDb,
    limit: usize,
    user: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let limit = limit.clamp(1, 500);
    let mut sql = "SELECT record::id(id) AS id, created_at, user, tier, engine, client, title \
                   FROM chat_session"
        .to_string();
    if user.is_some() {
        sql.push_str(" WHERE user = $user");
    }
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {limit}"));

    let mut q = db.query(&sql);
    if let Some(u) = user {
        q = q.bind(("user", u.to_string()));
    }
    let mut sessions: Vec<Value> = q.await?.take(0)?;

    for s in sessions.iter_mut() {
        let sid = s.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let agg = session_aggregates(db, &sid).await;
        if let Some(obj) = s.as_object_mut() {
            for (k, v) in agg.as_object().cloned().unwrap_or_default() {
                obj.insert(k, v);
            }
        }
    }
    Ok(sessions)
}

async fn session_aggregates(db: &SurrealDb, session_id: &str) -> Value {
    let rows: Vec<Value> = db
        .query(
            "SELECT count() AS turns, math::sum(cost_usd) AS total_cost, \
                    math::mean(cache_ratio) AS avg_cache_ratio, \
                    math::sum(prompt_tokens) AS prompt_tokens, \
                    math::sum(cached_tokens) AS cached_tokens \
             FROM chat_turn WHERE session = type::record('chat_session', $sid) GROUP ALL",
        )
        .bind(("sid", session_id.to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let comp: Vec<Value> = db
        .query(
            "SELECT count() AS n FROM chat_turn \
             WHERE session = type::record('chat_session', $sid) AND compaction IS NOT NONE GROUP ALL",
        )
        .bind(("sid", session_id.to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let f = |rows: &[Value], k: &str| -> f64 {
        rows.first()
            .and_then(|v| v.get(k))
            .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
            .unwrap_or(0.0)
    };
    json!({
        "turns": f(&rows, "turns") as i64,
        "total_cost_usd": (f(&rows, "total_cost") * 1e6).round() / 1e6,
        "avg_cache_ratio": (f(&rows, "avg_cache_ratio") * 1000.0).round() / 1000.0,
        "compactions": f(&comp, "n") as i64,
    })
}

/// A single session with its ordered turns (JSON), for the admin detail view.
pub async fn get_session(db: &SurrealDb, session_id: &str) -> anyhow::Result<Option<Value>> {
    let sess: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, created_at, user, tier, engine, client, title \
             FROM type::record('chat_session', $sid)",
        )
        .bind(("sid", session_id.to_string()))
        .await?
        .take(0)?;
    let Some(mut session) = sess.into_iter().next() else {
        return Ok(None);
    };
    let turns: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, idx, ts, question, answer, model, path, hedged, \
                    retries, brain_turns, tools_called, prompt_tokens, cached_tokens, \
                    completion_tokens, cost_usd, cache_ratio, latency_ms, compaction, error \
             FROM chat_turn WHERE session = type::record('chat_session', $sid) ORDER BY idx ASC",
        )
        .bind(("sid", session_id.to_string()))
        .await?
        .take(0)?;
    if let Some(obj) = session.as_object_mut() {
        obj.insert("turns".to_string(), json!(turns));
    }
    Ok(Some(session))
}

/// Markdown transcript of a session — the artifact the owner's analyst reads.
/// Per turn: the Q/A, a metrics line, and any compaction block (incl. the
/// summary text produced that turn).
pub async fn export_markdown(db: &SurrealDb, session_id: &str) -> anyhow::Result<Option<String>> {
    let Some(session) = get_session(db, session_id).await? else {
        return Ok(None);
    };
    Ok(Some(render_markdown(&session)))
}

/// Pure markdown renderer for a session Value (`{ header…, turns: [...] }`).
/// Split out so the export shape is unit-testable without a DB.
pub fn render_markdown(session: &Value) -> String {
    let g = |k: &str| session.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let mut out = String::new();
    out.push_str(&format!("# Chat transcript — {}\n\n", g("title")));
    out.push_str(&format!(
        "- session: `{}`\n- user: {}\n- tier: {}\n- engine: {}\n- client: {}\n- created: {}\n\n",
        g("id"),
        g("user"),
        g("tier"),
        g("engine"),
        g("client"),
        g("created_at"),
    ));

    let empty = vec![];
    let turns = session.get("turns").and_then(|v| v.as_array()).unwrap_or(&empty);
    for turn in turns {
        let idx = turn.get("idx").and_then(|v| v.as_i64()).unwrap_or(0);
        out.push_str(&format!("## Turn {}\n\n", idx + 1));

        if let Some(comp) = turn.get("compaction").filter(|v| !v.is_null()) {
            let dropped = comp.get("dropped_turns").and_then(|v| v.as_i64()).unwrap_or(0);
            let summary = comp.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!(
                "> **Compaction:** folded {dropped} oldest round(s) into the running summary.\n>\n> {}\n\n",
                summary.replace('\n', "\n> ")
            ));
        }

        out.push_str(&format!(
            "**Q:** {}\n\n**A:** {}\n\n",
            turn.get("question").and_then(|v| v.as_str()).unwrap_or(""),
            turn.get("answer").and_then(|v| v.as_str()).unwrap_or(""),
        ));

        let ni = |k: &str| turn.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let nf = |k: &str| turn.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let tools: Vec<String> = turn
            .get("tools_called")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "_metrics: model={} path={} hedged={} retries={} brain_turns={} tools=[{}] \
             prompt={} cached={} ({}%) completion={} cost_usd={:.6} latency_ms={}_\n\n",
            turn.get("model").and_then(|v| v.as_str()).unwrap_or("?"),
            turn.get("path").and_then(|v| v.as_str()).unwrap_or("?"),
            turn.get("hedged").and_then(|v| v.as_bool()).unwrap_or(false),
            ni("retries"),
            ni("brain_turns"),
            tools.join(", "),
            ni("prompt_tokens"),
            ni("cached_tokens"),
            (nf("cache_ratio") * 100.0).round() as i64,
            ni("completion_tokens"),
            nf("cost_usd"),
            ni("latency_ms"),
        ));
        if let Some(err) = turn.get("error").and_then(|v| v.as_str()) {
            out.push_str(&format!("_error: {err}_\n\n"));
        }
    }
    out
}

/// Per-model aggregate stats across all sessions: sessions, turns, cache hit
/// ratio, p50/p95 latency, avg cost per turn, compaction count.
pub async fn stats(db: &SurrealDb) -> anyhow::Result<Value> {
    // Pull the raw per-turn columns and aggregate in Rust: SurrealDB's
    // `math::percentile` is not a GROUP-BY aggregate (it maps per row → nulls),
    // so p50/p95 are computed here. chat_turn is node-local + retention-capped,
    // so a full scan is cheap.
    let rows: Vec<Value> = db
        .query(
            "SELECT model, latency_ms, cost_usd, prompt_tokens, cached_tokens, \
                    (compaction IS NOT NONE) AS compacted \
             FROM chat_turn",
        )
        .await?
        .take(0)?;

    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Acc {
        turns: i64,
        prompt: i64,
        cached: i64,
        cost_sum: f64,
        cost_n: i64,
        compactions: i64,
        latencies: Vec<i64>,
    }
    let mut by_model: BTreeMap<String, Acc> = BTreeMap::new();
    for r in &rows {
        let model = r.get("model").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let a = by_model.entry(model).or_default();
        a.turns += 1;
        a.prompt += r.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        a.cached += r.get("cached_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        if let Some(c) = r.get("cost_usd").and_then(|v| v.as_f64()) {
            a.cost_sum += c;
            a.cost_n += 1;
        }
        if r.get("compacted").and_then(|v| v.as_bool()).unwrap_or(false) {
            a.compactions += 1;
        }
        a.latencies.push(r.get("latency_ms").and_then(|v| v.as_i64()).unwrap_or(0));
    }

    let pct = |sorted: &[i64], p: f64| -> i64 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    };

    let mut models: Vec<Value> = Vec::new();
    for (model, mut a) in by_model {
        a.latencies.sort_unstable();
        let hit = if a.prompt > 0 {
            (a.cached as f64 / a.prompt as f64 * 1000.0).round() / 1000.0
        } else {
            0.0
        };
        let avg_cost = if a.cost_n > 0 { a.cost_sum / a.cost_n as f64 } else { 0.0 };
        models.push(json!({
            "model": model,
            "turns": a.turns,
            "cache_hit_ratio": hit,
            "p50_latency_ms": pct(&a.latencies, 50.0),
            "p95_latency_ms": pct(&a.latencies, 95.0),
            "avg_cost_per_turn_usd": (avg_cost * 1e6).round() / 1e6,
            "compactions": a.compactions,
        }));
    }
    let sessions: Vec<Value> = db
        .query("SELECT count() AS n FROM chat_session GROUP ALL")
        .await?
        .take(0)?;
    let session_count = sessions
        .first()
        .and_then(|v| v.get("n"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Ok(json!({ "sessions": session_count, "per_model": models }))
}

/// Delete a session and its turns.
pub async fn delete_session(db: &SurrealDb, session_id: &str) -> anyhow::Result<()> {
    db.query(
        "DELETE chat_turn WHERE session = type::record('chat_session', $sid); \
         DELETE type::record('chat_session', $sid)",
    )
    .bind(("sid", session_id.to_string()))
    .await?
    .check()?;
    Ok(())
}

/// Retention sweep (owner §9): drop sessions + turns older than `days`. Called
/// from the hourly maintenance loop. `days <= 0` disables (returns 0).
pub async fn sweep_retention(db: &SurrealDb, days: i64) -> anyhow::Result<u64> {
    if days <= 0 {
        return Ok(0);
    }
    let dur = format!("{}d", days);
    // Delete turns of stale sessions first, then the sessions. The tables may
    // not exist yet on a node that has never served a chat — treat that as a
    // no-op rather than an error (the hourly sweep would otherwise warn forever).
    let res = db
        .query(
            "DELETE chat_turn WHERE session.created_at < time::now() - type::duration($d); \
             DELETE chat_session WHERE created_at < time::now() - type::duration($d)",
        )
        .bind(("d", dur))
        .await
        .and_then(|mut r| r.check());
    if let Err(e) = res {
        let msg = e.to_string();
        if !msg.contains("does not exist") {
            return Err(e.into());
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::render_markdown;
    use serde_json::json;

    #[test]
    fn markdown_export_shape() {
        let session = json!({
            "id": "cs-1",
            "title": "ZetaBody 770 firmware",
            "user": "dimi",
            "tier": "master",
            "engine": "glm",
            "client": "dashboard",
            "created_at": "2026-09-02T10:00:00Z",
            "turns": [
                {
                    "idx": 0,
                    "question": "What firmware is on serial X?",
                    "answer": "Digital 3.1.",
                    "model": "z-ai/glm-5.3-flash",
                    "path": "or",
                    "hedged": false,
                    "retries": 0,
                    "brain_turns": 2,
                    "tools_called": [{ "name": "device_history", "args_preview": "{\"serial\":\"X\"}" }],
                    "prompt_tokens": 2000,
                    "cached_tokens": 1700,
                    "completion_tokens": 40,
                    "cost_usd": 0.00012,
                    "cache_ratio": 0.85,
                    "latency_ms": 1234,
                    "compaction": null,
                    "error": null
                },
                {
                    "idx": 1,
                    "question": "And the analog?",
                    "answer": "Analog 1.0.",
                    "model": "z-ai/glm-5.3-flash",
                    "path": "or",
                    "hedged": false,
                    "retries": 0,
                    "brain_turns": 1,
                    "tools_called": [],
                    "prompt_tokens": 2100,
                    "cached_tokens": 2000,
                    "completion_tokens": 20,
                    "cost_usd": 0.00009,
                    "cache_ratio": 0.952,
                    "latency_ms": 800,
                    "compaction": { "summary": "Discussed firmware of serial X.", "dropped_turns": 6, "chunk_text_len": 900 },
                    "error": null
                }
            ]
        });
        let md = render_markdown(&session);
        // Header + both turns present.
        assert!(md.contains("# Chat transcript — ZetaBody 770 firmware"));
        assert!(md.contains("- user: dimi"));
        assert!(md.contains("## Turn 1"));
        assert!(md.contains("## Turn 2"));
        // Q/A rendered.
        assert!(md.contains("**Q:** What firmware is on serial X?"));
        assert!(md.contains("**A:** Digital 3.1."));
        // Metrics line with cache pct and tools.
        assert!(md.contains("cached=1700 (85%)"));
        assert!(md.contains("tools=[device_history]"));
        // Compaction block with the summary text.
        assert!(md.contains("**Compaction:** folded 6 oldest round(s)"));
        assert!(md.contains("Discussed firmware of serial X."));
    }
}
