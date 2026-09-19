//! Lever 2B″ — place the residual office-pinned pile from an address OCR'd out
//! of the ticket's OWN attachments (invoices, service forms, delivery notes).
//!
//! Between "the summary had a PLZ-Ort" / "a trusted customer record has a city"
//! (2b / 2b′) and "guess the town from the phone area code" (2c) sits another
//! free, still-precise signal: a scanned invoice or form usually prints the
//! customer's full postal address, and the attachment-extraction sweep has
//! already OCR'd it into `file_resource.extracted_text`. Scanning that text for a
//! "PLZ Ort" gives a real postal code — better than the vorwahl centroid — at
//! zero cost and without any network egress.
//!
//! THE TRAP: every service form and invoice prints OUR OWN address in the header
//! (the operating company's HQ, e.g. its head-office street + PLZ-Ort). A naive
//! "first PLZ-Ort in the OCR" lever would pin every ticket to HQ, the exact opposite of
//! the goal. Two defences: (1) a HARD blacklist drops any candidate carrying the
//! HQ zip / city / street unconditionally; (2) we scan ALL addresses in the text
//! and pick by agreement — a candidate that confirms a location we already hold
//! for the customer, else the single most-frequent locality, and NEVER a tie
//! between different localities (ambiguous beats wrong). A blank template form
//! contains only the HQ header, so it yields no candidate and places nothing.
//!
//! PRIVACY MODEL: 100% local. The stored extract is scanned in memory; only the
//! resolved zip+city (a public place — never the street, never our header) is
//! handed to the same cached Nominatim resolver every other lever uses. Nominatim
//! is itself the last filter: an OCR-noise "12345 Serial" that is not a real
//! place simply fails to geocode and nothing is written.

use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Own-HQ blacklist — the operating company's own header address, which sits on
// EVERY form/invoice. The markers come from env `ECK_OWN_ADDRESS_MARKERS`
// (shared with ai::embeddings via eck_core; EMPTY = drop nothing) so no
// address is hardcoded here.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pure candidate extraction + selection (DB-free, unit-tested).
// ---------------------------------------------------------------------------

/// A geo candidate mined from attachment OCR text: a PLZ+city and how many times
/// that locality was seen across the scanned attachments (the frequency signal).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GeoCandidate {
    pub zip: String,
    pub city: String,
    pub count: usize,
}

/// Scan every text with the shared PLZ+Ort parser, drop our HQ header address,
/// and aggregate the survivors by locality (so the same address across several
/// attachments — or repeated on one — reinforces one candidate). Returned
/// most-frequent first, ties broken by zip for a stable order.
pub(crate) fn attachment_geo_candidates(texts: &[&str]) -> Vec<GeoCandidate> {
    attachment_geo_candidates_with(texts, eck_core::utils::anonymizer::own_address_markers())
}

/// Inner form taking the own-HQ address markers explicitly (env
/// `ECK_OWN_ADDRESS_MARKERS` in production; injected in tests). EMPTY markers =
/// drop nothing. The operating company's own HQ zip/city (env `ECK_GEO_HQ_ZIP` /
/// `ECK_GEO_HQ_CITY`, empty by default = drop nothing) is dropped unconditionally.
pub(crate) fn attachment_geo_candidates_with(
    texts: &[&str],
    own_markers: &[String],
) -> Vec<GeoCandidate> {
    attachment_geo_candidates_full(texts, own_markers, &hq_zip_env(), &hq_city_env())
}

/// The operating company's own HQ postal code — env `ECK_GEO_HQ_ZIP`, EMPTY by default
/// (multi-tenant: set per deployment, e.g. ECK_GEO_HQ_ZIP=65760 for the Eschborn office behind the `ECK_GEO_FALLBACK`
/// office pin). A candidate whose zip equals this is our own address, never the
/// customer's, and is dropped.
fn hq_zip_env() -> String {
    std::env::var("ECK_GEO_HQ_ZIP")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// The operating company's own HQ city — env `ECK_GEO_HQ_CITY`, EMPTY by default (multi-tenant: the operator sets it per deployment, e.g. ECK_GEO_HQ_CITY=Eschborn).
fn hq_city_env() -> String {
    std::env::var("ECK_GEO_HQ_CITY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Field-name labels that mark an address block as OURS (or a vendor's), never the
/// customer's — the tenant's form templates print return/sender/recipient headers
/// ("Absender <tenant>", "Rücksendeadresse", "Lieferant", "Empfänger") whose zip/city
/// must never pin the ticket. Matched as substrings of the lowercased field key.
fn is_own_address_label(key_lower: &str) -> bool {
    // Generic German form labels only — the operator's own company name comes
    // from `ECK_OWN_ADDRESS_MARKERS` (multi-tenant: no customer name in code).
    const LABELS: &[&str] = &[
        "lieferant", "empfänger", "empfaenger", "absender", "rücksende",
        "ruecksende", "rucksende",
    ];
    LABELS.iter().any(|l| key_lower.contains(l))
}

/// Drop any `Feldname: Wert` line whose field name is an own-address label (see
/// [`is_own_address_label`]) BEFORE either miner runs, so the HQ/vendor block on a
/// form template feeds neither the one-line scanner nor the field-pair pass. Lines
/// with no `:` (ordinary OCR/text-layer prose) pass through unchanged, so this is a
/// no-op on non-form text.
fn strip_own_labeled_lines(text: &str, own_markers: &[String]) -> String {
    text.lines()
        .filter(|line| match line.split_once(':') {
            Some((key, _)) => {
                let k = key.to_lowercase();
                !is_own_address_label(&k) && !own_markers.iter().any(|m| !m.is_empty() && k.contains(m.as_str()))
            }
            None => true,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Full inner form with the own-HQ markers AND the HQ zip/city injected (both
/// env-derived in production, hard-set in tests). A candidate is dropped when its
/// zip OR city equals the HQ, when it hits an own-address marker, or when it is a
/// newline-bridged scanner artifact (city carries a stray `:`).
fn attachment_geo_candidates_full(
    texts: &[&str],
    own_markers: &[String],
    hq_zip: &str,
    hq_city: &str,
) -> Vec<GeoCandidate> {
    let hq_zip = hq_zip.trim();
    let hq_city_norm = hq_city.trim().to_lowercase();
    let mut agg: HashMap<(String, String), GeoCandidate> = HashMap::new();
    let mut add = |zip: String, city: String| {
        // The one-line "PLZ Ort" scanner skips newlines, so on the appended
        // `--- Formularfelder ---` block it can bridge a bare `PLZ: 78467` line
        // into the NEXT field line and capture "Ort: Konstanz" (or any
        // "Feldname: Wert") as the city. Such an artifact always carries the
        // next field's colon; a real city never does — drop it (the pairing pass
        // below re-derives the clean candidate).
        if city.contains(':') {
            return;
        }
        let city_norm = city.trim().to_lowercase();
        // HARD own-HQ drop by exact zip/city — the operating company's own address
        // sits in EVERY form template's fields/header, so it must never be a
        // candidate regardless of which miner surfaced it (defect: 16/17 tickets
        // pinned to 65760 Eschborn, 2026-09-10).
        if (!hq_zip.is_empty() && zip == hq_zip) || (!hq_city_norm.is_empty() && city_norm == hq_city_norm) {
            return;
        }
        // Legacy own-HQ marker blacklist (env `ECK_OWN_ADDRESS_MARKERS`), kept for
        // deployments that configure additional markers beyond the HQ zip/city.
        if own_markers
            .iter()
            .any(|m| zip.contains(m.as_str()) || city_norm.contains(m.as_str()))
        {
            return;
        }
        agg.entry((zip.clone(), city_norm))
            .and_modify(|c| c.count += 1)
            .or_insert(GeoCandidate { zip, city, count: 1 });
    };
    for text in texts {
        // Strip our own return/sender/recipient field blocks up front so neither
        // miner ever mines them (they carry OUR address, not the customer's).
        let text = strip_own_labeled_lines(text, own_markers);
        // Reuse the geocoder's privacy-safe "PLZ Ort" scanner (the street always
        // precedes the postal code, so only zip+city are ever captured).
        let scanned = crate::services::geocoder::find_all_plz_ort(&text);
        let scanned_keys: HashSet<(String, String)> = scanned
            .iter()
            .map(|(z, c)| (z.clone(), c.trim().to_lowercase()))
            .collect();
        for (zip, city) in &scanned {
            add(zip.clone(), city.clone());
        }
        // A fillable form stores PLZ and Ort in SEPARATE fields, so the extractor
        // appends them as separate `PLZ: 78467` / `Ort: Konstanz` lines that the
        // one-line "PLZ Ort" scanner above never joins. Pair them here. Skip a
        // pair the line scanner already produced (a combined "PLZ/Ort" field) so
        // one address is not double-counted from both miners.
        for (zip, city) in mine_field_plz_ort_pairs(&text) {
            if scanned_keys.contains(&(zip.clone(), city.trim().to_lowercase())) {
                continue;
            }
            add(zip, city);
        }
    }
    let mut out: Vec<GeoCandidate> = agg.into_values().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.zip.cmp(&b.zip)));
    out
}

/// Pair a `PLZ`-ish form field (4–5 digit value) with a neighbouring `Ort`/`Stadt`
/// field into a `(zip, city)` candidate. Fillable forms print these on separate
/// `Feldname: Wert` lines (see `ai::attachments` `--- Formularfelder ---`), which
/// the single-line "PLZ Ort" scanner cannot join. Field names are tokenised on
/// non-letters so `Antwort`/`Standort` never match the `ort` token; a pending PLZ
/// is paired with the next city field seen (order-independent within the text).
fn mine_field_plz_ort_pairs(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut pending_zip: Option<String> = None;
    let mut pending_city: Option<String> = None;
    for line in text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let val = val.trim();
        if val.is_empty() {
            continue;
        }
        let key_l = key.to_lowercase();
        let tokens: Vec<&str> = key_l
            .split(|c: char| !c.is_alphabetic())
            .filter(|t| !t.is_empty())
            .collect();
        let is_plz_key = tokens.iter().any(|t| *t == "plz" || *t == "postleitzahl");
        let is_ort_key = tokens.iter().any(|t| *t == "ort" || *t == "stadt" || *t == "wohnort");

        if is_plz_key {
            let digits: String = val.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.len() == 4 || digits.len() == 5 {
                pending_zip = Some(digits);
            }
        } else if is_ort_key {
            // A plausible city: starts with a capitalised letter (drops "n/a",
            // "-", numeric noise). Terminate on a comma so a "City, Land" value
            // keeps only the city.
            let city: String = val.split(|c| matches!(c, ',' | '(')).next().unwrap_or(val).trim().to_string();
            if city.chars().next().map_or(false, |c| c.is_alphabetic() && c.is_uppercase()) {
                pending_city = Some(city);
            }
        }
        if let (Some(z), Some(c)) = (pending_zip.clone(), pending_city.clone()) {
            out.push((z, c));
            pending_zip = None;
            pending_city = None;
        }
    }
    out
}

/// Choose the winning locality, or None when we cannot be sure.
///
/// `known_zip`/`known_city` are an optional hint: a location we already hold for
/// this ticket's customer (its structured `meta.zip`/`meta.city`). If exactly one
/// distinct candidate locality confirms the hint, it wins outright — a form that
/// echoes the customer's own city is the strongest signal we can get. Otherwise
/// the single most-frequent candidate wins, but a tie between DIFFERENT localities
/// for the top spot yields None: ambiguous beats wrong (pinning the customer onto
/// a vendor address on their invoice is worse than leaving them on HQ).
pub(crate) fn choose_attachment_candidate<'a>(
    cands: &'a [GeoCandidate],
    known_zip: Option<&str>,
    known_city: Option<&str>,
) -> Option<&'a GeoCandidate> {
    if cands.is_empty() {
        return None;
    }

    // Preference: candidates that confirm a location we already hold.
    let kz = known_zip.map(str::trim).filter(|s| !s.is_empty());
    let kc = known_city
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    if kz.is_some() || kc.is_some() {
        let matches: Vec<&GeoCandidate> = cands
            .iter()
            .filter(|c| {
                let zip_ok = kz.map_or(false, |z| z == c.zip.as_str());
                let city_ok = kc
                    .as_deref()
                    .map_or(false, |k| k == c.city.trim().to_lowercase().as_str());
                zip_ok || city_ok
            })
            .collect();
        // Only let the hint decide when it points at ONE locality; if it somehow
        // confirmed two different cities, fall through to the frequency rule.
        let distinct: HashSet<String> =
            matches.iter().map(|c| c.city.trim().to_lowercase()).collect();
        if distinct.len() == 1 {
            // Most specific match first (an exact zip hit), then most frequent.
            return matches.into_iter().max_by_key(|c| {
                (kz.map_or(false, |z| z == c.zip.as_str()) as usize, c.count)
            });
        }
    }

    // Frequency winner with an ambiguity guard. `cands` is sorted count-desc, so
    // >1 candidate at the top count means DIFFERENT localities are equally common.
    let top_count = cands[0].count;
    if cands.iter().filter(|c| c.count == top_count).count() > 1 {
        return None;
    }
    Some(&cands[0])
}

// ---------------------------------------------------------------------------
// The lever (DB access) + the batch driver.
// ---------------------------------------------------------------------------

/// Read up to 8 of a ticket's attachments' non-empty stored extract texts, in one
/// graph query over the `has_attachment` edge (in = the document, out = the
/// file_resource). Uses the persisted `extracted_text` as-is (no re-OCR here).
async fn attachment_texts(db: &SurrealDb, id: &str) -> Vec<String> {
    let rows: Vec<Value> = db
        .query(
            "SELECT out.extracted_text AS text FROM has_attachment \
             WHERE in = type::record('document', $id) \
               AND out.extracted_text IS NOT NONE AND out.extracted_text != '' \
             LIMIT 8",
        )
        .bind(("id", id.to_string()))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    rows.iter()
        .filter_map(|r| r.get("text").and_then(|v| v.as_str()).map(str::to_string))
        .filter(|t| !t.trim().is_empty())
        .collect()
}

/// Place ONE office-pinned ticket from an address OCR'd out of its attachments.
///
/// Reads the ticket's stored attachment extracts, mines their "PLZ Ort" lines
/// (dropping our HQ header), picks a locality (customer-city hint, else frequency,
/// else give up on a tie), geocodes the chosen zip+city via the shared cached
/// resolver, and — on success — writes the result through the existing promote
/// helper (`save_geo_resolved` clears the HQ pin + bumps the clock), then stamps
/// `geo_source='attachment_ocr'` and mirrors the chosen zip/city into the same
/// structured `meta.zip`/`meta.city` fields the geocoder already maintains, and
/// re-advances the vector clock so the tag survives a mesh merge. Returns the
/// placed city on success. 100% local: only the resolved zip+city leaves the box.
pub async fn try_place_by_attachment_ocr(db: &SurrealDb, id: &str, meta: &Value) -> Option<String> {
    let texts = attachment_texts(db, id).await;
    if texts.is_empty() {
        return None;
    }
    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let cands = attachment_geo_candidates(&text_refs);

    // Disambiguation hint: a location we already hold for this ticket's customer.
    // The structured meta zip/city is the customer's own stated address; we only
    // reach this lever when it failed to geocode on its own (lever 2a), so using
    // it here as a *confirming* hint is safe — it merely tips a tie toward the
    // OCR'd address that matches what the customer told us.
    let known_zip = meta.get("zip").and_then(|v| v.as_str());
    let known_city = meta.get("city").and_then(|v| v.as_str());
    let chosen = choose_attachment_candidate(&cands, known_zip, known_city)?;

    // Geocode zip+city only (never the street), via the same cached resolver every
    // lever uses. Every candidate carries a real PLZ, so this is a precise (not
    // coarse) placement.
    let (lat, lng) = crate::services::geocoder::resolve_zip_city_cached(
        db,
        Some(chosen.zip.as_str()),
        Some(chosen.city.as_str()),
    )
    .await?;

    // Clear the HQ pin + set the resolved coords (bumps the clock inside).
    crate::services::geocoder::save_geo_resolved(db, "document", id, "meta", lat, lng, false).await;

    // Stamp the source + persist the structured zip/city, then re-bump the clock
    // (mirrors customer_geo's two-step write). Both are EXISTING meta fields, so
    // no new synced field is introduced — the geo block already mesh-syncs.
    let q = "UPDATE document SET \
             meta.geo_source = 'attachment_ocr', \
             meta.zip = $zip, meta.city = $city, \
             updated_at = time::now() \
         WHERE record::id(id) = $id RETURN NONE";
    if let Err(e) = db
        .query(q)
        .bind(("id", id.to_string()))
        .bind(("zip", chosen.zip.clone()))
        .bind(("city", chosen.city.clone()))
        .await
    {
        error!("[AttachmentGeo] {id}: source-tag write failed: {e}");
    }
    crate::services::geocoder::bump_geo_vclock(db, "document", id).await;

    info!(
        "[AttachmentGeo] {id}: matched {} {} from {} attachment(s)",
        chosen.zip,
        chosen.city,
        texts.len()
    );
    Some(chosen.city.clone())
}

/// On-demand batch: for residual office-pinned tickets, try to place each from an
/// address OCR'd out of its own attachments. Free + local — only the resolved
/// zip+city ever leaves the box. Also retries tickets parked as
/// `auto_nomatch` / `needs_grounding` / `derived_failed`, since this lever is free
/// and new (and the extractor re-read is still populating `extracted_text`).
/// Spawned; logs `[AttachmentGeo]`. `limit` caps the batch (0 = safety cap 1000).
/// Tickets the attachment lever found NO usable address for, with the time of
/// that verdict — consulted by `attachment_fill_batch` so the hourly window walks
/// forward instead of re-scanning the same front every run. Process-local by
/// design (see the comment in the batch).
fn recently_checked() -> &'static std::sync::Mutex<HashMap<String, std::time::Instant>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<HashMap<String, std::time::Instant>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Sweep prioritisation + budget accounting (DB-free, unit-tested).
// ---------------------------------------------------------------------------

/// Read a `meta.<field>` string out of a selected `{id, meta}` row, if present.
fn row_meta_str<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get("meta").and_then(|m| m.get(field)).and_then(|v| v.as_str())
}

/// Prioritise the over-fetched window so the sweep tries the likely hits first:
/// OPEN (non-closed) tickets before closed ones, and within each group the newest
/// `meta.created_time` first. Uses the SAME closed notion as the rest of the app
/// (`services::support::is_closed_ticket_status`). `created_time` is stored
/// ISO-8601, so a reverse lexical compare is chronological (mirrors `mcp::tools`).
/// Pure + stable → the hourly window order is deterministic and unit-testable.
fn prioritise_rows(rows: &mut [Value]) {
    rows.sort_by(|a, b| {
        let a_closed = row_meta_str(a, "status")
            .map_or(false, crate::services::support::is_closed_ticket_status);
        let b_closed = row_meta_str(b, "status")
            .map_or(false, crate::services::support::is_closed_ticket_status);
        // open (false) sorts before closed (true); then newest created_time first.
        a_closed.cmp(&b_closed).then_with(|| {
            row_meta_str(b, "created_time")
                .unwrap_or("")
                .cmp(row_meta_str(a, "created_time").unwrap_or(""))
        })
    });
}

/// Per-run accounting for the sweep, factored out so the "zero-candidate tickets
/// are free" budget rule is unit-testable without a DB. The `lim` budget counts
/// ONLY tickets whose local text scan produced ≥1 candidate — those cost a
/// networked geocode; a zero-candidate ticket is a free local no_match that must
/// not spend the budget, so the window keeps walking forward until `lim` tickets
/// WITH candidates have been tried (or the over-fetched list is exhausted).
#[derive(Debug, Default, PartialEq, Eq)]
struct SweepTally {
    scanned: usize,         // tickets whose attachment text we scanned
    with_candidates: usize, // tickets that had ≥1 candidate (consume budget)
    resolved: usize,        // geocoded + placed
    no_match: usize,        // no candidate, OR a candidate that failed to place
}

impl SweepTally {
    /// True while the per-run budget of `lim` with-candidate tickets is unspent.
    fn budget_left(&self, lim: usize) -> bool {
        self.with_candidates < lim
    }
    /// Record a ticket with NO candidate: a free local no_match, budget untouched.
    fn record_no_candidate(&mut self) {
        self.scanned += 1;
        self.no_match += 1;
    }
    /// Record a ticket that HAD candidates and was geocoded (a budget slot spent);
    /// `placed` says whether it resolved.
    fn record_geocoded(&mut self, placed: bool) {
        self.scanned += 1;
        self.with_candidates += 1;
        if placed {
            self.resolved += 1;
        } else {
            self.no_match += 1;
        }
    }
}

/// Pure driver mirroring the batch's budget walk (no DB), so the accounting can be
/// tested directly. Walks the priority list spending one budget slot only per
/// ticket WITH candidates, stopping once `lim` such tickets are handled;
/// `placed(i)` reports whether the i-th with-candidate ticket geocoded.
#[cfg(test)]
fn run_sweep_tally<F: Fn(usize) -> bool>(has_candidates: &[bool], lim: usize, placed: F) -> SweepTally {
    let mut tally = SweepTally::default();
    let mut geo_idx = 0usize;
    for &hc in has_candidates {
        if !tally.budget_left(lim) {
            break;
        }
        if hc {
            let ok = placed(geo_idx);
            geo_idx += 1;
            tally.record_geocoded(ok);
        } else {
            tally.record_no_candidate();
        }
    }
    tally
}

pub async fn attachment_fill_batch(db: SurrealDb, limit: usize) {
    let lim = if limit == 0 { 1000 } else { limit };
    // Over-fetch ×8 (was ×4): now that a zero-candidate ticket is a FREE local
    // no_match that does not spend the `lim` geocode budget, the window must be
    // able to walk past a long run of no-address tickets (the kiosk pile is ~500×
    // no_match at the front) and still reach `lim` tickets that DO carry an
    // address — so the fetched window has to cover far more than `lim` rows.
    let over = lim.saturating_mul(8);
    info!("[AttachmentGeo] start (limit={lim})");

    // Two eligible populations, both never touching an operator's `geo_override`:
    //   (1) HQ-pinned residual (`geo_fallback = true`), fresh or parked; and
    //   (2) coarsely area-code-placed rows (`geo_source = 'vorwahl'`, which CLEARS
    //       `geo_fallback`, so they are NOT in (1)) — an address OCR'd out of an
    //       attachment is exact and beats the vorwahl city-centre guess, so upgrade
    //       those too. `save_geo_resolved` overwrites the coarse pin with the
    //       precise coords and re-tags `geo_source='attachment_ocr'`.
    // Prioritise the window at the DB level too (so it survives the LIMIT when the
    // eligible pile ever exceeds `over`): OPEN tickets first, newest first. The
    // `is_open` predicate is the SAME lowercase-`closed` notion as
    // `handlers::support` / `services::support::is_closed_ticket_status`.
    // `prioritise_rows` re-applies the identical order in Rust as the authoritative
    // (and unit-tested) walk order.
    let sql = format!(
        "SELECT record::id(id) AS id, meta, \
                (meta.status IS NONE OR string::lowercase(meta.status) != 'closed') AS is_open \
         FROM document \
         WHERE type = 'support_ticket' \
           AND meta.geo_override IS NOT true \
           AND ( \
                (meta.geo_fallback = true \
                 AND (meta.geo_source IS NONE \
                      OR meta.geo_source IN ['auto_nomatch', 'needs_grounding', 'derived_failed'])) \
                OR (meta.geo_source = 'vorwahl' AND meta.geo_coarse = true) \
           ) \
         ORDER BY is_open DESC, meta.created_time DESC \
         LIMIT {over}"
    );
    // The window must ADVANCE: with a plain LIMIT the same first `lim` rows are
    // re-scanned every hour and everything behind them is never reached
    // (2026-09-10: #32030 sat outside the first 500 for two runs). A no_match is
    // remembered IN-PROCESS for a week (no DB write: `meta` is merkle-hashed
    // whole, a stamp there would churn the mesh every sweep) and the select
    // over-fetches ×8 so the skipped rows don't starve the window. A restart
    // forgets the set — the next sweep simply re-walks the front once.
    let mut rows: Vec<Value> = match db.query(&sql).await.and_then(|mut r| r.take(0)) {
        Ok(r) => r,
        Err(e) => {
            error!("[AttachmentGeo] select failed: {e}");
            return;
        }
    };
    if rows.is_empty() {
        info!("[AttachmentGeo] Done: scanned=0 with_candidates=0 resolved=0 no_match=0");
        return;
    }
    // Authoritative (and unit-tested) priority order: likely hits first.
    prioritise_rows(&mut rows);

    let mut tally = SweepTally::default();
    const RECHECK_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    for row in &rows {
        // Budget = `lim` tickets WITH candidates geocoded; free (zero-candidate)
        // no_matches below do NOT count, so the walk keeps advancing past them.
        if !tally.budget_left(lim) {
            break;
        }
        let id = row
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        {
            let mut seen = recently_checked().lock().unwrap();
            match seen.get(&id) {
                Some(t) if t.elapsed() < RECHECK_AFTER => continue,
                Some(_) => {
                    seen.remove(&id);
                }
                None => {}
            }
        }
        // Cheap LOCAL pre-check: does this ticket's attachment text even contain a
        // "PLZ Ort"-shaped line / PLZ:+Ort: field pair? The text scan is free; the
        // geocode is the networked/expensive part. Zero candidates ⇒ a remembered
        // no_match that does NOT spend the geocode budget.
        let texts = attachment_texts(&db, &id).await;
        let has_candidates = if texts.is_empty() {
            false
        } else {
            let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            !attachment_geo_candidates(&text_refs).is_empty()
        };
        if !has_candidates {
            tally.record_no_candidate();
            recently_checked().lock().unwrap().insert(id.clone(), std::time::Instant::now());
            continue;
        }

        // Has ≥1 candidate → spend a budget slot on the (networked) placement.
        let meta = row.get("meta").cloned().unwrap_or_else(|| json!({}));
        match try_place_by_attachment_ocr(&db, &id, &meta).await {
            Some(city) => {
                tally.record_geocoded(true);
                info!("[AttachmentGeo] {id}: {city}");
            }
            None => {
                tally.record_geocoded(false);
                recently_checked().lock().unwrap().insert(id.clone(), std::time::Instant::now());
            }
        }
    }
    info!(
        "[AttachmentGeo] Done: scanned={} with_candidates={} resolved={} no_match={}",
        tally.scanned, tally.with_candidates, tally.resolved, tally.no_match
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic service form header always carries our HQ address; the customer's
    // address (if any) sits further down the page.
    const HQ_HEADER: &str =
        "Example Med GmbH\nMusterallee 15-21\n12345 Beispielstadt\nDeutschland\n";

    // The own-HQ markers the fleet configures via `ECK_OWN_ADDRESS_MARKERS`,
    // injected here so the pure candidate extractor drops the HQ header.
    fn hq_markers() -> Vec<String> {
        vec![
            "65760".to_string(),
            "beispielstadt".to_string(),
            "musterallee".to_string(),
        ]
    }

    #[test]
    fn hq_header_is_blacklisted() {
        let cands = attachment_geo_candidates_with(&[HQ_HEADER], &hq_markers());
        assert!(cands.is_empty(), "HQ header must yield no candidate, got {cands:?}");
    }

    #[test]
    fn blank_template_yields_no_result() {
        // A shared blank form contains ONLY our HQ header → no candidate → none.
        let cands = attachment_geo_candidates_with(&[HQ_HEADER], &hq_markers());
        assert!(choose_attachment_candidate(&cands, None, None).is_none());
    }

    #[test]
    fn customer_address_survives_hq_header() {
        // HQ header + the customer's address below it → only the customer survives.
        let text = format!("{HQ_HEADER}\nKunde:\nProbststraße 12\n28217 Bremen\n");
        let cands = attachment_geo_candidates_with(&[text.as_str()], &hq_markers());
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].zip, "28217");
        assert_eq!(cands[0].city, "Bremen");
    }

    #[test]
    fn frequency_wins_without_a_hint() {
        // Bremen on two attachments, Köln on one → Bremen wins on frequency.
        let a = format!("{HQ_HEADER}\n28217 Bremen\n");
        let b = format!("{HQ_HEADER}\n28217 Bremen\n");
        let c = format!("{HQ_HEADER}\n50667 Köln\n");
        let cands = attachment_geo_candidates_with(&[a.as_str(), b.as_str(), c.as_str()], &hq_markers());
        let chosen = choose_attachment_candidate(&cands, None, None).expect("a winner");
        assert_eq!(chosen.city, "Bremen");
    }

    #[test]
    fn customer_city_hint_beats_frequency() {
        // Köln is more frequent, but the customer's known city is Bremen → Bremen.
        let a = format!("{HQ_HEADER}\n50667 Köln\n");
        let b = format!("{HQ_HEADER}\n50667 Köln\n");
        let c = format!("{HQ_HEADER}\n28217 Bremen\n");
        let cands = attachment_geo_candidates_with(&[a.as_str(), b.as_str(), c.as_str()], &hq_markers());
        let chosen =
            choose_attachment_candidate(&cands, None, Some("Bremen")).expect("hint winner");
        assert_eq!(chosen.city, "Bremen");
        assert_eq!(chosen.zip, "28217");
    }

    #[test]
    fn ambiguous_tie_yields_none() {
        // One Bremen, one Köln, no hint → equal frequency → ambiguous → none.
        let a = format!("{HQ_HEADER}\n28217 Bremen\n");
        let b = format!("{HQ_HEADER}\n50667 Köln\n");
        let cands = attachment_geo_candidates_with(&[a.as_str(), b.as_str()], &hq_markers());
        assert!(choose_attachment_candidate(&cands, None, None).is_none());
    }

    #[test]
    fn separate_plz_and_ort_fields_pair_into_a_candidate() {
        // A fillable form's appended `--- Formularfelder ---` block: PLZ and Ort
        // are SEPARATE lines the one-line "PLZ Ort" scanner never joins. The
        // pairing pass must still yield one candidate.
        let form = "--- Formularfelder ---\n\
                    Firma: Acme GmbH\n\
                    Strasse: Musterweg 3\n\
                    PLZ: 78467\n\
                    Ort: Konstanz\n\
                    Ansprechpartner: Max Mustermann\n";
        let cands = attachment_geo_candidates_with(&[form], &hq_markers());
        let chosen = choose_attachment_candidate(&cands, None, None).expect("a candidate");
        assert_eq!(chosen.zip, "78467");
        assert_eq!(chosen.city, "Konstanz");
    }

    #[test]
    fn field_key_lookalikes_do_not_match() {
        // "Antwort" / "Standort" contain the substring "ort" but are not Ort keys,
        // and there is no PLZ field, so nothing pairs.
        let form = "Antwort: 12345 something\nStandort intern: Halle 2\n";
        let cands = attachment_geo_candidates_with(&[form], &hq_markers());
        assert!(cands.is_empty(), "lookalike keys must not pair, got {cands:?}");
    }

    #[test]
    fn combined_plz_ort_field_is_not_double_counted() {
        // A combined "PLZ/Ort: 78467 Konstanz" line is caught by the one-line
        // scanner; the pairing pass must not add a second copy (count stays 1).
        let form = "PLZ/Ort: 78467 Konstanz\n";
        let cands = attachment_geo_candidates_with(&[form], &hq_markers());
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].zip, "78467");
        assert_eq!(cands[0].count, 1, "not double-counted from both miners");
    }

    #[test]
    fn hq_template_address_dropped_customer_survives() {
        // The 2026-09-10 kiosk regression: a form template carries the HQ address
        // (65760 Eschborn) in its own fields alongside the customer's. With NO
        // ECK_OWN_ADDRESS_MARKERS configured, the HQ zip/city rule alone must drop
        // Eschborn and keep only the customer.
        let form = "--- Formularfelder ---\n\
                    PLZ Ort: 65760 Eschborn\n\
                    PLZ Ort: 78467 Konstanz\n";
        let cands = attachment_geo_candidates_full(&[form], &[], "65760", "Eschborn");
        assert_eq!(cands.len(), 1, "only the customer survives, got {cands:?}");
        assert_eq!(cands[0].zip, "78467");
        assert_eq!(cands[0].city, "Konstanz");
        let chosen = choose_attachment_candidate(&cands, None, None).expect("Konstanz wins");
        assert_eq!(chosen.city, "Konstanz");
    }

    #[test]
    fn own_address_labeled_fields_are_dropped() {
        // Our return/sender block sits under own-address labels; the customer's is
        // plain PLZ/Ort fields. Even with the HQ zip/city env pointing ELSEWHERE
        // (99999/Nowhere), the field-name label filter drops the HQ block, so only
        // the customer's paired candidate survives.
        let form = "--- Formularfelder ---\n\
                    Absender Acme: Acme Deutschland GmbH\n\
                    Rücksendeadresse PLZ: 65760\n\
                    Rücksendeadresse Ort: Eschborn\n\
                    PLZ: 78467\n\
                    Ort: Konstanz\n";
        let cands = attachment_geo_candidates_full(&[form], &[], "99999", "Nowhere");
        assert_eq!(cands.len(), 1, "only the customer survives, got {cands:?}");
        assert_eq!(cands[0].zip, "78467");
        assert_eq!(cands[0].city, "Konstanz");
    }

    // --- sweep prioritisation --------------------------------------------

    fn row(id: &str, status: Option<&str>, created: &str) -> Value {
        let mut meta = json!({ "created_time": created });
        if let Some(s) = status {
            meta["status"] = json!(s);
        }
        json!({ "id": id, "meta": meta })
    }

    #[test]
    fn prioritise_open_before_closed_then_newest_first() {
        // Mixed open/closed, out of created order. Expected: open group first
        // (newest→oldest), then closed group (newest→oldest). "Auto closed" /
        // case-variant "closed" are NOT the canonical `Closed` — only an exact
        // (case-insensitive) "closed" counts (services::support notion).
        let mut rows = vec![
            row("old_open", Some("Open"), "2024-01-01T00:00:00Z"),
            row("new_closed", Some("Closed"), "2026-01-01T00:00:00Z"),
            row("new_open", Some("In Progress"), "2025-06-01T00:00:00Z"),
            row("old_closed", Some("closed"), "2023-01-01T00:00:00Z"),
            row("no_status", None, "2025-12-31T00:00:00Z"),
        ];
        prioritise_rows(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            order,
            vec!["no_status", "new_open", "old_open", "new_closed", "old_closed"],
            "open (incl. no-status) newest-first, then closed newest-first"
        );
    }

    #[test]
    fn prioritise_is_stable_and_handles_missing_created_time() {
        // Missing created_time sorts as empty string (oldest); no panic.
        let mut rows = vec![
            row("a", Some("Open"), "2025-01-01T00:00:00Z"),
            json!({ "id": "b", "meta": { "status": "Open" } }), // no created_time
            row("c", Some("Open"), "2025-06-01T00:00:00Z"),
        ];
        prioritise_rows(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(order, vec!["c", "a", "b"]);
    }

    // --- budget accounting: zero-candidate tickets are free ----------------

    #[test]
    fn zero_candidate_tickets_do_not_consume_budget() {
        // lim = 2 with-candidate placements. A long run of zero-candidate tickets
        // sits between the hits; they must all be scanned (as no_match) WITHOUT
        // spending the budget, and the walk must reach the 2nd hit.
        // pattern: F F T F F F T  → stop after the 2nd T.
        let has_candidates = [false, false, true, false, false, false, true, true];
        let tally = run_sweep_tally(&has_candidates, 2, |_| true);
        assert_eq!(tally.with_candidates, 2, "budget capped at lim with-candidate tickets");
        assert_eq!(tally.resolved, 2);
        assert_eq!(tally.scanned, 7, "5 free no_match + 2 placed, stops before the trailing T");
        assert_eq!(tally.no_match, 5, "the zero-candidate tickets");
    }

    #[test]
    fn budget_counts_failed_geocodes_but_not_missing_candidates() {
        // A with-candidate ticket that FAILS to place still spends a budget slot
        // (the geocode ran) and is a no_match; a zero-candidate one does not.
        let has_candidates = [true, false, true, false];
        // both with-candidate tickets fail to geocode
        let tally = run_sweep_tally(&has_candidates, 5, |_| false);
        assert_eq!(tally.with_candidates, 2);
        assert_eq!(tally.resolved, 0);
        assert_eq!(tally.scanned, 4, "budget not exhausted → whole list walked");
        assert_eq!(tally.no_match, 4, "2 failed geocodes + 2 zero-candidate");
    }

    #[test]
    fn all_zero_candidates_never_stops_early() {
        // No hits at all: every ticket is a free no_match, none consume budget,
        // and the whole window is walked (the point of the free-scan rule).
        let has_candidates = [false; 6];
        let tally = run_sweep_tally(&has_candidates, 2, |_| true);
        assert_eq!(tally.with_candidates, 0);
        assert_eq!(tally.scanned, 6);
        assert_eq!(tally.no_match, 6);
        assert_eq!(tally.resolved, 0);
    }

    #[test]
    fn zip_hint_also_confirms() {
        // Same two-locality tie, but the customer's known ZIP picks the winner.
        let a = format!("{HQ_HEADER}\n28217 Bremen\n");
        let b = format!("{HQ_HEADER}\n50667 Köln\n");
        let cands = attachment_geo_candidates_with(&[a.as_str(), b.as_str()], &hq_markers());
        let chosen =
            choose_attachment_candidate(&cands, Some("50667"), None).expect("zip-hint winner");
        assert_eq!(chosen.city, "Köln");
    }
}
