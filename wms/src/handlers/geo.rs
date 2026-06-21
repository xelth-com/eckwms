use axum::{extract::Query, extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// Company HQ — mirrors `geocoder.rs` HOME_OFFICE_{LAT,LNG}. Kept as a local
// const here (rather than re-exporting from `geocoder.rs`) because the
// geocoder module is a long-running background worker and pulling it into
// the request path for a single pair of floats isn't worth the coupling.
const HOME_OFFICE_LAT: f64 = 50.1407;
const HOME_OFFICE_LNG: f64 = 8.5721;

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
            let sql = format!(
                "UPDATE {table} SET \
                 {m}.geo = {{ lat: $lat, lng: $lng }}, \
                 {m}.geo_fallback = true, \
                 {m}.geo_override = true, \
                 {m}.geo_failed = NONE \
                 WHERE record::id(id) = $id \
                 RETURN AFTER",
                table = req.table,
                m = meta_field,
            );
            let updated: Vec<Value> = state
                .db
                .query(&sql)
                .bind(("id", req.id.clone()))
                .bind(("lat", HOME_OFFICE_LAT))
                .bind(("lng", HOME_OFFICE_LNG))
                .await
                .map_err(db_err)?
                .take(0)
                .map_err(db_err)?;
            if updated.is_empty() {
                return Err((StatusCode::NOT_FOUND, "record not found".into()));
            }
            info!("[geo/fix] reset_home {}:{}", req.table, req.id);
            Ok(Json(json!({
                "ok": true,
                "mode": "reset_home",
                "lat": HOME_OFFICE_LAT,
                "lng": HOME_OFFICE_LNG,
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
                 {m}.geo_override = NONE \
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
