use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Assumed language of source content. We do NOT language-detect; user-facing
/// content (starting with support-ticket AI summaries) is treated as authored in
/// this canonical language, so a request for it returns the original untouched.
pub const DEFAULT_SOURCE_LANG: &str = "en";

/// A claim older than this (10 min) is treated as STALE — the owner probably
/// died mid-flight, so any node may re-claim and retranslate. Bounds the window a
/// crashed translator can wedge a `(source,field,lang)` triple.
pub const CLAIM_TTL_SECS: i64 = 600;
/// A `failed` row younger than this (5 min) is NOT retried — the guardrail against
/// hot retry loops on a persistently-failing translation (bad key, quota, etc.).
pub const RETRY_AFTER_SECS: i64 = 300;

/// A cached machine translation of one `(source, field)` into one language, and
/// the mesh-synced WORK CLAIM for it — "I'm translating X" and "X is translated"
/// live in the SAME deterministic row, so any node in the mesh sees the claim and
/// doesn't duplicate the Gemini call (cross-node work dedup).
///
/// The record id is DETERMINISTIC — derived from `(source, field, lang)` via
/// [`translation_id`] — so a retranslation after the source text changes UPSERTs
/// the same row in place (no duplicates). `source_hash` pins the translation to
/// the exact source text it was produced from: when the source changes, the hash
/// no longer matches and the row is treated as STALE (served as pending, then
/// retranslated). Mirrors the repo's SHA256 → dashed-UUID addressing style (see
/// [`crate::models::i18n_label`], [`crate::utils::identity::compute_mesh_id`]).
///
/// Lifecycle via `status`:
///   * `claimed` — a node accepted the job and stamped `claimed_by`/`claimed_at`;
///     `text` is empty. Peers see this and back off until [`CLAIM_TTL_SECS`].
///   * `done`    — `text`/`translated_by` populated; served to viewers.
///   * `failed`  — `failed_at`/`error` set; not retried until [`RETRY_AFTER_SECS`].
///
/// Node-portable: `source`, `field`, `lang`, `text`, `model`, `source_hash`,
/// `status`, and the claim/result stamps are all content (the timestamp/`_vclock`
/// are stripped from the merkle hash), so a claim/result produced on one node
/// converges to peers instead of each node re-calling Gemini.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Translation {
    /// Thing string of the source row, e.g. ``"document:`12345`"``.
    pub source: String,
    /// Field on the source that was translated, e.g. `"summary"`.
    pub field: String,
    /// Target language code, e.g. `"ko"`.
    pub lang: String,
    /// The translated text. Empty while `status == "claimed"` / `"failed"`.
    pub text: String,
    /// Model that produced the translation, e.g. `"gemini-3.1-flash-lite"`.
    pub model: String,
    /// SHA-256 hex of the exact source text this translation was produced from.
    pub source_hash: String,
    /// `"claimed"` | `"done"` | `"failed"`. (Legacy rows written before the claim
    /// protocol have no `status`; a non-empty `text` still serves them.)
    #[serde(default)]
    pub status: String,
    /// instance_id of the node that CLAIMED the job (holds it until TTL).
    #[serde(default)]
    pub claimed_by: Option<String>,
    /// RFC3339 time the claim was stamped — freshness measured against it.
    #[serde(default)]
    pub claimed_at: Option<DateTime<Utc>>,
    /// instance_id of the node that produced the finished translation.
    #[serde(default)]
    pub translated_by: Option<String>,
    /// RFC3339 time a Gemini attempt failed — gates the retry backoff.
    #[serde(default)]
    pub failed_at: Option<DateTime<Utc>>,
    /// Short failure reason (kept small; it merkle-syncs).
    #[serde(default)]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// SHA-256 hex of a source text — pins a translation to the exact bytes it was
/// produced from, so a later source edit invalidates it (hash mismatch → stale).
pub fn source_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Deterministic record-id KEY for a translation, derived from
/// `(source, field, lang)`. Stable lowercase dashed UUID so CREATE/UPSERT on the
/// same triple always targets one row. `0x1f` unit separators between the three
/// fields prevent join collisions (e.g. `("ab","c",…)` vs `("a","bc",…)`).
/// Returns just the id key (no table prefix); address the row as
/// ``translation:`<key>` `` via `type::record`.
pub fn translation_id(source: &str, field: &str, lang: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update([0x1f]);
    hasher.update(field.as_bytes());
    hasher.update([0x1f]);
    hasher.update(lang.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// A translation row already stored for a `(source, field, lang)` triple — the
/// fields the resolver needs to decide serve-vs-pending-vs-enqueue, including the
/// mesh-synced claim state (so we don't duplicate a peer's in-flight Gemini call).
#[derive(Clone, Debug, PartialEq)]
pub struct ExistingTranslation {
    pub source_hash: String,
    pub text: String,
    /// `"claimed"` | `"done"` | `"failed"` | `""` (legacy row, no claim protocol).
    pub status: String,
    /// When the current claim was stamped (only meaningful for `status=claimed`).
    pub claimed_at: Option<DateTime<Utc>>,
    /// When the last Gemini attempt failed (only meaningful for `status=failed`).
    pub failed_at: Option<DateTime<Utc>>,
}

/// Outcome of the resolver's pure decision: which text to serve NOW, the language
/// that text is actually in, whether a translation is still pending, and whether
/// a background job must be enqueued.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub text: String,
    pub lang: String,
    pub pending: bool,
    pub enqueue: bool,
}

/// Pure resolver decision (no DB, no IO) — unit-tested exhaustively.
///
/// `now` makes claim/failure freshness testable. Truth table (once `req_lang` is
/// non-empty and differs from `source_lang`, and the stored `source_hash` MATCHES
/// the current text hash — a mismatch means the source changed and is always
/// treated as missing → pending + enqueue):
///
/// | stored status | condition                          | pending | enqueue |
/// |---------------|------------------------------------|---------|---------|
/// | (none/missing)| —                                  | yes     | yes     |
/// | `done`        | text non-empty                     | no      | no (serve) |
/// | `done`        | text empty (defensive)             | yes     | yes     |
/// | `claimed`     | claim fresh (< CLAIM_TTL)           | yes     | **no**  |
/// | `claimed`     | claim stale (≥ CLAIM_TTL / no ts)  | yes     | yes     |
/// | `failed`      | recent (< RETRY_AFTER)             | yes     | **no**  |
/// | `failed`      | old (≥ RETRY_AFTER / no ts)        | yes     | yes     |
/// | `""` (legacy) | text non-empty                     | no      | no (serve) |
/// | `""` (legacy) | text empty                         | yes     | yes     |
///
/// The `claimed`-fresh row is the cross-node dedup: a peer owns the Gemini call,
/// so we serve pending WITHOUT enqueuing a duplicate. A `claimed` row's empty
/// `text` is never served — only `done` (or legacy non-empty) rows serve.
///
/// When pending, `lang` is the SOURCE language (the served text is the original),
/// not the requested one — so the caller's `summary_lang` honestly names what the
/// bytes are, and `summary_pending` tells the client to re-fetch after the
/// `translation_ready` push.
pub fn decide(
    original: &str,
    req_lang: &str,
    source_lang: &str,
    existing: Option<&ExistingTranslation>,
    current_hash: &str,
    now: DateTime<Utc>,
) -> Decision {
    let req = req_lang.trim();
    if req.is_empty() || req.eq_ignore_ascii_case(source_lang) {
        return Decision {
            text: original.to_string(),
            lang: source_lang.to_string(),
            pending: false,
            enqueue: false,
        };
    }

    // Pending, serve the original in the source language.
    let pending = |enqueue: bool| Decision {
        text: original.to_string(),
        lang: source_lang.to_string(),
        pending: true,
        enqueue,
    };
    let served = |text: &str| Decision {
        text: text.to_string(),
        lang: req.to_string(),
        pending: false,
        enqueue: false,
    };

    let Some(e) = existing else {
        return pending(true); // missing → translate
    };
    // Source text changed since this row was produced → treat as missing.
    if e.source_hash != current_hash {
        return pending(true);
    }

    match e.status.as_str() {
        "done" => {
            if e.text.is_empty() {
                pending(true) // defensive: a done row must carry text
            } else {
                served(&e.text)
            }
        }
        "claimed" => {
            // A FRESH claim means a peer (or us) owns the Gemini call — do NOT
            // enqueue a duplicate. A stale claim (owner likely died) → re-claim.
            let fresh = e
                .claimed_at
                .map(|t| (now - t).num_seconds() < CLAIM_TTL_SECS)
                .unwrap_or(false);
            pending(!fresh)
        }
        "failed" => {
            // Don't hot-retry: a recent failure serves pending without enqueue.
            let recent = e
                .failed_at
                .map(|t| (now - t).num_seconds() < RETRY_AFTER_SECS)
                .unwrap_or(false);
            pending(!recent)
        }
        // Legacy row with no claim protocol: serve if it has text, else translate.
        _ => {
            if e.text.is_empty() {
                pending(true)
            } else {
                served(&e.text)
            }
        }
    }
}

/// Whether another translation may be enqueued given today's running count and
/// the configured daily cap. The cap is the runaway-Gemini guardrail (we once
/// burned ~191 calls on a single doc); at/over the cap, callers must NOT enqueue.
pub fn under_daily_cap(count_today: u32, cap: u32) -> bool {
    count_today < cap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finished (`done`) translation row.
    fn ex(hash: &str, text: &str) -> ExistingTranslation {
        ExistingTranslation {
            source_hash: hash.to_string(),
            text: text.to_string(),
            status: "done".to_string(),
            claimed_at: None,
            failed_at: None,
        }
    }

    /// A `claimed` row whose claim was stamped `age_secs` ago.
    fn claimed(hash: &str, age_secs: i64, now: DateTime<Utc>) -> ExistingTranslation {
        ExistingTranslation {
            source_hash: hash.to_string(),
            text: String::new(),
            status: "claimed".to_string(),
            claimed_at: Some(now - chrono::Duration::seconds(age_secs)),
            failed_at: None,
        }
    }

    /// A `failed` row whose failure was stamped `age_secs` ago.
    fn failed(hash: &str, age_secs: i64, now: DateTime<Utc>) -> ExistingTranslation {
        ExistingTranslation {
            source_hash: hash.to_string(),
            text: String::new(),
            status: "failed".to_string(),
            claimed_at: None,
            failed_at: Some(now - chrono::Duration::seconds(age_secs)),
        }
    }

    #[test]
    fn record_id_is_deterministic_uuid() {
        let a = translation_id("document:`1`", "summary", "ko");
        let b = translation_id("document:`1`", "summary", "ko");
        assert_eq!(a, b);
        assert_eq!(a.len(), 36); // dashed UUID
        assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn record_id_separates_the_three_fields() {
        let base = translation_id("document:`1`", "summary", "ko");
        assert_ne!(base, translation_id("document:`2`", "summary", "ko")); // source
        assert_ne!(base, translation_id("document:`1`", "title", "ko"));   // field
        assert_ne!(base, translation_id("document:`1`", "summary", "de")); // lang
        // No naive-concatenation collision across the field boundary.
        assert_ne!(
            translation_id("ab", "c", "ko"),
            translation_id("a", "bc", "ko"),
        );
    }

    #[test]
    fn source_hash_changes_with_text() {
        assert_eq!(source_hash("hello"), source_hash("hello"));
        assert_ne!(source_hash("hello"), source_hash("hello "));
        assert_eq!(source_hash("hello").len(), 64); // sha256 hex
    }

    #[test]
    fn empty_req_lang_serves_original() {
        let d = decide("Original", "", "en", None, "h", Utc::now());
        assert_eq!(d.text, "Original");
        assert_eq!(d.lang, "en");
        assert!(!d.pending);
        assert!(!d.enqueue);
    }

    #[test]
    fn same_lang_serves_original_case_insensitive() {
        let d = decide("Original", "EN", "en", Some(&ex("h", "irrelevant")), "h", Utc::now());
        assert_eq!(d.text, "Original");
        assert!(!d.pending);
        assert!(!d.enqueue);
    }

    #[test]
    fn fresh_translation_is_served() {
        let d = decide("Original", "ko", "en", Some(&ex("h", "번역")), "h", Utc::now());
        assert_eq!(d.text, "번역");
        assert_eq!(d.lang, "ko");
        assert!(!d.pending);
        assert!(!d.enqueue);
    }

    #[test]
    fn stale_hash_serves_original_and_enqueues() {
        // Stored translation exists but was produced from a different source text.
        let d = decide("New source", "ko", "en", Some(&ex("OLD", "stale ko")), "NEW", Utc::now());
        assert_eq!(d.text, "New source"); // stale never served
        assert_eq!(d.lang, "en");
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn stale_hash_enqueues_even_over_a_fresh_claim() {
        // Source text changed → the peer's fresh claim is for the OLD text; we
        // must retranslate regardless of claim state (hash mismatch wins).
        let now = Utc::now();
        let d = decide("New source", "ko", "en", Some(&claimed("OLD", 5, now)), "NEW", now);
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn missing_translation_serves_original_and_enqueues() {
        let d = decide("Original", "ko", "en", None, "h", Utc::now());
        assert_eq!(d.text, "Original");
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn done_but_empty_text_is_treated_as_missing() {
        let d = decide("Original", "ko", "en", Some(&ex("h", "")), "h", Utc::now());
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn legacy_row_without_status_still_serves_on_text() {
        // Rows written before the claim protocol have status="".
        let mut legacy = ex("h", "번역");
        legacy.status = String::new();
        let d = decide("Original", "ko", "en", Some(&legacy), "h", Utc::now());
        assert_eq!(d.text, "번역");
        assert!(!d.pending);
        assert!(!d.enqueue);
    }

    #[test]
    fn fresh_claim_by_peer_is_pending_no_enqueue() {
        // The dedup core: a peer owns this job (claim < 10 min) → wait, don't
        // spawn a duplicate Gemini call.
        let now = Utc::now();
        let d = decide("Original", "ko", "en", Some(&claimed("h", 60, now)), "h", now);
        assert_eq!(d.text, "Original");
        assert_eq!(d.lang, "en");
        assert!(d.pending);
        assert!(!d.enqueue);
    }

    #[test]
    fn stale_claim_is_reclaimed() {
        // Owner died mid-flight: claim older than CLAIM_TTL → re-claim + enqueue.
        let now = Utc::now();
        let d = decide("Original", "ko", "en", Some(&claimed("h", CLAIM_TTL_SECS + 1, now)), "h", now);
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn claim_without_timestamp_is_reclaimed() {
        // A malformed claim (no claimed_at) is treated as stale, not fresh.
        let mut c = claimed("h", 0, Utc::now());
        c.claimed_at = None;
        let d = decide("Original", "ko", "en", Some(&c), "h", Utc::now());
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn recent_failure_is_not_retried() {
        // No hot retry loop: a failure younger than RETRY_AFTER waits.
        let now = Utc::now();
        let d = decide("Original", "ko", "en", Some(&failed("h", 30, now)), "h", now);
        assert!(d.pending);
        assert!(!d.enqueue);
    }

    #[test]
    fn old_failure_is_retried() {
        let now = Utc::now();
        let d = decide("Original", "ko", "en", Some(&failed("h", RETRY_AFTER_SECS + 1, now)), "h", now);
        assert!(d.pending);
        assert!(d.enqueue);
    }

    #[test]
    fn daily_cap_gate() {
        assert!(under_daily_cap(0, 500));
        assert!(under_daily_cap(499, 500));
        assert!(!under_daily_cap(500, 500));
        assert!(!under_daily_cap(501, 500));
        assert!(!under_daily_cap(0, 0)); // cap of 0 disables translation
    }
}
