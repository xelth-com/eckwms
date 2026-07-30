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
/// drop nothing.
pub(crate) fn attachment_geo_candidates_with(
    texts: &[&str],
    own_markers: &[String],
) -> Vec<GeoCandidate> {
    let mut agg: HashMap<(String, String), GeoCandidate> = HashMap::new();
    for text in texts {
        // Reuse the geocoder's privacy-safe "PLZ Ort" scanner (the street always
        // precedes the postal code, so only zip+city are ever captured).
        for (zip, city) in crate::services::geocoder::find_all_plz_ort(text) {
            let city_norm = city.trim().to_lowercase();
            // HARD own-HQ blacklist — the operating company's own header address
            // must never be geocoded. Markers from env `ECK_OWN_ADDRESS_MARKERS`.
            if own_markers
                .iter()
                .any(|m| zip.contains(m.as_str()) || city_norm.contains(m.as_str()))
            {
                continue;
            }
            agg.entry((zip.clone(), city_norm))
                .and_modify(|c| c.count += 1)
                .or_insert(GeoCandidate { zip, city, count: 1 });
        }
    }
    let mut out: Vec<GeoCandidate> = agg.into_values().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.zip.cmp(&b.zip)));
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
pub async fn attachment_fill_batch(db: SurrealDb, limit: usize) {
    let lim = if limit == 0 { 1000 } else { limit };
    info!("[AttachmentGeo] start (limit={lim})");

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
            error!("[AttachmentGeo] select failed: {e}");
            return;
        }
    };
    if rows.is_empty() {
        info!("[AttachmentGeo] Done: scanned=0 resolved=0 no_match=0");
        return;
    }

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
        let meta = row.get("meta").cloned().unwrap_or_else(|| json!({}));
        match try_place_by_attachment_ocr(&db, &id, &meta).await {
            Some(city) => {
                resolved += 1;
                info!("[AttachmentGeo] {id}: {city}");
            }
            None => no_match += 1,
        }
    }
    info!("[AttachmentGeo] Done: scanned={scanned} resolved={resolved} no_match={no_match}");
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
