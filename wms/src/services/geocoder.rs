use crate::services::support::parse_zip_city;
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

/// On-demand zip+city → (lat,lng), cached in `geo_cache` (resolve-once). Backs
/// `GET /api/geo/resolve` so the **browser never calls Nominatim itself** — it
/// asks us, we resolve server-side (zip+city only, never the street) and cache.
/// Swap `nominatim_lookup` for a self-hosted geocoder later in ONE place.
pub async fn resolve_zip_city_cached(
    db: &SurrealDb,
    zip: Option<&str>,
    city: Option<&str>,
) -> Option<(f64, f64)> {
    let zip_n = zip.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let city_n = city.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if zip_n.is_none() && city_n.is_none() {
        return None;
    }
    let key = format!(
        "{}|{}",
        zip_n.clone().unwrap_or_default().to_lowercase(),
        city_n.clone().unwrap_or_default().to_lowercase()
    );
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

    // 2. miss → resolve server-side (zip+city only) and cache the result
    let result = geolookup_zip_city(geo_client(), zip_n.as_deref(), city_n.as_deref()).await;
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

fn extract_address_from_summary(summary: &str) -> Option<String> {
    let marker = "**Adressen:**";
    let start = summary.find(marker)?;
    let after = &summary[start + marker.len()..];
    for line in after.lines() {
        let clean = line.trim().trim_start_matches('-').trim_start_matches('*').trim();
        if clean.is_empty() || clean.starts_with("===") {
            if clean.starts_with("===") { break; }
            continue;
        }
        let lower = clean.to_lowercase();
        if lower.contains("eschborn") || lower.contains("mergenthalerallee") || lower.contains("inbody") {
            continue;
        }

        let mut final_addr = clean;
        if let Some(idx) = final_addr.rfind('(').or_else(|| final_addr.rfind('[')) {
            let inside_parens = final_addr[idx..].to_lowercase();
            if inside_parens.contains("ermittelt")
                || inside_parens.contains("domain")
                || inside_parens.contains("homepage")
                || inside_parens.contains("google")
                || inside_parens.contains("suche")
                || inside_parens.contains("internet")
                || inside_parens.contains("website")
            {
                final_addr = final_addr[..idx].trim();
            }
        }

        return Some(final_addr.to_string());
    }
    None
}

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
    // Extract just its zip+city — never forward the street.
    if let Some(summary) = record.get("ai_summary").and_then(|v| v.as_str()) {
        if let Some(ai_addr) = extract_address_from_summary(summary) {
            let (ai_zip, ai_city) = parse_zip_city(&ai_addr);
            if ai_zip.is_some() || ai_city.is_some() {
                info!(
                    "[Geocoder] Meta lookup empty; trying AI-summary zip+city for {}: {:?} / {:?}",
                    id, ai_zip, ai_city
                );
                if let Some((lat, lng)) = geolookup_zip_city(client, ai_zip.as_deref(), ai_city.as_deref()).await {
                    save_geo(db, table, id, meta_field, lat, lng, false).await;
                    return;
                }
            }
        }
    }

    info!(
        "[Geocoder] All lookups failed for {}, falling back to home office ({}, {})",
        id, HOME_OFFICE_LAT, HOME_OFFICE_LNG
    );
    save_geo(db, table, id, meta_field, HOME_OFFICE_LAT, HOME_OFFICE_LNG, true).await;
}

/// Build a zip+city query (in that order — Nominatim weights leading tokens
/// more heavily, and "12345 München" is more specific than "München 12345")
/// and run the Nominatim lookup. Returns None when both inputs are empty.
async fn geolookup_zip_city(client: &Client, zip: Option<&str>, city: Option<&str>) -> Option<(f64, f64)> {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(z) = zip.map(str::trim).filter(|s| !s.is_empty()) { parts.push(z); }
    if let Some(c) = city.map(str::trim).filter(|s| !s.is_empty()) { parts.push(c); }
    if parts.is_empty() { return None; }
    let query = parts.join(", ");
    nominatim_lookup(client, &query).await
}

async fn nominatim_lookup(client: &Client, search_str: &str) -> Option<(f64, f64)> {
    debug!("[Geocoder] Looking up: {}", search_str);

    tokio::time::sleep(Duration::from_millis(1100)).await;

    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=json&countrycodes=de&q={}",
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

async fn save_geo(db: &SurrealDb, table: &str, id: &str, meta_field: &str, lat: f64, lng: f64, fallback: bool) {
    let q = if fallback {
        format!(
            "UPDATE {table} SET \
             {meta_field}.geo = {{ lat: $lat, lng: $lng }}, \
             {meta_field}.geo_fallback = true, \
             {meta_field}.geo_failed = NONE \
             WHERE record::id(id) = $id \
             RETURN NONE"
        )
    } else {
        format!(
            "UPDATE {table} SET \
             {meta_field}.geo = {{ lat: $lat, lng: $lng }}, \
             {meta_field}.geo_fallback = NONE, \
             {meta_field}.geo_failed = NONE \
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
                debug!("[Geocoder] Saved coordinates for {}:{}", table, id);
            }
        }
        Err(e) => {
            error!("[Geocoder] Query execution failed for {}:{}: {}", table, id, e);
        }
    }
}
