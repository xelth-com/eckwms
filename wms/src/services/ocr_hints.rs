//! Pure heuristics behind `POST /api/ai/ocr-hints` (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md`
//! §10) — the PDA's "OCR review": the worker taps the words that form a code
//! (a power-supply serial printed without a barcode, say) off the camera
//! still's word boxes, and the server tells it which ones look meaningful.
//!
//! v1 is HEURISTIC ONLY: no LLM call, no network, nothing logged but counts.
//! The line text itself may carry a customer address (a shipping label), so
//! it must never leave this module's return value or hit a log line.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Boxes are normalized `[x, y, w, h]`, origin top-left, 0..1.
pub type Box4 = [f64; 4];

/// One OCR word/line box as the PDA reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct OcrLine {
    pub text: String,
    #[serde(rename = "box")]
    pub box_: Box4,
}

/// One extracted field in the response.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Field {
    pub kind: &'static str,
    pub text: String,
    pub line_index: Option<usize>,
    #[serde(rename = "box")]
    pub box_: Box4,
    pub confidence: f64,
}

const MAX_FIELDS: usize = 12;

/// Narrow a line's box along x proportionally to a character span within the
/// line's text, clamped to 0..1. `start`/`end` are BYTE offsets into `line`
/// (every caller passes ASCII-only spans, so byte == char here).
fn narrow_box(line_box: Box4, line_len: usize, start: usize, end: usize) -> Box4 {
    let [x, y, w, h] = line_box;
    if line_len == 0 || w <= 0.0 {
        return line_box;
    }
    let len = line_len as f64;
    let nx = (x + w * (start as f64) / len).clamp(0.0, 1.0);
    let span_w = (w * ((end.saturating_sub(start)) as f64) / len).max(0.0);
    let nw = (nx + span_w).min(1.0) - nx;
    [nx, y, nw.max(0.0), h]
}

/// Does `b` look like a device serial by pattern alone (no label nearby)?
/// Mirrors `crate::services::claimlist::looks_like_serial`'s intent but with
/// the brief's own two shapes: 1-2 letters + 6-10 digits (optionally `,NN`),
/// OR 7-20 alnum chars with >= 4 digits and >= 1 letter.
fn looks_like_serial_pattern(tok: &str) -> bool {
    let core = tok.split(',').next().unwrap_or(tok);
    if crate::services::claimlist::looks_like_serial(core) {
        return true;
    }
    // 1-2 letters followed by 6-10 digits, optionally ",NN".
    let letters = core.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let digits_after: String = core.chars().skip(letters).collect();
    if (1..=2).contains(&letters)
        && (6..=10).contains(&digits_after.len())
        && digits_after.chars().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    // 7-20 alnum chars, >= 4 digits, >= 1 letter.
    if (7..=20).contains(&tok.len())
        && tok.chars().all(|c| c.is_ascii_alphanumeric())
        && tok.chars().filter(|c| c.is_ascii_digit()).count() >= 4
        && tok.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    false
}

fn re_mac() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:[0-9A-F]{2}[:-]){5}[0-9A-F]{2}\b").expect("valid mac regex")
    })
}

/// `dd.mm.yyyy`, `yyyy-mm-dd`, `mm/yy` or `mm/yyyy`.
fn re_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:(\d{1,2})\.(\d{1,2})\.(\d{4})|(\d{4})-(\d{1,2})-(\d{1,2})|(\d{1,2})/(\d{2}|\d{4}))\b")
            .expect("valid date regex")
    })
}

/// A run of alnum / `,` / `-` characters — the generic "token" unit the label
/// rules and the pattern-only serial/model checks scan over.
fn re_token() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z0-9][A-Za-z0-9,\-]*").expect("valid token regex"))
}

/// Case-insensitive label groups, ordered longest-alias-first so e.g.
/// "Serien-Nr" doesn't get shadowed by a shorter alias matching a substring.
struct LabelRule {
    kind: &'static str,
    aliases: &'static [&'static str],
    confidence: f64,
}

const LABEL_RULES: &[LabelRule] = &[
    LabelRule {
        kind: "serial",
        aliases: &["serien-nr", "seriennummer", "seriennr", "ser.no", "serial", "s/n", "sn"],
        confidence: 0.9,
    },
    LabelRule {
        kind: "model",
        aliases: &["modell", "model", "typ", "type", "mod."],
        confidence: 0.9,
    },
    LabelRule {
        kind: "manufacturer",
        aliases: &["hersteller", "manufacturer", "mfr", "made by"],
        confidence: 0.85,
    },
    LabelRule {
        kind: "part_number",
        aliases: &["artikelnr", "artikel", "art.-nr", "part no", "p/n", "pn", "ref"],
        confidence: 0.9,
    },
    LabelRule {
        kind: "date",
        aliases: &["herstellungsdatum", "datum", "mfd", "date"],
        confidence: 0.85,
    },
];

/// Tokens on `line`, each as `(start_byte, end_byte, raw_text)`.
fn tokenize(line: &str) -> Vec<(usize, usize, &str)> {
    re_token()
        .find_iter(line)
        .map(|m| (m.start(), m.end(), m.as_str()))
        .collect()
}

/// Luhn check on a 15-digit string (IMEI).
fn luhn_ok(digits: &str) -> bool {
    if digits.len() != 15 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let sum: u32 = digits
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| {
            let d = c.to_digit(10).unwrap();
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                d
            }
        })
        .sum();
    sum % 10 == 0
}

fn valid_date_ymd(y: i32, m: i32, d: i32) -> bool {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return false;
    }
    if !(1990..=2099).contains(&y) {
        return false;
    }
    let max_day = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 31,
    };
    d <= max_day
}

/// Does `line` carry one of a rule's label aliases? Aliases are checked in
/// the caller's order (longest/most-specific first), so e.g. "Serien-Nr"
/// matches before the bare "sn" alias would. Returns the byte offset right
/// after the matched alias — the label rule then looks for the first token
/// starting at or after that offset as the value.
fn find_label(line_lower: &str, aliases: &[&str]) -> Option<usize> {
    aliases.iter().find_map(|alias| line_lower.find(alias).map(|pos| pos + alias.len()))
}

/// Extract fields from one OCR line against every label rule.
fn label_fields(idx: usize, line: &OcrLine, out: &mut Vec<Field>) {
    let lower = line.text.to_ascii_lowercase();
    let toks = tokenize(&line.text);
    for rule in LABEL_RULES {
        let Some(after) = find_label(&lower, rule.aliases) else { continue };
        // The first token entirely after the label position is the value.
        if let Some((ts, te, raw)) = toks.iter().copied().find(|(ts, _, _)| *ts >= after) {
            // Strip a leading separator the token regex itself can't skip
            // (":" / "." are not in the token class, so nothing to trim here;
            // the search above already starts after the label text).
            let value = raw.trim_matches(',');
            if value.is_empty() {
                continue;
            }
            out.push(Field {
                kind: rule.kind,
                text: value.to_string(),
                line_index: Some(idx),
                box_: narrow_box(line.box_, line.text.len(), ts, te),
                confidence: rule.confidence,
            });
        }
    }
}

/// Pattern-only fields (no label needed): serial candidates, MAC, IMEI, dates.
fn pattern_fields(idx: usize, line: &OcrLine, out: &mut Vec<Field>) {
    let toks = tokenize(&line.text);
    let line_is_single_token = toks.len() == 1 && toks[0].1 - toks[0].0 == line.text.trim().len();

    for (ts, te, raw) in &toks {
        let tok = *raw;
        if looks_like_serial_pattern(tok) {
            let whole_line = line_is_single_token;
            let confidence = if whole_line { 0.7 } else { 0.6 };
            out.push(Field {
                kind: "serial",
                text: tok.to_string(),
                line_index: Some(idx),
                box_: if whole_line {
                    line.box_
                } else {
                    narrow_box(line.box_, line.text.len(), *ts, *te)
                },
                confidence,
            });
        }
    }

    // MAC address (may span a punctuation-only token sequence the generic
    // tokenizer doesn't carve the same way — match on the raw line text).
    for m in re_mac().find_iter(&line.text) {
        out.push(Field {
            kind: "mac",
            text: m.as_str().to_string(),
            line_index: Some(idx),
            box_: narrow_box(line.box_, line.text.len(), m.start(), m.end()),
            confidence: 0.95,
        });
    }

    // IMEI: 15 consecutive digits passing Luhn.
    for (ts, te, raw) in &toks {
        if raw.len() == 15 && raw.chars().all(|c| c.is_ascii_digit()) && luhn_ok(raw) {
            out.push(Field {
                kind: "imei",
                text: raw.to_string(),
                line_index: Some(idx),
                box_: narrow_box(line.box_, line.text.len(), *ts, *te),
                confidence: 0.9,
            });
        }
    }

    // Dates — label boost is applied in `label_fields`-adjacent pass below;
    // here we just find valid-range date patterns at base confidence 0.5,
    // then `boost_dates_near_label` raises ones that sit after a date label.
    let lower = line.text.to_ascii_lowercase();
    let has_date_label = find_label(&lower, &["herstellungsdatum", "datum", "mfd", "date"]).is_some();
    for caps in re_date().captures_iter(&line.text) {
        let whole = caps.get(0).unwrap();
        let valid = if let (Some(d), Some(m), Some(y)) = (caps.get(1), caps.get(2), caps.get(3)) {
            // dd.mm.yyyy
            let (d, m, y) = (
                d.as_str().parse::<i32>().unwrap_or(0),
                m.as_str().parse::<i32>().unwrap_or(0),
                y.as_str().parse::<i32>().unwrap_or(0),
            );
            valid_date_ymd(y, m, d)
        } else if let (Some(y), Some(m), Some(d)) = (caps.get(4), caps.get(5), caps.get(6)) {
            // yyyy-mm-dd
            let (y, m, d) = (
                y.as_str().parse::<i32>().unwrap_or(0),
                m.as_str().parse::<i32>().unwrap_or(0),
                d.as_str().parse::<i32>().unwrap_or(0),
            );
            valid_date_ymd(y, m, d)
        } else if let (Some(m), Some(_y)) = (caps.get(7), caps.get(8)) {
            // mm/yy or mm/yyyy — no day to validate, just the month range.
            let m = m.as_str().parse::<i32>().unwrap_or(0);
            (1..=12).contains(&m)
        } else {
            false
        };
        if !valid {
            continue;
        }
        out.push(Field {
            kind: "date",
            text: whole.as_str().to_string(),
            line_index: Some(idx),
            box_: narrow_box(line.box_, line.text.len(), whole.start(), whole.end()),
            confidence: if has_date_label { 0.85 } else { 0.5 },
        });
    }
}

// ── Reusable field validators (shared with `ai::ocr_verify` grounding) ───────
//
// The OCR-verify worker's anti-hallucination gate must validate a `mac`/`imei`/
// `date` fact with the SAME rules this heuristic uses, so they live here as pure
// public wrappers over the private regex/Luhn/date logic.

/// Does the WHOLE trimmed string look like a MAC address?
pub fn is_mac(s: &str) -> bool {
    let s = s.trim();
    re_mac().find(s).map(|m| m.as_str() == s).unwrap_or(false)
}

/// Is the WHOLE trimmed string a 15-digit IMEI passing the Luhn check?
pub fn is_imei(s: &str) -> bool {
    luhn_ok(s.trim())
}

/// Is the WHOLE trimmed string a valid-range date (`dd.mm.yyyy`, `yyyy-mm-dd`,
/// `mm/yy`, `mm/yyyy`)? Mirrors the `date` validity logic in [`pattern_fields`].
pub fn is_date(s: &str) -> bool {
    let s = s.trim();
    re_date().captures_iter(s).any(|caps| {
        let whole = caps.get(0).unwrap();
        if whole.as_str() != s {
            return false; // must be the whole token, not a date embedded in prose
        }
        if let (Some(d), Some(m), Some(y)) = (caps.get(1), caps.get(2), caps.get(3)) {
            valid_date_ymd(
                y.as_str().parse().unwrap_or(0),
                m.as_str().parse().unwrap_or(0),
                d.as_str().parse().unwrap_or(0),
            )
        } else if let (Some(y), Some(m), Some(d)) = (caps.get(4), caps.get(5), caps.get(6)) {
            valid_date_ymd(
                y.as_str().parse().unwrap_or(0),
                m.as_str().parse().unwrap_or(0),
                d.as_str().parse().unwrap_or(0),
            )
        } else if let Some(m) = caps.get(7) {
            (1..=12).contains(&m.as_str().parse::<i32>().unwrap_or(0))
        } else {
            false
        }
    })
}

/// `context` is currently unused by the heuristics (all rules are generic
/// across receiving/repair/warehouse) but kept in the signature for the
/// request shape / future tuning.
pub fn extract_fields(_context: &str, lines: &[OcrLine]) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        label_fields(idx, line, &mut out);
        pattern_fields(idx, line, &mut out);
    }

    // Dedupe identical (kind, text), keep the highest-confidence instance.
    let mut dedup: Vec<Field> = Vec::new();
    for f in out {
        if let Some(existing) = dedup.iter_mut().find(|e: &&mut Field| e.kind == f.kind && e.text == f.text) {
            if f.confidence > existing.confidence {
                *existing = f;
            }
        } else {
            dedup.push(f);
        }
    }

    dedup.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    dedup.truncate(MAX_FIELDS);
    dedup
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, b: Box4) -> OcrLine {
        OcrLine { text: text.to_string(), box_: b }
    }

    #[test]
    fn label_serial() {
        let lines = vec![line("S/N: PS123456789", [0.1, 0.2, 0.5, 0.05])];
        let fields = extract_fields("repair", &lines);
        let f = fields.iter().find(|f| f.kind == "serial").expect("serial found");
        assert_eq!(f.text, "PS123456789");
        assert_eq!(f.confidence, 0.9);
        assert_eq!(f.line_index, Some(0));
    }

    #[test]
    fn pattern_serial_substring() {
        let lines = vec![line("Model Acme770 I81800562 CE", [0.0, 0.0, 1.0, 0.1])];
        let fields = extract_fields("receiving", &lines);
        let f = fields.iter().find(|f| f.kind == "serial" && f.text == "I81800562").expect("pattern serial");
        assert_eq!(f.confidence, 0.6);
    }

    #[test]
    fn whole_line_serial_token() {
        let lines = vec![line("I81800562", [0.1, 0.1, 0.3, 0.05])];
        let fields = extract_fields("receiving", &lines);
        let f = fields.iter().find(|f| f.kind == "serial").unwrap();
        assert_eq!(f.confidence, 0.7);
        assert_eq!(f.box_, [0.1, 0.1, 0.3, 0.05]);
    }

    #[test]
    fn mac_address() {
        let lines = vec![line("MAC: 00:1B:44:11:3A:B7", [0.0, 0.0, 1.0, 0.05])];
        let fields = extract_fields("warehouse", &lines);
        let f = fields.iter().find(|f| f.kind == "mac").unwrap();
        assert_eq!(f.text, "00:1B:44:11:3A:B7");
        assert_eq!(f.confidence, 0.95);
    }

    #[test]
    fn imei_valid_luhn() {
        // 490154203237518 is the textbook valid-Luhn IMEI example.
        let lines = vec![line("IMEI 490154203237518", [0.0, 0.0, 1.0, 0.05])];
        let fields = extract_fields("warehouse", &lines);
        assert!(fields.iter().any(|f| f.kind == "imei" && f.text == "490154203237518"));
    }

    #[test]
    fn imei_invalid_luhn_dropped() {
        let lines = vec![line("IMEI 490154203237519", [0.0, 0.0, 1.0, 0.05])];
        let fields = extract_fields("warehouse", &lines);
        assert!(!fields.iter().any(|f| f.kind == "imei"));
    }

    #[test]
    fn date_formats() {
        let lines = vec![
            line("MFD 24.03.2023", [0.0, 0.0, 0.5, 0.05]),
            line("2023-03-24", [0.0, 0.1, 0.5, 0.05]),
            line("03/23", [0.0, 0.2, 0.3, 0.05]),
        ];
        let fields = extract_fields("warehouse", &lines);
        let d1 = fields.iter().find(|f| f.kind == "date" && f.text == "24.03.2023").unwrap();
        assert_eq!(d1.confidence, 0.85); // label-boosted
        assert!(fields.iter().any(|f| f.kind == "date" && f.text == "2023-03-24"));
        assert!(fields.iter().any(|f| f.kind == "date" && f.text == "03/23"));
    }

    #[test]
    fn part_number_label() {
        let lines = vec![line("Art.-Nr: AB-12345", [0.0, 0.0, 1.0, 0.05])];
        let fields = extract_fields("warehouse", &lines);
        let f = fields.iter().find(|f| f.kind == "part_number").unwrap();
        assert_eq!(f.text, "AB-12345");
        assert_eq!(f.confidence, 0.9);
    }

    #[test]
    fn manufacturer_label() {
        let lines = vec![line("Hersteller: Acme Co. Ltd", [0.0, 0.0, 1.0, 0.05])];
        let fields = extract_fields("warehouse", &lines);
        let f = fields.iter().find(|f| f.kind == "manufacturer").unwrap();
        assert_eq!(f.text, "Acme");
        assert_eq!(f.confidence, 0.85);
    }

    #[test]
    fn model_label() {
        let lines = vec![line("Model: X-1000", [0.0, 0.0, 1.0, 0.05])];
        let fields = extract_fields("warehouse", &lines);
        let f = fields.iter().find(|f| f.kind == "model").unwrap();
        assert_eq!(f.text, "X-1000");
        assert_eq!(f.confidence, 0.9);
    }

    #[test]
    fn empty_input_is_empty() {
        let fields = extract_fields("warehouse", &[]);
        assert!(fields.is_empty());
    }

    #[test]
    fn cap_and_dedupe() {
        // Fourteen lines, each the SAME whole-line serial token → deduped to
        // one field, well under the 12-field cap.
        let lines: Vec<OcrLine> = (0..14).map(|_| line("I81800562", [0.0, 0.0, 0.3, 0.05])).collect();
        let fields = extract_fields("warehouse", &lines);
        assert_eq!(fields.len(), 1);
    }

    #[test]
    fn cap_at_twelve_distinct_fields() {
        // 15 DISTINCT whole-line serial tokens — capped down to exactly 12.
        let lines: Vec<OcrLine> = (0..15)
            .map(|i| line(&format!("I818{:06}", i), [0.0, 0.0, 0.3, 0.05]))
            .collect();
        let fields = extract_fields("warehouse", &lines);
        assert_eq!(fields.len(), MAX_FIELDS);
    }

    #[test]
    fn box_narrowing_for_a_substring_match() {
        // "S/N: PS123456789" — the value starts at byte 5, 12 chars long, in a
        // 17-char line with box x=0.0 w=1.0.
        let lines = vec![line("S/N: PS123456789", [0.0, 0.0, 1.0, 0.1])];
        let fields = extract_fields("warehouse", &lines);
        let f = fields.iter().find(|f| f.kind == "serial" && f.confidence == 0.9).unwrap();
        let [x, _y, w, _h] = f.box_;
        assert!(x > 0.0 && x < 1.0);
        assert!(w > 0.0 && w < 1.0);
        assert!(x + w <= 1.0 + 1e-9);
    }
}
