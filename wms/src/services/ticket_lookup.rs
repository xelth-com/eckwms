//! Find the Zoho ticket behind a walk-in device, from OCR garbage.
//!
//! The parcel lane has a postal barcode to key on. The WALK-IN lane has
//! nothing but whatever the receiver's camera read off the paper
//! "Reparaturauftrag" the customer carried in — a blob of mixed-case,
//! newline-riddled, confusable-mangled text — or a bare serial scanned off
//! the device. From that we must land on the right `document` row
//! (`type = 'support_ticket'`) so the receipt carries customer + ticket and
//! the claim can be minted without anyone retyping an address.
//!
//! Deliberately EMBEDDING-FREE. The kiosk must resolve a walk-in while the
//! customer is standing at the counter, on a node whose embedding worker may
//! be hours behind (or off) — so this is a deterministic token scorer over
//! `document.meta`, not a KNN query. `search_database` / `similar_tickets`
//! stay the semantic lane; this is the counter lane.
//!
//! Two OCR facts drive the whole design:
//!
//!  * **Confusables run both ways.** The tenant's serials look like `I81700403`,
//!    `F91702145`, `T425002043`. OCR reads the leading `I` as `1`; a human
//!    typing the ticket in Zoho does the same. So `181700403` must find
//!    `I81700403` AND `I81700403` must find a ticket typed `181700403`.
//!    Both sides are folded through [`normalize_confusables`] before
//!    comparison (I/l/1 → 1, O → 0, S → 5, B → 8), which also means a serial
//!    candidate may not contain a letter at all — requiring one would defeat
//!    the exact case this exists for.
//!  * **German words fold.** `Müller` scanned, `Mueller` in Zoho (and back).
//!    Word tokens are lowercased and umlaut-folded on both sides.
//!
//! Cost: one projected scan of `SELECT record::id(id), meta, status FROM
//! document WHERE type = 'support_ticket'` (~2100 rows, `document_type_idx`).
//! The indexed exact paths (`doc_serial_idx`, `doc_ticket_number_idx`) cannot
//! serve the normalised comparison — an index holds the stored spelling, not
//! the folded one — so the scan IS the lookup rather than a fallback behind it.

use eck_core::db::SurrealDb;
use serde::Serialize;
use serde_json::{json, Value};

/// Minimum length of a token we bother keeping at all.
const MIN_TOKEN_LEN: usize = 3;
/// Minimum length of an alphabetic token matched against company/city/address.
const MIN_WORD_LEN: usize = 4;

// ─── Normalisation ───────────────────────────────────────────────────────────

/// Fold the character pairs OCR (and human typists) confuse, so the two
/// spellings of one serial collapse onto the same string. Uppercases first, so
/// `F9l7O2145` and `F91702145` both become `F91702145`-shaped.
///
/// The map is intentionally lossy and SYMMETRIC — `I`, `l` and `1` all become
/// `1`; there is no attempt to guess which one was meant.
pub fn normalize_confusables(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' | '1' => '1',
            'O' | '0' => '0',
            'S' | '5' => '5',
            'B' | '8' => '8',
            other => other,
        })
        .collect()
}

/// Lowercase + fold German umlauts (and `ß`) so `Müller` == `Mueller`, plus the
/// other Latin-1 diacritics Zoho carries for AT/CH customers.
pub fn fold_word(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            'á' | 'à' | 'â' | 'å' => out.push('a'),
            'é' | 'è' | 'ê' => out.push('e'),
            'í' | 'ì' | 'î' => out.push('i'),
            'ó' | 'ò' | 'ô' => out.push('o'),
            'ú' | 'ù' | 'û' => out.push('u'),
            'ç' => out.push('c'),
            'ñ' => out.push('n'),
            other => out.push(other),
        }
    }
    out
}

/// Word tokens of a ticket-side field (company, city, address …), folded the
/// same way the query side is.
fn field_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(fold_word)
        .filter(|t| t.chars().count() >= MIN_WORD_LEN)
        .collect()
}

/// Last 7 digits of a phone number — the only part that survives country
/// prefix, leading zero and the receiver's spacing (`+49 40 / 123 45 67`).
fn phone_tail(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits.len() >= 7).then(|| digits[digits.len() - 7..].to_string())
}

// ─── Tokenisation ────────────────────────────────────────────────────────────

/// Everything a query blob decomposed into, one bucket per thing it can match.
/// Pure — built by [`tokenize`] with no DB access, so it is unit-testable.
#[derive(Debug, Default, Clone)]
pub struct QueryTokens {
    /// Every kept token, in order, for the response's `query_tokens` echo.
    pub all: Vec<String>,
    /// 8–12 alphanumerics with at least one digit, confusable-normalised.
    pub serials: Vec<String>,
    /// 4–6 digits (a leading `#` is stripped).
    pub ticket_numbers: Vec<String>,
    /// 4 digits (AT) or 5 digits (DE).
    pub zips: Vec<String>,
    /// Lowercased.
    pub emails: Vec<String>,
    /// Last-7-digit tails of anything phone-shaped.
    pub phone_tails: Vec<String>,
    /// Alphabetic tokens ≥4 chars, lowercased + umlaut-folded.
    pub words: Vec<String>,
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.iter().any(|x| *x == s) {
        v.push(s);
    }
}

/// Split an OCR blob into scoring candidates.
///
/// Whitespace chunks come first (a phone number keeps its `+`/`/`/`-` grouping
/// that far), then each chunk is split on punctuation into bare alphanumeric
/// tokens. A single token can land in several buckets on purpose: `21614` is
/// both a plausible DE zip and a plausible ticket number, and only the ticket
/// corpus can say which it was.
/// Digit-runs that could be a phone number. A printed number is broken up by
/// spaces and slashes as often as not (`+43 5572 / 123 4567`), so a run is
/// anything made of digits and DIALLING punctuation — it survives whitespace
/// but stops at the first letter, comma or newline, which is where the phone
/// stops on the page too.
fn phone_runs(q: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    for c in q.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if matches!(c, ' ' | '\t' | '+' | '-' | '/' | '(' | ')' | '.') && !cur.is_empty() {
            // dialling punctuation only continues a run, never starts one
        } else {
            if !cur.is_empty() {
                runs.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    runs
}

pub fn tokenize(q: &str) -> QueryTokens {
    let mut t = QueryTokens::default();

    for run in phone_runs(q) {
        if let Some(tail) = phone_tail(&run) {
            push_unique(&mut t.phone_tails, tail);
        }
    }

    for chunk in q.split_whitespace() {
        // An email survives punctuation splitting only if we grab it here.
        let trimmed = chunk.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.');
        if trimmed.contains('@') && trimmed.contains('.') {
            push_unique(&mut t.emails, trimmed.to_lowercase());
        }
        for tok in chunk.split(|c: char| !c.is_alphanumeric()) {
            if tok.chars().count() < MIN_TOKEN_LEN {
                continue;
            }
            push_unique(&mut t.all, tok.to_string());

            let is_ascii_alnum = tok.chars().all(|c| c.is_ascii_alphanumeric());
            let all_digits = tok.chars().all(|c| c.is_ascii_digit());
            let has_digit = tok.chars().any(|c| c.is_ascii_digit());
            let len = tok.chars().count();

            // Serial: 8–12 alphanumerics carrying at least one digit. NOT
            // "letters AND digits" — OCR routinely turns the leading letter of
            // a serial into a digit (`I81700403` → `181700403`), which
            // is precisely the case this lookup exists to rescue.
            if is_ascii_alnum && (8..=12).contains(&len) && has_digit {
                push_unique(&mut t.serials, normalize_confusables(tok));
            }
            if all_digits && (4..=6).contains(&len) {
                push_unique(&mut t.ticket_numbers, tok.to_string());
            }
            if all_digits && (4..=5).contains(&len) {
                push_unique(&mut t.zips, tok.to_string());
            }
            if len >= MIN_WORD_LEN && !has_digit {
                push_unique(&mut t.words, fold_word(tok));
            }
        }
    }

    // A bare `#32834` loses its hash to the punctuation split above, which is
    // what we want — but a chunk that WAS only a hashed number must still be
    // a ticket-number candidate even at 4 chars, so nothing extra is needed.
    t
}

// ─── Scoring ─────────────────────────────────────────────────────────────────

/// Points per match kind. Named rather than inlined so the unit tests and the
/// ranking doctrine read off one place.
mod pts {
    pub const SERIAL: i64 = 1000;
    pub const TICKET_NUMBER: i64 = 900;
    pub const EMAIL: i64 = 700;
    pub const PHONE: i64 = 600;
    pub const ZIP: i64 = 250;
    pub const ZIP_CITY_BONUS: i64 = 150;
    pub const NAME_TOKEN: i64 = 120;
    pub const CITY: i64 = 100;
    pub const ADDRESS_TOKEN: i64 = 60;
    pub const RECENT_120D: i64 = 30;
    pub const RECENT_YEAR: i64 = 15;
    pub const STATUS_ACTIVE: i64 = 20;
}

fn meta_str<'a>(meta: &'a Value, k: &str) -> &'a str {
    meta.get(k).and_then(|v| v.as_str()).unwrap_or("")
}

/// Days since a Zoho `createdTime` string, or `None` when it is absent/unparsable.
fn age_days(created_time: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let s = created_time.trim();
    if s.is_empty() {
        return None;
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|| {
            // Zoho occasionally hands back `YYYY-MM-DDTHH:MM:SS.mmmZ` variants
            // that already parse above, and bare `YYYY-MM-DD` on backfilled rows.
            chrono::NaiveDate::parse_from_str(&s[..s.len().min(10)], "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|dt| dt.and_utc())
        })?;
    Some((now - parsed).num_days())
}

/// A ticket status that still wants someone's attention.
fn status_is_active(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "open" | "pending" | "on hold" | "offen" | "in bearbeitung"
    )
}

/// Score one ticket's `meta` against the tokenised query. Pure: no DB, no
/// clock of its own (`now` is passed in so the recency bonus is testable).
///
/// Returns `(score, matched)` where `matched` names the kinds that fired, in a
/// stable order — the UI shows it as "why this one".
pub fn score_ticket(
    tokens: &QueryTokens,
    meta: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> (i64, Vec<&'static str>) {
    let mut score = 0i64;
    let mut matched: Vec<&'static str> = Vec::new();

    // ── serial (the strongest signal a walk-in can produce) ──
    let ticket_serial = normalize_confusables(meta_str(meta, "serial_number"));
    if !ticket_serial.is_empty() && tokens.serials.iter().any(|s| *s == ticket_serial) {
        score += pts::SERIAL;
        matched.push("serial");
    }

    // ── ticket number ──
    let tn = meta_str(meta, "ticket_number").trim();
    if !tn.is_empty() && tokens.ticket_numbers.iter().any(|c| c == tn) {
        score += pts::TICKET_NUMBER;
        matched.push("ticket_number");
    }

    // ── email ──
    let email = meta_str(meta, "email").trim().to_lowercase();
    if !email.is_empty() && tokens.emails.iter().any(|c| *c == email) {
        score += pts::EMAIL;
        matched.push("email");
    }

    // ── phone (digits only, last 7 — prefixes and spacing never survive OCR) ──
    if let Some(tail) = phone_tail(meta_str(meta, "phone")) {
        if tokens.phone_tails.iter().any(|c| *c == tail) {
            score += pts::PHONE;
            matched.push("phone");
        }
    }

    // ── place ──
    let city_words = field_words(meta_str(meta, "city"));
    let city_hit = !city_words.is_empty()
        && city_words.iter().any(|w| tokens.words.iter().any(|t| t == w));

    let zip = meta_str(meta, "zip").trim();
    if !zip.is_empty() && tokens.zips.iter().any(|c| c == zip) {
        score += pts::ZIP;
        matched.push("zip");
        // A zip alone is a whole town; a zip that AGREES with the town name is
        // near-certainly the same customer's letterhead.
        if city_hit {
            score += pts::ZIP_CITY_BONUS;
        }
    }
    if city_hit {
        score += pts::CITY;
        matched.push("city");
    }

    // ── who ──
    // company and customer share the pot: one token counts once, whichever
    // field carried it ("Magic Fit Dornbirn" as company and as contact name
    // must not score twice for the same word).
    let mut name_words = field_words(meta_str(meta, "company"));
    for w in field_words(meta_str(meta, "customer")) {
        if !name_words.contains(&w) {
            name_words.push(w);
        }
    }
    let name_hits = name_words
        .iter()
        .filter(|w| tokens.words.iter().any(|t| t == *w))
        .count() as i64;
    if name_hits > 0 {
        score += name_hits * pts::NAME_TOKEN;
        matched.push("name");
    }

    // ── street ──
    // Address words already counted as city/name are not counted again — the
    // address line normally repeats the town.
    let addr_hits = field_words(meta_str(meta, "address"))
        .into_iter()
        .filter(|w| !city_words.contains(w) && !name_words.contains(w))
        .filter(|w| tokens.words.iter().any(|t| t == w))
        .count() as i64;
    if addr_hits > 0 {
        score += addr_hits * pts::ADDRESS_TOKEN;
        matched.push("address");
    }

    // Nothing identified this ticket — recency and status are tie-breakers, not
    // reasons, so they never lift a zero into the result set.
    if score == 0 {
        return (0, matched);
    }

    match age_days(meta_str(meta, "created_time"), now) {
        Some(d) if d <= 120 => score += pts::RECENT_120D,
        Some(d) if d <= 365 => score += pts::RECENT_YEAR,
        _ => {}
    }
    if status_is_active(meta_str(meta, "status")) {
        score += pts::STATUS_ACTIVE;
    }

    (score, matched)
}

// ─── The endpoint's payload ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TicketHit {
    pub ticket_id: String,
    pub ticket_number: Value,
    pub subject: Value,
    pub status: Value,
    pub created_time: Value,
    pub company: Value,
    pub customer: Value,
    pub city: Value,
    pub zip: Value,
    pub address: Value,
    pub phone: Value,
    pub email: Value,
    pub serial_number: Value,
    pub device_model: Value,
    pub manufacturing_date: Value,
    pub score: i64,
    pub matched: Vec<&'static str>,
}

fn field(meta: &Value, k: &str) -> Value {
    match meta.get(k) {
        Some(Value::String(s)) if s.trim().is_empty() => Value::Null,
        Some(v) if !v.is_null() => v.clone(),
        _ => Value::Null,
    }
}

/// Rank every support ticket against `q`, best first. Rows scoring 0 are
/// dropped — an empty result is a real answer ("we have never seen this
/// device"), not an error the receiver has to interpret.
///
/// Ties break on recency: two Joy-Fitness tickets from the same town score
/// identically on the letterhead alone, and the fresher one is the one the
/// customer is standing there about.
pub async fn lookup_tickets(db: &SurrealDb, q: &str, limit: usize) -> Result<Value, String> {
    let tokens = tokenize(q);
    let limit = limit.clamp(1, 100);

    // Nothing usable in the blob — say so instead of ranking the whole corpus
    // by recency.
    if tokens.all.is_empty() {
        return Ok(json!({ "count": 0, "query_tokens": [], "tickets": [] }));
    }

    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, meta, status FROM document \
             WHERE type = 'support_ticket'",
        )
        .await
        .map_err(|e| format!("ticket scan: {}", e))?
        .take(0)
        .map_err(|e| format!("ticket scan: {}", e))?;

    let now = chrono::Utc::now();
    let mut hits: Vec<(TicketHit, i64)> = Vec::new();
    for row in &rows {
        let meta = row.get("meta").cloned().unwrap_or(Value::Null);
        if !meta.is_object() {
            continue;
        }
        let (score, matched) = score_ticket(&tokens, &meta, now);
        if score <= 0 {
            continue;
        }
        // `status` lives on `meta` for imported tickets and on the row for
        // legacy ones; prefer meta, fall back to the column.
        let status = match field(&meta, "status") {
            Value::Null => row.get("status").cloned().unwrap_or(Value::Null),
            v => v,
        };
        let created = meta_str(&meta, "created_time").to_string();
        let recency = age_days(&created, now).map(|d| -d).unwrap_or(i64::MIN);

        hits.push((
            TicketHit {
                ticket_id: row
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ticket_number: field(&meta, "ticket_number"),
                subject: field(&meta, "subject"),
                status,
                created_time: field(&meta, "created_time"),
                company: field(&meta, "company"),
                customer: field(&meta, "customer"),
                city: field(&meta, "city"),
                zip: field(&meta, "zip"),
                address: field(&meta, "address"),
                phone: field(&meta, "phone"),
                email: field(&meta, "email"),
                serial_number: field(&meta, "serial_number"),
                device_model: field(&meta, "device_model"),
                manufacturing_date: field(&meta, "manufacturing_date"),
                score,
                matched,
            },
            recency,
        ));
    }

    hits.sort_by(|a, b| b.0.score.cmp(&a.0.score).then(b.1.cmp(&a.1)));
    let total = hits.len();
    let tickets: Vec<TicketHit> = hits.into_iter().take(limit).map(|(h, _)| h).collect();

    Ok(json!({
        "count": tickets.len(),
        "total_matches": total,
        "query_tokens": tokens.all,
        "tickets": tickets,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(
        number: &str,
        company: &str,
        city: &str,
        zip: &str,
        address: &str,
        serial: &str,
    ) -> Value {
        json!({
            "ticket_number": number,
            "subject": "Reparatur",
            "status": "Open",
            "company": company,
            "customer": company,
            "city": city,
            "zip": zip,
            "address": address,
            "serial_number": serial,
            "device_model": "ZetaBody 770",
            "email": "",
            "phone": "",
            "created_time": "2020-01-01T00:00:00.000Z",
        })
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-10T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn confusables_fold_both_ways() {
        assert_eq!(
            normalize_confusables("181700403"),
            normalize_confusables("I81700403")
        );
        assert_eq!(
            normalize_confusables("F9l7O2145"),
            normalize_confusables("F91702145")
        );
        // S/5 and B/8 too, and separators are dropped.
        assert_eq!(
            normalize_confusables("S8-1234"),
            normalize_confusables("5b1234")
        );
        // Two genuinely different serials must NOT collapse.
        assert_ne!(
            normalize_confusables("T425002043"),
            normalize_confusables("H222000549")
        );
    }

    #[test]
    fn umlauts_fold() {
        assert_eq!(fold_word("Müller"), "mueller");
        assert_eq!(fold_word("MUELLER"), "mueller");
        assert_eq!(fold_word("Straße"), "strasse");
    }

    #[test]
    fn tokenizer_buckets_an_ocr_blob() {
        let t = tokenize(
            "Reparaturauftrag\nMagic Fit Dornbirn\nBildgasse 18\n6850 Dornbirn\nZetaBody 770  S/N 181700403",
        );
        assert!(t.serials.contains(&normalize_confusables("I81700403")));
        assert!(t.zips.contains(&"6850".to_string()));
        assert!(t.ticket_numbers.contains(&"6850".to_string()));
        assert!(t.words.contains(&"dornbirn".to_string()));
        assert!(t.words.contains(&"bildgasse".to_string()));
        assert!(t.words.contains(&"magic".to_string()));
        // "Fit", "18", "S", "N" are below the length floors.
        assert!(!t.words.contains(&"fit".to_string()));
        // 9-digit run is a serial candidate, not a word.
        assert!(!t.words.iter().any(|w| w.contains("181700403")));
    }

    #[test]
    fn tokenizer_finds_email_and_phone() {
        let t = tokenize("Kontakt: Anna.Mueller@magic-fit.at  Tel. +43 5572 / 123 4567");
        assert_eq!(t.emails, vec!["anna.mueller@magic-fit.at".to_string()]);
        assert!(t.phone_tails.contains(&"1234567".to_string()));
    }

    /// The headline case: a letterhead blob must rank its own ticket first.
    #[test]
    fn ocr_blob_ranks_the_matching_ticket_first() {
        let t = tokenize(
            "Reparaturauftrag Magic Fit Dornbirn Bildgasse 18 6850 ZetaBody 770 S/N 181700403",
        );
        let mine = ticket(
            "32834",
            "Magic Fit Dornbirn",
            "Dornbirn",
            "6850",
            "Bildgasse 18",
            "181700403",
        );
        let other = ticket(
            "30654",
            "Joy Fitness",
            "Buxtehude",
            "21614",
            "Hauptstrasse 4",
            "F91702145",
        );
        // A third ticket from the SAME town but another firm: place matches,
        // serial does not — it must land behind.
        let neighbour = ticket(
            "31000",
            "Fitness Studio Vorarlberg",
            "Dornbirn",
            "6850",
            "Messestrasse 2",
            "T425002043",
        );

        let (s_mine, m_mine) = score_ticket(&t, &mine, now());
        let (s_other, _) = score_ticket(&t, &other, now());
        let (s_neighbour, _) = score_ticket(&t, &neighbour, now());

        assert!(m_mine.contains(&"serial"), "matched: {m_mine:?}");
        assert!(s_mine > s_neighbour, "{s_mine} vs {s_neighbour}");
        assert!(s_neighbour > s_other, "{s_neighbour} vs {s_other}");
        assert_eq!(s_other, 0, "an unrelated ticket must score nothing");
    }

    /// A serial the camera mangled must still find the ticket typed correctly.
    #[test]
    fn ocr_mangled_serial_still_finds_the_ticket() {
        let t = tokenize("F9l7O2145");
        let joy = ticket(
            "32862",
            "Joy Fitness",
            "Buxtehude",
            "21614",
            "Hauptstrasse 4",
            "F91702145",
        );
        let (score, matched) = score_ticket(&t, &joy, now());
        assert!(matched.contains(&"serial"));
        assert!(score >= pts::SERIAL, "score {score}");
    }

    /// …and the reverse: a ticket typed with the OCR spelling is found by the
    /// serial actually printed on the device.
    #[test]
    fn correct_serial_finds_a_ticket_typed_with_confusables() {
        let t = tokenize("I81700403");
        let typo = ticket("32834", "Magic Fit", "Dornbirn", "6850", "Bildgasse 18", "181700403");
        let (score, matched) = score_ticket(&t, &typo, now());
        assert!(matched.contains(&"serial"), "matched: {matched:?}");
        assert!(score >= pts::SERIAL);
    }

    #[test]
    fn zip_plus_town_beats_zip_alone() {
        let t = tokenize("Buxtehude 21614");
        let joy = ticket("30654", "Joy Fitness", "Buxtehude", "21614", "Hauptstrasse 4", "F91702145");
        let zip_only = ticket("30655", "Andere GmbH", "Neu Wulmstorf", "21614", "Weg 1", "H222000549");
        let (s_joy, m_joy) = score_ticket(&t, &joy, now());
        let (s_zip, _) = score_ticket(&t, &zip_only, now());
        assert!(m_joy.contains(&"zip") && m_joy.contains(&"city"));
        assert_eq!(s_joy - s_zip, pts::ZIP_CITY_BONUS + pts::CITY);
    }

    #[test]
    fn ticket_number_and_email_and_phone_score() {
        let t = tokenize("#32834 anna@magic-fit.at +43 5572 1234567");
        let mut m = ticket("32834", "Magic Fit", "Dornbirn", "6850", "Bildgasse 18", "x");
        m["email"] = json!("Anna@Magic-Fit.at");
        m["phone"] = json!("0043 5572 123 4567");
        let (score, matched) = score_ticket(&t, &m, now());
        assert!(matched.contains(&"ticket_number"));
        assert!(matched.contains(&"email"));
        assert!(matched.contains(&"phone"));
        assert!(score >= pts::TICKET_NUMBER + pts::EMAIL + pts::PHONE);
    }

    #[test]
    fn recency_and_status_only_break_ties_never_create_hits() {
        let t = tokenize("Buxtehude");
        let mut stale = ticket("1", "Joy Fitness", "Buxtehude", "21614", "Weg 1", "s");
        stale["status"] = json!("Closed");
        let mut fresh = stale.clone();
        fresh["created_time"] = json!("2026-08-01T00:00:00.000Z");
        fresh["status"] = json!("Open");

        let (s_stale, _) = score_ticket(&t, &stale, now());
        let (s_fresh, _) = score_ticket(&t, &fresh, now());
        assert_eq!(s_fresh - s_stale, pts::RECENT_120D + pts::STATUS_ACTIVE);

        // An unrelated ticket stays at zero however fresh and open it is.
        let unrelated = {
            let mut u = ticket("2", "Nichts AG", "Kiel", "24103", "Weg 9", "z");
            u["created_time"] = json!("2026-09-01T00:00:00.000Z");
            u
        };
        assert_eq!(score_ticket(&t, &unrelated, now()).0, 0);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let t = tokenize("   ,, -- ");
        assert!(t.all.is_empty());
        let any = ticket("1", "Joy Fitness", "Buxtehude", "21614", "Weg 1", "s");
        assert_eq!(score_ticket(&t, &any, now()).0, 0);
    }

    /// End-to-end against a `mem://` DB: the scan, the projection, the ordering.
    #[tokio::test]
    async fn lookup_scans_and_ranks() {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();
        db.query("DEFINE TABLE IF NOT EXISTS document SCHEMALESS")
            .await
            .unwrap()
            .check()
            .unwrap();

        for (id, meta) in [
            (
                "t1",
                ticket("32834", "Magic Fit Dornbirn", "Dornbirn", "6850", "Bildgasse 18", "181700403"),
            ),
            (
                "t2",
                ticket("30654", "Joy Fitness", "Buxtehude", "21614", "Hauptstrasse 4", "F91702145"),
            ),
        ] {
            db.query("UPSERT type::record($rid) MERGE { type: 'support_ticket', meta: $meta }")
                .bind(("rid", format!("document:`{id}`")))
                .bind(("meta", meta))
                .await
                .unwrap()
                .check()
                .unwrap();
        }

        let res = lookup_tickets(
            &db,
            "Reparaturauftrag Magic Fit Dornbirn Bildgasse 18 6850 ZetaBody 770 S/N 181700403",
            20,
        )
        .await
        .unwrap();
        assert_eq!(res["count"], json!(1));
        assert_eq!(res["tickets"][0]["ticket_number"], json!("32834"));
        assert_eq!(res["tickets"][0]["ticket_id"], json!("t1"));

        // The OCR-mangled serial finds the other one.
        let res = lookup_tickets(&db, "F9l7O2145", 20).await.unwrap();
        assert_eq!(res["tickets"][0]["ticket_number"], json!("30654"));
        assert_eq!(res["tickets"][0]["device_model"], json!("ZetaBody 770"));

        // Nothing in the corpus matches.
        let res = lookup_tickets(&db, "Flensburg 24937", 20).await.unwrap();
        assert_eq!(res["count"], json!(0));
    }
}
