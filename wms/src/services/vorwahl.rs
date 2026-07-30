//! Lever 2B — place the residual office-pinned pile by German phone area code.
//!
//! After AI discovery (2A) the tickets still on HQ are mostly private individuals
//! (gmail/gmx/… — no public business address to look up). The only remaining
//! geographic signal is a **landline** phone: its Vorwahl (Ortsnetzkennzahl)
//! maps to a town. This resolves that **entirely locally** — the phone number
//! never leaves the box; we extract only the area code, map it to a town name via
//! the embedded Bundesnetzagentur ONB table, and geocode the *town* (a public
//! place, not PII). Placement is city-centre, so it's tagged `geo_coarse`.
//!
//! Mobile (015x/016x/017x) and service (0700/0800/…) numbers have no geography
//! and are skipped; foreign numbers (+41/+43/…) are skipped too (the table is DE).

use eck_core::db::SurrealDb;
use std::collections::HashMap;
use std::sync::OnceLock;
use tracing::{error, info};

/// Bundesnetzagentur ONB list (Ortsnetzkennzahl;Ortsnetzname;KennzeichenAktiv),
/// already UTF-8. Embedded so the lookup is offline and deterministic.
const ONB_CSV: &str = include_str!("../data/onb_vorwahl.csv");

/// Ortsnetzkennzahl (no leading 0) → Ortsname, built once from the embedded CSV.
/// The German numbering plan is prefix-free, so at most one code prefixes any
/// valid national number.
fn onb() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        for line in ONB_CSV.lines().skip(1) {
            let mut it = line.split(';');
            let code = it.next().unwrap_or("").trim();
            let name = it.next().unwrap_or("").trim();
            let active = it.next().unwrap_or("1").trim();
            if code.is_empty() || name.is_empty() || active == "0" {
                continue;
            }
            if code.chars().all(|c| c.is_ascii_digit()) {
                m.insert(code.to_string(), name.to_string());
            }
        }
        m
    })
}

/// Reduce a raw phone string to German national digits (no leading 0). Returns
/// None for mobile/service numbers, foreign numbers, or anything unrecognisable.
fn national_digits(phone: &str) -> Option<String> {
    let had_plus = phone.trim_start().starts_with('+');
    let raw: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if raw.len() < 4 {
        return None;
    }
    let nat = if let Some(r) = raw.strip_prefix("0049") {
        r.to_string()
    } else if had_plus && raw.starts_with("49") {
        raw[2..].to_string()
    } else if raw.starts_with("00") {
        return None; // foreign country code other than DE
    } else if had_plus {
        return None; // +<non-49>
    } else if let Some(r) = raw.strip_prefix('0') {
        r.to_string()
    } else {
        return None; // not in national format
    };
    // German geographic area codes start 2–9; a leading 1 is mobile/service.
    if nat.is_empty() || nat.starts_with('1') {
        return None;
    }
    Some(nat)
}

/// Map a phone to its Ortsname via the area code (longest-prefix, 2–5 digits).
pub fn vorwahl_city(phone: &str) -> Option<&'static str> {
    let nat = national_digits(phone)?;
    let table = onb();
    for len in (2..=5).rev() {
        if nat.len() >= len {
            if let Some(name) = table.get(&nat[..len]) {
                return Some(name.as_str());
            }
        }
    }
    None
}

/// Place one ticket by its landline area code: phone → town (local), geocode the
/// town (coarse), write geo + `geo_source="vorwahl"`. Returns the town on
/// success. Only the town name (public) ever leaves the box — never the phone.
/// Shared by the bulk batch and the per-ticket auto-fill worker.
pub async fn try_place_by_vorwahl(db: &SurrealDb, id: &str, phone: &str) -> Option<&'static str> {
    let city = vorwahl_city(phone)?;
    let (lat, lng) = crate::services::geocoder::resolve_zip_city_cached(db, None, Some(city)).await?;
    let q = "UPDATE document SET \
        meta.geo = { lat: $lat, lng: $lng }, \
        meta.geo_fallback = NONE, meta.geo_failed = NONE, \
        meta.geo_source = 'vorwahl', meta.geo_coarse = true, \
        updated_at = time::now() \
     WHERE record::id(id) = $id RETURN NONE";
    match db.query(q).bind(("id", id.to_string())).bind(("lat", lat)).bind(("lng", lng)).await {
        Ok(_) => {
            // Advance causality so the area-code placement survives mesh merge.
            crate::services::geocoder::bump_geo_vclock(db, "document", id).await;
            Some(city)
        }
        Err(e) => { error!("[Vorwahl] {id}: db update failed: {e}"); None }
    }
}

/// On-demand batch: for residual office-pinned tickets with a landline phone,
/// geocode the town behind the area code (coarse, city-centre). Free + local —
/// only the resulting town name (public) is sent to the geocoder, never the
/// phone. Spawned; logs `[Vorwahl]`. `limit` caps the batch (0 = safety cap).
pub async fn vorwahl_fill_batch(db: SurrealDb, limit: usize) {
    let lim = if limit == 0 { 1000 } else { limit };
    info!("[Vorwahl] start (limit={lim}, table={} codes)", onb().len());

    let sql = format!(
        "SELECT record::id(id) AS id, meta.phone AS phone FROM document \
         WHERE type = 'support_ticket' AND meta.geo_fallback = true \
           AND meta.geo_override IS NOT true AND meta.geo_source IS NONE \
           AND meta.phone != NONE AND meta.phone != '' LIMIT {lim}"
    );
    let rows: Vec<serde_json::Value> = match db.query(&sql).await.and_then(|mut r| r.take(0)) {
        Ok(r) => r,
        Err(e) => { error!("[Vorwahl] select failed: {e}"); return; }
    };

    let mut scanned = 0usize;
    let mut resolved = 0usize;
    let mut no_match = 0usize;

    for row in &rows {
        scanned += 1;
        let id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let phone = row.get("phone").and_then(|v| v.as_str()).unwrap_or("");
        match try_place_by_vorwahl(&db, &id, phone).await {
            Some(city) => { resolved += 1; info!("[Vorwahl] {id}: {city}"); }
            None => { no_match += 1; }
        }
    }

    info!("[Vorwahl] Done: scanned={scanned} resolved={resolved} no_match={no_match}");
}

#[cfg(test)]
mod tests {
    use super::{national_digits, vorwahl_city};

    #[test]
    fn parses_national_formats() {
        assert_eq!(national_digits("0201 12345").as_deref(), Some("20112345"));
        assert_eq!(national_digits("+49 201 12345").as_deref(), Some("20112345"));
        assert_eq!(national_digits("0049 201 12345").as_deref(), Some("20112345"));
    }

    #[test]
    fn rejects_mobile_service_and_foreign() {
        assert_eq!(national_digits("0176 1234567"), None); // mobile (leading 1)
        assert_eq!(national_digits("+41 76 802 29 99"), None); // Swiss via +
        assert_eq!(national_digits("0041768022999"), None); // Swiss via 00
        // Toll-free 0800 passes the format filter but has no geographic code,
        // so the table lookup yields nothing.
        assert_eq!(vorwahl_city("0800 1234567"), None);
    }

    #[test]
    fn maps_known_area_codes() {
        // From the embedded ONB table.
        assert_eq!(vorwahl_city("030 123456"), Some("Berlin"));
        assert_eq!(vorwahl_city("089 7654321"), Some("München"));
        assert_eq!(vorwahl_city("0221 4711"), Some("Köln"));
        assert_eq!(vorwahl_city("08841 6280333"), Some("Murnau a Staffelsee"));
        assert_eq!(vorwahl_city("0176 5551234"), None);
    }
}
