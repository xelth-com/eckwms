//! Ungated PII-NER routing facade.
//!
//! Chooses the person/org entity backend from `ECK_PII_NER` and dispatches to
//! it, so callers branch on ONE mode string instead of a `#[cfg]`-gated
//! `local_ner::ner_mode()` plus their own stub arms. Three modes:
//!
//! * `local`  — on-device German NER (`local_ner`), requires the `local-embed`
//!   feature; if the binary was built without it we warn once and fall back to
//!   `gemini` (preserving today's `ner_mode()` semantics).
//! * `remote` — the stateless "eck masker" span service (`pii_remote`), reached
//!   over HTTP. Works regardless of the `local-embed` feature — a cloud-only
//!   build can be a remote-masking CLIENT. See `.eck/PII_REMOTE_MASKING.md`.
//! * `gemini` — no local/remote NER backend; entity extraction is not available
//!   through this facade (the Gemini extraction path lives at each call site and
//!   is selected by the caller when the mode is `gemini`). Calling
//!   [`extract_entities`] in this mode is a programming error and returns `Err`.
//!
//! This module is deliberately NOT feature-gated (unlike `local_ner`) so the
//! `remote` path — and the mode resolver — compile in every build profile.

use anyhow::Result;

/// Runtime PII-NER backend selector: `"local" | "remote" | "gemini"`. Parsed
/// from `ECK_PII_NER` once and cached (the value can't change mid-process),
/// which also makes the "asked for local but built cloud-only" warning fire
/// exactly once. Mirrors `local_ner::ner_mode`, extended with `remote`.
pub fn pii_ner_mode() -> &'static str {
    use std::sync::OnceLock;
    static MODE: OnceLock<&'static str> = OnceLock::new();
    *MODE.get_or_init(|| {
        let raw = std::env::var("ECK_PII_NER").ok();
        let mode = resolve_pii_ner_mode(raw.as_deref());
        let asked_local = raw
            .as_deref()
            .map(|v| v.trim().eq_ignore_ascii_case("local"))
            .unwrap_or(false);
        if asked_local && mode == "gemini" {
            tracing::warn!(
                "[NER] ECK_PII_NER=local requested but this binary was built \
                 without the `local-embed` feature — using Gemini PII extraction"
            );
        }
        mode
    })
}

/// Pure mode resolver: maps the raw `ECK_PII_NER` value to
/// `"local"`/`"remote"`/`"gemini"` with the feature gate applied to `local`,
/// and no process-global state. Split out of `pii_ner_mode` so it is
/// unit-testable without racing other tests on the env. `remote` needs no
/// feature — a cloud-only client can still call the eck masker.
fn resolve_pii_ner_mode(env_val: Option<&str>) -> &'static str {
    let v = env_val.map(str::trim).unwrap_or("");
    if v.eq_ignore_ascii_case("remote") {
        return "remote";
    }
    if v.eq_ignore_ascii_case("local") {
        #[cfg(feature = "local-embed")]
        {
            return "local";
        }
        #[cfg(not(feature = "local-embed"))]
        {
            return "gemini";
        }
    }
    "gemini"
}

/// Extract `(PER, ORG)` entity strings from `text` via the configured backend.
///
/// Dispatch:
/// * `local`  → [`crate::ai::local_ner::extract_entities`] (behind the feature);
/// * `remote` → [`crate::ai::pii_remote::extract_entities`];
/// * `gemini` → `Err` — there is no NER backend in this mode, and callers MUST
///   branch on [`pii_ner_mode`] before calling. We return a clear error rather
///   than panic so a mis-wired caller degrades to its lexicon backstop.
///
/// Fail-closed everywhere: a transport/parse/backend error is `Err` (callers
/// degrade to lexicon/regex masking), never a silent empty `Ok`.
pub async fn extract_entities(text: &str) -> Result<(Vec<String>, Vec<String>)> {
    match pii_ner_mode() {
        "local" => {
            #[cfg(feature = "local-embed")]
            {
                crate::ai::local_ner::extract_entities(text).await
            }
            #[cfg(not(feature = "local-embed"))]
            {
                // Unreachable at runtime (resolver maps local→gemini without the
                // feature) but must compile in cloud-only builds.
                let _ = text;
                anyhow::bail!("pii_ner_mode=local but the `local-embed` feature is not compiled")
            }
        }
        "remote" => crate::ai::pii_remote::extract_entities(text).await,
        other => anyhow::bail!(
            "pii_mode::extract_entities called in mode `{other}` (no NER backend); \
             callers must branch on pii_ner_mode() before calling"
        ),
    }
}

/// `(PER, ORG)` reduced to just the person names — the common masking case.
pub async fn extract_person_names(text: &str) -> Result<Vec<String>> {
    Ok(extract_entities(text).await?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_pii_ner_mode_matrix() {
        // Pure parser — no env mutation, so it can't race other tests. With the
        // default feature set `local-embed` is compiled in, so "local" resolves
        // to local; a cloud-only build resolves "local" to gemini. "remote" is
        // feature-independent; anything else/unset is gemini.
        let local_expected = if cfg!(feature = "local-embed") {
            "local"
        } else {
            "gemini"
        };
        assert_eq!(resolve_pii_ner_mode(None), "gemini");
        assert_eq!(resolve_pii_ner_mode(Some("")), "gemini");
        assert_eq!(resolve_pii_ner_mode(Some("   ")), "gemini");
        assert_eq!(resolve_pii_ner_mode(Some("gemini")), "gemini");
        assert_eq!(resolve_pii_ner_mode(Some("cloud")), "gemini");
        assert_eq!(resolve_pii_ner_mode(Some("local")), local_expected);
        assert_eq!(resolve_pii_ner_mode(Some("  LOCAL  ")), local_expected);
        assert_eq!(resolve_pii_ner_mode(Some("remote")), "remote");
        assert_eq!(resolve_pii_ner_mode(Some("  ReMoTe ")), "remote");
    }
}
