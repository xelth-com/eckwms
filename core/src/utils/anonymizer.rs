use sha2::{Sha256, Digest};
use std::sync::OnceLock;
use regex::Regex;

/// Compute a 64-bit SimHash from character bigrams, keyed with SYNC_SECRET as pepper.
/// Similar strings produce hashes with low Hamming distance,
/// enabling privacy-preserving record linkage (PPRL).
/// The pepper prevents dictionary attacks on the hash tokens.
fn simhash(text: &str) -> u64 {
    // The pepper MUST come from SYNC_SECRET — there is deliberately NO fallback
    // default. A known/public pepper makes the obfuscated PII tokens reversible
    // by dictionary attack (hash candidate names with the known pepper, match
    // tokens), which would void the Zero-Knowledge / "PII never leaves in
    // plaintext" guarantee. Fail closed rather than ship reversible anonymisation.
    let pepper = std::env::var("SYNC_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .expect(
            "SYNC_SECRET (PPRL pepper) is unset — refusing to anonymise PII with a \
             publicly-known default pepper (it would be reversible). Set SYNC_SECRET.",
        );
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();

    if chars.len() < 2 {
        // Too short for bigrams — keyed SHA256 hash directly
        let mut hasher = Sha256::new();
        hasher.update(lower.as_bytes());
        hasher.update(pepper.as_bytes());
        let digest = hasher.finalize();
        return u64::from_be_bytes(digest[0..8].try_into().unwrap());
    }

    let mut v = [0i32; 64];

    for window in chars.windows(2) {
        let bigram: String = window.iter().collect();
        let mut hasher = Sha256::new();
        hasher.update(bigram.as_bytes());
        hasher.update(pepper.as_bytes());
        let digest = hasher.finalize();
        let h = u64::from_be_bytes(digest[0..8].try_into().unwrap());

        for i in 0..64 {
            if (h >> i) & 1 == 1 {
                v[i] += 1;
            } else {
                v[i] -= 1;
            }
        }
    }

    let mut result: u64 = 0;
    for i in 0..64 {
        if v[i] > 0 {
            result |= 1u64 << i;
        }
    }
    result
}

/// Obfuscate a PII string using keyed SimHash.
/// Returns a token like `Name_8E5F3A1B00000000` that preserves similarity
/// (similar names → similar hashes) without revealing the original value.
pub fn obfuscate_pii(text: &str, pii_type: &str) -> String {
    format!("{}_{:016X}", pii_type, simhash(text))
}

/// Deterministic regex backstop for structured PII that an LLM extractor can
/// miss (or that ships on an extractor-failure fallback path) — emails, phone
/// numbers, IBANs, payment cards, German VAT-IDs. Each match is replaced with
/// the same `obfuscate_pii` SimHash token as the LLM path, so tokens stay
/// stable across runs and linkage is preserved. This is a GDPR safety net:
/// nothing matching these high-confidence patterns must reach a cloud model.
///
/// Patterns are deliberately conservative — phone/card patterns REQUIRE a `+`
/// prefix or explicit separators so bare digit runs (serial numbers, order
/// numbers) are NOT masked and embedding search quality is preserved. Returns
/// the scrubbed text plus the list of tokens that were substituted.
pub fn scrub_pii_regex(text: &str) -> (String, Vec<String>) {
    // (label, regex). Order matters: email & IBAN before phone so their digits
    // aren't half-eaten by the phone pattern.
    let patterns = pii_patterns();

    let mut out = text.to_string();
    let mut fingerprints = Vec::new();

    for (label, re) in patterns {
        // Collect distinct matches first (replacing while iterating is unsafe).
        let mut matches: Vec<String> = re
            .find_iter(&out)
            .map(|m| m.as_str().to_string())
            .collect();
        matches.sort_by_key(|m| std::cmp::Reverse(m.len())); // longest first → no partial overlap
        matches.dedup();
        for original in matches {
            if original.trim().is_empty() { continue; }
            let token = obfuscate_pii(original.trim(), label);
            if out.contains(&original) {
                out = out.replace(&original, &token);
                fingerprints.push(token);
            }
        }
    }

    (out, fingerprints)
}

/// Parse a mail address header like `"Max Beispiel"<mbeispiel@x.de>,"S H"<s@y.de>`
/// (Zoho's `fromEmailAddress`/`to`/`cc` shape) into `(display_name, email)`
/// pairs. The display name is the signature name that repeats in every message
/// — registering it lets the masker tokenise it everywhere, not just where the
/// structured contact appears. Split on commas (emails never contain them;
/// quoted names with commas are vanishingly rare in this data).
pub fn parse_named_addresses(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for seg in s.split(',') {
        if let Some(lt) = seg.find('<') {
            let name = seg[..lt].trim().trim_matches('"').trim().to_string();
            let email = seg[lt + 1..].split('>').next().unwrap_or("").trim().to_string();
            out.push((name, email));
        } else {
            let t = seg.trim().trim_matches('"').trim();
            if t.contains('@') {
                out.push((String::new(), t.to_string()));
            }
        }
    }
    out
}

/// REVEAL side of `scrub_pii_regex`. Re-runs the exact same deterministic
/// patterns over the *raw source* and returns the `token → original` pairs the
/// masker would have produced. Used at DISPLAY time to un-mask a stored summary
/// for an authorised viewer: because `obfuscate_pii` is a pure function of
/// (value, label) + the SYNC_SECRET pepper, re-hashing the raw value yields the
/// identical token that was baked into the summary — so we can swap it back with
/// NO plaintext vault. Low-privilege viewers simply never get this map applied.
///
/// Returned longest-original-first so a longer match (e.g. full email) is
/// revealed before any shorter substring token.
pub fn pprl_reveal_map(text: &str) -> Vec<(String, String)> {
    let patterns = pii_patterns();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (label, re) in patterns {
        for m in re.find_iter(text) {
            let original = m.as_str().trim();
            if original.is_empty() {
                continue;
            }
            let token = obfuscate_pii(original, label);
            if seen.insert(token.clone()) {
                pairs.push((token, original.to_string()));
            }
        }
    }
    pairs.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    pairs
}

/// Every pseudonym token shape this module can emit — the label set must stay
/// in sync with `pii_patterns` labels plus the LLM/dictionary labels used by
/// the wms maskers (Name/Company come from those, not from regex patterns).
fn token_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:Name|Email|Phone|Address|Company|Iban|VatId|Card)_[0-9A-F]{16}\b")
            .unwrap()
    })
}

/// Collect every pseudonym token present in `text` (first-seen order, deduped).
/// Used to derive a document's `pii_fingerprints` from its already-masked
/// fields — pure text scan, no pepper needed, so ANY node can derive it
/// locally from meshed content.
pub fn extract_pii_tokens(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in token_regex().find_iter(text) {
        let t = m.as_str().to_string();
        if seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out
}

/// If `s` (trimmed) is EXACTLY one pseudonym token, return its type label
/// ("Name", "Email", …). Lets query surfaces route token-shaped input to an
/// exact-match path instead of feeding it to full-text/vector retrieval,
/// where a hash token can only produce noise.
pub fn parse_pii_token(s: &str) -> Option<&str> {
    let s = s.trim();
    let m = token_regex().find(s)?;
    if m.start() != 0 || m.end() != s.len() {
        return None;
    }
    s.split('_').next()
}

/// Lowercased common first names (DACH-heavy + international), embedded at
/// compile time — the trigger lexicon for [`mask_person_names_heuristic`].
fn first_name_lexicon() -> &'static std::collections::HashSet<&'static str> {
    static SET: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        include_str!("first_names.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect()
    })
}

/// Capitalized words that follow a first name in this domain's subject lines
/// WITHOUT being a surname — German nouns (always capitalized) and product
/// terms. They terminate a masked span so "Simone Model 770" keeps its
/// product/search signal; the lone first name is still masked.
fn surname_stoplist() -> &'static std::collections::HashSet<&'static str> {
    static SET: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "angebot", "rechnung", "reparatur", "kostenvoranschlag", "service",
            "wartung", "termin", "lieferung", "lieferschein", "bestellung",
            "retoure", "abholung", "versand", "garantie", "anfrage", "gerät",
            "geraet", "waage", "akku", "netzteil", "drucker", "fehler",
            "defekt", "ersatzteil", "ersatzteile", "kalibrierung", "software",
            "update", "fehlermeldung", "rücksendung", "ruecksendung",
            "austausch", "einweisung", "schulung", "vertrag", "kündigung",
            "kuendigung", "mahnung", "zahlung", "gutschrift", "storno",
            "messung", "messungen", "analyse", "analysen", "ergebnis",
            "ergebnisse", "daten", "bericht", "fehlercode", "abbruch",
            "problem", "ausfall", "störung", "stoerung", "wiegen",
            "gewicht", "display", "monitor", "sensor", "batterie",
        ]
        .into_iter()
        .collect()
    })
}

/// Extra span-terminator tokens sourced from deployment config
/// (env `ECK_BRAND_STOPLIST`, comma-separated lowercase brand tokens; EMPTY by
/// default). Merged with [`surname_stoplist`] at match time so the operating
/// company's product brand — which used to be hardcoded in the stoplist — is
/// configuration, not code. Read once (OnceLock).
fn brand_stoplist() -> &'static std::collections::HashSet<String> {
    static SET: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        std::env::var("ECK_BRAND_STOPLIST")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Lowercased substrings marking the OPERATING COMPANY'S OWN address (its HQ
/// header printed on every form/invoice) — not customer PII, so address maskers
/// and the attachment-geo lever skip any line/candidate carrying one. Sourced
/// from env `ECK_OWN_ADDRESS_MARKERS` (comma-separated substrings); EMPTY by
/// default (skip nothing). Shared by wms `ai::embeddings` and
/// `services::attachment_geo`. Read once (OnceLock).
pub fn own_address_markers() -> &'static [String] {
    static SET: OnceLock<Vec<String>> = OnceLock::new();
    SET.get_or_init(|| {
        std::env::var("ECK_OWN_ADDRESS_MARKERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Organization markers: a person-shaped span IMMEDIATELY followed by one of
/// these is a BRAND, not a person ("Emil Krause Fitness", "Anna Schmidt
/// GmbH") — company names stay in the clear by policy, and masking them
/// breaks business search (`ticket_search "Emil Krause"` must keep finding
/// the club's tickets). Such a span is not masked at all. Checked
/// case-insensitively against the word right after the candidate span.
fn org_marker() -> &'static std::collections::HashSet<&'static str> {
    static SET: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "fitness", "studio", "gym", "sport", "sports", "club", "verein",
            "zentrum", "center", "centre", "centrum", "gmbh", "ag", "kg",
            "ug", "ohg", "gbr", "mbh", "kgaa", "co", "praxis", "klinik",
            "klinikum", "apotheke", "hotel", "spa", "wellness", "resort",
            "akademie", "institut", "gesellschaft", "gruppe", "group",
            "holding", "consulting", "solutions", "systems", "team",
            "physio", "physiotherapie", "kosmetik",
        ]
        .into_iter()
        .collect()
    })
}

/// Plausibility gate for a MODEL-proposed person span before it is masked.
/// NER models mis-tag ordinary German phrases as PER on short, context-free
/// subject lines ("Messungen brechen" — noun + verb — observed live on ticket
/// 14371): German capitalizes every noun, so capitalization alone is no
/// person signal. The gate is deterministic:
///   * every word must be capitalized (noble connectors von/de/… excepted) —
///     a lowercase verb/adjective inside the span is the signature of a
///     sentence fragment, never of a name;
///   * no word may be a known domain/common noun (`surname_stoplist`) or an
///     org marker;
///   * a SINGLE-word span must be a known first name (lexicon) — one
///     capitalized word carries no structure to judge by.
/// Rejecting here only leaves the value unmasked (fail-open toward the
/// heuristic + regex layers); it never blocks dictionary or regex masking.
pub fn plausible_person_name(span: &str) -> bool {
    plausible_person_name_with(span, brand_stoplist())
}

fn plausible_person_name_with(span: &str, brand: &std::collections::HashSet<String>) -> bool {
    const CONNECTORS: &[&str] = &["von", "van", "de", "der", "den", "zu", "da", "di", "del", "dos", "al"];
    let words: Vec<&str> = span
        .split_whitespace()
        .filter(|w| !CONNECTORS.contains(&w.to_lowercase().as_str()))
        .collect();
    if words.is_empty() {
        return false;
    }
    for w in &words {
        let bare = w.trim_matches(|c: char| !c.is_alphanumeric());
        if bare.chars().count() < 2 {
            return false;
        }
        if !bare.chars().next().is_some_and(|c| c.is_uppercase()) {
            return false;
        }
        let lower = bare.to_lowercase();
        if surname_stoplist().contains(lower.as_str())
            || brand.contains(lower.as_str())
            || org_marker().contains(lower.as_str())
        {
            return false;
        }
    }
    if words.len() == 1 {
        let lower = words[0]
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        return first_name_lexicon().contains(lower.as_str());
    }
    true
}

/// Heuristic person-name masker — the NER-gap backstop for THIRD-PARTY names
/// that no structured dictionary can know (an end-customer named only in a
/// ticket subject, e.g. a bare "Erika Musterfrau" subject on a ticket whose
/// contact is the dealer). Two triggers, both high-precision:
///
///   1. a personal title (Frau/Herr/Hr./Fr./Mr/Mrs/…, optionally followed by
///      academic titles) followed by 1-2 capitalized words → the words after
///      the title are masked (the title itself stays);
///   2. a capitalized word from the embedded first-name lexicon, optionally
///      followed by noble connectors (von/van/de/…) and up to 2 further
///      capitalized words → the whole span is masked as ONE `Name_…` token.
///
/// The stoplist (plus any brand tokens from `ECK_BRAND_STOPLIST`) keeps German
/// domain nouns and product brands ("Rechnung", a device brand) out of the
/// surname slot, so over-masking stays bounded — and where it does over-mask,
/// the token is still revealable (deterministic hash of the matched span).
/// Words touching `_` or alphanumerics are skipped, which makes the pass
/// idempotent over already-baked `Name_<hex>` tokens. Returns the masked text
/// plus every `(token, original-span)` pair for the reveal dictionary.
pub fn mask_person_names_heuristic(text: &str) -> (String, Vec<(String, String)>) {
    mask_person_names_heuristic_with(text, brand_stoplist())
}

fn mask_person_names_heuristic_with(
    text: &str,
    brand: &std::collections::HashSet<String>,
) -> (String, Vec<(String, String)>) {
    struct W {
        start: usize,
        end: usize,
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut words: Vec<W> = Vec::new();
    let mut k = 0;
    while k < chars.len() {
        if chars[k].1.is_alphabetic() {
            let run_start = k;
            let mut j = k;
            while j < chars.len()
                && (chars[j].1.is_alphabetic() || chars[j].1 == '-' || chars[j].1 == '\'')
            {
                j += 1;
            }
            // Trim trailing hyphens/apostrophes off the word.
            let mut endk = j;
            while endk > run_start && !chars[endk - 1].1.is_alphabetic() {
                endk -= 1;
            }
            // Reject words glued to `_`, digits or letters (hex fragments and
            // baked pseudonym tokens) — they are never natural-language names.
            let before_ok = run_start == 0 || {
                let pc = chars[run_start - 1].1;
                pc != '_' && !pc.is_alphanumeric()
            };
            let after_ok = endk >= chars.len() || {
                let nc = chars[endk].1;
                nc != '_' && !nc.is_alphanumeric()
            };
            if before_ok && after_ok && endk > run_start {
                words.push(W {
                    start: chars[run_start].0,
                    end: if endk < chars.len() { chars[endk].0 } else { text.len() },
                });
            }
            k = j;
        } else {
            k += 1;
        }
    }

    let word = |w: &W| &text[w.start..w.end];
    let is_cap = |w: &W| {
        let s = word(w);
        s.chars().count() >= 2 && s.chars().next().is_some_and(|c| c.is_uppercase())
    };
    let is_first_name = |w: &W| {
        is_cap(w)
            && word(w)
                .split('-')
                .all(|seg| !seg.is_empty() && first_name_lexicon().contains(seg.to_lowercase().as_str()))
    };
    let is_title = |w: &W| {
        is_cap(w)
            && matches!(
                word(w).to_lowercase().as_str(),
                "frau" | "herr" | "hr" | "fr" | "fam" | "familie" | "mr" | "mrs" | "ms"
            )
    };
    let is_academic = |w: &W| {
        matches!(word(w).to_lowercase().as_str(), "dr" | "prof" | "med" | "dipl" | "ing" | "mag")
    };
    let is_connector = |w: &W| {
        matches!(word(w), "von" | "van" | "de" | "der" | "den" | "zu" | "da" | "di" | "del" | "dos" | "al")
    };
    let is_stop = |w: &W| {
        let lw = word(w).to_lowercase();
        surname_stoplist().contains(lw.as_str()) || brand.contains(lw.as_str())
    };
    let is_org = |w: &W| org_marker().contains(word(w).to_lowercase().as_str());
    // Words belong to one span only if nothing but whitespace separates them —
    // a comma/dash/paren ends the name. Directly after a title/academic word a
    // single abbreviation dot is also allowed ("Hr. Müller", "Dr. Weber").
    let gap_ok = |a: &W, b: &W| text[a.end..b.start].chars().all(char::is_whitespace);
    let gap_dot = |a: &W, b: &W| {
        text[a.end..b.start].chars().all(|c| c.is_whitespace() || c == '.')
    };
    let name_word = |w: &W| is_cap(w) && !is_stop(w) && !is_org(w) && !is_title(w) && !is_academic(w);

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        // Title trigger: mask the 1-2 capitalized words after the title.
        if is_title(&words[i]) {
            let mut j = i + 1;
            while j < words.len() && gap_dot(&words[j - 1], &words[j]) && is_academic(&words[j]) {
                j += 1;
            }
            let first = j;
            let mut last: Option<usize> = None;
            while j < words.len()
                && j - first < 2
                && (if j == first {
                    gap_dot(&words[j - 1], &words[j])
                } else {
                    gap_ok(&words[j - 1], &words[j])
                })
                && name_word(&words[j])
            {
                last = Some(j);
                j += 1;
            }
            if let Some(l) = last {
                spans.push((words[first].start, words[l].end));
                i = l + 1;
                continue;
            }
            i += 1;
            continue;
        }
        // Lexicon trigger: first name + connectors + up to 2 capitalized words.
        if is_first_name(&words[i]) {
            let mut last = i;
            let mut extra = 0;
            let mut j = i + 1;
            while j < words.len() && extra < 2 && gap_ok(&words[j - 1], &words[j]) {
                if is_connector(&words[j]) {
                    j += 1;
                    continue;
                }
                if name_word(&words[j]) {
                    last = j;
                    extra += 1;
                    j += 1;
                    continue;
                }
                break;
            }
            // Brand veto: an org marker right after the span means the whole
            // "name" is an organization ("Emil Krause Fitness") — leave it in
            // the clear, business search depends on it.
            let brand = last + 1 < words.len()
                && gap_ok(&words[last], &words[last + 1])
                && is_org(&words[last + 1]);
            if !brand {
                spans.push((words[i].start, words[last].end));
            }
            i = last + 1;
            continue;
        }
        i += 1;
    }

    if spans.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let mut out = String::with_capacity(text.len());
    let mut pairs = Vec::new();
    let mut pos = 0;
    for (s, e) in spans {
        out.push_str(&text[pos..s]);
        let original = &text[s..e];
        let token = obfuscate_pii(original, "Name");
        out.push_str(&token);
        pairs.push((token, original.to_string()));
        pos = e;
    }
    out.push_str(&text[pos..]);
    (out, pairs)
}

#[allow(clippy::type_complexity)]
fn pii_patterns() -> &'static [(&'static str, Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Email — RFC-ish, high confidence.
            ("Email", Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap()),
            // IBAN — 2 letters + 2 check digits + 11..30 alnum (covers DE + EU).
            ("Iban", Regex::new(r"\b[A-Z]{2}\d{2}[A-Z0-9]{11,30}\b").unwrap()),
            // German VAT-Id (USt-IdNr).
            ("VatId", Regex::new(r"\bDE\d{9}\b").unwrap()),
            // Payment card — 4-4-4-(2..4) with explicit space/dash separators.
            ("Card", Regex::new(r"\b\d{4}[ \-]\d{4}[ \-]\d{4}[ \-]\d{2,4}\b").unwrap()),
            // International phone: leading + then 7..16 digits/separators.
            ("Phone", Regex::new(r"\+\d[\d\s().\-/]{6,}\d").unwrap()),
            // National phone: leading 0, then at least one separator group —
            // requires a separator so bare serial/order numbers don't match.
            ("Phone", Regex::new(r"\b0\d{1,4}[\s().\-/]\d{2,4}(?:[\s().\-/]?\d{2,4}){1,5}").unwrap()),
            // German street + house number. A capitalized street name ending in
            // (or followed by) a common street suffix, then a mandatory house
            // number. The house number is REQUIRED so a bare "Bahnhofstraße"
            // (no number) is left alone, and the pattern stops before the zip so
            // "Musterstraße 42, 12345 Berlin" masks only "Musterstraße 42" —
            // city + zip stay in the clear (coarse geo, not PII). Suffix matching
            // is case-insensitive (`(?i:…)`) to catch both glued lowercase forms
            // ("Bahnhofstraße", "Hauptweg") and standalone capitalized ones
            // ("Berliner Allee", "Am Markt"); the leading capital keeps it to
            // proper nouns. Masked as `Address_<hash>`, same token as the
            // structured/LLM address paths.
            ("Address", Regex::new(
                r"\b(?:(?:[A-ZÄÖÜ][A-Za-zÄÖÜäöüß.\-]*\s+){1,2}[A-Za-zÄÖÜäöüß.\-]*(?i:straße|strasse|str\.|weg|allee|platz|gasse|ring|damm|ufer|chaussee|steig|stieg|pfad|hof|markt)|[A-ZÄÖÜ][A-Za-zÄÖÜäöüß.\-]*(?i:straße|strasse|str\.|weg|allee|platz|gasse|ring|damm|ufer|chaussee|steig|stieg|pfad|hof|markt))\s+\d+[a-zA-Z]?(?:-\d+)?"
            ).unwrap()),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        std::env::set_var("SYNC_SECRET", "test_secret");
    }

    #[test]
    fn similar_strings_have_low_hamming_distance() {
        setup();
        let h1 = simhash("Hans Müller Berlin");
        let h2 = simhash("Hans Mueller Berlin");
        let distance = (h1 ^ h2).count_ones();
        assert!(distance < 25, "Hamming distance too high: {distance}");
    }

    #[test]
    fn different_strings_have_high_hamming_distance() {
        setup();
        let h1 = simhash("Hans Müller");
        let h2 = simhash("ZetaBody 770 Reparatur");
        let distance = (h1 ^ h2).count_ones();
        assert!(distance > 10, "Hamming distance too low for dissimilar strings: {distance}");
    }

    #[test]
    fn obfuscate_format() {
        setup();
        let token = obfuscate_pii("Ivan Petrov", "Name");
        assert!(token.starts_with("Name_"));
        assert_eq!(token.len(), 5 + 16); // "Name_" + 16 hex chars
    }

    #[test]
    fn deterministic_with_same_pepper() {
        setup();
        let h1 = simhash("Test Person");
        let h2 = simhash("Test Person");
        assert_eq!(h1, h2, "Same input + same pepper must produce same hash");
    }

    #[test]
    fn regex_backstop_catches_email_phone_iban() {
        setup();
        let text = "Bitte an hans.mueller@example.de schreiben oder +49 30 1234567 anrufen. \
                    IBAN DE89370400440532013000, UStID DE123456789.";
        let (scrubbed, fps) = scrub_pii_regex(text);
        assert!(!scrubbed.contains("hans.mueller@example.de"), "email leaked: {scrubbed}");
        assert!(!scrubbed.contains("DE89370400440532013000"), "IBAN leaked: {scrubbed}");
        assert!(!scrubbed.contains("DE123456789"), "VAT-Id leaked: {scrubbed}");
        assert!(!scrubbed.contains("+49 30 1234567"), "phone leaked: {scrubbed}");
        assert!(scrubbed.contains("Email_"), "no email token: {scrubbed}");
        assert!(scrubbed.contains("Iban_"), "no IBAN token: {scrubbed}");
        assert!(fps.len() >= 4, "expected >=4 fingerprints, got {}: {fps:?}", fps.len());
    }

    #[test]
    fn regex_backstop_preserves_serials_and_order_numbers() {
        setup();
        // Bare digit runs (serial, order no, device model) must NOT be masked —
        // they carry search signal and are not PII.
        let text = "ZetaBody 770, Serial 12345678, Auftrag 90011234, Status offen.";
        let (scrubbed, fps) = scrub_pii_regex(text);
        assert_eq!(scrubbed, text, "non-PII digits were masked: {scrubbed}");
        assert!(fps.is_empty(), "false-positive fingerprints: {fps:?}");
    }

    #[test]
    fn regex_backstop_is_deterministic_and_idempotent() {
        setup();
        let text = "mail: a@b.de";
        let (once, _) = scrub_pii_regex(text);
        let (twice, fps2) = scrub_pii_regex(&once);
        assert_eq!(once, twice, "second pass changed already-scrubbed text");
        assert!(fps2.is_empty(), "token re-matched as PII: {fps2:?}");
    }

    #[test]
    fn street_pattern_masks_street_keeps_zip_city() {
        setup();
        // Only the street+number is PII; the zip+city are coarse geo and stay.
        let text = "Lieferung an Musterstraße 42, 12345 Berlin bitte.";
        let (scrubbed, fps) = scrub_pii_regex(text);
        assert!(!scrubbed.contains("Musterstraße 42"), "street leaked: {scrubbed}");
        assert!(scrubbed.contains("Address_"), "no Address token: {scrubbed}");
        assert!(scrubbed.contains("12345 Berlin"), "zip+city must stay: {scrubbed}");
        assert!(
            fps.iter().any(|f| f.starts_with("Address_")),
            "expected an Address fingerprint, got {fps:?}"
        );
    }

    #[test]
    fn street_pattern_masks_abbrev_and_letter_suffix() {
        setup();
        // Abbreviated "str." + a letter-suffixed house number "12a".
        let text = "Bahnhofstr. 12a";
        let (scrubbed, _) = scrub_pii_regex(text);
        assert!(!scrubbed.contains("Bahnhofstr. 12a"), "abbrev street leaked: {scrubbed}");
        assert!(scrubbed.contains("Address_"), "no Address token: {scrubbed}");
    }

    #[test]
    fn street_pattern_masks_standalone_suffix_word() {
        setup();
        // Suffix as a separate capitalized word ("Berliner Allee").
        let text = "Praxis in Berliner Allee 5 gelegen.";
        let (scrubbed, _) = scrub_pii_regex(text);
        assert!(!scrubbed.contains("Berliner Allee 5"), "multi-word street leaked: {scrubbed}");
        assert!(scrubbed.contains("Address_"), "no Address token: {scrubbed}");
    }

    #[test]
    fn street_pattern_ignores_street_without_number() {
        setup();
        // No house number → not an address, must be left untouched.
        let text = "Die Bahnhofstraße ist gesperrt.";
        let (scrubbed, fps) = scrub_pii_regex(text);
        assert_eq!(scrubbed, text, "bare street (no number) was masked: {scrubbed}");
        assert!(fps.is_empty(), "false-positive fingerprints: {fps:?}");
    }

    #[test]
    fn street_pattern_ignores_zip_city() {
        setup();
        // A bare "12345 Berlin" has no street suffix and must not be masked.
        let text = "PLZ-Ort: 12345 Berlin";
        let (scrubbed, fps) = scrub_pii_regex(text);
        assert_eq!(scrubbed, text, "zip+city was masked: {scrubbed}");
        assert!(fps.is_empty(), "false-positive fingerprints: {fps:?}");
    }

    #[test]
    fn street_pattern_is_idempotent() {
        setup();
        let text = "Adresse: Musterstraße 42, 12345 Berlin";
        let (once, _) = scrub_pii_regex(text);
        let (twice, fps2) = scrub_pii_regex(&once);
        assert_eq!(once, twice, "second pass changed already-scrubbed text: {twice}");
        assert!(fps2.is_empty(), "Address token re-matched as PII: {fps2:?}");
    }

    #[test]
    fn extract_tokens_dedups_and_keeps_order() {
        let text = "Contact: Name_5AC7269A88303052 <Email_D7EACC17A65A5553>, \
                    again Name_5AC7269A88303052, phone Phone_0123456789ABCDEF.";
        let tokens = extract_pii_tokens(text);
        assert_eq!(
            tokens,
            vec![
                "Name_5AC7269A88303052".to_string(),
                "Email_D7EACC17A65A5553".to_string(),
                "Phone_0123456789ABCDEF".to_string(),
            ]
        );
    }

    #[test]
    fn extract_tokens_ignores_lookalikes() {
        // Wrong label, lowercase hex, wrong length — none are our tokens.
        let text = "Foo_5AC7269A88303052 Name_5ac7269a88303052 Name_5AC7 Serial_123";
        assert!(extract_pii_tokens(text).is_empty());
    }

    #[test]
    fn heuristic_masks_third_party_full_name() {
        setup();
        // The Katharina case: the WHOLE subject is an end-customer's name and
        // she is nobody's structured contact — only the lexicon can catch her.
        let (out, pairs) = mask_person_names_heuristic("Katharina Niedermayer");
        assert!(!out.contains("Katharina"), "first name leaked: {out}");
        assert!(!out.contains("Niedermayer"), "last name leaked: {out}");
        assert!(out.starts_with("Name_"), "no Name token: {out}");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, "Katharina Niedermayer");
        assert_eq!(pairs[0].0, obfuscate_pii("Katharina Niedermayer", "Name"));
    }

    #[test]
    fn heuristic_title_trigger_masks_unknown_surname() {
        setup();
        // "Musterfrau" is in no lexicon — the title is the trigger.
        let (out, _) = mask_person_names_heuristic("Rückfrage Frau Musterfrau wegen Termin");
        assert!(!out.contains("Musterfrau"), "surname leaked: {out}");
        assert!(out.starts_with("Rückfrage Frau Name_"), "unexpected shape: {out}");
        assert!(out.ends_with("wegen Termin"), "context damaged: {out}");
        // Academic titles between personal title and name are skipped, not masked.
        let (out2, _) = mask_person_names_heuristic("Herr Dr. Weber");
        assert!(!out2.contains("Weber"), "surname leaked: {out2}");
        assert!(out2.starts_with("Herr Dr. Name_"), "unexpected shape: {out2}");
    }

    #[test]
    fn heuristic_stoplist_preserves_product_words() {
        setup();
        // The lone first name is masked but the product span survives — the
        // product brand is supplied via the injected brand stoplist (the pure
        // equivalent of setting env `ECK_BRAND_STOPLIST=zetabody`).
        let brand: std::collections::HashSet<String> =
            ["zetabody".to_string()].into_iter().collect();
        let (out, _) = mask_person_names_heuristic_with("Simone ZetaBody 770 defekt", &brand);
        assert!(!out.contains("Simone"), "first name leaked: {out}");
        assert!(out.contains("ZetaBody 770 defekt"), "product signal damaged: {out}");
    }

    #[test]
    fn heuristic_leaves_non_names_alone() {
        setup();
        for s in [
            "Reparatur ZetaBody 770 dringend",
            "SWC Eferding GmbH Angebot",
            "Kostenvoranschlag Waage",
            "Die Bahnhofstraße ist gesperrt.",
        ] {
            let (out, pairs) = mask_person_names_heuristic(s);
            assert_eq!(out, s, "false positive on: {s}");
            assert!(pairs.is_empty());
        }
    }

    #[test]
    fn plausible_person_gate_rejects_german_phrases() {
        setup();
        // The ticket-14371 regression: NER tagged "Messungen brechen" as PER.
        assert!(!plausible_person_name("Messungen brechen"), "noun+verb passed");
        assert!(!plausible_person_name("Fehlermeldung Waage"), "noun pair passed");
        assert!(!plausible_person_name("brechen"), "lowercase word passed");
        assert!(!plausible_person_name("Messungen"), "domain noun passed");
        // Real names — lexicon and non-lexicon — pass.
        assert!(plausible_person_name("Katharina Niedermayer"));
        assert!(plausible_person_name("Zbigniewa Kowalczyk"), "non-lexicon full name rejected");
        assert!(plausible_person_name("Anna von Berg"));
        // Single word: only a known first name qualifies.
        assert!(plausible_person_name("Katharina"));
        assert!(!plausible_person_name("Niedermayer"), "lone unknown word passed");
    }

    #[test]
    fn heuristic_brand_veto_keeps_org_names_clear() {
        setup();
        // Person-shaped BRANDS (org marker right after the span) stay in the
        // clear — company names are policy-clear and business search must
        // keep finding them (the ticket-32115 regression).
        for s in [
            "Analyseabbruch/Fehlermeldung ZetaBody 970 - Emil Krause Fitness am Schillerplatz, 1010 Wien",
            "Anna Schmidt GmbH Rechnung offen",
            "Angebot für Emil Krause Fitness",
        ] {
            let (out, pairs) = mask_person_names_heuristic(s);
            assert_eq!(out, s, "brand masked: {s}");
            assert!(pairs.is_empty(), "brand fingerprinted: {pairs:?}");
        }
        // ...while a person before a NEUTRAL noun (product, not org) is
        // still masked.
        let (out, _) = mask_person_names_heuristic("Simone ZetaBody 770 defekt");
        assert!(!out.contains("Simone"), "person leaked: {out}");
    }

    #[test]
    fn heuristic_connector_and_comma_boundary() {
        setup();
        // Noble connector joins the span; a comma terminates it.
        let (out, pairs) = mask_person_names_heuristic("Anna von Berg, dringend");
        assert!(!out.contains("Anna"), "name leaked: {out}");
        assert!(!out.contains("Berg"), "surname leaked: {out}");
        assert!(out.ends_with(", dringend"), "comma boundary broken: {out}");
        assert_eq!(pairs[0].1, "Anna von Berg");
    }

    #[test]
    fn heuristic_is_idempotent_over_baked_tokens() {
        setup();
        let (once, _) = mask_person_names_heuristic("Frau Katharina Niedermayer und Herr Weber");
        let (twice, pairs2) = mask_person_names_heuristic(&once);
        assert_eq!(once, twice, "second pass changed masked text: {twice}");
        assert!(pairs2.is_empty(), "token re-matched as a name: {pairs2:?}");
    }

    #[test]
    fn parse_token_full_match_only() {
        assert_eq!(parse_pii_token("Name_5AC7269A88303052"), Some("Name"));
        assert_eq!(parse_pii_token("  Email_D7EACC17A65A5553 "), Some("Email"));
        // Token embedded in a longer query is a full-text query, not a token.
        assert_eq!(parse_pii_token("who is Name_5AC7269A88303052?"), None);
        assert_eq!(parse_pii_token("Katharina"), None);
    }
}
