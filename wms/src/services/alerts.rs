//! Shared dashboard-alert sink.
//!
//! One deterministic, deduplicated place to `raise` and `resolve` the
//! `system_alert` rows the dashboard shows as a red banner. Modeled on the AI
//! Observer's `save_and_broadcast_alert` (ai/observer.rs) — same row shape and
//! same `SYSTEM_ALERT` WS payload the UI already consumes — but keyed so a
//! recurring condition (a dead provider, a downed scraper) updates ONE row
//! instead of spamming a new alert every cycle, and self-clears on recovery.
//!
//! Keyed dedupe rules (per `key`, only over `status = 'unread'` rows):
//!   * re-raise    → UPDATE title/message/severity (never downgrade
//!                   critical→warning), bump `occurrences`, refresh
//!                   `last_seen_at`; re-broadcast on WS only if severity
//!                   escalated OR the last broadcast is > 6 h old.
//!   * first raise → CREATE the row and broadcast.
//!   * resolve     → mark unread rows read + broadcast `SYSTEM_ALERT_RESOLVED`.

use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::error;

/// Re-broadcast a still-unresolved alert at most this often (unless it escalates).
const REBROADCAST_AFTER_SECS: i64 = 6 * 3600;

/// Severity rank for the no-downgrade / escalation rules. Unknown → warning.
fn rank(sev: &str) -> u8 {
    match sev {
        "critical" => 2,
        _ => 1, // "warning" and anything else
    }
}

/// Build the UI payload — the Observer's `SYSTEM_ALERT` shape plus `key`.
fn ws_alert(key: &str, severity: &str, title: &str, message: &str, ts: &str) -> String {
    serde_json::to_string(&json!({
        "type": "SYSTEM_ALERT",
        "key": key,
        "title": title,
        "message": message,
        "severity": severity,
        "timestamp": ts,
    }))
    .unwrap_or_default()
}

/// Raise (or refresh) a keyed alert. See module docs for the dedupe rules.
pub async fn raise(
    db: &SurrealDb,
    ws: Option<&broadcast::Sender<String>>,
    key: &str,
    severity: &str,
    title: &str,
    message: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();

    // Newest unread row for this key, if any.
    let existing: Option<Value> = db
        .query("SELECT * FROM system_alert WHERE key = $key AND status = 'unread' ORDER BY created_at DESC LIMIT 1")
        .bind(("key", key.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    if let Some(row) = existing {
        let prev_sev = row.get("severity").and_then(|v| v.as_str()).unwrap_or("warning");
        // Never downgrade an already-critical alert back to warning.
        let new_sev = if rank(severity) >= rank(prev_sev) { severity } else { prev_sev };
        let escalated = rank(new_sev) > rank(prev_sev);

        let stale = row
            .get("last_broadcast_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc)).num_seconds() >= REBROADCAST_AFTER_SECS)
            .unwrap_or(true);
        let rebroadcast = escalated || stale;

        // Update by key (dedupe guarantees at most one unread row per key going
        // forward; matching on key also dodges rebinding a record-id Thing that
        // the SDK handed back as a JSON string). `occurrences += 1` and an
        // optional `last_broadcast_at` bump in one statement.
        let set_broadcast = if rebroadcast {
            format!(", last_broadcast_at = '{}'", now)
        } else {
            String::new()
        };
        let q = format!(
            "UPDATE system_alert SET title = $title, message = $message, severity = $sev, \
             last_seen_at = $now, occurrences = (occurrences ?? 1) + 1{} \
             WHERE key = $key AND status = 'unread'",
            set_broadcast
        );
        let res = db
            .query(q)
            .bind(("key", key.to_string()))
            .bind(("title", title.to_string()))
            .bind(("message", message.to_string()))
            .bind(("sev", new_sev.to_string()))
            .bind(("now", now.clone()))
            .await;
        if let Err(e) = res {
            error!("[alerts] failed to update alert {}: {}", key, e);
            return;
        }

        if rebroadcast {
            if let Some(tx) = ws {
                let _ = tx.send(ws_alert(key, new_sev, title, message, &now));
            }
        }
        return;
    }

    // First occurrence — create and broadcast.
    let row = json!({
        "key": key,
        "source": "watchdog",
        "title": title,
        "message": message,
        "severity": severity,
        "status": "unread",
        "mitigated": false,
        "occurrences": 1,
        "created_at": &now,
        "last_seen_at": &now,
        "last_broadcast_at": &now,
        "reported_to_cloud": false,
    });
    if let Err(e) = db.create::<Option<Value>>("system_alert").content(row).await {
        error!("[alerts] failed to create alert {}: {}", key, e);
        return;
    }
    if let Some(tx) = ws {
        let _ = tx.send(ws_alert(key, severity, title, message, &now));
    }
}

/// Resolve every unread alert with this `key`. Returns true if any row changed
/// (and a `SYSTEM_ALERT_RESOLVED` was broadcast so open dashboards clear the
/// banner without a reload).
pub async fn resolve(
    db: &SurrealDb,
    ws: Option<&broadcast::Sender<String>>,
    key: &str,
    note: &str,
) -> bool {
    let now = chrono::Utc::now().to_rfc3339();
    let changed: Vec<Value> = db
        .query(
            "UPDATE system_alert SET status = 'read', resolved_note = $note, resolved_at = $now \
             WHERE key = $key AND status = 'unread' RETURN AFTER",
        )
        .bind(("key", key.to_string()))
        .bind(("note", note.to_string()))
        .bind(("now", now.clone()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .unwrap_or_default();

    if changed.is_empty() {
        return false;
    }

    let title = changed
        .first()
        .and_then(|v| v.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if let Some(tx) = ws {
        let msg = serde_json::to_string(&json!({
            "type": "SYSTEM_ALERT_RESOLVED",
            "key": key,
            "title": title,
            "timestamp": now,
        }))
        .unwrap_or_default();
        let _ = tx.send(msg);
    }
    true
}
