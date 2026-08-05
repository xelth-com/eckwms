use axum::{extract::Query, extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tracing::{debug, info};

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// Company HQ — mirrors `geocoder.rs` `hq_fallback_coords()`. Kept as a local
// function here (rather than re-exporting from `geocoder.rs`) because the
// geocoder module is a long-running background worker and pulling it into
// the request path for a single pair of floats isn't worth the coupling.
//
/// HQ fallback pin for the `reset_home` operator action, from
/// `ECK_GEO_FALLBACK_LAT` / `ECK_GEO_FALLBACK_LNG` (parsed once). `None` when
/// either var is unset or unparseable — a deployment without a configured HQ
/// simply can't fulfil this request (see `fix_location`'s `reset_home` arm).
fn hq_fallback_coords() -> Option<(f64, f64)> {
    static V: OnceLock<Option<(f64, f64)>> = OnceLock::new();
    *V.get_or_init(|| {
        let lat = std::env::var("ECK_GEO_FALLBACK_LAT").ok();
        let lng = std::env::var("ECK_GEO_FALLBACK_LNG").ok();
        parse_hq_fallback(lat.as_deref(), lng.as_deref())
    })
}

/// Pure parser for the HQ fallback pin — unit-testable without touching
/// process env.
fn parse_hq_fallback(lat: Option<&str>, lng: Option<&str>) -> Option<(f64, f64)> {
    let lat: f64 = lat?.trim().parse().ok()?;
    let lng: f64 = lng?.trim().parse().ok()?;
    Some((lat, lng))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GeoFixRequest {
    /// Which table holds the record. Accepts `order` (RMA/repair) or
    /// `document` (support ticket). Any other value is rejected 400.
    pub table: String,
    /// Record id without the `table:` prefix — matches the `record::id(id)`
    /// projection used elsewhere (e.g. `/api/rma/:id`, `/api/support/...`).
    pub id: String,
    /// Operation mode:
    /// * `reset_home` — pin at Eschborn HQ, marks `geo_override=true` so
    ///   the geocoder worker won't overwrite it on the next tick.
    /// * `edit` — replace `zip` / `city` in the record's meta block and
    ///   unset `geo` + `geo_failed`. The geocoder worker re-runs within
    ///   ~15s because its skip-condition (`geo IS NONE`) becomes false.
    pub mode: String,
    pub zip: Option<String>,
    pub city: Option<String>,
}

#[derive(Deserialize)]
pub struct GeoResolveQuery {
    pub zip: Option<String>,
    pub city: Option<String>,
}

/// POST /api/geo/regeocode-fallback — one-shot rescue of the HQ-fallback pile.
///
/// WHY: ~1000 support tickets sit on the office marker because the background
/// geocoder pinned them to HQ before the summarizer had written their resolved
/// address, and the worker's `geo IS NONE` filter never revisits a pinned
/// record. This re-reads every `geo_fallback = true` record and resolves it
/// from `meta.zip`/`meta.city` and the AI summary's address block (multi-country
/// DE/AT/CH, cached). Only writes on success — unresolved records stay pinned.
///
/// Nominatim is rate-limited (~1 req/s), so the batch runs detached and logs
/// progress; the response returns immediately with the candidate count. Optional
/// body `{ "limit": N }` caps the batch (0 / omitted = all).
pub async fn regeocode_fallback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    // Quick candidate count so the operator sees the scope up front.
    let counts: Vec<Value> = state.db
        .query(
            "SELECT count() AS c FROM document \
             WHERE type = 'support_ticket' AND meta.geo_fallback = true \
             AND meta.geo_override IS NOT true GROUP ALL",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let candidates = counts.first().and_then(|v| v.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);

    let db = state.db.clone();
    tokio::spawn(async move {
        crate::services::geocoder::regeocode_fallback_batch(db, limit).await;
    });

    info!("[geo/regeocode-fallback] started, ~{candidates} ticket candidates (limit={limit})");
    Ok(Json(json!({
        "ok": true,
        "started": true,
        "ticket_candidates": candidates,
        "limit": limit,
        "note": "running in background (~1 req/s); watch logs for [Regeocode] progress",
    })))
}

/// GET /api/geo/grounding-config — read the address-discovery (Lever 2A) switch.
pub async fn get_grounding_config(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let cfg: Vec<Value> = state.db
        .query("SELECT grounding_enabled, updated_at FROM system_config:geo")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let enabled = cfg.first()
        .and_then(|v| v.get("grounding_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(Json(json!({ "grounding_enabled": enabled })))
}

/// POST /api/geo/grounding-config — flip the address-discovery switch.
/// Body `{ "enabled": true|false }`. The same `system_config:geo` record the
/// internal agent can UPSERT directly. Off by default (no cloud spend / egress).
pub async fn set_grounding_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let enabled = body.get("enabled").and_then(|v| v.as_bool())
        .ok_or((StatusCode::BAD_REQUEST, "body needs { enabled: bool }".to_string()))?;
    state.db
        .query("UPSERT system_config:geo SET grounding_enabled = $enabled, updated_at = time::now()")
        .bind(("enabled", enabled))
        .await
        .map_err(db_err)?;
    // Turning grounding ON releases tickets the auto-fill worker parked while it
    // was off, so they get an AI lookup on the next cycle. Per record + vclock
    // bump (not one bulk UPDATE): an un-bumped release loses the mesh merge and
    // the tickets snap back to parked.
    if enabled {
        let ids: Vec<Value> = state.db
            .query("SELECT record::id(id) AS id FROM document WHERE meta.geo_source = 'needs_grounding'")
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        for row in &ids {
            let Some(id) = row.get("id").and_then(|v| v.as_str()) else { continue };
            let ok = state.db
                .query("UPDATE document SET meta.geo_source = NONE, updated_at = time::now() \
                        WHERE record::id(id) = $id")
                .bind(("id", id.to_string()))
                .await
                .is_ok();
            if ok {
                crate::services::geocoder::bump_geo_vclock(&state.db, "document", id).await;
            }
        }
        info!("[geo/grounding-config] released {} needs_grounding ticket(s)", ids.len());
    }
    info!("[geo/grounding-config] grounding_enabled = {enabled}");
    Ok(Json(json!({ "ok": true, "grounding_enabled": enabled })))
}

/// POST /api/geo/discover-addresses — run Lever 2A: look up the public business
/// address (company name + email domain) of office-pinned tickets via Gemini +
/// Google grounding, then geocode. Gated by `system_config:geo.grounding_enabled`
/// (returns 409 when off). Optional body `{ "limit": N }`. Runs detached; watch
/// logs for `[Discover]` progress. Costs Gemini budget — start with a small limit.
pub async fn discover_addresses(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let enabled: Vec<Value> = state.db
        .query("SELECT grounding_enabled FROM system_config:geo")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let on = enabled.first()
        .and_then(|v| v.get("grounding_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !on {
        return Err((
            StatusCode::CONFLICT,
            "grounding disabled: POST /api/geo/grounding-config { enabled: true } first".into(),
        ));
    }

    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let counts: Vec<Value> = state.db
        .query(
            "SELECT count() AS c FROM document \
             WHERE type = 'support_ticket' AND meta.geo_fallback = true \
               AND meta.geo_override IS NOT true AND meta.geo_source IS NONE \
               AND status != 'Closed' AND status != 'Auto closed' \
               AND ((meta.company != NONE AND meta.company != '') \
                    OR (meta.email != NONE AND meta.email != '')) GROUP ALL",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let candidates = counts.first().and_then(|v| v.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);

    let model = std::env::var("GEMINI_GENERATION_MODEL")
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "GEMINI_GENERATION_MODEL unset".to_string()))?;
    let db = state.db.clone();
    tokio::spawn(async move {
        crate::ai::address_discovery::discover_addresses_batch(db, model, limit).await;
    });

    info!("[geo/discover-addresses] started, ~{candidates} candidates (limit={limit})");
    Ok(Json(json!({
        "ok": true,
        "started": true,
        "candidates": candidates,
        "limit": limit,
        "note": "running in background; watch logs for [Discover]; costs Gemini budget",
    })))
}

/// POST /api/geo/vorwahl-fill — Lever 2B: place residual office-pinned tickets
/// by their landline area code (Bundesnetzagentur ONB table). Entirely local —
/// the phone never leaves the box, only the resolved town name is geocoded.
/// Coarse (city-centre). Optional body `{ "limit": N }`. Runs detached.
pub async fn vorwahl_fill(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let counts: Vec<Value> = state.db
        .query(
            "SELECT count() AS c FROM document \
             WHERE type = 'support_ticket' AND meta.geo_fallback = true \
               AND meta.geo_override IS NOT true AND meta.geo_source IS NONE \
               AND meta.phone != NONE AND meta.phone != '' GROUP ALL",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let candidates = counts.first().and_then(|v| v.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);

    let db = state.db.clone();
    tokio::spawn(async move {
        crate::services::vorwahl::vorwahl_fill_batch(db, limit).await;
    });

    info!("[geo/vorwahl-fill] started, ~{candidates} phone candidates (limit={limit})");
    Ok(Json(json!({
        "ok": true,
        "started": true,
        "phone_candidates": candidates,
        "limit": limit,
        "note": "local area-code lookup; coarse city-centre; watch logs for [Vorwahl]",
    })))
}

/// POST /api/geo/customer-fill — Lever 2B′: place residual office-pinned tickets
/// from customer records we already trust (another located ticket or an Exact
/// partner sharing the same email / phone / company). Entirely local — identity
/// keys are matched in memory; only a stored city/zip is geocoded. Also retries
/// tickets parked as `auto_nomatch` / `needs_grounding` / `derived_failed`,
/// since this lever is free and its source universe grows over time. Optional
/// body `{ "limit": N }`. Runs detached; watch logs for `[CustomerGeo]`.
pub async fn customer_fill(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let counts: Vec<Value> = state.db
        .query(
            "SELECT count() AS c FROM document \
             WHERE type = 'support_ticket' AND meta.geo_fallback = true \
               AND meta.geo_override IS NOT true \
               AND (meta.geo_source IS NONE \
                    OR meta.geo_source IN ['auto_nomatch', 'needs_grounding', 'derived_failed']) \
             GROUP ALL",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let candidates = counts.first().and_then(|v| v.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);

    let db = state.db.clone();
    tokio::spawn(async move {
        crate::services::customer_geo::customer_fill_batch(db, limit).await;
    });

    info!("[geo/customer-fill] started, ~{candidates} candidates (limit={limit})");
    Ok(Json(json!({
        "ok": true,
        "started": true,
        "candidates": candidates,
        "limit": limit,
        "note": "local customer-record match; watch logs for [CustomerGeo]",
    })))
}

/// GET /api/geo/resolve?zip=&city= — server-side, cached geocoding so the
/// browser never calls Nominatim itself (no client→OSM leak). Only zip+city are
/// accepted/forwarded — never a street address. Returns {lat,lng} or nulls.
pub async fn resolve_location(
    State(state): State<Arc<AppState>>,
    Query(q): Query<GeoResolveQuery>,
) -> ApiResult<Json<Value>> {
    let zip = q.zip.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let city = q.city.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if zip.is_none() && city.is_none() {
        return Err((StatusCode::BAD_REQUEST, "need zip or city".into()));
    }
    match crate::services::geocoder::resolve_zip_city_cached(&state.db, zip, city).await {
        Some((lat, lng)) => Ok(Json(json!({ "lat": lat, "lng": lng }))),
        None => Ok(Json(json!({ "lat": Value::Null, "lng": Value::Null }))),
    }
}

/// POST /api/geo/fix — operator override for wrong geocoded markers.
///
/// WHY: Nominatim + our OCR'd custom fields occasionally place tickets on
/// the wrong continent (see `fix: deterministic address masking + zip+city-only
/// geolookup` commit). Until the geocoder learns to reject obvious outliers
/// we give operators a one-click escape hatch — reset to HQ or hand-edit
/// zip+city — from the map popup and the ticket detail page. This keeps
/// the audit trail (`geo_override`, original `zip`/`city` in meta) intact.
pub async fn fix_location(
    State(state): State<Arc<AppState>>,
    Json(req): Json<GeoFixRequest>,
) -> ApiResult<Json<Value>> {
    let meta_field = match req.table.as_str() {
        "order" => "metadata",
        "document" => "meta",
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unsupported table '{}': expected 'order' or 'document'", req.table),
            ));
        }
    };

    match req.mode.as_str() {
        "reset_home" => {
            let Some((lat, lng)) = hq_fallback_coords() else {
                debug!(
                    "[geo/fix] reset_home requested for {}:{} but no HQ fallback configured \
                     (ECK_GEO_FALLBACK_LAT/ECK_GEO_FALLBACK_LNG unset)",
                    req.table, req.id
                );
                return Err((
                    StatusCode::CONFLICT,
                    "HQ fallback pin not configured: set ECK_GEO_FALLBACK_LAT and \
                     ECK_GEO_FALLBACK_LNG first".to_string(),
                ));
            };
            let sql = format!(
                "UPDATE {table} SET \
                 {m}.geo = {{ lat: $lat, lng: $lng }}, \
                 {m}.geo_fallback = true, \
                 {m}.geo_override = true, \
                 {m}.geo_failed = NONE, \
                 updated_at = time::now() \
                 WHERE record::id(id) = $id \
                 RETURN AFTER",
                table = req.table,
                m = meta_field,
            );
            let updated: Vec<Value> = state
                .db
                .query(&sql)
                .bind(("id", req.id.clone()))
                .bind(("lat", lat))
                .bind(("lng", lng))
                .await
                .map_err(db_err)?
                .take(0)
                .map_err(db_err)?;
            if updated.is_empty() {
                return Err((StatusCode::NOT_FOUND, "record not found".into()));
            }
            // Operator pin must never be reverted by a stale mesh peer.
            crate::services::geocoder::bump_geo_vclock(&state.db, &req.table, &req.id).await;
            info!("[geo/fix] reset_home {}:{}", req.table, req.id);
            Ok(Json(json!({
                "ok": true,
                "mode": "reset_home",
                "lat": lat,
                "lng": lng,
            })))
        }
        "edit" => {
            let zip = req.zip.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let city = req.city.as_deref().map(str::trim).filter(|s| !s.is_empty());
            if zip.is_none() && city.is_none() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "edit mode requires at least one of zip/city".into(),
                ));
            }

            // Clear geo so the worker reprocesses. We do NOT pin coords
            // here because Nominatim-derived lat/lng is more accurate than
            // whatever the operator could type, and because setting
            // `geo_override=true` with new zip/city would keep a stale
            // pin until someone noticed.
            let sql = format!(
                "UPDATE {table} SET \
                 {m}.zip = $zip, \
                 {m}.city = $city, \
                 {m}.geo = NONE, \
                 {m}.geo_failed = NONE, \
                 {m}.geo_override = NONE, \
                 updated_at = time::now() \
                 WHERE record::id(id) = $id \
                 RETURN AFTER",
                table = req.table,
                m = meta_field,
            );
            let updated: Vec<Value> = state
                .db
                .query(&sql)
                .bind(("id", req.id.clone()))
                .bind(("zip", zip.map(String::from)))
                .bind(("city", city.map(String::from)))
                .await
                .map_err(db_err)?
                .take(0)
                .map_err(db_err)?;
            if updated.is_empty() {
                return Err((StatusCode::NOT_FOUND, "record not found".into()));
            }
            // Propagate the operator's zip/city edit (+ geo clear) ahead of peers.
            crate::services::geocoder::bump_geo_vclock(&state.db, &req.table, &req.id).await;
            info!(
                "[geo/fix] edit {}:{} zip={:?} city={:?}",
                req.table, req.id, zip, city
            );
            Ok(Json(json!({
                "ok": true,
                "mode": "edit",
                "note": "geocoder worker will reprocess within ~15s",
            })))
        }
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("unknown mode '{}': expected 'reset_home' or 'edit'", other),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hq_fallback;

    #[test]
    fn parses_valid_pair() {
        assert_eq!(parse_hq_fallback(Some("50.1407"), Some("8.5721")), Some((50.1407, 8.5721)));
    }

    #[test]
    fn none_when_either_missing() {
        assert_eq!(parse_hq_fallback(None, Some("8.5721")), None);
        assert_eq!(parse_hq_fallback(Some("50.1407"), None), None);
        assert_eq!(parse_hq_fallback(None, None), None);
    }

    #[test]
    fn none_when_unparseable() {
        assert_eq!(parse_hq_fallback(Some("not-a-number"), Some("8.5721")), None);
        assert_eq!(parse_hq_fallback(Some("50.1407"), Some("")), None);
    }
}
