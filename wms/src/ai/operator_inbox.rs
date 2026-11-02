//! Shared Operator Inbox filing.
//!
//! The Operator Inbox shown on the kiosk chat page ("Agenten-Posteingang") is
//! the set of `ai_task` rows in state `awaiting_human` (listed by
//! `handlers::ai::list_tasks?state=awaiting_human`). The orchestrator's
//! LIVE SELECT / poll only ever claims tasks in state `ready`/`resumed`, so an
//! item filed here in `awaiting_human` is a passive, human-facing note: it is
//! NEVER auto-executed. (A human reply via `POST /api/ai/tasks/:id/reply` flips
//! it to `resumed`, which would hand it to the ReAct executor — but applying a
//! confirmed action is out of scope in v1; see TECH_DEBT.)
//!
//! Two callers share this: the OCR-verify worker (`ai::ocr_verify`) and the
//! PII-leak self-report pipeline (`ai::summarization`). Both only ever file
//! PII-SAFE content. Idempotency is by a caller-supplied `dedup_key`: filing
//! the same key twice is a no-op (returns `false`).

use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use tracing::warn;

/// One Operator-Inbox item to file. `context` must be PII-safe — it is stored
/// verbatim on the `ai_task` and shown to any operator who can open the inbox.
pub struct InboxItem {
    /// Where the item came from (`ocr_verify`, `pii_leak`, …). Stored on
    /// `context.source` for filtering.
    pub source: String,
    /// Idempotency key — one `ai_task` per key. For OCR-verify this is the
    /// unit id + the review's `input_hash`, so a re-review of the same evidence
    /// never files a second item.
    pub dedup_key: String,
    /// The human-facing question/prompt (stored on `awaiting_input_schema`).
    pub question: String,
    /// PII-SAFE context object. `meta.status` is forced to `"open"` so the row
    /// passes `list_tasks`' `context.meta.status != 'Closed'` filter.
    pub context: Value,
}

/// File one Operator-Inbox item (an `awaiting_human` `ai_task`), idempotently by
/// `item.dedup_key`. Returns `true` when a new row was created, `false` when an
/// item with that key already exists (or on a soft DB error — best-effort, never
/// fails the caller). Mirrors the `ai_task` seed shape in `services::support`.
pub async fn file(db: &SurrealDb, instance_id: &str, item: InboxItem) -> bool {
    // Dedup: one row per dedup_key.
    let existing: Result<Option<Value>, _> = async {
        db.query(
            "SELECT record::id(id) AS id FROM ai_task \
             WHERE context.dedup_key = $k LIMIT 1",
        )
        .bind(("k", item.dedup_key.clone()))
        .await?
        .take(0)
    }
    .await;
    match existing {
        Ok(Some(_)) => return false, // already filed
        Ok(None) => {}
        Err(e) => {
            warn!("[operator-inbox] dedup query failed: {e}");
            return false;
        }
    }

    // Force the inbox-visibility invariants onto the stored context.
    let mut context = item.context;
    if !context.is_object() {
        context = json!({});
    }
    context["source"] = json!(item.source);
    context["dedup_key"] = json!(item.dedup_key);
    let meta = context
        .get("meta")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut meta = meta;
    meta["status"] = json!("open");
    context["meta"] = meta;

    let res = db
        .query(
            "INSERT INTO ai_task { \
                state: 'awaiting_human', \
                owner_instance_id: $iid, \
                context: $context, \
                awaiting_input_schema: { question: $question }, \
                created_at: time::now(), \
                updated_at: time::now() \
             }",
        )
        .bind(("iid", instance_id.to_string()))
        .bind(("context", context))
        .bind(("question", item.question))
        .await;
    match res.and_then(|r| r.check().map(|_| ())) {
        Ok(()) => true,
        Err(e) => {
            warn!("[operator-inbox] insert failed: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        db.query("DEFINE TABLE ai_task SCHEMALESS;")
            .await
            .unwrap()
            .check()
            .unwrap();
        db
    }

    #[tokio::test]
    async fn files_once_per_dedup_key() {
        let db = test_db().await;
        let mk = || InboxItem {
            source: "ocr_verify".into(),
            dedup_key: "unit1:hashabc".into(),
            question: "Serial looks wrong — confirm?".into(),
            context: json!({ "unit_id": "unit1", "reason": "mismatch" }),
        };
        assert!(file(&db, "node-a", mk()).await, "first file creates a row");
        assert!(!file(&db, "node-a", mk()).await, "same key is a no-op");

        let rows: Vec<Value> = db
            .query("SELECT state, context FROM ai_task")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(rows.len(), 1, "exactly one task filed");
        assert_eq!(rows[0].get("state").and_then(|v| v.as_str()), Some("awaiting_human"));
        // meta.status forced to "open" so list_tasks shows it.
        assert_eq!(
            rows[0].pointer("/context/meta/status").and_then(|v| v.as_str()),
            Some("open")
        );
    }
}
