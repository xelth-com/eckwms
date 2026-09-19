//! In-memory fuzzy PII index (typo-tolerant person + address resolution) —
//! the runtime side of `.eck/PPRL_FUZZY.md` (§C, extended by §F).
//!
//! `Name_<hex>`/`Address_<hex>` tokens are exact keyed SimHashes: a query with a
//! typo yields a different token and zero matches. This index holds a
//! **closeness-preserving** signature ([`FuzzySig`]) next to each token so the
//! agent/mesh tier can resolve a person or address from a misspelt query
//! WITHOUT ever seeing a clear value — it returns tokens + scores only.
//!
//! Two channels feed it (see [`FuzzyIndex::reload`]):
//!   * Channel B — the `document.pii_fuzzy` array (stored signatures, incl.
//!     names that live ONLY as tokens in extracted prose);
//!   * a clear-field derivation of `order.customer_name` (Name) computed on the
//!     fly, mirroring the exact/substring paths those surfaces already use.
//!
//! Held in `AppState`; reloaded at startup and every 10 min (picks up
//! mesh-synced docs), and `upsert`ed from the `derive_pii_fuzzy` write sites for
//! immediacy on the authoring node.

use std::collections::HashMap;
use std::sync::RwLock;

use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::{
    fuzzy_score, obfuscate_pii, pii_fuzzy_sig_kind, FuzzySig, SigKind, PII_FUZZY_THRESHOLD,
};
use serde_json::Value;
use tracing::{info, warn};

/// One fuzzy-resolution hit: the resolved pseudonym token, its match score, and
/// which kind of signature matched (so mixed name/address results stay tagged).
#[derive(Clone, Debug)]
pub struct Candidate {
    pub token: String,
    pub score: f32,
    pub kind: SigKind,
}

/// Token → signature map, rebuilt wholesale on each [`reload`](FuzzyIndex::reload)
/// (so GDPR-erased / re-derived rows drop out) and point-`upsert`ed on writes.
pub struct FuzzyIndex {
    inner: RwLock<HashMap<String, FuzzySig>>,
}

impl Default for FuzzyIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn have_pepper() -> bool {
    eck_core::keyring::pepper().is_some()
}

impl FuzzyIndex {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Rebuild the whole index from the DB: Channel-B `document.pii_fuzzy`
    /// entries + on-the-fly Name signatures for every `order.customer_name`.
    pub async fn reload(&self, db: &SurrealDb) {
        let mut fresh: HashMap<String, FuzzySig> = HashMap::new();

        let doc_rows: Vec<Value> = match db
            .query("SELECT pii_fuzzy FROM document WHERE pii_fuzzy != NONE AND pii_fuzzy != []")
            .await
            .and_then(|mut r| r.take(0))
        {
            Ok(v) => v,
            Err(e) => {
                warn!("[FuzzyIndex] document pii_fuzzy load failed: {e}");
                Vec::new()
            }
        };
        for row in &doc_rows {
            let Some(arr) = row.get("pii_fuzzy").and_then(|v| v.as_array()) else {
                continue;
            };
            for entry in arr {
                add_entry(&mut fresh, entry);
            }
        }

        // Clear-field channel: `order` has no fingerprint array, only a
        // `customer_name` — re-derive its Name token + signature (deterministic,
        // same as the exact `search_by_token`/customer_360 order paths).
        if have_pepper() {
            let order_rows: Vec<Value> = match db
                .query(
                    "SELECT customer_name FROM order \
                     WHERE customer_name != NONE AND customer_name != ''",
                )
                .await
                .and_then(|mut r| r.take(0))
            {
                Ok(v) => v,
                Err(e) => {
                    warn!("[FuzzyIndex] order customer_name load failed: {e}");
                    Vec::new()
                }
            };
            for row in &order_rows {
                if let Some(name) = row.get("customer_name").and_then(|v| v.as_str()) {
                    let name = name.trim();
                    if name.is_empty() {
                        continue;
                    }
                    let token = obfuscate_pii(name, "Name");
                    fresh
                        .entry(token)
                        .or_insert_with(|| pii_fuzzy_sig_kind(name, SigKind::Name));
                }
            }
        }

        let n = fresh.len();
        if let Ok(mut w) = self.inner.write() {
            *w = fresh;
        }
        info!("[FuzzyIndex] reloaded {n} name/address signatures");
    }

    /// Insert / replace one token's signature (immediacy on the authoring node).
    pub fn upsert(&self, token: String, sig: FuzzySig) {
        if let Ok(mut m) = self.inner.write() {
            m.insert(token, sig);
        }
    }

    /// Fold a `pii_fuzzy` array Value (as produced by `derive_pii_fuzzy`) into
    /// the index — used by the write sites.
    pub fn upsert_from_value(&self, pii_fuzzy: &Value) {
        let Some(arr) = pii_fuzzy.as_array() else {
            return;
        };
        if let Ok(mut m) = self.inner.write() {
            for entry in arr {
                if let (Some(t), Some(fs)) = (
                    entry.get("t").and_then(|v| v.as_str()),
                    entry
                        .get("s")
                        .and_then(|s| serde_json::from_value::<FuzzySig>(s.clone()).ok()),
                ) {
                    m.insert(t.to_string(), fs);
                }
            }
        }
    }

    /// Score the query against every stored signature of `kind`, keep matches
    /// ≥ threshold, sort by score desc then token (stable), truncate to `limit`.
    pub fn resolve(&self, query: &str, kind: SigKind, limit: usize) -> Vec<Candidate> {
        if !have_pepper() {
            return Vec::new();
        }
        let q = pii_fuzzy_sig_kind(query, kind);
        if q.w.is_empty() {
            return Vec::new();
        }
        let Ok(map) = self.inner.read() else {
            return Vec::new();
        };
        let mut cands: Vec<Candidate> = map
            .iter()
            .filter(|(_, sig)| sig.kind == kind)
            .filter_map(|(token, sig)| {
                let s = fuzzy_score(&q, sig);
                (s >= PII_FUZZY_THRESHOLD).then(|| Candidate {
                    token: token.clone(),
                    score: s,
                    kind,
                })
            })
            .collect();
        cands.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.token.cmp(&b.token))
        });
        cands.truncate(limit);
        cands
    }

    /// Kind-agnostic entry point for the search surfaces: pick the kind(s) to
    /// search from the query shape (§F), returning candidates tagged by kind.
    ///   * looks like an address (digit token + word, or a bare 5-digit zip) →
    ///     Address sigs;
    ///   * a purely-alphabetic 1–2 word query (city-only, or a bare name) →
    ///     BOTH kinds (partial address queries are the norm);
    ///   * otherwise, if it looks like a person name → Name sigs.
    pub fn resolve_query(&self, query: &str, limit: usize) -> Vec<Candidate> {
        if looks_like_address(query) {
            return self.resolve(query, SigKind::Address, limit);
        }
        if alpha_short_query(query) {
            let mut out = self.resolve(query, SigKind::Name, limit);
            out.extend(self.resolve(query, SigKind::Address, limit));
            return out;
        }
        if looks_like_person_name(query) {
            return self.resolve(query, SigKind::Name, limit);
        }
        Vec::new()
    }
}

fn add_entry(map: &mut HashMap<String, FuzzySig>, entry: &Value) {
    if let (Some(t), Some(fs)) = (
        entry.get("t").and_then(|v| v.as_str()),
        entry
            .get("s")
            .and_then(|s| serde_json::from_value::<FuzzySig>(s.clone()).ok()),
    ) {
        map.insert(t.to_string(), fs);
    }
}

/// Whether `q` plausibly names a person: 1–4 words, only letters / `-` / `'` /
/// `.`, each word ≥ 2 chars, no digits, and not an ALL-CAPS code.
pub fn looks_like_person_name(q: &str) -> bool {
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.is_empty() || words.len() > 4 {
        return false;
    }
    let mut any_letter = false;
    let mut all_upper = true;
    for w in &words {
        let letters = w
            .chars()
            .filter(|c| *c != '-' && *c != '\'' && *c != '.')
            .count();
        if letters < 2 {
            return false;
        }
        for c in w.chars() {
            if c == '-' || c == '\'' || c == '.' {
                continue;
            }
            if !c.is_alphabetic() {
                return false; // digits / symbols disqualify a name
            }
            any_letter = true;
            if c.is_lowercase() {
                all_upper = false;
            }
        }
    }
    any_letter && !all_upper
}

/// Whether `q` looks like a postal address: at least one digit-bearing token AND
/// one alphabetic word, OR a bare 5-digit zip.
pub fn looks_like_address(q: &str) -> bool {
    let trimmed = q.trim();
    if trimmed.len() == 5 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let tokens: Vec<&str> = q
        .split(|c: char| c.is_whitespace() || ",/.-".contains(c))
        .filter(|t| !t.is_empty())
        .collect();
    let has_digit_token = tokens.iter().any(|t| t.chars().any(|c| c.is_ascii_digit()));
    let has_alpha_word = tokens
        .iter()
        .any(|t| t.chars().count() >= 2 && t.chars().all(|c| c.is_alphabetic()));
    has_digit_token && has_alpha_word
}

/// A purely-alphabetic 1–2 word query — searched against BOTH kinds because a
/// partial address ("Berlin") and a bare name look identical.
fn alpha_short_query(q: &str) -> bool {
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.is_empty() || words.len() > 2 {
        return false;
    }
    words.iter().all(|w| {
        w.chars().count() >= 2
            && w.chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn person_name_gate() {
        assert!(looks_like_person_name("Hans Müller"));
        assert!(looks_like_person_name("Anna von Berg"));
        assert!(!looks_like_person_name("Hauptstr 12"));
        assert!(!looks_like_person_name("GMBH"));
        assert!(!looks_like_person_name(""));
        assert!(!looks_like_person_name("a b c d e"));
    }

    #[test]
    fn address_gate() {
        assert!(looks_like_address("Hauptstr. 12, 10115 Berlin"));
        assert!(looks_like_address("10115")); // bare zip
        assert!(!looks_like_address("Berlin")); // no digit → city-only handled elsewhere
        assert!(!looks_like_address("Hans Müller"));
    }

    #[test]
    fn short_alpha_query_is_both_kinds() {
        assert!(alpha_short_query("Berlin"));
        assert!(alpha_short_query("Hans Müller"));
        assert!(!alpha_short_query("Hans Peter Müller"));
        assert!(!alpha_short_query("Hauptstr 12"));
    }
}
