//! Lever 2B′ — place the residual office-pinned pile from customer records we
//! ALREADY trust, entirely on the box.
//!
//! Between "the summary had a PLZ-Ort" (2b) and "guess the town from the phone
//! area code" (2c) sits a stronger, still-free signal: the *same customer* often
//! appears on ANOTHER support ticket that we already located, or on an Exact
//! Online `partner` row that carries a city. If we can identify the customer
//! behind an office-pinned ticket — by an exact email, the same phone number, or
//! the same company name — we can copy the location we already hold for them.
//! That beats area-code guessing on precision (a real zip vs a town centroid).
//!
//! PRIVACY MODEL: this lever is 100% local. Identity keys (email / phone /
//! company) are matched *in memory* against records already in our own database;
//! nothing about the customer is sent anywhere. The only thing that ever leaves
//! the box is a **city/zip string we already stored** for that customer, handed
//! to the same cached Nominatim resolver every other lever uses (never the
//! street, never the phone, never the name). A value that is really a PPRL
//! pseudonym (`Company_<hex>`, `Email_<hex>`, …) is treated as absent for every
//! kind of matching — a hash is not an identity and must never be geocoded or,
//! elsewhere, searched for.
//!
//! Matching is deterministic and conservative: identity tiers are tried
//! strongest-first (email → phone → company-exact → company-fuzzy) and we STOP
//! at the first tier that yields any candidate. Whatever that tier produced then
//! has to pass a confidence gate — every candidate must agree on the city, or we
//! give up. "Only place it when we're sure."

use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use std::collections::HashSet;
use tracing::{error, info};

/// PPRL accept threshold for the fuzzy company tier. Names at or above this
/// trigram-Jaccard similarity are treated as the same business.
const FUZZY_ACCEPT: f64 = 0.85;
/// A runner-up in a DIFFERENT city that scores at/above this is "still plausible"
/// — its presence makes the match ambiguous and we bail.
const FUZZY_AMBIG: f64 = 0.70;

// ---------------------------------------------------------------------------
// PPRL token guard
// ---------------------------------------------------------------------------

/// True when a value is a PPRL pseudonym (e.g. `Company_89A102ED05B7577E`),
/// not real data — i.e. it matches `^(Company|Name|Email|Phone|Address)_[0-9A-Fa-f]{8,}$`.
///
/// Such tokens must be treated as ABSENT for every kind of matching here, and
/// (via the address-discovery call sites that reuse this) must never be sent to
/// Google grounding — we never ask the world to search for a hash.
pub(crate) fn is_pprl_token(s: &str) -> bool {
    let s = s.trim();
    let rest = ["Company_", "Name_", "Email_", "Phone_", "Address_"]
        .iter()
        .find_map(|p| s.strip_prefix(p));
    match rest {
        Some(hex) => hex.len() >= 8 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Identity-key normalization (both the office-pinned ticket and every source
// record are reduced to the SAME normalized keys before comparison).
// ---------------------------------------------------------------------------

/// Reduce an email to an identity key: trimmed + lowercased. None for empty
/// values, PPRL tokens, or anything without an `@` (not a real address). Freemail
/// is fine here — full-address equality is the same person regardless of provider.
fn email_key(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() || is_pprl_token(t) {
        return None;
    }
    let lc = t.to_lowercase();
    if lc.contains('@') {
        Some(lc)
    } else {
        None
    }
}

/// Reduce a phone to a national-number identity key: digits only, strip a German
/// country code (`0049` / `+49`) or a single leading trunk `0`. Unlike the
/// vorwahl parser we deliberately KEEP mobiles — a mobile number carries no
/// geography but is a perfectly good *identity* key. None for PPRL tokens, empty
/// values, or a normalized form shorter than 6 digits (too short to trust).
fn phone_key(phone: &str) -> Option<String> {
    if is_pprl_token(phone.trim()) {
        return None;
    }
    let had_plus = phone.trim_start().starts_with('+');
    let raw: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if raw.is_empty() {
        return None;
    }
    let nat = if let Some(r) = raw.strip_prefix("0049") {
        r.to_string()
    } else if had_plus && raw.starts_with("49") {
        raw[2..].to_string()
    } else if let Some(r) = raw.strip_prefix('0') {
        r.to_string()
    } else {
        raw
    };
    if nat.len() < 6 {
        return None;
    }
    Some(nat)
}

/// Normalize a company name for comparison: HTML-entity-decode the handful of
/// entities the scrapers leave behind, lowercase, punctuation → space, drop
/// standalone legal-form tokens, collapse whitespace. Both sides of any company
/// comparison run through this identically.
fn normalize_company(raw: &str) -> String {
    // Decode the few entities that actually appear in scraped names, before
    // lowercasing (HTML entities are lowercase; `&#39;` carries digits).
    let decoded = raw
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let lower = decoded.to_lowercase();
    // Everything that is not a (unicode) letter/digit becomes a space, so `&`,
    // dots, slashes, dashes all split tokens. Umlauts survive (is_alphanumeric).
    let spaced: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    // Standalone legal-form / conjunction tokens carry no identity — "GmbH",
    // "e.V." (→ "e v"), "u." (→ "u"), "& Co." (→ "co") etc.
    const LEGAL: &[&str] = &[
        "gmbh", "ag", "kg", "ug", "ohg", "gbr", "mbh", "co", "ev", "e", "u", "v", "ltd",
    ];
    let tokens: Vec<&str> = spaced
        .split_whitespace()
        .filter(|t| !LEGAL.contains(t))
        .collect();
    tokens.join(" ")
}

/// Company identity key: normalized, or None for empty / PPRL-token inputs.
fn company_key(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() || is_pprl_token(t) {
        return None;
    }
    let n = normalize_company(t);
    if n.is_empty() {
        None
    } else {
        Some(n)
    }
}

// ---------------------------------------------------------------------------
// Trigram-set Jaccard similarity (inline — no new dependency). Used only by the
// company-fuzzy tier as a last resort.
// ---------------------------------------------------------------------------

/// The set of character trigrams of `s`, padded so short strings still yield
/// boundary-sensitive grams.
fn trigrams(s: &str) -> HashSet<String> {
    let padded = format!(" {s} ");
    let chars: Vec<char> = padded.chars().collect();
    let mut set = HashSet::new();
    if chars.len() < 3 {
        if !s.is_empty() {
            set.insert(s.to_string());
        }
        return set;
    }
    for w in chars.windows(3) {
        set.insert(w.iter().collect::<String>());
    }
    set
}

/// Trigram-set Jaccard similarity of two already-normalized strings, in [0,1]:
/// |A∩B| / |A∪B|. Because the union sits in the denominator this metric PUNISHES
/// length differences — a short name embedded in a longer, more specific one
/// scores low, which is exactly what we want (we only fuzzy-accept near-identical
/// names, never a substring).
fn trigram_similarity(a: &str, b: &str) -> f64 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

// ---------------------------------------------------------------------------
// Source universe + the confidence gate (pure, DB-free helpers).
// ---------------------------------------------------------------------------

/// One trusted location record (another located ticket, or an Exact partner),
/// reduced to its identity keys + the single location we'd copy from it.
struct Source {
    /// Record id for the provenance trail (`meta.customer_match.ref`).
    ref_id: String,
    email: Option<String>,
    phone: Option<String>,
    /// Normalized company/partner name.
    company: Option<String>,
    /// Zip that belongs to `city` on the SAME record (never mixed across records).
    zip: Option<String>,
    /// City as stored (display form).
    city: String,
    /// City lowercased+trimmed, for the agreement check.
    city_norm: String,
}

/// The confidence gate. Given the candidate sources of the WINNING tier, decide
/// the single location to write, or None when we cannot be sure:
/// - every candidate must agree on the (normalized) city, else give up;
/// - among the agreeing candidates, take the most specific (longest, non-empty)
///   zip and the city FROM THE SAME record — never mix a zip from one candidate
///   with a city from another.
fn decide_location(cands: &[&Source]) -> Option<(Option<String>, String, String)> {
    let first = *cands.first()?;
    if cands.iter().any(|c| c.city_norm != first.city_norm) {
        return None; // conflicting cities → not sure → give up
    }
    let chosen = cands
        .iter()
        .copied()
        .filter(|c| c.zip.as_deref().map(|z| !z.is_empty()).unwrap_or(false))
        .max_by_key(|c| c.zip.as_deref().map(|z| z.len()).unwrap_or(0))
        .unwrap_or(first);
    let zip = chosen.zip.clone().filter(|z| !z.is_empty());
    Some((zip, chosen.city.clone(), chosen.ref_id.clone()))
}

/// Company-fuzzy tier (last resort). Scores every source's normalized company
/// against the ticket's with trigram-set Jaccard and returns the winning set
/// ONLY when we're confident: the best score is >= `FUZZY_ACCEPT` AND no
/// DIFFERENT-city competitor scores >= `FUZZY_AMBIG`. None otherwise, so an
/// ambiguous name is never placed. The returned set (all sources at/above the
/// accept threshold) is still passed through `decide_location`, which enforces a
/// single city.
fn fuzzy_candidates<'a>(ticket_company: &str, sources: &'a [Source]) -> Option<Vec<&'a Source>> {
    let mut scored: Vec<(f64, &Source)> = sources
        .iter()
        .filter_map(|s| {
            s.company
                .as_deref()
                .map(|c| (trigram_similarity(ticket_company, c), s))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (best_score, best) = scored[0];
    if best_score < FUZZY_ACCEPT {
        return None;
    }
    let ambiguous = scored
        .iter()
        .skip(1)
        .any(|(sc, s)| *sc >= FUZZY_AMBIG && s.city_norm != best.city_norm);
    if ambiguous {
        return None;
    }
    let winners: Vec<&Source> = scored
        .iter()
        .filter(|(sc, _)| *sc >= FUZZY_ACCEPT)
        .map(|(_, s)| *s)
        .collect();
    Some(winners)
}

/// Location truth of a source ticket, as a PAIR from ONE record: prefer the
/// structured `meta.zip`/`meta.city`, else the publicly-derived
/// `derived_address.zip`/`city`. None when neither carries a city.
fn ticket_location(
    meta_zip: Option<&str>,
    meta_city: Option<&str>,
    der_zip: Option<&str>,
    der_city: Option<&str>,
) -> Option<(Option<String>, String)> {
    let clean = |o: Option<&str>| o.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    if let Some(c) = clean(meta_city) {
        return Some((clean(meta_zip), c));
    }
    if let Some(c) = clean(der_city) {
        return Some((clean(der_zip), c));
    }
    None
}

// ---------------------------------------------------------------------------
// DB access
// ---------------------------------------------------------------------------

/// Load the trusted location universe: every OTHER support ticket that already
/// has a real (non-fallback, non-vorwahl-guessed) location, plus every Exact
/// `partner` row that carries a city. Projections are kept tiny. Records with no
/// usable identity key or no city are dropped here.
async fn load_sources(db: &SurrealDb, self_id: &str) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();

    // Located support tickets. `geo_fallback IS NOT true` drops the HQ pile;
    // the `geo_source = 'vorwahl'` filter (applied in Rust) drops area-code
    // guesses — those are coarse and must not become "truth" for someone else.
    let tsql = "SELECT record::id(id) AS id, meta.email AS email, meta.phone AS phone, \
                meta.company AS company, meta.zip AS zip, meta.city AS city, \
                meta.derived_address AS derived, meta.geo_source AS geo_source \
                FROM document \
                WHERE type = 'support_ticket' AND meta.geo != NONE \
                  AND meta.geo_fallback IS NOT true \
                  AND (meta.zip != '' OR meta.city != '' OR meta.derived_address.city != NONE)";
    match db.query(tsql).await.and_then(|mut r| r.take(0)) {
        Ok(rows) => {
            let rows: Vec<Value> = rows;
            for row in &rows {
                let rid = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if rid.is_empty() || rid == self_id {
                    continue;
                }
                if row.get("geo_source").and_then(|v| v.as_str()) == Some("vorwahl") {
                    continue;
                }
                let der = row.get("derived");
                let (zip, city) = match ticket_location(
                    row.get("zip").and_then(|v| v.as_str()),
                    row.get("city").and_then(|v| v.as_str()),
                    der.and_then(|d| d.get("zip")).and_then(|v| v.as_str()),
                    der.and_then(|d| d.get("city")).and_then(|v| v.as_str()),
                ) {
                    Some(x) => x,
                    None => continue,
                };
                let email = row.get("email").and_then(|v| v.as_str()).and_then(email_key);
                let phone = row.get("phone").and_then(|v| v.as_str()).and_then(phone_key);
                let company = row.get("company").and_then(|v| v.as_str()).and_then(company_key);
                if email.is_none() && phone.is_none() && company.is_none() {
                    continue;
                }
                let city_norm = city.trim().to_lowercase();
                out.push(Source {
                    ref_id: format!("document:{rid}"),
                    email,
                    phone,
                    company,
                    zip,
                    city,
                    city_norm,
                });
            }
        }
        Err(e) => error!("[CustomerGeo] source ticket select failed: {e}"),
    }

    // Exact Online partners with a city (sparse today; coverage grows later).
    let psql = "SELECT record::id(id) AS id, name, phone, email, city FROM partner WHERE city != ''";
    match db.query(psql).await.and_then(|mut r| r.take(0)) {
        Ok(rows) => {
            let rows: Vec<Value> = rows;
            for row in &rows {
                let rid = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if rid.is_empty() {
                    continue;
                }
                let city = match row
                    .get("city")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(c) => c.to_string(),
                    None => continue,
                };
                let email = row.get("email").and_then(|v| v.as_str()).and_then(email_key);
                let phone = row.get("phone").and_then(|v| v.as_str()).and_then(phone_key);
                let company = row.get("name").and_then(|v| v.as_str()).and_then(company_key);
                if email.is_none() && phone.is_none() && company.is_none() {
                    continue;
                }
                let city_norm = city.trim().to_lowercase();
                out.push(Source {
                    // Partners carry no zip → coarse (city-centre) placement.
                    ref_id: format!("partner:{rid}"),
                    email,
                    phone,
                    company,
                    zip: None,
                    city,
                    city_norm,
                });
            }
        }
        Err(e) => error!("[CustomerGeo] partner select failed: {e}"),
    }

    out
}

/// Run the confidence gate over one tier's candidates and, if it commits to a
/// single location, geocode + write it. Writes mirror the vorwahl source-stamp:
/// `save_geo_resolved` clears the fallback and sets geo/coarse, then a small
/// second UPDATE tags `geo_source='customer_db'` + the `customer_match`
/// provenance, and we re-advance the vector clock so the tag survives a mesh
/// merge. Returns the placed city, or None (ambiguous tier / ungeocodable).
async fn place_from_candidates(
    db: &SurrealDb,
    id: &str,
    via: &str,
    cands: &[&Source],
) -> Option<String> {
    let (zip, city, ref_id) = decide_location(cands)?;
    let (lat, lng) =
        crate::services::geocoder::resolve_zip_city_cached(db, zip.as_deref(), Some(city.as_str()))
            .await?;
    let coarse = zip.is_none();

    // Clear the HQ pin, set the resolved coords + geo_coarse, bump the clock.
    crate::services::geocoder::save_geo_resolved(db, "document", id, "meta", lat, lng, coarse).await;

    // Stamp the source tag + provenance separately (small UPDATE), then re-bump.
    let matchinfo = json!({ "via": via, "ref": ref_id, "city": city, "zip": zip });
    let q = "UPDATE document SET \
             meta.geo_source = 'customer_db', \
             meta.customer_match = $m, \
             updated_at = time::now() \
         WHERE record::id(id) = $id RETURN NONE";
    if let Err(e) = db
        .query(q)
        .bind(("id", id.to_string()))
        .bind(("m", matchinfo))
        .await
    {
        error!("[CustomerGeo] {id}: source-tag write failed: {e}");
    }
    crate::services::geocoder::bump_geo_vclock(db, "document", id).await;

    info!("[CustomerGeo] {id}: matched via {via} -> {city} ({ref_id})");
    Some(city)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Place ONE office-pinned ticket from a customer record we already trust.
///
/// Identifies the customer behind `id` from its (clear-text) `meta.email`,
/// `meta.phone`, or `meta.company`, matches those keys against the local source
/// universe strongest-first (email → phone → company-exact → company-fuzzy),
/// stops at the first tier that yields any candidate, and — if that tier's
/// candidates agree on a city — geocodes the stored city/zip via the cached
/// resolver and writes the result tagged `geo_source='customer_db'`. Returns the
/// placed city on success. 100% local: only a city/zip we already hold is ever
/// looked up; the customer's identity never leaves the box.
pub async fn try_place_by_customer_db(db: &SurrealDb, id: &str, meta: &Value) -> Option<String> {
    let sources = load_sources(db, id).await;
    try_place_with_sources(db, id, meta, &sources).await
}

/// Same as [`try_place_by_customer_db`] but against a pre-loaded source
/// universe, so a batch caller loads it ONCE instead of per ticket (the hourly
/// sweep would otherwise re-scan ~1.7k documents for every pinned ticket).
/// A pinned ticket can never appear in its own source universe (sources require
/// a real, non-fallback location), so sharing one universe across the batch is
/// safe.
async fn try_place_with_sources(
    db: &SurrealDb,
    id: &str,
    meta: &Value,
    sources: &[Source],
) -> Option<String> {
    let ticket_email = meta.get("email").and_then(|v| v.as_str()).and_then(email_key);
    let ticket_phone = meta.get("phone").and_then(|v| v.as_str()).and_then(phone_key);
    let ticket_company = meta.get("company").and_then(|v| v.as_str()).and_then(company_key);
    if ticket_email.is_none() && ticket_phone.is_none() && ticket_company.is_none() {
        return None;
    }
    if sources.is_empty() {
        return None;
    }

    // Tier 1 — exact email.
    if let Some(te) = &ticket_email {
        let cands: Vec<&Source> = sources
            .iter()
            .filter(|s| s.email.as_deref() == Some(te.as_str()))
            .collect();
        if !cands.is_empty() {
            return place_from_candidates(db, id, "email", &cands).await;
        }
    }
    // Tier 2 — normalized phone (mobiles included).
    if let Some(tp) = &ticket_phone {
        let cands: Vec<&Source> = sources
            .iter()
            .filter(|s| s.phone.as_deref() == Some(tp.as_str()))
            .collect();
        if !cands.is_empty() {
            return place_from_candidates(db, id, "phone", &cands).await;
        }
    }
    // Tier 3 — exact-normalized company, then Tier 4 — fuzzy company.
    if let Some(tc) = &ticket_company {
        let cands: Vec<&Source> = sources
            .iter()
            .filter(|s| s.company.as_deref() == Some(tc.as_str()))
            .collect();
        if !cands.is_empty() {
            return place_from_candidates(db, id, "company_exact", &cands).await;
        }
        if let Some(cands) = fuzzy_candidates(tc, &sources) {
            return place_from_candidates(db, id, "company_fuzzy", &cands).await;
        }
    }

    None
}

/// On-demand batch: for residual office-pinned tickets, try to place each from a
/// trusted customer record (email / phone / company). Free + local — only stored
/// city/zip strings reach the geocoder. Spawned; logs under `[CustomerGeo]`.
/// `limit` caps the batch (0 = safety cap of 1000).
pub async fn customer_fill_batch(db: SurrealDb, limit: usize) {
    let lim = if limit == 0 { 1000 } else { limit };
    info!("[CustomerGeo] start (limit={lim})");

    let sql = format!(
        "SELECT record::id(id) AS id, meta FROM document \
         WHERE type = 'support_ticket' AND meta.geo_fallback = true \
           AND meta.geo_override IS NOT true \
           AND (meta.geo_source IS NONE \
                OR meta.geo_source IN ['auto_nomatch', 'needs_grounding', 'derived_failed']) \
         LIMIT {lim}"
    );
    let rows: Vec<Value> = match db.query(&sql).await.and_then(|mut r| r.take(0)) {
        Ok(r) => r,
        Err(e) => {
            error!("[CustomerGeo] select failed: {e}");
            return;
        }
    };
    if rows.is_empty() {
        info!("[CustomerGeo] Done: scanned=0 resolved=0 no_match=0");
        return;
    }

    // One source-universe load for the whole batch (see try_place_with_sources).
    let sources = load_sources(&db, "").await;

    let mut scanned = 0usize;
    let mut resolved = 0usize;
    let mut no_match = 0usize;

    for row in &rows {
        scanned += 1;
        let id = row
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let meta = row.get("meta").cloned().unwrap_or(json!({}));
        match try_place_with_sources(&db, &id, &meta, &sources).await {
            Some(city) => {
                resolved += 1;
                info!("[CustomerGeo] {id}: {city}");
            }
            None => no_match += 1,
        }
    }

    info!("[CustomerGeo] Done: scanned={scanned} resolved={resolved} no_match={no_match}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_normalization_decodes_entities_and_drops_legal_forms() {
        assert_eq!(
            normalize_company("Koschel Sport &amp; Health GmbH"),
            "koschel sport health"
        );
        // Entity-decoded + legal-form-stripped forms of the two spellings collapse
        // to the same key.
        assert_eq!(
            company_key("Koschel Sport &amp; Health GmbH"),
            company_key("koschel sport & health")
        );
        assert_eq!(
            company_key("relax Fitness &amp; Vital Lounge"),
            company_key("relax fitness & vital lounge")
        );
        assert_eq!(
            company_key("relax Fitness &amp; Vital Lounge").as_deref(),
            Some("relax fitness vital lounge")
        );
    }

    #[test]
    fn phone_key_normalizes_national_formats() {
        // Same number written three ways → one identity key.
        assert_eq!(phone_key("0170/8125735").as_deref(), Some("1708125735"));
        assert_eq!(phone_key("+49 170 8125735").as_deref(), Some("1708125735"));
        assert_eq!(phone_key("0049 170 8125735").as_deref(), Some("1708125735"));
        assert_eq!(phone_key("0170/8125735"), phone_key("+49 170 8125735"));
        assert_eq!(phone_key("0170/8125735"), phone_key("0049 170 8125735"));
        // Too short after normalization → rejected.
        assert!(phone_key("12345").is_none());
        assert!(phone_key("040 55").is_none()); // "4055" = 4 digits
    }

    #[test]
    fn pprl_tokens_are_treated_as_absent() {
        assert!(is_pprl_token("Company_89A102ED05B7577E"));
        assert!(is_pprl_token("Name_0011AABB"));
        assert!(is_pprl_token("Address_7544865BC65416D4"));
        assert!(!is_pprl_token("Koschel Sport")); // real name passes through
        assert!(!is_pprl_token("Company_XYZ")); // not hex
        assert!(!is_pprl_token("Company_ABC1")); // < 8 hex
        // A token in any identity field yields no key.
        assert!(company_key("Company_89A102ED05B7577E").is_none());
        assert!(phone_key("Phone_89A102ED05B7577E").is_none());
        assert!(email_key("Email_89A102ED05B7577E").is_none());
    }

    #[test]
    fn trigram_fuzzy_rejects_substring_expansion() {
        // "terra sports essen werden" is a specific branch (its Essen-Werden
        // district spelled out); a bare "terra sports" must NOT be treated as the
        // same business. Trigram-set Jaccard keeps the union in the denominator,
        // so the extra location tokens drag similarity well under the 0.85 accept
        // threshold — we only fuzzy-accept near-identical names, never a short
        // name embedded in a longer, more specific one.
        let a = normalize_company("terra sports Essen Werden");
        let b = normalize_company("Terra Sports");
        let sim = trigram_similarity(&a, &b);
        assert!(sim < FUZZY_ACCEPT, "expected < {FUZZY_ACCEPT}, got {sim}");
        // Identical names, of course, are a perfect match.
        assert!(
            trigram_similarity(&normalize_company("Terra Sports"), &b) >= FUZZY_ACCEPT
        );
    }

    fn src(ref_id: &str, zip: Option<&str>, city: &str) -> Source {
        Source {
            ref_id: ref_id.into(),
            email: None,
            phone: None,
            company: None,
            zip: zip.map(String::from),
            city: city.into(),
            city_norm: city.trim().to_lowercase(),
        }
    }

    #[test]
    fn ambiguity_gate_rejects_conflicting_cities() {
        // Winning tier produced two candidates in different cities → give up.
        let a = src("document:a", Some("50667"), "Köln");
        let b = src("document:b", Some("20095"), "Hamburg");
        assert!(decide_location(&[&a, &b]).is_none());

        // Same city → resolves, taking the most specific zip and its own city.
        let c = src("document:c", None, "Köln");
        let (zip, city, chosen_ref) = decide_location(&[&c, &a]).expect("agreeing cities resolve");
        assert_eq!(city, "Köln");
        assert_eq!(zip.as_deref(), Some("50667"));
        assert_eq!(chosen_ref, "document:a"); // the record that carried the zip
    }
}
