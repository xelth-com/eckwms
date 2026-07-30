//! Resolver for on-demand content translation.
//!
//! Given a source row + field + the text we currently hold + the requesting
//! user's language, decide what to serve NOW and (if needed) enqueue a background
//! translation. The decision itself is the PURE
//! [`eck_core::models::translation::decide`] — this layer only does the DB lookup
//! and the enqueue, so the interesting branching stays unit-tested in `eck_core`.

use eck_core::db::SurrealDb;
use eck_core::models::translation as model;
use serde_json::Value;

/// What the handler should put on the wire for this field.
pub struct Resolved {
    /// Best available text NOW (fresh translation, else the original).
    pub text: String,
    /// The language `text` is actually in (`req_lang` when translated, else the
    /// source language — honest even while a translation is pending).
    pub lang: String,
    /// A fresh translation does not yet exist; the client should re-fetch after
    /// the `translation_ready` WebSocket push.
    pub pending: bool,
}

/// Resolve `field` of `source_thing` into `req_lang`.
///
/// `source_thing` is a Thing literal string (e.g. ``document:`12345` ``).
/// `original_text` is the text we hold for the field (for support-ticket
/// summaries it is already PPRL-masked — translating it leaks nothing new).
/// A stale cached translation (source text changed) is treated as missing:
/// served as pending and retranslated, never served stale.
pub async fn resolve_translation(
    db: &SurrealDb,
    source_thing: &str,
    field: &str,
    original_text: &str,
    req_lang: &str,
) -> Resolved {
    let req = req_lang.trim();
    // Fast path: no translation wanted (empty or same language) — no DB hit.
    if req.is_empty() || req.eq_ignore_ascii_case(model::DEFAULT_SOURCE_LANG) {
        return Resolved {
            text: original_text.to_string(),
            lang: model::DEFAULT_SOURCE_LANG.to_string(),
            pending: false,
        };
    }

    let current_hash = model::source_hash(original_text);
    let id = model::translation_id(source_thing, field, req);
    let trid = format!("translation:`{}`", id);

    // Backtick-quoted `type::record` (not `type::thing`) is the repo idiom that
    // matches every id shape reliably. `type::string(...)` renders the datetime
    // claim/failure stamps as RFC3339 so we can parse them for freshness. We serve
    // text ONLY from a `done` row — a `claimed` row carries empty `text`, so even
    // if it leaked through, `decide` returns it as pending (never serves it).
    let row: Option<Value> = db
        .query(
            "SELECT source_hash, text, status, \
                    type::string(claimed_at) AS claimed_at, \
                    type::string(failed_at) AS failed_at \
             FROM type::record($trid) LIMIT 1",
        )
        .bind(("trid", trid))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    let parse_ts = |v: &Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&chrono::Utc))
    };
    let existing = row.map(|v| model::ExistingTranslation {
        source_hash: v.get("source_hash").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        text: v.get("text").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        status: v.get("status").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        claimed_at: parse_ts(&v, "claimed_at"),
        failed_at: parse_ts(&v, "failed_at"),
    });

    let d = model::decide(
        original_text,
        req,
        model::DEFAULT_SOURCE_LANG,
        existing.as_ref(),
        &current_hash,
        chrono::Utc::now(),
    );
    if d.enqueue {
        crate::ai::translation::enqueue(source_thing, field, req, original_text, &current_hash);
    }
    Resolved { text: d.text, lang: d.lang, pending: d.pending }
}
