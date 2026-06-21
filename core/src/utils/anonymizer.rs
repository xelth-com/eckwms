use sha2::{Sha256, Digest};
use std::sync::OnceLock;
use regex::Regex;

/// Compute a 64-bit SimHash from character bigrams, keyed with SYNC_SECRET as pepper.
/// Similar strings produce hashes with low Hamming distance,
/// enabling privacy-preserving record linkage (PPRL).
/// The pepper prevents dictionary attacks on the hash tokens.
fn simhash(text: &str) -> u64 {
    let pepper = std::env::var("SYNC_SECRET").unwrap_or_else(|_| "eck_default_pepper".to_string());
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
        let h2 = simhash("InBody 770 Reparatur");
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
        let text = "InBody 770, Serial 12345678, Auftrag 90011234, Status offen.";
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
}
