use eck_core::db::SurrealDb;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{debug, error, info};

/// Shared HTTP client for on-demand geocoding (the background worker builds its
/// own). Single UA-identified client, reused across requests.
static GEO_CLIENT: OnceLock<Client> = OnceLock::new();
fn geo_client() -> &'static Client {
    GEO_CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("eckWMS-Server/1.0 (internal-geocoder)")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    })
}

/// Local node instance id, set once at startup (`set_instance_id`). The geo
/// writers are free functions that only get a `&db`, so we stash the id here so
/// they can stamp the local node's vector clock after a write — see
/// [`bump_geo_vclock`].
static LOCAL_INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// Register the local node's instance id so geo writes can advance causality.
/// Call once from `main` before spawning the geocoder worker.
pub fn set_instance_id(id: String) {
    let _ = LOCAL_INSTANCE_ID.set(id);
}

/// Advance the local vector clock for a record we just wrote `meta.geo` onto.
///
/// `save_geo*` issue a direct `UPDATE … SET <synced field>` that bypasses
/// `resolve_and_upsert`, so without this the resolved coordinates inherit the
/// record's old clock and tie with a stale HQ-fallback version — letting an
/// offline node that rejoins silently revert the geo (the office-pile
/// regression). Bumping `_vclock` makes the resolved write causally win, exactly
/// like the device handlers do after their direct writes. `_vclock` is in
/// `merkle::IGNORED_FIELDS`, so this does not perturb the content hash. No-op if
/// the instance id was never set (tests / standalone tooling).
pub(crate) async fn bump_geo_vclock(db: &SurrealDb, table: &str, id: &str) {
    if let Some(iid) = LOCAL_INSTANCE_ID.get() {
        let rid = format!("{table}:`{id}`");
        if let Err(e) = eck_core::sync::conflict::bump_local_vclock(db, &rid, iid).await {
            debug!("[Geocoder] vclock bump failed for {table}:{id}: {e}");
        }
    }
}

/// On-demand zip+city → (lat,lng), cached in `geo_cache` (resolve-once). Backs
/// `GET /api/geo/resolve` so the **browser never calls Nominatim itself** — it
/// asks us, we resolve server-side (zip+city only, never the street) and cache.
/// Swap `nominatim_lookup` for a self-hosted geocoder later in ONE place.
pub async fn resolve_zip_city_cached(
    db: &SurrealDb,
    zip: Option<&str>,
    city: Option<&str>,
) -> Option<(f64, f64)> {
    resolve_zip_city_cc_cached(db, zip, city, DEFAULT_COUNTRYCODES).await
}

/// Map an AI-reported country to a Nominatim `countrycodes` filter. DACH names
/// and anything unrecognised fall back to the broad DE/AT/CH default; a known
/// non-DACH country returns its ISO code so e.g. a Swedish "211 20 Malmö"
/// geocodes in Sweden instead of being forced into a wrong German match.
pub(crate) fn cc_for_country(country: Option<&str>) -> &'static str {
    let c = match country {
        Some(s) => s.trim().to_lowercase(),
        None => return DEFAULT_COUNTRYCODES,
    };
    match c.as_str() {
        "schweden" | "sweden" | "sverige" | "se" => "se",
        "luxemburg" | "luxembourg" | "lu" => "lu",
        "niederlande" | "netherlands" | "holland" | "nl" => "nl",
        "belgien" | "belgium" | "belgique" | "be" => "be",
        "frankreich" | "france" | "fr" => "fr",
        "dänemark" | "daenemark" | "denmark" | "dk" => "dk",
        "polen" | "poland" | "pl" => "pl",
        "italien" | "italy" | "italia" | "it" => "it",
        "tschechien" | "czechia" | "czech republic" | "cz" => "cz",
        "spanien" | "spain" | "es" => "es",
        "norwegen" | "norway" | "no" => "no",
        "finnland" | "finland" | "fi" => "fi",
        "vereinigtes königreich" | "united kingdom" | "great britain" | "uk" | "gb" => "gb",
        _ => DEFAULT_COUNTRYCODES,
    }
}

/// As `resolve_zip_city_cached` but with an explicit Nominatim `countrycodes`
/// filter. The country is folded into the cache key (except for the default
/// DE/AT/CH, which keeps the legacy key so existing cache entries still hit).
pub async fn resolve_zip_city_cc_cached(
    db: &SurrealDb,
    zip: Option<&str>,
    city: Option<&str>,
    cc: &str,
) -> Option<(f64, f64)> {
    let zip_n = zip.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let city_n = city.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if zip_n.is_none() && city_n.is_none() {
        return None;
    }
    let mut key = format!(
        "{}|{}",
        zip_n.clone().unwrap_or_default().to_lowercase(),
        city_n.clone().unwrap_or_default().to_lowercase()
    );
    if cc != DEFAULT_COUNTRYCODES {
        key.push('|');
        key.push_str(cc);
    }
    // Sanitize to a safe record id (umlauts/spaces/pipe → '-'); collisions are
    // harmless (same city → same coords) and zip disambiguates.
    let slug: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let rid = format!("geo_cache:{}", slug);

    // 1. cache hit (positive or negative)
    if let Ok(mut res) = db
        .query("SELECT lat, lng, not_found FROM type::record($rid)")
        .bind(("rid", rid.clone()))
        .await
    {
        let rows: Vec<Value> = res.take(0).unwrap_or_default();
        if let Some(row) = rows.first() {
            if row.get("not_found").and_then(|v| v.as_bool()).unwrap_or(false) {
                return None;
            }
            if let (Some(la), Some(ln)) = (
                row.get("lat").and_then(|v| v.as_f64()),
                row.get("lng").and_then(|v| v.as_f64()),
            ) {
                return Some((la, ln));
            }
        }
    }

    // 2. miss → resolve server-side (zip+city only, in the given country) and cache
    let result = geolookup_zip_city_cc(geo_client(), zip_n.as_deref(), city_n.as_deref(), cc).await;
    let now = chrono::Utc::now().to_rfc3339();
    match result {
        Some((lat, lng)) => {
            let _ = db
                .query("UPSERT type::record($rid) SET key=$key, lat=$lat, lng=$lng, not_found=false, resolved_at=$now")
                .bind(("rid", rid))
                .bind(("key", key))
                .bind(("lat", lat))
                .bind(("lng", lng))
                .bind(("now", now))
                .await;
            Some((lat, lng))
        }
        None => {
            let _ = db
                .query("UPSERT type::record($rid) SET key=$key, not_found=true, resolved_at=$now")
                .bind(("rid", rid))
                .bind(("key", key))
                .bind(("now", now))
                .await;
            None
        }
    }
}

pub async fn start_geocoder_worker(db: SurrealDb) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    info!("[Geocoder] Background worker started");

    let client = Client::builder()
        .user_agent("eckWMS-Server/1.0 (internal-geocoder)")
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let mut interval = tokio::time::interval(Duration::from_secs(15));

    loop {
        interval.tick().await;

        // 1. Process Orders (Repairs)
        // `geo_override` is set by `POST /api/geo/fix` — the operator has
        // manually pinned this record and we must not overwrite it.
        let query_orders = "SELECT record::id(id) AS id, metadata FROM order \
                            WHERE metadata.geo IS NONE \
                            AND metadata.geo_failed IS NONE \
                            AND metadata.geo_override IS NOT true \
                            AND (metadata.city IS NOT NONE OR metadata.address IS NOT NONE OR metadata.zip IS NOT NONE) \
                            LIMIT 3";
        if let Ok(mut res) = db.query(query_orders).await {
            let orders: Vec<Value> = res.take(0).unwrap_or_default();
            for order in orders {
                process_record(&db, &client, "order", order, "metadata").await;
            }
        }

        // 2. Process Documents (Tickets) - includes ai_summary fallback
        let query_docs = "SELECT record::id(id) AS id, meta, ai_summary FROM document \
                          WHERE type = 'support_ticket' \
                          AND meta.geo IS NONE \
                          AND meta.geo_failed IS NONE \
                          AND meta.geo_override IS NOT true \
                          AND (meta.city IS NOT NONE OR meta.address IS NOT NONE OR meta.country IS NOT NONE OR ai_summary IS NOT NONE) \
                          LIMIT 3";
        if let Ok(mut res) = db.query(query_docs).await {
            let docs: Vec<Value> = res.take(0).unwrap_or_default();
            for doc in docs {
                process_record(&db, &client, "document", doc, "meta").await;
            }
        }
    }
}

const HOME_OFFICE_LAT: f64 = 50.1407;
const HOME_OFFICE_LNG: f64 = 8.5721;

async fn process_record(db: &SurrealDb, client: &Client, table: &str, record: Value, meta_field: &str) {
    let id = record.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    let meta = record.get(meta_field).cloned().unwrap_or(json!({}));

    let zip = meta.get("zip").and_then(|v| v.as_str()).map(String::from);
    let city = meta.get("city").and_then(|v| v.as_str()).map(String::from);

    // Geocoding only needs coarse location (zip + city). The street address
    // is PII — we intentionally do NOT ship it to OpenStreetMap's public
    // Nominatim endpoint even when we have it. Zip+city resolves to the
    // same 1-2km neighborhood on the map, which is all the UI uses.
    if let Some((lat, lng)) = geolookup_zip_city(client, zip.as_deref(), city.as_deref()).await {
        save_geo(db, table, id, meta_field, lat, lng, false).await;
        return;
    }

    // Fallback: AI summary may contain a Google-Search-resolved address.
    // Extract just its zip+city (never the street) via the shared parser —
    // same logic the bulk re-geocode pass uses, so new tickets get the same
    // multi-country / city-only rescue quality.
    if let Some(summary) = record.get("ai_summary").and_then(|v| v.as_str()) {
        if let Some((ai_zip, ai_city)) = parse_summary_address(summary) {
            info!(
                "[Geocoder] Meta lookup empty; trying AI-summary zip+city for {}: {} / {}",
                id, ai_zip, ai_city
            );
            if let Some((lat, lng)) = geolookup_zip_city(client, Some(&ai_zip), Some(&ai_city)).await {
                save_geo_resolved(db, table, id, meta_field, lat, lng, false).await;
                return;
            }
        }
    }

    info!(
        "[Geocoder] All lookups failed for {}, falling back to home office ({}, {})",
        id, HOME_OFFICE_LAT, HOME_OFFICE_LNG
    );
    save_geo(db, table, id, meta_field, HOME_OFFICE_LAT, HOME_OFFICE_LNG, true).await;
}

/// Default country scope for the DACH support market. We used to hard-pin
/// `countrycodes=de`, which silently dropped every Austrian/Swiss ticket (e.g.
/// "6330 Kufstein") onto the HQ fallback pile. DE+AT+CH covers the actual
/// customer base; a per-record country (parsed from the summary) narrows it
/// further when known.
const DEFAULT_COUNTRYCODES: &str = "de,at,ch";

/// Build a zip+city query (in that order — Nominatim weights leading tokens
/// more heavily, and "12345 München" is more specific than "München 12345")
/// and run the Nominatim lookup. Returns None when both inputs are empty.
async fn geolookup_zip_city(client: &Client, zip: Option<&str>, city: Option<&str>) -> Option<(f64, f64)> {
    geolookup_zip_city_cc(client, zip, city, DEFAULT_COUNTRYCODES).await
}

/// Same as `geolookup_zip_city` but with an explicit Nominatim `countrycodes`
/// filter (comma-separated ISO codes), for callers that have parsed a specific
/// country out of the address and want to narrow the search.
async fn geolookup_zip_city_cc(
    client: &Client,
    zip: Option<&str>,
    city: Option<&str>,
    countrycodes: &str,
) -> Option<(f64, f64)> {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(z) = zip.map(str::trim).filter(|s| !s.is_empty()) { parts.push(z); }
    if let Some(c) = city.map(str::trim).filter(|s| !s.is_empty()) { parts.push(c); }
    if parts.is_empty() { return None; }
    let query = parts.join(", ");
    nominatim_lookup(client, &query, countrycodes).await
}

async fn nominatim_lookup(client: &Client, search_str: &str, countrycodes: &str) -> Option<(f64, f64)> {
    debug!("[Geocoder] Looking up: {} (cc={})", search_str, countrycodes);

    tokio::time::sleep(Duration::from_millis(1100)).await;

    let cc = if countrycodes.trim().is_empty() { DEFAULT_COUNTRYCODES } else { countrycodes };
    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=json&countrycodes={}&q={}",
        urlencoding::encode(cc),
        urlencoding::encode(search_str)
    );
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Vec<Value> = resp.json().await.ok()?;
    let first = data.first()?;
    let lat: f64 = first.get("lat").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())?;
    let lng: f64 = first.get("lon").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())?;
    info!("[Geocoder] Resolved '{}' -> {}, {}", search_str, lat, lng);
    Some((lat, lng))
}

/// Strip PPRL `Address_<hex>` tokens out of a masked summary line. The stored
/// AI summary masks the *street* (PII) as `Address_7544865BC65416D4` but keeps
/// zip/city/country in the clear — we only ever geocode the latter.
fn strip_address_tokens(s: &str) -> String {
    let mut result = s.to_string();
    while let Some(pos) = result.find("Address_") {
        let tail = &result[pos + "Address_".len()..];
        let hex_len = tail.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        let end = pos + "Address_".len() + hex_len;
        result.replace_range(pos..end, "");
    }
    result
}

/// Find a postal code of exactly `width` consecutive digits with non-digit
/// Clean a city fragment: drop leading punctuation, strip a trailing country
/// word, collapse whitespace. Returns None when nothing alphabetic survives.
fn clean_city(s: &str) -> Option<String> {
    let mut c = s.trim().trim_start_matches([',', '-', ' ']).trim().to_string();
    for suffix in [", deutschland", ", germany", ", österreich", ", osterreich",
                   ", austria", ", schweiz", ", switzerland", " deutschland",
                   " germany", " österreich", " austria", " schweiz", " switzerland"] {
        let lower = c.to_lowercase();
        if lower.ends_with(suffix) {
            c.truncate(c.len() - suffix.len());
            c = c.trim().trim_end_matches(',').trim().to_string();
        }
    }
    let c = c.split_whitespace().collect::<Vec<_>>().join(" ");
    if c.chars().any(|ch| ch.is_alphabetic()) { Some(c) } else { None }
}

/// Find a German/Austrian/Swiss "PLZ Ort" pattern: a 4–5 digit postal code
/// followed by whitespace and a capitalised city name. Returns (zip, city).
///
/// This is the privacy-safe signal: the street always sits *before* the postal
/// code ("Prechtlerstrasse 18, 4710 Grieskirchen"), so anchoring on the PLZ
/// lets us forward only zip+city and never the street. Lines that are a bare
/// street, a company name, or just a country ("Philippstraße 1-5", "Deutschland")
/// have no "digits + Capital city" and simply don't match — exactly the cases
/// we must NOT geocode.
fn find_plz_ort(s: &str) -> Option<(String, String)> {
    find_all_plz_ort(s).into_iter().next()
}

/// Every "PLZ Ort" occurrence in `s`, in document order. Shares the exact same
/// scanner as [`find_plz_ort`] — it just doesn't stop at the first hit. The
/// summary path takes `.next()`; the attachment-OCR lever needs them all, because
/// a scanned form carries SEVERAL addresses (our HQ header, the customer's,
/// sometimes a vendor/bank line) and must weigh them against each other.
pub(crate) fn find_all_plz_ort(s: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < n {
        if !chars[i].is_ascii_digit() { i += 1; continue; }
        let start = i;
        while i < n && chars[i].is_ascii_digit() { i += 1; }
        let len = i - start;
        let before_ok = start == 0 || !chars[start - 1].is_ascii_digit();
        if !(before_ok && (len == 4 || len == 5)) { continue; }

        // Require whitespace then a capitalised letter (start of the city).
        let mut j = i;
        let mut saw_space = false;
        while j < n && chars[j].is_whitespace() { j += 1; saw_space = true; }
        if !saw_space || j >= n || !(chars[j].is_alphabetic() && chars[j].is_uppercase()) {
            continue;
        }

        let zip: String = chars[start..i].iter().collect();
        // City runs until a comma / paren / newline / end — country & extra notes
        // follow those. Newlines matter for multi-line OCR text; the summary path
        // only ever passes a single line, so terminating on them changes nothing
        // there but keeps a scanned "12345 Bremen\nTel: …" from swallowing the
        // next line into the city.
        let mut k = j;
        while k < n && !matches!(chars[k], ',' | '(' | '\n' | '\r') { k += 1; }
        let raw_city: String = chars[j..k].iter().collect();
        if let Some(city) = clean_city(&raw_city) {
            out.push((zip, city));
            i = k; // resume past the city we just consumed
        }
    }
    out
}

/// Pull a (zip, city) out of the AI summary's `**Adressen:**` block. The stored
/// summary is PII-masked (street → `Address_…` token, then stripped here); zip
/// and city survive in clear text and are the only parts we geocode. Returns
/// None when the block is missing, says no address is known ("keine Adresse",
/// "nicht angegeben", …), or carries no recognisable "PLZ Ort" — we deliberately
/// do NOT fall back to a bare city/street/company name (that leaked streets to
/// Nominatim and scattered tickets onto country centroids).
pub fn parse_summary_address(summary: &str) -> Option<(String, String)> {
    parse_summary_address_with(summary, eck_core::utils::anonymizer::own_address_markers())
}

/// Pure inner variant with the own-HQ marker list injected — unit-testable
/// without touching process env (same pattern as the anonymizer stoplist).
fn parse_summary_address_with(summary: &str, own_markers: &[String]) -> Option<(String, String)> {
    let marker = "**Adressen:**";
    let start = summary.find(marker)?;
    let after = &summary[start + marker.len()..];

    let mut line = String::new();
    for raw in after.lines() {
        let clean = raw.trim().trim_start_matches('-').trim_start_matches('*').trim();
        if clean.is_empty() { continue; }
        if clean.starts_with("===") || clean.starts_with("**") { break; }
        line = clean.to_string();
        break;
    }
    if line.is_empty() { return None; }

    let lower = line.to_lowercase();
    for sentinel in ["keine adresse", "keine adressdaten", "nicht angegeben",
                     "not specified", "not provided", "no address", "unbekannt", "unknown"] {
        if lower.contains(sentinel) { return None; }
    }
    // Don't re-pin the operating company's own HQ if the AI echoed it back —
    // markers come from ECK_OWN_ADDRESS_MARKERS (deployment config, empty by
    // default; the fleet sets its own street/zip/town tokens).
    if own_markers.iter().any(|m| lower.contains(m.as_str())) {
        return None;
    }

    let stripped = strip_address_tokens(&line);
    find_plz_ort(&stripped)
}

/// Write resolved coordinates onto a previously HQ-pinned record: set geo,
/// clear the fallback flag, and tag `geo_coarse` so the UI / a later precise
/// pass can tell city-centre placements from exact ones. `geo_override` is
/// never touched (operator pins stay sacred — the caller filters them out).
pub(crate) async fn save_geo_resolved(db: &SurrealDb, table: &str, id: &str, meta_field: &str, lat: f64, lng: f64, coarse: bool) {
    let coarse_set = if coarse { "true" } else { "NONE" };
    let q = format!(
        "UPDATE {table} SET \
         {meta_field}.geo = {{ lat: $lat, lng: $lng }}, \
         {meta_field}.geo_fallback = NONE, \
         {meta_field}.geo_failed = NONE, \
         {meta_field}.geo_coarse = {coarse_set}, \
         updated_at = time::now() \
         WHERE record::id(id) = $id \
         RETURN NONE"
    );
    match db.query(&q)
        .bind(("id", id.to_string()))
        .bind(("lat", lat))
        .bind(("lng", lng))
        .await
    {
        Ok(_) => bump_geo_vclock(db, table, id).await,
        Err(e) => error!("[Regeocode] DB update failed for {}:{}: {}", table, id, e),
    }
}

/// Pin a document to HQ with `geo_fallback=true` and a terminal `geo_source`
/// marker (e.g. "auto_nomatch" / "needs_grounding"), so it is counted as
/// office-pinned but never re-attempted. Used when address discovery has
/// exhausted every option for a ticket.
pub(crate) async fn pin_home_with_source(db: &SurrealDb, id: &str, source: &str) {
    let q = "UPDATE document SET \
        meta.geo = { lat: $lat, lng: $lng }, \
        meta.geo_fallback = true, meta.geo_failed = NONE, \
        meta.geo_source = $source, \
        updated_at = time::now() \
     WHERE record::id(id) = $id RETURN NONE";
    match db.query(q)
        .bind(("id", id.to_string()))
        .bind(("lat", HOME_OFFICE_LAT))
        .bind(("lng", HOME_OFFICE_LNG))
        .bind(("source", source.to_string()))
        .await
    {
        Ok(_) => bump_geo_vclock(db, "document", id).await,
        Err(e) => error!("[Geo] pin_home_with_source failed for {id}: {e}"),
    }
}

/// One-shot re-geocode of the HQ-fallback pile. Many tickets were pinned to HQ
/// by a startup race: the geocoder runs (15s) before the summarizer (20s+) has
/// written the Google-Search-resolved address into `ai_summary`. Once pinned,
/// `meta.geo` is set, so the worker's `geo IS NONE` filter never revisits them —
/// even after the summary lands. This pass re-reads those records and resolves
/// them from (a) `meta.zip`/`meta.city`, then (b) the summary's `**Adressen:**`
/// block, using the cached multi-country (DE/AT/CH) resolver so shared cities
/// are looked up once. Only writes on success; unresolved records stay pinned.
///
/// Runs detached (Nominatim is rate-limited to ~1 req/s) and logs progress.
pub async fn regeocode_fallback_batch(db: SurrealDb, limit: usize) {
    let lim = if limit == 0 { 5000 } else { limit };
    info!("[Regeocode] Fallback re-geocode pass started (limit={lim})");

    // Drop negative cache entries so any combos that failed under a previous
    // (buggy / rate-limited) attempt get a fresh lookup. Positive cache entries
    // are kept — they save real Nominatim calls for shared cities.
    if let Err(e) = db.query("DELETE geo_cache WHERE not_found = true").await {
        error!("[Regeocode] failed to clear negative geo_cache: {e}");
    }

    // Undo coarse/city-only placements (incl. any garbage from an earlier buggy
    // run that scattered tickets onto streets or country centroids): send them
    // back to the HQ-fallback pool so this pass re-evaluates them with the
    // current precise (zip+city only) parser. Operator pins are left alone.
    let revert = format!(
        "UPDATE document SET meta.geo = {{ lat: {lat}, lng: {lng} }}, \
             meta.geo_fallback = true, meta.geo_coarse = NONE \
         WHERE type = 'support_ticket' AND meta.geo_coarse = true \
             AND meta.geo_override IS NOT true RETURN NONE",
        lat = HOME_OFFICE_LAT, lng = HOME_OFFICE_LNG
    );
    if let Err(e) = db.query(&revert).await {
        error!("[Regeocode] failed to revert coarse placements: {e}");
    }

    // (table, meta_field) pairs — tickets are the bulk; repairs reuse the logic.
    let targets: [(&str, &str, &str); 2] = [
        ("document", "meta", "type = 'support_ticket' AND "),
        ("order", "metadata", ""),
    ];

    let mut total = 0usize;
    let mut resolved = 0usize;
    let mut coarse_n = 0usize;
    let mut still_pinned = 0usize;

    for (table, meta_field, type_filter) in targets {
        let sql = format!(
            "SELECT record::id(id) AS id, {mf} AS m, ai_summary FROM {tbl} \
             WHERE {tf}{mf}.geo_fallback = true AND {mf}.geo_override IS NOT true \
             LIMIT {lim}",
            mf = meta_field, tbl = table, tf = type_filter
        );
        let rows: Vec<Value> = match db.query(&sql).await.and_then(|mut r| r.take(0)) {
            Ok(r) => r,
            Err(e) => { error!("[Regeocode] select {table} failed: {e}"); continue; }
        };

        for row in &rows {
            total += 1;
            let id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let m = row.get("m").cloned().unwrap_or(json!({}));
            let meta_zip = m.get("zip").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
            let meta_city = m.get("city").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());

            // 1. Retry the structured meta (now multi-country).
            if meta_zip.is_some() || meta_city.is_some() {
                if let Some((lat, lng)) = resolve_zip_city_cached(&db, meta_zip, meta_city).await {
                    save_geo_resolved(&db, table, &id, meta_field, lat, lng, meta_zip.is_none()).await;
                    resolved += 1;
                    if meta_zip.is_none() { coarse_n += 1; }
                    continue;
                }
            }

            // 2. Recover from the AI summary's address block (zip+city only).
            if let Some(summary) = row.get("ai_summary").and_then(|v| v.as_str()) {
                if let Some((zip, city)) = parse_summary_address(summary) {
                    if let Some((lat, lng)) =
                        resolve_zip_city_cached(&db, Some(&zip), Some(&city)).await
                    {
                        save_geo_resolved(&db, table, &id, meta_field, lat, lng, false).await;
                        resolved += 1;
                        continue;
                    }
                }
            }

            // 3. Recover from an address OCR'd out of the ticket's own attachments
            //    (documents only — the `has_attachment` edge is a document→file
            //    link; orders carry no ticket forms). Our own HQ header address on
            //    every own-company form is hard-blacklisted inside the lever.
            if table == "document"
                && crate::services::attachment_geo::try_place_by_attachment_ocr(&db, &id, &m)
                    .await
                    .is_some()
            {
                resolved += 1;
                continue;
            }

            still_pinned += 1;
        }
    }

    info!(
        "[Regeocode] Done: scanned={total} resolved={resolved} (coarse/city-only={coarse_n}) still_pinned={still_pinned}"
    );
}

async fn save_geo(db: &SurrealDb, table: &str, id: &str, meta_field: &str, lat: f64, lng: f64, fallback: bool) {
    let q = if fallback {
        format!(
            "UPDATE {table} SET \
             {meta_field}.geo = {{ lat: $lat, lng: $lng }}, \
             {meta_field}.geo_fallback = true, \
             {meta_field}.geo_failed = NONE, \
             updated_at = time::now() \
             WHERE record::id(id) = $id \
             RETURN NONE"
        )
    } else {
        format!(
            "UPDATE {table} SET \
             {meta_field}.geo = {{ lat: $lat, lng: $lng }}, \
             {meta_field}.geo_fallback = NONE, \
             {meta_field}.geo_failed = NONE, \
             updated_at = time::now() \
             WHERE record::id(id) = $id \
             RETURN NONE"
        )
    };

    match db.query(&q)
        .bind(("id", id.to_string()))
        .bind(("lat", lat))
        .bind(("lng", lng))
        .await
    {
        Ok(res) => {
            if let Err(e) = res.check() {
                error!("[Geocoder] DB update failed for {}:{}: {}", table, id, e);
            } else {
                // Advance causality so this write isn't reverted on mesh merge.
                bump_geo_vclock(db, table, id).await;
                debug!("[Geocoder] Saved coordinates for {}:{}", table, id);
            }
        }
        Err(e) => {
            error!("[Geocoder] Query execution failed for {}:{}: {}", table, id, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{find_plz_ort, parse_summary_address, parse_summary_address_with, strip_address_tokens};

    fn adr(body: &str) -> String {
        format!("=== LOGISTIK & KONTAKTE ===\n**Adressen:** {body}\n=== TECHNISCHE DETAILS ===\n")
    }

    #[test]
    fn extracts_de_zip_city_dropping_street_token() {
        // The street is a masked PPRL token; only zip+city must survive.
        let got = parse_summary_address(&adr("Address_7544865BC65416D4, 28217 Bremen, Deutschland"));
        assert_eq!(got, Some(("28217".into(), "Bremen".into())));
    }

    #[test]
    fn extracts_at_4digit_zip() {
        let got = parse_summary_address(&adr("Address_DEADBEEF00000000, 4710 Grieskirchen, Österreich"));
        assert_eq!(got, Some(("4710".into(), "Grieskirchen".into())));
    }

    #[test]
    fn keeps_multiword_city() {
        let got = parse_summary_address(&adr("60311 Frankfurt am Main"));
        assert_eq!(got, Some(("60311".into(), "Frankfurt am Main".into())));
    }

    #[test]
    fn unmasked_street_never_leaks_as_city() {
        // A bare street line (the AI sometimes outputs one) must NOT geocode —
        // the street is PII and must never reach Nominatim.
        assert_eq!(parse_summary_address(&adr("Philippstraße 1-5")), None);
        assert_eq!(parse_summary_address(&adr("Schwarze-Bären-Straße 10")), None);
    }

    #[test]
    fn street_before_plz_is_not_forwarded() {
        // "<street>, 4710 Grieskirchen" → only ("4710","Grieskirchen") comes back,
        // the house number 18 is not mistaken for a postal code.
        let got = parse_summary_address(&adr("Prechtlerstrasse 18, 4710 Grieskirchen"));
        assert_eq!(got, Some(("4710".into(), "Grieskirchen".into())));
    }

    #[test]
    fn company_and_country_only_lines_are_skipped() {
        assert_eq!(parse_summary_address(&adr("Hector Sport-Centrum")), None);
        assert_eq!(parse_summary_address(&adr("Deutschland")), None);
        assert_eq!(parse_summary_address(&adr("bo's sport")), None);
    }

    #[test]
    fn no_address_sentinels_are_skipped() {
        assert_eq!(parse_summary_address(&adr("Keine Adresse im Ticket angegeben.")), None);
        assert_eq!(parse_summary_address(&adr("Nicht angegeben.")), None);
    }

    #[test]
    fn own_hq_is_not_repinned() {
        // Injected marker list (the pure equivalent of ECK_OWN_ADDRESS_MARKERS).
        let own = vec!["musterallee".to_string()];
        assert_eq!(
            parse_summary_address_with(&adr("Musterallee 15-21, 12345 Beispielstadt"), &own),
            None
        );
        // Without markers configured the same address parses normally.
        assert!(parse_summary_address_with(&adr("Musterallee 15-21, 12345 Beispielstadt"), &[]).is_some());
    }

    #[test]
    fn missing_block_returns_none() {
        assert_eq!(parse_summary_address("no adressen block here"), None);
    }

    #[test]
    fn token_stripper_is_utf8_safe() {
        assert_eq!(strip_address_tokens("Straße Address_ABCDEF12, 12345 Köln"), "Straße , 12345 Köln");
    }

    #[test]
    fn find_plz_ort_ignores_year_like_4digits_without_capital_city() {
        // "2024 wurde..." — 4 digits followed by a lowercase word is not a PLZ Ort.
        assert_eq!(find_plz_ort("Im Jahr 2024 wurde das Gerät gekauft"), None);
    }
}
