//! Language aggregation for the per-user i18n language picker.
//!
//! The active set of UI languages a node offers is derived from the languages
//! its (active) users actually speak — no hardcoded language list lives in the
//! committed code, keeping the app company-generic. See
//! `wms/src/handlers/i18n.rs` for the HTTP surface.

use std::collections::HashMap;

/// Normalize a raw language code to a lowercase 2-letter primary subtag.
///
/// `"de"` → `Some("de")`, `"de-DE"` → `Some("de")`, `"KO"` → `Some("ko")`,
/// `"en_US"` → `Some("en")`. Anything that doesn't yield at least 2 ASCII
/// letters (empty, `"x"`, whitespace) → `None` so junk never reaches the UI.
pub fn normalize_lang(code: &str) -> Option<String> {
    let lower = code.trim().to_lowercase();
    // Primary subtag is everything before the first '-' or '_' separator.
    let primary = lower.split(['-', '_']).next().unwrap_or("");
    let two: String = primary.chars().take(2).collect();
    if two.len() == 2 && two.chars().all(|c| c.is_ascii_alphabetic()) {
        Some(two)
    } else {
        None
    }
}

/// Aggregate the distinct languages spoken across users, ordered by how many
/// users speak each (frequency DESC), ties broken alphabetically (ASC).
///
/// Each element of `per_user` is one user's raw language codes (typically the
/// user's `preferred_language` followed by every entry of their `languages`
/// list). Codes are normalized via [`normalize_lang`]; duplicates *within* a
/// single user count once (a user speaking `["de","de-AT"]` contributes a
/// single vote for `de`).
pub fn aggregate_languages(per_user: &[Vec<String>]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for user in per_user {
        // Distinct set of normalized codes for THIS user, so each user votes at
        // most once per language regardless of how they listed it.
        let mut seen: Vec<String> = Vec::new();
        for raw in user {
            if let Some(norm) = normalize_lang(raw) {
                if !seen.contains(&norm) {
                    seen.push(norm);
                }
            }
        }
        for lang in seen {
            *counts.entry(lang).or_insert(0) += 1;
        }
    }

    let mut ordered: Vec<(String, usize)> = counts.into_iter().collect();
    // Frequency DESC, then language code ASC for stable, deterministic ties.
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ordered.into_iter().map(|(lang, _)| lang).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_region_and_case() {
        assert_eq!(normalize_lang("de"), Some("de".into()));
        assert_eq!(normalize_lang("DE"), Some("de".into()));
        assert_eq!(normalize_lang("de-DE"), Some("de".into()));
        assert_eq!(normalize_lang("en_US"), Some("en".into()));
        assert_eq!(normalize_lang("  Ko  "), Some("ko".into()));
        assert_eq!(normalize_lang(""), None);
        assert_eq!(normalize_lang("x"), None);
        assert_eq!(normalize_lang("12"), None);
    }

    #[test]
    fn orders_by_frequency_desc_then_alpha() {
        // de=15, ko=5, en=3, ru=1 — mirrors the initial seed set.
        let mut per_user: Vec<Vec<String>> = Vec::new();
        for _ in 0..15 {
            per_user.push(vec!["de".into()]);
        }
        // ko total 5: 4 dedicated + 1 shared below
        for _ in 0..4 {
            per_user.push(vec!["ko".into()]);
        }
        // en total 3: 2 dedicated + 1 shared below
        for _ in 0..2 {
            per_user.push(vec!["en".into()]);
        }
        // one user speaks en + ko (dedup within user still counts once each)
        per_user.push(vec!["en".into(), "ko".into()]);
        // one user speaks ru
        per_user.push(vec!["ru".into()]);

        let out = aggregate_languages(&per_user);
        assert_eq!(out, vec!["de", "ko", "en", "ru"]);
    }

    #[test]
    fn ties_break_alphabetically() {
        // fr, es, de each spoken by exactly 2 users → alpha order de, es, fr.
        let per_user: Vec<Vec<String>> = vec![
            vec!["fr".into()],
            vec!["fr".into()],
            vec!["es".into()],
            vec!["es".into()],
            vec!["de".into()],
            vec!["de".into()],
        ];
        assert_eq!(aggregate_languages(&per_user), vec!["de", "es", "fr"]);
    }

    #[test]
    fn dedups_within_a_single_user() {
        // A lone user listing de twice (via region variants) is one vote.
        let per_user: Vec<Vec<String>> = vec![vec!["de".into(), "de-AT".into(), "DE".into()]];
        assert_eq!(aggregate_languages(&per_user), vec!["de"]);
    }
}
