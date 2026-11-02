use sha2::{Sha256, Digest};
use std::sync::{Arc, OnceLock, Mutex};
use std::collections::HashMap;
use zeroize::Zeroizing;
use serde::{Serialize, Deserialize};
use regex::Regex;

/// Compute a 64-bit SimHash from character bigrams, keyed with SYNC_SECRET as pepper.
/// Similar strings produce hashes with low Hamming distance,
/// enabling privacy-preserving record linkage (PPRL).
/// The pepper prevents dictionary attacks on the hash tokens.
fn simhash(text: &str) -> u64 {
    // The pepper MUST be configured — there is deliberately NO fallback
    // default. A known/public pepper makes the obfuscated PII tokens reversible
    // by dictionary attack (hash candidate names with the known pepper, match
    // tokens), which would void the Zero-Knowledge / "PII never leaves in
    // plaintext" guarantee. Fail closed rather than ship reversible anonymisation.
    // Sourced from the process-global KeyRing (authority / keyfile / env
    // SYNC_SECRET); env-only config gives the identical bytes → identical tokens.
    let pepper = crate::keyring::pepper().expect(
        "SYNC_SECRET (PPRL pepper) is unset — refusing to anonymise PII with a \
         publicly-known default pepper (it would be reversible). Set SYNC_SECRET.",
    );
    let pepper = pepper.as_slice();
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();

    if chars.len() < 2 {
        // Too short for bigrams — keyed SHA256 hash directly
        let mut hasher = Sha256::new();
        hasher.update(lower.as_bytes());
        hasher.update(pepper);
        let digest = hasher.finalize();
        return u64::from_be_bytes(digest[0..8].try_into().unwrap());
    }

    let mut v = [0i32; 64];

    for window in chars.windows(2) {
        let bigram: String = window.iter().collect();
        let mut hasher = Sha256::new();
        hasher.update(bigram.as_bytes());
        hasher.update(pepper);
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
///
/// Side effect (Name only): when the value carries ≥ 2 letters, its fuzzy
/// signature is recorded in the process-wide MINT LOG keyed by the emitted
/// token. That lets Channel B derive a `FuzzySig` for tokens that appear ONLY
/// as `Name_<hex>` in NER/LLM-extracted prose (no structured clear field to
/// re-derive from) — see [`fuzzy_sig_for_token`] and `derive_pii_fuzzy`.
///
/// Idempotent: if `text` is ALREADY exactly one PPRL token, it is returned
/// (trimmed) unchanged, and NO MINT-LOG signature is recorded for it. The vault
/// tokenizes stored meta at ingest (`services::support::tokenize_meta_into_vault`),
/// so downstream maskers — the summariser prompt scrubber, the embedding
/// ident/address maskers — routinely re-apply this to a value that is already a
/// token. Re-hashing a token would mint a second-level `Address_<hash-of-token>`
/// that decodes to nothing, and a MINT-LOG entry for it would poison fuzzy
/// lookup. The class label is deliberately ignored here: a token is opaque no
/// matter which field slot re-masks it, so a class mismatch still returns it
/// untouched.
pub fn obfuscate_pii(text: &str, pii_type: &str) -> String {
    if parse_pii_token(text).is_some() {
        return text.trim().to_string();
    }
    let token = format!("{}_{:016X}", pii_type, simhash(text));
    if text.chars().filter(|c| c.is_alphabetic()).count() >= 2 {
        match pii_type {
            "Name" => note_pii_sig(&token, text, SigKind::Name),
            "Address" => note_pii_sig(&token, text, SigKind::Address),
            _ => {}
        }
    }
    token
}

// ─── PPRL fuzzy name resolution (typo-tolerant person lookup) ────────────────
// Per-word 256-bit Bloom over padded character bigrams (k=2) scored by Dice,
// OR-ed with a keyed Kölner-Phonetik word key — the `.eck/PPRL_FUZZY.md` design,
// bench-tuned in `.eck/bench/pprl_bench{,2}.py`. Threshold and 256-bit/k=2
// parameters are FIXED by the bench; do not retune.

/// Similarity threshold for a fuzzy name match (bench winner — do not retune).
pub const PII_FUZZY_THRESHOLD: f32 = 0.75;

/// The same fail-closed pepper `simhash` uses — a known/public pepper would make
/// the Bloom/phonetic signatures reversible by dictionary attack, so refuse to
/// derive one without `SYNC_SECRET`.
fn pprl_pepper() -> Arc<Zeroizing<Vec<u8>>> {
    crate::keyring::pepper().expect(
        "SYNC_SECRET (PPRL pepper) is unset — refusing to derive reversible \
         fuzzy PII signatures with a publicly-known default pepper. Set SYNC_SECRET.",
    )
}

/// Map an accented Latin letter to its base ASCII letter. Manual Latin-1
/// Supplement + Latin Extended-A table (no new crate) — the diacritic strip in
/// [`fold_name`], applied AFTER ß/ä/ö/ü have already been expanded. Returns
/// `None` for a char that is not an accented Latin letter (caller keeps it if it
/// is `[a-z0-9]`, drops it otherwise).
fn strip_diacritic(c: char) -> Option<char> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ţ' | 'ť' | 'ŧ' => 't',
        'ù' | 'ú' | 'û' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        _ => return None,
    })
}

/// Fold a name to comparable lowercase ASCII words: lowercase → ß→ss, ä/ö/ü→
/// ae/oe/ue → strip other diacritics → keep `[a-z0-9]`; split on whitespace,
/// `-`, `'`, `.`; drop words < 2 chars; dedupe preserving order.
pub fn fold_name(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut buf = String::with_capacity(lower.len());
    for c in lower.chars() {
        match c {
            'ß' => buf.push_str("ss"),
            'ä' => buf.push_str("ae"),
            'ö' => buf.push_str("oe"),
            'ü' => buf.push_str("ue"),
            // Explicit split characters become a boundary.
            ' ' | '\t' | '\n' | '\r' | '-' | '\'' | '.' => buf.push(' '),
            _ => {
                if let Some(base) = strip_diacritic(c) {
                    buf.push(base);
                } else if c.is_ascii_alphanumeric() {
                    buf.push(c);
                }
                // Any other char (punctuation, symbol) is dropped — keep [a-z0-9].
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for w in buf.split_whitespace() {
        if w.chars().count() < 2 {
            continue;
        }
        if seen.insert(w.to_string()) {
            out.push(w.to_string());
        }
    }
    out
}

/// What a [`FuzzySig`] describes — a person name or a postal address. Selects
/// the fold + per-word encoding and tags search output; scoring is unified
/// (see [`fuzzy_score`]). Serialized as `"Name"` / `"Address"`.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum SigKind {
    Name,
    Address,
}

impl SigKind {
    /// The `obfuscate_pii` label for this kind.
    pub fn label(self) -> &'static str {
        match self {
            SigKind::Name => "Name",
            SigKind::Address => "Address",
        }
    }
}

fn default_sig_kind() -> SigKind {
    SigKind::Name
}

/// Per-word peppered signature: a 256-bit Bloom over padded bigrams (as 64 hex)
/// and a keyed key (16 hex, or "" when absent). For an alphabetic word the key
/// is the keyed Kölner-Phonetik code; for a numeric/alphanumeric ADDRESS word
/// the Bloom is all-zero and the key is a keyed hash of the EXACT token, so it
/// matches only on exact equality (house numbers / zips must not fuzz).
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct WordSig {
    /// 64 hex = 256-bit bloom filter of padded character bigrams (all-zero for
    /// an exact-match numeric token).
    pub b: String,
    /// 16 hex keyed phonetic/exact key, or "" (never matches).
    pub p: String,
}

/// Fuzzy signature of a whole name or address — one [`WordSig`] per folded
/// word. JSON shape: `{ "k": "Name"|"Address", "w": [...] }`.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct FuzzySig {
    /// Discriminator (`k` in JSON); defaults to `Name` for pre-`kind` data.
    #[serde(rename = "k", default = "default_sig_kind")]
    pub kind: SigKind,
    pub w: Vec<WordSig>,
}

/// 256-bit Bloom (k=2) over the DEDUPED padded character bigrams of `word`.
/// `idx = u64::from_be_bytes(sha256([i] ++ bigram ++ pepper)[0..8]) % 256`.
fn bloom_word(word: &str, pepper: &[u8]) -> [u8; 32] {
    let padded = format!("_{word}_");
    let chars: Vec<char> = padded.chars().collect();
    let mut bits = [0u8; 32];
    for win in chars.windows(2) {
        let bigram: String = win.iter().collect();
        for i in 0u8..2 {
            let mut hasher = Sha256::new();
            hasher.update([i]);
            hasher.update(bigram.as_bytes());
            hasher.update(pepper);
            let digest = hasher.finalize();
            let idx = (u64::from_be_bytes(digest[0..8].try_into().unwrap()) % 256) as usize;
            bits[idx / 8] |= 1 << (idx % 8);
        }
    }
    bits
}

/// Kölner Phonetik code of a single word — ported VERBATIM from the bench
/// (`koelner` in `.eck/bench/pprl_bench.py`), including consecutive-dedupe,
/// inner-zero drop and `-` removal. Input is expected already folded; non-alpha
/// chars are dropped exactly as the bench does.
fn koelner(word: &str) -> String {
    let w: Vec<char> = word.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    let mut out = String::new();
    for i in 0..w.len() {
        let c = w[i];
        let p = if i > 0 { w[i - 1] } else { '\0' };
        let n = if i + 1 < w.len() { w[i + 1] } else { '\0' };
        let code: &str = match c {
            'a' | 'e' | 'i' | 'j' | 'o' | 'u' | 'y' => "0",
            'h' => "-",
            'b' => "1",
            'p' => {
                if n == 'h' {
                    "3"
                } else {
                    "1"
                }
            }
            'd' | 't' => {
                if matches!(n, 'c' | 's' | 'z') {
                    "8"
                } else {
                    "2"
                }
            }
            'f' | 'v' | 'w' => "3",
            'g' | 'k' | 'q' => "4",
            'c' => {
                if i == 0 {
                    if matches!(n, 'a' | 'h' | 'k' | 'l' | 'o' | 'q' | 'r' | 'u' | 'x') {
                        "4"
                    } else {
                        "8"
                    }
                } else if matches!(p, 's' | 'z') || !matches!(n, 'a' | 'h' | 'k' | 'o' | 'q' | 'u' | 'x') {
                    "8"
                } else {
                    "4"
                }
            }
            'x' => {
                if matches!(p, 'c' | 'k' | 'q') {
                    "8"
                } else {
                    "48"
                }
            }
            'l' => "5",
            'm' | 'n' => "6",
            'r' => "7",
            's' | 'z' => "8",
            _ => "",
        };
        out.push_str(code);
    }
    // Dedupe consecutive equal codes.
    let mut dedup = String::new();
    let mut prev: Option<char> = None;
    for ch in out.chars() {
        if prev != Some(ch) {
            dedup.push(ch);
        }
        prev = Some(ch);
    }
    // Keep the first char, drop inner '0's, then strip '-'.
    let mut res = String::new();
    for (i, ch) in dedup.chars().enumerate() {
        if i == 0 || ch != '0' {
            res.push(ch);
        }
    }
    res.replace('-', "")
}

/// Expand a purely-alphabetic German street/place abbreviation to its canonical
/// form. `str`/`strasse` (also glued: `hauptstr` → `hauptstrasse`) → `strasse`;
/// `pl`/`platz` → `platz`; `st` (from `st.`) → `sankt`. Everything else
/// (`weg`, `allee`, `gasse`, plain words) is returned unchanged.
fn expand_street_abbrev(w: &str) -> String {
    match w {
        "str" | "strasse" => return "strasse".to_string(),
        "pl" | "platz" => return "platz".to_string(),
        "st" => return "sankt".to_string(),
        _ => {}
    }
    if w.ends_with("strasse") {
        return w.to_string();
    }
    if let Some(stem) = w.strip_suffix("str") {
        if !stem.is_empty() {
            return format!("{stem}strasse");
        }
    }
    w.to_string()
}

/// Fold a postal address to comparable tokens (spec §F). Lowercase + umlaut/ß
/// folding, split letters from a following digit run (`str12` → `str 12`;
/// `12a` stays), split on `,` `/` `-` `.` and whitespace, expand street
/// abbreviations, KEEP numeric/alphanumeric tokens of ANY length (the ≥2-char
/// rule applies to purely alphabetic words only), drop country words, dedupe
/// preserving order.
pub fn fold_address(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut buf = String::with_capacity(lower.len());
    let mut prev_alpha = false;
    for c in lower.chars() {
        match c {
            'ß' => {
                buf.push_str("ss");
                prev_alpha = true;
            }
            'ä' => {
                buf.push_str("ae");
                prev_alpha = true;
            }
            'ö' => {
                buf.push_str("oe");
                prev_alpha = true;
            }
            'ü' => {
                buf.push_str("ue");
                prev_alpha = true;
            }
            ' ' | '\t' | '\n' | '\r' | ',' | '/' | '-' | '.' => {
                buf.push(' ');
                prev_alpha = false;
            }
            _ => {
                let mapped =
                    strip_diacritic(c).or_else(|| c.is_ascii_alphanumeric().then_some(c));
                if let Some(ch) = mapped {
                    // Split letters from a following digit (letter→digit only).
                    if ch.is_ascii_digit() && prev_alpha {
                        buf.push(' ');
                    }
                    buf.push(ch);
                    prev_alpha = ch.is_ascii_alphabetic();
                } else {
                    prev_alpha = false;
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in buf.split_whitespace() {
        let has_digit = raw.chars().any(|c| c.is_ascii_digit());
        let token = if has_digit {
            // Numeric / alphanumeric: keep verbatim at ANY length.
            raw.to_string()
        } else {
            let t = expand_street_abbrev(raw);
            // Drop country words + purely-alphabetic tokens shorter than 2 chars.
            if matches!(t.as_str(), "deutschland" | "germany" | "de") {
                continue;
            }
            if t.chars().count() < 2 {
                continue;
            }
            t
        };
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
    out
}

/// Build the per-word signature for one folded token under `kind`.
fn word_sig(word: &str, kind: SigKind, pepper: &[u8]) -> WordSig {
    let numeric_exact = kind == SigKind::Address && word.chars().any(|c| c.is_ascii_digit());
    if numeric_exact {
        // Exact-match token: zero bloom, keyed hash of the EXACT token. The
        // `num:` prefix keeps it from ever colliding with a phonetic code
        // (koelner codes are digit strings, never containing ':').
        let mut hasher = Sha256::new();
        hasher.update(b"num:");
        hasher.update(word.as_bytes());
        hasher.update(pepper);
        return WordSig {
            b: hex::encode([0u8; 32]),
            p: hex::encode(&hasher.finalize()[0..8]),
        };
    }
    let bits = bloom_word(word, pepper);
    let code = koelner(word);
    let p = if code.is_empty() {
        String::new()
    } else {
        let mut hasher = Sha256::new();
        hasher.update(code.as_bytes());
        hasher.update(pepper);
        hex::encode(&hasher.finalize()[0..8])
    };
    WordSig {
        b: hex::encode(bits),
        p,
    }
}

/// Derive the [`FuzzySig`] of a person name from its clear value (Name kind).
pub fn pii_fuzzy_sig(name: &str) -> FuzzySig {
    pii_fuzzy_sig_kind(name, SigKind::Name)
}

/// Derive the [`FuzzySig`] of `value` under `kind` (needs the pepper). Names use
/// [`fold_name`]; addresses use [`fold_address`] with exact-match numeric tokens.
pub fn pii_fuzzy_sig_kind(value: &str, kind: SigKind) -> FuzzySig {
    let pepper = pprl_pepper();
    let words = match kind {
        SigKind::Name => fold_name(value),
        SigKind::Address => fold_address(value),
    };
    let w = words
        .iter()
        .map(|word| word_sig(word, kind, pepper.as_slice()))
        .collect();
    FuzzySig { kind, w }
}

/// Build an Address [`FuzzySig`] from separate address components (street /
/// zip / city). The word list is the UNION of `fold_address` over each part in
/// the given order (deduped), so a city-only or zip-only query can hit the sig
/// even when the clear address line holds only the street. The stored *token*
/// stays derived from whatever exact string the reveal path hashes — this only
/// widens the sig's word set. Needs the pepper.
pub fn pii_fuzzy_sig_address(components: &[&str]) -> FuzzySig {
    let pepper = pprl_pepper();
    let mut seen = std::collections::HashSet::new();
    let mut words: Vec<String> = Vec::new();
    for c in components {
        for w in fold_address(c) {
            if seen.insert(w.clone()) {
                words.push(w);
            }
        }
    }
    let w = words
        .iter()
        .map(|word| word_sig(word, SigKind::Address, pepper.as_slice()))
        .collect();
    FuzzySig {
        kind: SigKind::Address,
        w,
    }
}

/// Dice coefficient over two Bloom-bit byte slices: `2·|a∧b| / (|a|+|b|)`.
/// Either side empty (no bits) → 0.
fn dice_bytes(a: &[u8], b: &[u8]) -> f32 {
    let ca: u32 = a.iter().map(|x| x.count_ones()).sum();
    let cb: u32 = b.iter().map(|x| x.count_ones()).sum();
    if ca + cb == 0 {
        return 0.0;
    }
    let inter: u32 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x & y).count_ones())
        .sum();
    2.0 * inter as f32 / (ca + cb) as f32
}

/// Fuzzy match score of query signature `q` against document signature `d`:
/// mean over `q.w` of the max over `d.w` of the per-word score. Per word:
///   * numeric/alphanumeric ADDRESS token (all-zero bloom) — 1.0 ONLY on an
///     exact keyed-token match, else 0 (house numbers / zips must not fuzz);
///   * alphabetic word — when the keyed phonetic keys are equal AND non-empty,
///     `Dice.max(0.85)`; else plain `Dice` of the Blooms.
/// The `.max(0.85)` floor keeps recall (any phonetic match clears the 0.75
/// threshold) while letting a spelling-exact pair (Dice = 1.0) outrank a
/// phonetic-only one (e.g. Müller vs Möller), so results prefer exact spelling.
/// Empty either side → 0. Directional — call as `fuzzy_score(query_sig, doc_sig)`.
pub fn fuzzy_score(q: &FuzzySig, d: &FuzzySig) -> f32 {
    if q.w.is_empty() || d.w.is_empty() {
        return 0.0;
    }
    let d_decoded: Vec<(Vec<u8>, &str)> = d
        .w
        .iter()
        .map(|dw| (hex::decode(&dw.b).unwrap_or_default(), dw.p.as_str()))
        .collect();
    let mut total = 0.0f32;
    for qw in &q.w {
        let qbits = hex::decode(&qw.b).unwrap_or_default();
        let numeric = qbits.iter().all(|&b| b == 0);
        let mut best = 0.0f32;
        for (dbits, dp) in &d_decoded {
            let phonetic_eq = !qw.p.is_empty() && qw.p.as_str() == *dp;
            let s = if numeric {
                // Exact-match token: full credit only on an exact keyed match.
                if phonetic_eq { 1.0 } else { 0.0 }
            } else if phonetic_eq {
                dice_bytes(&qbits, dbits).max(0.85)
            } else {
                dice_bytes(&qbits, dbits)
            };
            if s > best {
                best = s;
            }
        }
        total += best;
    }
    total / q.w.len() as f32
}

/// Process-wide MINT LOG: `Name_<hex>` / `Address_<hex>` token → its
/// [`FuzzySig`], populated by [`obfuscate_pii`] as names/addresses are masked.
/// Lets Channel B resolve tokens that live ONLY as pseudonyms in extracted
/// prose (no structured clear field). Capped at 50 000 entries; on
/// overflow the whole map is cleared and repopulated (a fresh sweep re-derives).
fn mint_log() -> &'static Mutex<HashMap<String, FuzzySig>> {
    static M: OnceLock<Mutex<HashMap<String, FuzzySig>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn note_pii_sig(token: &str, value: &str, kind: SigKind) {
    let sig = pii_fuzzy_sig_kind(value, kind);
    if let Ok(mut map) = mint_log().lock() {
        if map.len() >= 50_000 {
            map.clear();
        }
        map.insert(token.to_string(), sig);
    }
}

/// Look up the mint-logged [`FuzzySig`] for a `Name_<hex>` token, if this
/// process has masked that value since start.
pub fn fuzzy_sig_for_token(token: &str) -> Option<FuzzySig> {
    mint_log().lock().ok()?.get(token).cloned()
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
            if *label == "Phone" && is_date_like(&original) { continue; }
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
            if *label == "Phone" && is_date_like(original) {
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

/// A national-phone candidate that is really a DATE — `01.09.26`, `05.04.2026`,
/// `08.07.` (day.month with the year elided), or a `01.09.-05.09.` range half.
/// The national pattern (`0…` + separators) cannot tell them apart, and masking
/// a delivery/appointment date as `Phone_…` destroyed the one fact the ticket
/// carried (2026-09-07 bench, ticket 32791). Dates are not PII on their own.
fn is_date_like(s: &str) -> bool {
    static DATE: OnceLock<Regex> = OnceLock::new();
    let re = DATE.get_or_init(|| {
        Regex::new(r"^\d{1,2}[./]\d{1,2}(?:[./](?:\d{2}|\d{4})?)?[./]?$").unwrap()
    });
    re.is_match(s.trim())
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
    fn obfuscate_idempotent_on_matching_token() {
        setup();
        // An already-minted token re-masked under its own class comes back
        // verbatim (vault tokenizes stored meta at ingest; downstream maskers
        // re-apply this) — no second-level hash.
        let t = "Address_DEADBEEF00000000";
        assert_eq!(obfuscate_pii(t, "Address"), t);
    }

    #[test]
    fn obfuscate_idempotent_on_class_mismatch() {
        setup();
        // A token re-masked under a DIFFERENT class is STILL returned unchanged:
        // a token is opaque regardless of which field slot re-masks it.
        let t = "Name_CC0068898836CB06";
        assert_eq!(obfuscate_pii(t, "Address"), t);
    }

    #[test]
    fn obfuscate_mints_fresh_token_for_plain_text() {
        setup();
        let token = obfuscate_pii("Ivan Petrov", "Name");
        assert_eq!(parse_pii_token(&token), Some("Name"), "not a well-formed Name token: {token}");
        assert_ne!(token, "Ivan Petrov");
    }

    #[test]
    fn obfuscate_trims_whitespace_around_token() {
        setup();
        // A token with surrounding whitespace returns the TRIMMED token, not a
        // re-hash of the padded string.
        assert_eq!(
            obfuscate_pii("  Address_DEADBEEF00000000  ", "Address"),
            "Address_DEADBEEF00000000"
        );
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
    fn regex_backstop_leaves_dates_alone_but_masks_phones() {
        setup();
        let (scrubbed, toks) = scrub_pii_regex(
            "Gerät wird am 01.09.26 geliefert, Termin 05.04.2026, Abholung 08.07. bis 10.07.; Rückruf 0391 886 4384 oder 0176/64988533.",
        );
        assert!(scrubbed.contains("01.09.26"), "date masked: {scrubbed}");
        assert!(scrubbed.contains("05.04.2026"), "date masked: {scrubbed}");
        assert!(scrubbed.contains("08.07."), "date masked: {scrubbed}");
        assert!(!scrubbed.contains("0391 886 4384"), "phone leaked: {scrubbed}");
        assert!(!scrubbed.contains("64988533"), "phone leaked: {scrubbed}");
        assert_eq!(toks.len(), 2, "exactly the two phones: {toks:?}");
        assert!(is_date_like("01.09.26") && is_date_like("5.4.2026") && is_date_like("08.07.") && is_date_like("01/09/2026"));
        assert!(!is_date_like("0391 886 4384") && !is_date_like("0176/64988533"));
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

    // ── PPRL fuzzy name resolution (spec `.eck/PPRL_FUZZY.md` §A) ─────────────

    #[test]
    fn fold_name_splits_dedups_and_drops_short() {
        setup();
        assert_eq!(fold_name("Müller-Lüdenscheidt"), vec!["mueller", "luedenscheidt"]);
        // Umlaut expansion, diacritic strip, lowercase.
        assert_eq!(fold_name("Hans Müller"), vec!["hans", "mueller"]);
        assert_eq!(fold_name("José Öztürk"), vec!["jose", "oeztuerk"]);
        // Words < 2 chars dropped ("o" from an apostrophe split), dedupe preserves order.
        assert_eq!(fold_name("O'Brien Brien"), vec!["brien"]);
    }

    #[test]
    fn phonetic_keys_equal_for_meyer_maier_mayer() {
        setup();
        let m = pii_fuzzy_sig("Meyer");
        let a = pii_fuzzy_sig("Maier");
        let y = pii_fuzzy_sig("Mayer");
        assert_eq!(m.w.len(), 1);
        assert!(!m.w[0].p.is_empty(), "phonetic key must be present");
        assert_eq!(m.w[0].p, a.w[0].p, "Meyer/Maier phonetic keys differ");
        assert_eq!(m.w[0].p, y.w[0].p, "Meyer/Mayer phonetic keys differ");
        // Phonetic-equal but spelling-different pairs clear the 0.85 floor
        // (recall preserved) yet stay BELOW an exact-spelling self-match (1.0),
        // so exact spelling ranks first.
        let s_ma = fuzzy_score(&m, &a);
        assert!(s_ma >= 0.85, "Meyer/Maier phonetic score too low: {s_ma}");
        assert!(s_ma < 1.0, "phonetic-only pair must not tie an exact match: {s_ma}");
        assert_eq!(fuzzy_score(&m, &m), 1.0, "exact spelling must score 1.0");
    }

    #[test]
    fn fuzzy_matches_typos_above_threshold() {
        setup();
        let base = pii_fuzzy_sig("Hans Müller");
        for variant in ["Hans Muller", "Hnas Müller", "Müller Hans", "Hans Mueller"] {
            let q = pii_fuzzy_sig(variant);
            let s = fuzzy_score(&q, &base);
            assert!(
                s >= PII_FUZZY_THRESHOLD,
                "'{variant}' vs 'Hans Müller' = {s} < {PII_FUZZY_THRESHOLD}"
            );
        }
        // Surname-only query resolves to the full name (mean over 1 word = 1.0).
        let surname = pii_fuzzy_sig("Müller");
        assert!(
            fuzzy_score(&surname, &base) >= PII_FUZZY_THRESHOLD,
            "surname-only query should match the full name"
        );
    }

    #[test]
    fn fuzzy_separates_unrelated_names() {
        setup();
        let a = pii_fuzzy_sig("Stefan Schmidt");
        let b = pii_fuzzy_sig("Hans Müller");
        let s = fuzzy_score(&a, &b);
        assert!(s < 0.5, "unrelated names scored too high: {s}");
    }

    #[test]
    fn fuzzy_sig_serde_round_trip() {
        setup();
        let sig = pii_fuzzy_sig("Hans Müller");
        let json = serde_json::to_string(&sig).unwrap();
        let back: FuzzySig = serde_json::from_str(&json).unwrap();
        assert_eq!(sig, back);
    }

    #[test]
    fn mint_log_records_name_signatures() {
        setup();
        let token = obfuscate_pii("Katharina Niedermayer", "Name");
        let sig = fuzzy_sig_for_token(&token).expect("Name mint should log a signature");
        // The logged sig equals a fresh derivation from the clear value.
        assert_eq!(sig, pii_fuzzy_sig("Katharina Niedermayer"));
        // Non-Name labels are not logged.
        let email = obfuscate_pii("a@b.de", "Email");
        assert!(fuzzy_sig_for_token(&email).is_none());
    }

    // ── PPRL fuzzy ADDRESS resolution (spec `.eck/PPRL_FUZZY.md` §F) ──────────

    #[test]
    fn fold_address_normalises_and_expands() {
        setup();
        // Abbrev expansion (glued + standalone), split letters from digits,
        // split on comma, keep zip + house number, drop nothing valid.
        assert_eq!(
            fold_address("Hauptstr. 12, 10115 Berlin"),
            vec!["hauptstrasse", "12", "10115", "berlin"]
        );
        assert_eq!(fold_address("Bahnhofstr 5"), vec!["bahnhofstrasse", "5"]);
        assert_eq!(fold_address("Bahnhofstraße 5"), vec!["bahnhofstrasse", "5"]);
        // "12a" stays a single alphanumeric token; country word dropped.
        assert_eq!(
            fold_address("Musterweg 12a, Deutschland"),
            vec!["musterweg", "12a"]
        );
    }

    #[test]
    fn address_sig_kind_and_serde() {
        setup();
        let sig = pii_fuzzy_sig_kind("Hauptstr. 12, 10115 Berlin", SigKind::Address);
        assert_eq!(sig.kind, SigKind::Address);
        let json = serde_json::to_string(&sig).unwrap();
        assert!(json.contains("\"k\":\"Address\""), "kind must serialize as k: {json}");
        let back: FuzzySig = serde_json::from_str(&json).unwrap();
        assert_eq!(sig, back);
        // Backward-compat: JSON without `k` defaults to Name.
        let legacy: FuzzySig = serde_json::from_str("{\"w\":[]}").unwrap();
        assert_eq!(legacy.kind, SigKind::Name);
    }

    #[test]
    fn address_fuzzy_matches_variants_but_not_wrong_number() {
        setup();
        let doc = pii_fuzzy_sig_kind("Hauptstraße 12 Berlin", SigKind::Address);
        let q = pii_fuzzy_sig_kind("Hauptstr. 12, 10115 Berlin", SigKind::Address);
        let s = fuzzy_score(&q, &doc);
        assert!(s >= PII_FUZZY_THRESHOLD, "typo'd address should match: {s}");
        // Wrong house number must pull the mean below threshold.
        let wrong = pii_fuzzy_sig_kind("Hauptstraße 14 Berlin", SigKind::Address);
        let s_wrong = fuzzy_score(&q, &wrong);
        assert!(s_wrong < PII_FUZZY_THRESHOLD, "wrong number matched: {s_wrong}");
        // Abbreviation-only difference still matches.
        let a = pii_fuzzy_sig_kind("Bahnhofstrasse 5", SigKind::Address);
        let b = pii_fuzzy_sig_kind("Bahnhofstr 5", SigKind::Address);
        assert!(fuzzy_score(&a, &b) >= PII_FUZZY_THRESHOLD);
    }

    #[test]
    fn address_zip_only_matches_record() {
        setup();
        let doc = pii_fuzzy_sig_kind("Hauptstraße 12, 10115 Berlin", SigKind::Address);
        let zip = pii_fuzzy_sig_kind("10115", SigKind::Address);
        assert!(
            (fuzzy_score(&zip, &doc) - 1.0).abs() < 1e-6,
            "zip-only query should score 1.0 against a matching record"
        );
        // A different zip must not match.
        let other = pii_fuzzy_sig_kind("99999", SigKind::Address);
        assert_eq!(fuzzy_score(&other, &doc), 0.0);
    }

    #[test]
    fn address_sig_from_components_matches_city_and_zip_only() {
        setup();
        // Real-data shape: street on `address`, zip/city in their own fields.
        let doc = pii_fuzzy_sig_address(&["Musterallee 15-21", "12345", "Beispielstadt"]);
        let city = pii_fuzzy_sig_kind("Beispielstadt", SigKind::Address);
        let zip = pii_fuzzy_sig_kind("12345", SigKind::Address);
        assert!(
            (fuzzy_score(&city, &doc) - 1.0).abs() < 1e-6,
            "city-only query must hit the combined address sig"
        );
        assert!(
            (fuzzy_score(&zip, &doc) - 1.0).abs() < 1e-6,
            "zip-only query must hit the combined address sig"
        );
    }

    #[test]
    fn address_mint_log_records_signatures() {
        setup();
        let token = obfuscate_pii("Hauptstraße 12, 10115 Berlin", "Address");
        let sig = fuzzy_sig_for_token(&token).expect("Address mint should log a signature");
        assert_eq!(sig.kind, SigKind::Address);
    }
}
