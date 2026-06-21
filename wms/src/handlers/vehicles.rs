//! Vehicle registry for the Fahrtenbuch.
//!
//! Identifies the car per trip via its official plate (amtliches Kennzeichen —
//! a GoBD-required Fahrtenbuch field). A vehicle is captured ONCE (plate OCR on
//! the PDA, or typed) and thereafter picked from this list; if a fleet has
//! exactly one active vehicle the PDA auto-fills it. This is plain reference
//! data (no PII), replicated across the customer's own paid mesh and mirrored
//! by the PDA for offline selection (`GET /api/vehicles`).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Canonical comparable plate form: uppercase, runs of spaces/dashes collapsed
/// to a single space, trimmed. Keeps dedupe stable across OCR/typing variants
/// ("b- x123" / "B  X123" → "B X123") without trying to validate the format
/// (the client owns the German-plate regex; the server stores what it's given).
pub fn normalize_plate(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_sep = false;
    for c in raw.trim().to_uppercase().chars() {
        if c.is_whitespace() || c == '-' {
            if !prev_sep && !out.is_empty() {
                out.push(' ');
            }
            prev_sep = true;
        } else {
            out.push(c);
            prev_sep = false;
        }
    }
    out.trim().to_string()
}

#[derive(Deserialize)]
pub struct VehicleListQuery {
    /// Include retired (inactive) vehicles too.
    pub all: Option<bool>,
    pub limit: Option<i64>,
}

/// GET /api/vehicles — registered vehicles (active first). The PDA mirrors this
/// for offline selection at trip start.
pub async fn list_vehicles(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VehicleListQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let where_clause = if q.all.unwrap_or(false) { "" } else { "WHERE active = true " };
    let query = format!(
        "SELECT record::id(id) AS id, plate, label, photo_file_id, active, created_at \
         FROM vehicle {}ORDER BY active DESC, plate ASC LIMIT $limit",
        where_clause
    );
    let rows: Vec<Value> = state
        .db
        .query(&query)
        .bind(("limit", limit))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateVehicleRequest {
    pub plate: String,
    #[serde(default)]
    pub label: Option<String>,
    /// CAS uuid of the plate photo (evidence the vehicle was photographed).
    #[serde(default)]
    pub photo_file_id: Option<String>,
}

/// POST /api/vehicles — register a vehicle from its plate (+ optional plate
/// photo CAS ref). Idempotent by normalized plate: re-posting an existing plate
/// re-activates it (and fills a missing label/photo) instead of duplicating.
pub async fn create_vehicle(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateVehicleRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let plate = normalize_plate(&body.plate);
    if plate.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "plate is required".into()));
    }
    let now = Utc::now().to_rfc3339();

    // Dedupe by normalized plate — re-photographing the same car must not
    // create a second registry entry.
    let existing: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS id FROM vehicle WHERE plate = $plate LIMIT 1")
        .bind(("plate", plate.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    if let Some(id) = existing.first().and_then(|r| r.get("id")).and_then(|v| v.as_str()) {
        let updated: Vec<Value> = state
            .db
            .query(
                "UPDATE type::record('vehicle', $id) SET active = true, updated_at = $now, \
                 label = label ?? $label, photo_file_id = $photo ?? photo_file_id \
                 RETURN record::id(id) AS id, plate, label, photo_file_id, active",
            )
            .bind(("id", id.to_string()))
            .bind(("now", now))
            .bind(("label", body.label.clone()))
            .bind(("photo", body.photo_file_id.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .unwrap_or_default();
        return Ok((StatusCode::OK, Json(updated.into_iter().next().unwrap_or(Value::Null))));
    }

    let id = uuid::Uuid::new_v4().to_string();
    // UPSERT type::record (the project-standard reliable create path; bare
    // `CREATE ... SET` can silently no-op on schemaless tables — see TECH_DEBT).
    let content = json!({
        "uuid": id,
        "plate": plate,
        "label": body.label,
        "photo_file_id": body.photo_file_id,
        "active": true,
        "created_at": now,
        "updated_at": now,
    });
    let created: Vec<Value> = state
        .db
        .query(
            "UPSERT type::record('vehicle', $id) MERGE $content \
             RETURN record::id(id) AS id, plate, label, photo_file_id, active",
        )
        .bind(("id", id.clone()))
        .bind(("content", content))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    info!("Vehicle registered: {} ({})", id, created.first().and_then(|v| v.get("plate")).and_then(|v| v.as_str()).unwrap_or(""));
    Ok((StatusCode::CREATED, Json(created.into_iter().next().unwrap_or(Value::Null))))
}

#[derive(Deserialize)]
pub struct UpdateVehicleRequest {
    #[serde(default)]
    pub plate: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub photo_file_id: Option<String>,
    /// Retire (false) or re-activate (true) a vehicle.
    #[serde(default)]
    pub active: Option<bool>,
}

/// PUT /api/vehicles/:id — edit a vehicle (rename, swap photo, retire/reactivate).
pub async fn update_vehicle(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateVehicleRequest>,
) -> ApiResult<Json<Value>> {
    let mut patch = serde_json::Map::new();
    if let Some(p) = body.plate {
        patch.insert("plate".into(), json!(normalize_plate(&p)));
    }
    if let Some(l) = body.label {
        patch.insert("label".into(), json!(l));
    }
    if let Some(f) = body.photo_file_id {
        patch.insert("photo_file_id".into(), json!(f));
    }
    if let Some(a) = body.active {
        patch.insert("active".into(), json!(a));
    }
    patch.insert("updated_at".into(), json!(Utc::now().to_rfc3339()));

    let updated: Vec<Value> = state
        .db
        .query(
            "UPDATE type::record('vehicle', $id) MERGE $patch \
             RETURN record::id(id) AS id, plate, label, photo_file_id, active",
        )
        .bind(("id", id))
        .bind(("patch", Value::Object(patch)))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    if updated.is_empty() {
        return Err((StatusCode::NOT_FOUND, "vehicle not found".into()));
    }
    Ok(Json(updated.into_iter().next().unwrap_or(Value::Null)))
}

#[cfg(test)]
mod tests {
    use super::normalize_plate;

    #[test]
    fn normalizes_separators_and_case() {
        assert_eq!(normalize_plate("b- x123"), "B X123");
        assert_eq!(normalize_plate("  F  AB   1234 "), "F AB 1234");
        assert_eq!(normalize_plate("m-zz-99"), "M ZZ 99");
        assert_eq!(normalize_plate(""), "");
    }
}
