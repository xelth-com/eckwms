//! Deterministic "customer delivered, we are silent" pre-check.
//!
//! Runs BEFORE any model call in the triage brain (orchestrator) and, when it
//! fires, injects a hard fact into the prompt: the LAST message on this ticket
//! is from the customer, our last outbound was N days earlier, and here is what
//! they sent. The 2026-09-07 backlog audit found 9 of 22 old open tickets whose
//! newest thread was the customer's signed forms / QC report / purchase order,
//! with our last reply 2–11 months earlier — and every model re-asked the
//! customer for the very thing already sitting on the ticket.
//!
//! The gate is pure ([`verdict`]) so it is unit-testable without a DB; [`detect`]
//! is the thin DB wrapper that assembles the rows and enriches with attachment
//! names + a masked excerpt. Any failure in `detect` is the caller's to swallow
//! (log at debug, run the model anyway) — this check never blocks a triage run.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use eck_core::db::SurrealDb;

/// Env: silence threshold in days (default 7). `"0"` disables the whole check.
fn min_days() -> i64 {
    std::env::var("ECK_STALE_REPLY_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(7)
}

/// The deterministic verdict handed to the model. Serialised verbatim into bench
/// telemetry and onto the `ai_task` row (`meta.stale_reply`).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StaleReply {
    pub last_inbound_at: String,
    pub last_outbound_at: String,
    pub silent_days: i64,
    /// File names of inbound attachments (blank templates excluded), max 8.
    pub attachments: Vec<String>,
    /// First ~240 chars of the newest inbound thread body, PII-masked.
    pub inbound_excerpt: String,
}

/// Minimal thread projection the pure gate reasons over.
pub(crate) struct ThreadRow {
    pub direction: String,
    pub created_time: String,
}

/// Outcome of the pure gate — the timing facts, before attachment/excerpt
/// enrichment (which needs the DB and lives in [`detect`]).
pub(crate) struct Verdict {
    pub last_inbound_at: String,
    pub last_outbound_at: String,
    pub silent_days: i64,
}

/// Lenient RFC3339 parse: accepts `Z`, `+00:00`, and Zoho's colon-less `+0000`.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    // Zoho Desk stamps like "2026-08-01T12:00:00.000+0000" (no colon in offset).
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%z"] {
        if let Ok(d) = DateTime::parse_from_str(s, fmt) {
            return Some(d.with_timezone(&Utc));
        }
    }
    None
}

/// Pure gate. `Some` iff the newest thread is inbound AND we have been silent
/// (now − newest-inbound) for at least `min_days`. `last_outbound_at` is our
/// most recent outbound (thread or `meta.last_outbound_at`), falling back to the
/// ticket's creation time when we never replied at all.
pub(crate) fn verdict(
    threads: &[ThreadRow],
    ticket_created_time: &str,
    meta_last_outbound_at: &str,
    now: DateTime<Utc>,
    min_days: i64,
) -> Option<Verdict> {
    if min_days <= 0 {
        return None;
    }

    // Newest thread by timestamp. Its direction decides everything: if the last
    // word on the ticket is ours, we already replied — not stale.
    let (newest_ts, newest) = threads
        .iter()
        .filter_map(|t| parse_ts(&t.created_time).map(|ts| (ts, t)))
        .max_by_key(|(ts, _)| *ts)?;
    if newest.direction != "in" {
        return None;
    }

    let silent_days = (now - newest_ts).num_days();
    if silent_days < min_days {
        return None;
    }

    // Our last outbound = latest of any "out" thread and the maintained
    // meta.last_outbound_at; if none, the ticket-created time is the baseline.
    let mut last_out: Option<DateTime<Utc>> = None;
    for t in threads {
        if t.direction == "out" {
            if let Some(ts) = parse_ts(&t.created_time) {
                last_out = Some(last_out.map_or(ts, |c| c.max(ts)));
            }
        }
    }
    if let Some(ts) = parse_ts(meta_last_outbound_at) {
        last_out = Some(last_out.map_or(ts, |c| c.max(ts)));
    }
    let last_outbound_at = last_out
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| ticket_created_time.trim().to_string());

    Some(Verdict {
        last_inbound_at: newest_ts.to_rfc3339(),
        last_outbound_at,
        silent_days,
    })
}

/// Char-safe truncation to at most `n` characters (never splits a UTF-8 scalar).
fn take_chars(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// Crude HTML-tag strip + whitespace collapse — thread bodies arrive as HTML
/// (`payload.content`) or plain text (`payload.plainText`); the excerpt only
/// needs to read as prose to the model, not round-trip.
fn strip_html(s: &str) -> String {
    // Drop <style>/<script> blocks first (Outlook mail bodies open with a CSS
    // blob that would otherwise become the whole excerpt), and the bare CSS
    // rule text Zoho sometimes leaves without tags (`div.zm_… { margin: 0cm; }`).
    let mut src = s.to_string();
    for tag in ["style", "script"] {
        let lower = src.to_ascii_lowercase();
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut cur = String::new();
        let mut rest = 0usize;
        while let Some(i) = lower[rest..].find(&open) {
            let i = rest + i;
            cur.push_str(&src[rest..i]);
            match lower[i..].find(&close) {
                Some(j) => rest = i + j + close.len(),
                None => { rest = src.len(); break; }
            }
        }
        cur.push_str(&src[rest..]);
        src = cur;
    }
    // Untagged CSS rules: `selector { … }` runs — strip anything inside braces
    // when the text starts like a stylesheet.
    if src.trim_start().starts_with("div.") || src.trim_start().starts_with('.') || src.contains("MsoNormal {") {
        let mut cur = String::new();
        let mut depth = 0i32;
        for c in src.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                _ if depth == 0 => cur.push(c),
                _ => {}
            }
        }
        // drop the selector words that preceded each `{`
        src = cur
            .split_whitespace()
            .filter(|w| !(w.starts_with("div.") || w.starts_with("p.") || w.starts_with("li.") || w.starts_with("span.") || (w.ends_with(',') && w.contains('.'))))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let s = src.replace("&nbsp;", " ");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// DB entry point. Returns `Some` when the ticket is a "customer delivered, we
/// are silent" case, enriched with inbound attachment names and a masked body
/// excerpt. Any DB error propagates; the caller logs at debug and runs anyway.
pub(crate) async fn detect(
    db: &SurrealDb,
    http: &reqwest::Client,
    doc_id: &str,
) -> anyhow::Result<Option<StaleReply>> {
    let min = min_days();
    if min <= 0 {
        return Ok(None);
    }

    // Ticket header: status (closed ⇒ history, never stale) + timing baselines.
    let ticket: Option<Value> = db
        .query(
            "SELECT status, meta.status AS meta_status, \
                    meta.created_time AS created_time, \
                    meta.last_outbound_at AS last_outbound_at \
             FROM document \
             WHERE type = 'support_ticket' AND record::id(id) = $tid LIMIT 1",
        )
        .bind(("tid", doc_id.to_string()))
        .await?
        .take(0)?;
    let ticket = match ticket {
        Some(t) => t,
        None => return Ok(None),
    };

    let status = ticket
        .get("status")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| ticket.get("meta_status").and_then(|v| v.as_str()))
        .unwrap_or("");
    if crate::services::support::is_closed_ticket_status(status) {
        return Ok(None);
    }

    let ticket_created = ticket
        .get("created_time")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let meta_last_outbound = ticket
        .get("last_outbound_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // All threads (header-only; bodies live in document_raw). ORDER BY is done in
    // Rust so we never trip the v3 "order field must be selected" rule.
    let thread_rows: Vec<Value> = db
        .query(
            "SELECT direction, created_time FROM document \
             WHERE type = 'support_thread' AND ticket_id = $tid",
        )
        .bind(("tid", doc_id.to_string()))
        .await?
        .take(0)?;
    let threads: Vec<ThreadRow> = thread_rows
        .into_iter()
        .map(|r| ThreadRow {
            direction: r.get("direction").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            created_time: r.get("created_time").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
        .collect();

    let v = match verdict(&threads, ticket_created, meta_last_outbound, Utc::now(), min) {
        Some(v) => v,
        None => return Ok(None),
    };

    // Inbound attachment file names, blank templates excluded (same edge + blank
    // filter as ListTicketAttachmentsTool). Best-effort: empty on any hiccup.
    let mut attachments: Vec<String> = Vec::new();
    let att_rows: Vec<Value> = db
        .query(
            "SELECT out.original_name AS name, out.doc_class AS doc_class \
             FROM has_attachment \
             WHERE in = type::record($trid) AND out.cas_uuid IS NOT NONE",
        )
        .bind(("trid", format!("document:`{}`", doc_id)))
        .await?
        .take(0)?;
    for r in att_rows {
        let is_blank = r
            .get("doc_class")
            .filter(|dc| dc.is_object())
            .and_then(|dc| dc.get("is_blank"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_blank {
            continue;
        }
        if let Some(name) = r.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            attachments.push(name.to_string());
            if attachments.len() >= 8 {
                break;
            }
        }
    }

    // Newest inbound body from local document_raw (absent on thin nodes → empty
    // excerpt, still a valid verdict). Pick the max createdTime in Rust.
    let raw_rows: Vec<Value> = db
        .query(
            "SELECT payload.content AS content, payload.plainText AS plain, \
                    payload.createdTime AS ct \
             FROM document_raw \
             WHERE type = 'support_thread' AND ticket_id = $tid \
             AND payload.direction = 'in'",
        )
        .bind(("tid", doc_id.to_string()))
        .await
        .and_then(|mut r| r.take::<Vec<Value>>(0))
        .unwrap_or_default();
    let mut best: Option<(DateTime<Utc>, String)> = None;
    for r in raw_rows {
        let ct = r.get("ct").and_then(|v| v.as_str()).unwrap_or("");
        let body = r
            .get("content")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| r.get("plain").and_then(|v| v.as_str()))
            .unwrap_or("");
        if body.is_empty() {
            continue;
        }
        let ts = parse_ts(ct).unwrap_or_else(Utc::now);
        if best.as_ref().map_or(true, |(b, _)| ts >= *b) {
            best = Some((ts, body.to_string()));
        }
    }
    let inbound_excerpt = match best {
        Some((_, body)) => {
            let clipped = take_chars(&strip_html(&body), 240);
            crate::ai::orchestrator::mask_free_text(http, &clipped).await
        }
        None => String::new(),
    };

    Ok(Some(StaleReply {
        last_inbound_at: v.last_inbound_at,
        last_outbound_at: v.last_outbound_at,
        silent_days: v.silent_days,
        attachments,
        inbound_excerpt,
    }))
}

/// The prompt block injected next to the KB digest when [`detect`] fires.
pub(crate) fn context_block(sr: &StaleReply) -> String {
    let carried = if sr.attachments.is_empty() {
        "no attachments".to_string()
    } else {
        sr.attachments.join(", ")
    };
    format!(
        "\n\n## CUSTOMER DELIVERED, NO REPLY FROM US (deterministic pre-check)\n\
         Last customer message: {inbound} ({days} days ago), our last outbound: {outbound}. \
         It carried: {carried}. Excerpt: \"{excerpt}\".\n\
         RULE: this ticket is RED. Do NOT ask the customer for anything again and do NOT \
         ask_human about it; open with a short apology for the delay, then give the concrete \
         next step (pickup date, quote, shipment, protocol) based on what they sent. Read the \
         delivered attachments first.",
        inbound = sr.last_inbound_at,
        days = sr.silent_days,
        outbound = sr.last_outbound_at,
        carried = carried,
        excerpt = sr.inbound_excerpt,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_drops_style_blocks_and_outlook_css() {
        let outlook = "<html><head><style>p.MsoNormal { margin: 0cm; }</style></head><body><p>Hallo,&nbsp;anbei die Unterlagen.</p></body></html>";
        assert_eq!(strip_html(outlook), "Hallo, anbei die Unterlagen.");
        let zoho = "div.zm_1_parse_2 p.MsoNormal, div.zm_1_parse_2 li.MsoNormal { margin: 0cm; font-size: 12pt; } Hallo, leider warten wir seit 3 Wochen.";
        let t = strip_html(zoho);
        assert!(t.starts_with("Hallo, leider"), "css leaked: {t}");
    }

    fn ago(days: i64) -> String {
        (Utc::now() - chrono::Duration::days(days)).to_rfc3339()
    }
    fn t(dir: &str, days: i64) -> ThreadRow {
        ThreadRow { direction: dir.to_string(), created_time: ago(days) }
    }

    #[test]
    fn a_stale_newest_inbound() {
        let threads = vec![t("out", 30), t("in", 10)];
        let v = verdict(&threads, &ago(60), "", Utc::now(), 7).expect("stale");
        assert_eq!(v.silent_days, 10);
        // our last outbound is the 30-day-old thread
        assert!(parse_ts(&v.last_outbound_at).unwrap() < Utc::now() - chrono::Duration::days(25));
    }

    #[test]
    fn b_newest_outbound_is_not_stale() {
        let threads = vec![t("in", 20), t("out", 5)];
        assert!(verdict(&threads, &ago(60), "", Utc::now(), 7).is_none());
    }

    #[test]
    fn c_recent_inbound_below_threshold() {
        let threads = vec![t("out", 30), t("in", 3)];
        assert!(verdict(&threads, &ago(60), "", Utc::now(), 7).is_none());
    }

    #[test]
    fn d_no_outbound_uses_ticket_created_baseline() {
        let threads = vec![t("in", 9)];
        let created = ago(20);
        let v = verdict(&threads, &created, "", Utc::now(), 7).expect("stale");
        assert_eq!(v.silent_days, 9);
        // no outbound at all → baseline is the ticket creation time
        assert_eq!(v.last_outbound_at, created);
    }

    #[test]
    fn d_meta_last_outbound_beats_thread() {
        // No "out" thread, but meta.last_outbound_at is maintained → use it.
        let threads = vec![t("in", 9)];
        let meta_out = ago(40);
        let v = verdict(&threads, &ago(60), &meta_out, Utc::now(), 7).expect("stale");
        assert_eq!(parse_ts(&v.last_outbound_at).unwrap(), parse_ts(&meta_out).unwrap());
    }

    #[test]
    fn e_disabled_when_min_days_zero() {
        let threads = vec![t("out", 30), t("in", 10)];
        assert!(verdict(&threads, &ago(60), "", Utc::now(), 0).is_none());
    }

    #[test]
    fn f_blank_templates_excluded_from_attachments() {
        // Mirrors detect's blank filter over the same doc_class shape.
        let rows = vec![
            serde_json::json!({ "name": "signed_form.pdf", "doc_class": { "is_blank": false } }),
            serde_json::json!({ "name": "blank_template.pdf", "doc_class": { "is_blank": true } }),
            serde_json::json!({ "name": "qc_report.pdf", "doc_class": null }),
        ];
        let kept: Vec<String> = rows
            .iter()
            .filter(|r| {
                !r.get("doc_class")
                    .filter(|dc| dc.is_object())
                    .and_then(|dc| dc.get("is_blank"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(String::from))
            .collect();
        assert_eq!(kept, vec!["signed_form.pdf", "qc_report.pdf"]);
    }
}
