//! `POST /api/pii/spans` — the "eck masker" serving endpoint (slice 2).
//!
//! Stateless: accepts `{ "text": ... }`, runs the on-device German NER via the
//! routing facade's LOCAL path, and returns the detected `(PER, ORG)` entities
//! plus their byte-offset spans within the request text. The token substitution
//! (`Name_<hex>` …) stays on the CLIENT — this endpoint returns spans only and
//! holds no mapping (see `.eck/PII_REMOTE_MASKING.md` §2).
//!
//! Hard invariants:
//! * Never proxies to Gemini. If `ECK_PII_NER != local` (which also covers a
//!   binary built without `local-embed`, since the resolver maps `local`→`gemini`
//!   there) it answers `503 { "error": "ner_unavailable" }`.
//! * Input capped at 64 KiB → `413` above.
//! * No-content logging: lengths / counts / latency only, never text or entities.
//! * Mounted ONLY when `ECK_PII_SPANS_SERVE=1`, behind an authenticated router
//!   (see wms/src/main.rs). Metering/paidgate is out of scope here.

use axum::{http::StatusCode, response::IntoResponse, Extension, Json};
use serde::Deserialize;
use serde_json::json;

use crate::middleware::spans_auth::SpansAuth;

/// Max request text size in bytes. Larger inputs are rejected (413) rather than
/// windowed — the masker is a per-field/per-subject service, not a bulk pipe.
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Model revision string echoed in the response. Only reachable when the local
/// NER is actually compiled+selected, so the feature-off value is inert.
#[cfg(feature = "local-embed")]
const MODEL_NAME: &str = crate::ai::local_ner::MODEL_NAME;
#[cfg(not(feature = "local-embed"))]
const MODEL_NAME: &str = "ner-unavailable";

#[derive(Deserialize)]
pub struct SpansRequest {
    #[serde(default)]
    pub text: String,
}

pub async fn spans(
    Extension(auth): Extension<SpansAuth>,
    Json(req): Json<SpansRequest>,
) -> impl IntoResponse {
    let text = req.text;
    let req_len = text.len();

    // TODO(paidgate): meter this call through the xelixir plan engine (rung-2
    // debit, per .eck/PII_REMOTE_MASKING.md §8) before returning spans.

    if req_len > MAX_TEXT_BYTES {
        tracing::warn!(req_len, cap = MAX_TEXT_BYTES, "[NER-serve] request over cap");
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "text_too_large", "max_bytes": MAX_TEXT_BYTES })),
        );
    }

    // Serve local NER ONLY. `remote`/`gemini`/feature-off all resolve to a mode
    // that is not "local" → 503. This is what guarantees the server never proxies
    // raw text onward to Gemini or to another masker.
    if crate::ai::pii_mode::pii_ner_mode() != "local" {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "ner_unavailable" })),
        );
    }

    let started = std::time::Instant::now();
    let (per, org) = match crate::ai::pii_mode::extract_entities(&text).await {
        Ok(ents) => ents,
        Err(e) => {
            // No-content logging: the error string is model/infra shaped, never
            // the input, but keep it terse.
            tracing::warn!(req_len, error = %e, "[NER-serve] local NER extraction failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "ner_unavailable" })),
            );
        }
    };

    let mut spans: Vec<serde_json::Value> = Vec::new();
    for (kind, list) in [("PER", &per), ("ORG", &org)] {
        for ent in list {
            for (start, end) in find_spans_ci(&text, ent) {
                spans.push(json!({ "start": start, "end": end, "kind": kind }));
            }
        }
    }

    let latency_ms = started.elapsed().as_millis() as u64;
    // No-content serving line: lengths / counts / auth mode only — never text or
    // entities. `client` is the license client-id PREFIX (empty for a JWT).
    let (auth_kind, client) = match &auth {
        SpansAuth::Jwt => ("jwt", String::new()),
        SpansAuth::License(c) => ("license", c.chars().take(8).collect()),
    };
    tracing::info!(
        "[pii-spans] served len={} entities={} auth={} client={}",
        req_len,
        per.len() + org.len(),
        auth_kind,
        client
    );
    tracing::debug!(
        req_len,
        per = per.len(),
        org = org.len(),
        spans = spans.len(),
        latency_ms,
        "[NER-serve] ok"
    );

    (
        StatusCode::OK,
        Json(json!({
            "model": MODEL_NAME,
            "entities": { "per": per, "org": org },
            "spans": spans,
        })),
    )
}

/// Case-insensitive, non-overlapping substring search returning `(start, end)`
/// BYTE offsets into `haystack`. Both offsets are guaranteed UTF-8 char
/// boundaries because they are derived from `haystack`'s own char boundaries —
/// never from byte arithmetic on the needle (this codebase has a `&s[..N]`
/// panic history; see `crate::ai::floor_char_boundary`). Case folding is done
/// per-char via `char::to_lowercase`, so umlauts and emoji in the haystack shift
/// no offsets.
fn find_spans_ci(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    let needle_lc: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if needle_lc.is_empty() {
        return Vec::new();
    }
    // Byte offset of every char boundary (start of each char) in the haystack.
    let starts: Vec<usize> = haystack.char_indices().map(|(i, _)| i).collect();

    let mut out = Vec::new();
    let mut i = 0usize;
    while i < starts.len() {
        let s = starts[i];
        if let Some(end) = match_at(haystack, s, &needle_lc) {
            out.push((s, end));
            // Advance past the match so occurrences don't overlap.
            while i < starts.len() && starts[i] < end {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// If `needle_lc` (already lowercased char sequence) matches `haystack` starting
/// at byte `start`, return the byte offset just past the match (a char
/// boundary). Otherwise `None`.
fn match_at(haystack: &str, start: usize, needle_lc: &[char]) -> Option<usize> {
    let mut ni = 0usize;
    let mut end = start;
    for (off, hc) in haystack[start..].char_indices() {
        if ni == needle_lc.len() {
            break;
        }
        for lc in hc.to_lowercase() {
            if ni < needle_lc.len() && needle_lc[ni] == lc {
                ni += 1;
            } else {
                return None;
            }
        }
        end = start + off + hc.len_utf8();
    }
    if ni == needle_lc.len() {
        Some(end)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_land_on_char_boundaries_with_umlauts_and_emoji() {
        // Emoji before/after + a name with an umlaut. The reported span must
        // slice out exactly the name and never panic on a byte boundary.
        let text = "🎉 Kontakt: Herr Müller 🎉 und Julia Weber";
        for name in ["Müller", "Julia Weber"] {
            let spans = find_spans_ci(text, name);
            assert_eq!(spans.len(), 1, "exactly one occurrence of {name}");
            let (s, e) = spans[0];
            assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
            assert_eq!(&text[s..e], name);
        }
    }

    #[test]
    fn spans_are_case_insensitive_but_offsets_stay_exact() {
        let text = "kunde hans MÜLLER meldet";
        let spans = find_spans_ci(text, "Hans Müller");
        assert_eq!(spans.len(), 1);
        let (s, e) = spans[0];
        assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
        assert_eq!(&text[s..e], "hans MÜLLER");
    }

    #[test]
    fn spans_finds_all_non_overlapping_occurrences() {
        let text = "Weber und Weber GmbH, Ansprechpartner Weber";
        let spans = find_spans_ci(text, "Weber");
        assert_eq!(spans.len(), 3);
        for (s, e) in spans {
            assert_eq!(&text[s..e], "Weber");
        }
    }

    #[test]
    fn spans_absent_needle_returns_empty() {
        assert!(find_spans_ci("no such person here", "Zebra Xylophon").is_empty());
        assert!(find_spans_ci("anything", "   ").is_empty());
    }

    #[test]
    fn spans_emoji_needle_offsets_are_safe() {
        // A multi-byte non-name needle still yields boundary-safe offsets.
        let text = "start 🎉 end";
        let spans = find_spans_ci(text, "🎉");
        assert_eq!(spans.len(), 1);
        let (s, e) = spans[0];
        assert_eq!(&text[s..e], "🎉");
    }
}
