//! Trip recording (movFast PDA Fahrtenbuch).
//!
//! The PDA records trips passively via cell towers (no GPS): each point
//! carries either resolved coordinates (fused provider) or raw cell identity
//! (MCC/MNC/TAC/CID) that the cell_resolver worker geocodes server-side
//! against the cell_tower cache / OpenCelliD. Odometer readings come from
//! photos (OCR-assisted) or are estimated from the resolved track.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct TripUpload {
    pub trip_uuid: String,
    #[serde(default)]
    pub device_id: String,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub start_odometer_km: Option<f64>,
    #[serde(default)]
    pub start_odometer_source: Option<String>, // photo | manual | estimated
    #[serde(default)]
    pub start_odometer_photo: Option<String>, // CAS uuid
    #[serde(default)]
    pub end_odometer_km: Option<f64>,
    #[serde(default)]
    pub end_odometer_source: Option<String>,
    #[serde(default)]
    pub end_odometer_photo: Option<String>,
    #[serde(default)]
    pub driver_user_id: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>, // business | private | commute
    /// Structured purpose (Level A). Business reference for the GoBD-required
    /// "aufgesuchter Geschäftspartner" — e.g. "visit_task:xxx" | "order:yyy".
    #[serde(default)]
    pub purpose_ref: Option<String>,
    /// Human label of the destination/partner (customer name / visit title).
    #[serde(default)]
    pub purpose_label: Option<String>,
    /// When the purpose was FIRST declared (RFC3339, set at trip start). The
    /// server preserves the earliest value — anti-fabrication anchor.
    #[serde(default)]
    pub purpose_declared_at: Option<String>,
    /// How it was entered: planned | voice | text | manual.
    #[serde(default)]
    pub purpose_source: Option<String>,
    /// Vehicle (Fahrtenbuch): registry id + denormalized plate (amtliches
    /// Kennzeichen) at trip time. The plate folds into the GoBD seal (v3) and
    /// the Finanzamt export. Kept for private trips too (km accrue to the car).
    #[serde(default)]
    pub vehicle_id: Option<String>,
    #[serde(default)]
    pub vehicle_plate: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Points: {seq, ts, source: cell|fused|gps, lat?, lng?, accuracy_m?,
    ///          mcc?, mnc?, tac?, cid?, signal_dbm?}
    #[serde(default)]
    pub points: Vec<Value>,
}

/// POST /api/trips — upsert a trip from the PDA (idempotent by trip_uuid).
///
/// The device may upload the same trip several times (open trip checkpoints,
/// then the final version with end odometer) — last write wins, points are
/// replaced wholesale. Resolution restarts whenever new unresolved points
/// arrive.
pub async fn upload_trip(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TripUpload>,
) -> ApiResult<Json<Value>> {
    if body.trip_uuid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "trip_uuid is required".into()));
    }

    super::pda::require_active_device(&state, &body.device_id).await?;

    // Privatfahrt (DSGVO): private trips must never carry coordinates —
    // only the odometer delta is stored. The client already omits points;
    // this is defense in depth. A private trip also carries NO destination
    // (purpose_ref/label), since "where" is exactly what stays private.
    let is_private = body.purpose.as_deref() == Some("private");
    let points = if is_private { Vec::new() } else { body.points };
    let (purpose_ref, purpose_label) = if is_private {
        (None, None)
    } else {
        (body.purpose_ref.clone(), body.purpose_label.clone())
    };

    // Normalize the plate the same way the registry does, so the trip's
    // denormalized Kennzeichen matches the vehicle row and the seal is stable.
    let vehicle_plate = body
        .vehicle_plate
        .as_deref()
        .map(super::vehicles::normalize_plate)
        .filter(|p| !p.is_empty());

    let has_unresolved_cells = points.iter().any(|p| {
        p.get("lat").and_then(|v| v.as_f64()).is_none()
            && p.get("cid").and_then(|v| v.as_i64()).is_some()
    });

    let ended = body.ended_at.is_some();
    let now = Utc::now().to_rfc3339();

    let content = json!({
        "trip_uuid": body.trip_uuid,
        "device_id": body.device_id,
        "started_at": body.started_at,
        "ended_at": body.ended_at,
        "status": if ended { "ended" } else { "open" },
        "start_odometer_km": body.start_odometer_km,
        "start_odometer_source": body.start_odometer_source,
        "start_odometer_photo": body.start_odometer_photo,
        "end_odometer_km": body.end_odometer_km,
        "end_odometer_source": body.end_odometer_source,
        "end_odometer_photo": body.end_odometer_photo,
        "driver_user_id": body.driver_user_id,
        "purpose": body.purpose.unwrap_or_else(|| "business".into()),
        "purpose_ref": purpose_ref,
        "purpose_label": purpose_label,
        "purpose_source": body.purpose_source,
        "vehicle_id": body.vehicle_id,
        "vehicle_plate": vehicle_plate,
        "note": body.note,
        "point_count": points.len(),
        "points": points,
        // worker control flags
        "needs_resolution": has_unresolved_cells || ended,
        "computed_distance_km": Value::Null,
        "updated_at": now,
    });

    // UPSERT keeps created_at on re-upload
    let upserted: Vec<Value> = state
        .db
        .query(
            "UPSERT type::record('trip', $id) MERGE $content \
             RETURN record::id(id) AS id, status, point_count",
        )
        .bind(("id", body.trip_uuid.clone()))
        .bind(("content", content))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    // Preserve the EARLIEST values across re-uploads: created_at and the
    // purpose-declaration timestamp (GoBD anti-fabrication anchor — the first
    // declaration wins, later re-uploads never push it forward).
    let _: Vec<Value> = state
        .db
        .query(
            "UPDATE type::record('trip', $id) SET \
             created_at = created_at ?? $now, \
             purpose_declared_at = purpose_declared_at ?? $declared",
        )
        .bind(("id", body.trip_uuid.clone()))
        .bind(("now", Utc::now().to_rfc3339()))
        .bind(("declared", body.purpose_declared_at.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    info!("Trip upsert: {} (ended={})", body.trip_uuid, ended);

    Ok(Json(json!({
        "success": true,
        "trip": upserted.first().cloned().unwrap_or(Value::Null),
    })))
}

#[derive(Deserialize)]
pub struct TripListQuery {
    pub device_id: Option<String>,
    pub from: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/trips — trip summaries (no points), newest first.
pub async fn list_trips(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TripListQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);

    let mut conditions = Vec::new();
    if q.device_id.is_some() {
        conditions.push("device_id = $dev");
    }
    if q.from.is_some() {
        conditions.push("started_at >= $from");
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {} ", conditions.join(" AND "))
    };

    let query = format!(
        "SELECT record::id(id) AS id, trip_uuid, device_id, started_at, ended_at, status, \
         start_odometer_km, end_odometer_km, computed_distance_km, purpose, note, \
         point_count, needs_resolution, driver_user_id \
         FROM trip {}ORDER BY started_at DESC LIMIT $limit",
        where_clause
    );

    let mut stmt = state.db.query(&query).bind(("limit", limit));
    if let Some(dev) = q.device_id {
        stmt = stmt.bind(("dev", dev));
    }
    if let Some(from) = q.from {
        stmt = stmt.bind(("from", from));
    }

    let trips: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();
    Ok(Json(trips))
}

/// GET /api/trips/:id — full trip including resolved points (for map display).
pub async fn get_trip(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let trip: Option<Value> = state
        .db
        .select(("trip", &*id))
        .await
        .map_err(db_err)?;

    trip.map(Json)
        .ok_or((StatusCode::NOT_FOUND, format!("Trip '{id}' not found")))
}

// ─── Live position (consent-gated dashboard visibility) ──────────────────────

#[derive(Deserialize)]
pub struct TripLivePing {
    pub trip_uuid: String,
    #[serde(default)]
    pub device_id: String,
    pub lat: f64,
    pub lng: f64,
    /// Course over ground in degrees (0=N, 90=E), to rotate the car marker.
    #[serde(default)]
    pub heading: Option<f64>,
    #[serde(default)]
    pub speed_kmh: Option<f64>,
    /// Denormalized amtliches Kennzeichen — the marker label on the map.
    #[serde(default)]
    pub vehicle_plate: Option<String>,
    /// Client timestamp of the fix (RFC3339); server fills `now` if absent.
    #[serde(default)]
    pub ts: Option<String>,
}

/// POST /api/trips/live — ephemeral live position of an in-progress trip for the
/// dashboard map (consent-gated visibility).
///
/// **Nothing is persisted.** The fine-grained live position is transient — DSGVO:
/// only the sealed trip *aggregate* is retained (10y), while the live track is
/// never stored server-side. This endpoint only re-broadcasts the position as a
/// `TRIP_LIVE` WebSocket event the dashboard renders as a moving car marker
/// labeled with the Kennzeichen.
///
/// The PDA calls this **only** when the driver has opted into live sharing AND
/// the trip is business. Defense in depth: a trip whose stored purpose is
/// `private` is suppressed here regardless of what the client sends — a
/// Privatfahrt's coordinates must never surface (it carries none to begin with).
pub async fn trip_live(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TripLivePing>,
) -> ApiResult<Json<Value>> {
    if body.trip_uuid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "trip_uuid is required".into()));
    }
    super::pda::require_active_device(&state, &body.device_id).await?;

    // Suppress private trips even if the client mislabels: look up the stored
    // purpose. (An open trip has already been upserted as a checkpoint, so the
    // row exists; a missing row → not private → allowed.)
    let purpose_rows: Vec<Value> = state
        .db
        .query("SELECT VALUE purpose FROM type::record('trip', $id)")
        .bind(("id", body.trip_uuid.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();
    if purpose_rows.first().and_then(|v| v.as_str()) == Some("private") {
        return Ok(Json(json!({ "ok": true, "suppressed": "private" })));
    }

    let plate = body
        .vehicle_plate
        .as_deref()
        .map(super::vehicles::normalize_plate)
        .filter(|p| !p.is_empty());

    let event = json!({
        "type": "TRIP_LIVE",
        "trip_uuid": body.trip_uuid,
        "vehicle_plate": plate,
        "lat": body.lat,
        "lng": body.lng,
        "heading": body.heading,
        "speed_kmh": body.speed_kmh,
        "ts": body.ts.unwrap_or_else(|| Utc::now().to_rfc3339()),
    });
    // Best-effort: no subscribers (empty dashboard) is not an error.
    let _ = state.ws_tx.send(event.to_string());

    Ok(Json(json!({ "ok": true })))
}

// ─── Purpose candidates (Level A — declared at trip start) ───────────────────

#[derive(Deserialize)]
pub struct PurposeCandidateQuery {
    pub device_id: Option<String>,
    /// Current rough position at trip start — ranks the nearest planned stop
    /// first (route-planner-lite). Optional.
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub limit: Option<i64>,
}

fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let r = 6371.0_f64;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lng2 - lng1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

/// GET /api/trips/purpose-candidates — planned trip purposes for the start
/// screen. Sourced from OPEN visit_tasks (the "Besuch planen" plan); each
/// becomes a one-tap purpose whose `ref` documents the GoBD-required
/// "aufgesuchter Geschäftspartner" as a verifiable CRM/ticket reference instead
/// of free text. If a position is supplied, the nearest planned stop ranks
/// first (route-planner-lite); otherwise by due date (overdue first).
pub async fn purpose_candidates(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PurposeCandidateQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    // a small forward window so a stop done a bit early still shows up
    let horizon = (Utc::now() + chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();
    let limit = q.limit.unwrap_or(8).clamp(1, 50);

    let mut conditions = vec!["status IN ['open', 'checked_in']", "due_date <= $horizon"];
    if q.device_id.is_some() {
        conditions.push("(assigned_device_id = $dev OR assigned_device_id IS NONE)");
    }
    let query = format!(
        "SELECT record::id(id) AS id, title, address, lat, lng, due_date, \
         target_entity_type, target_entity_id, note \
         FROM visit_task WHERE {} ORDER BY due_date ASC LIMIT $limit",
        conditions.join(" AND ")
    );
    let mut stmt = state
        .db
        .query(&query)
        .bind(("horizon", horizon))
        .bind(("limit", limit));
    if let Some(dev) = q.device_id {
        stmt = stmt.bind(("dev", dev));
    }
    let visits: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();

    let mut candidates: Vec<Value> = visits
        .into_iter()
        .map(|v| {
            let vid = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            // strongest business reference: the linked order/ticket if present,
            // else the visit task itself
            let et = v.get("target_entity_type").and_then(|x| x.as_str());
            let eid = v.get("target_entity_id").and_then(|x| x.as_str());
            let purpose_ref = match (et, eid) {
                (Some(t), Some(i)) if !t.is_empty() && !i.is_empty() => format!("{t}:{i}"),
                _ => format!("visit_task:{vid}"),
            };
            let due = v.get("due_date").and_then(|x| x.as_str()).unwrap_or("");
            let (vlat, vlng) = (
                v.get("lat").and_then(|x| x.as_f64()),
                v.get("lng").and_then(|x| x.as_f64()),
            );
            let distance_km = match (q.lat, q.lng, vlat, vlng) {
                (Some(a), Some(b), Some(c), Some(d)) => {
                    Some((haversine_km(a, b, c, d) * 10.0).round() / 10.0)
                }
                _ => None,
            };
            json!({
                "purpose_ref": purpose_ref,
                "visit_id": vid,
                "label": v.get("title").cloned().unwrap_or(Value::Null),
                "address": v.get("address").cloned().unwrap_or(Value::Null),
                "lat": vlat, "lng": vlng,
                "due_date": due,
                "overdue": due < today.as_str(),
                "distance_km": distance_km,
                "source": "planned",
            })
        })
        .collect();

    // Rank: if a position was given, nearest planned stop first (null distance
    // last); otherwise keep the due-date order from the query.
    if q.lat.is_some() && q.lng.is_some() {
        candidates.sort_by(|a, b| {
            let da = a.get("distance_km").and_then(|x| x.as_f64()).unwrap_or(f64::INFINITY);
            let db = b.get("distance_km").and_then(|x| x.as_f64()).unwrap_or(f64::INFINITY);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    Ok(Json(candidates))
}

// ─── Destinations: cities + fuzzy ticket search (trip-mode console) ───────────

#[derive(Deserialize)]
pub struct DestinationQuery {
    /// Free text — fuzzy match across ALL open tickets (name/address/city/no.)
    pub q: Option<String>,
    /// Restrict to one city (the city chips)
    pub city: Option<String>,
    pub device_id: Option<String>,
    pub limit: Option<i64>,
    /// `ai=true` → smart Gemini fallback (corrects mis-heard/typo queries like
    /// "treutlingen" → "Reutlingen"). Cost-controlled: only on explicit request
    /// (client shows a "🤖 KI-Suche" button when the local search is empty).
    pub ai: Option<bool>,
}

/// Gemini fallback: match a (possibly mis-heard) query to the open tickets.
/// Uses 1-based indices in the prompt so the model can't mangle record ids.
async fn gemini_match_tickets(orders: &[Value], query: &str, limit: usize) -> Option<Vec<Value>> {
    if !eck_core::ai::AiAuth::is_enabled_in_env() {
        return None;
    }
    let model = std::env::var("GEMINI_GENERATION_MODEL").ok()?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .ok()?;
    let auth = eck_core::ai::AiAuth::resolve(&http).await.ok()?;
    if !auth.is_configured() {
        return None;
    }

    let capped: Vec<&Value> = orders
        .iter()
        .filter(|o| o.get("customerName").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false))
        .take(300)
        .collect();
    if capped.is_empty() {
        return Some(Vec::new());
    }
    let mut list = String::new();
    for (i, o) in capped.iter().enumerate() {
        let name = o.get("customerName").and_then(|v| v.as_str()).unwrap_or("");
        list.push_str(&format!("{}\t{}\n", i + 1, name));
    }

    let system = "Du ordnest ein per Spracherkennung oder Tippen eingegebenes Suchwort dem \
        richtigen Kunden zu. Das Suchwort kann verhört oder vertippt sein (z. B. \
        'treutlingen' = 'Reutlingen'). Gegeben eine nummerierte Liste 'Nr<TAB>Kundenname', \
        gib NUR ein JSON-Array der Nummern der am besten passenden Kunden zurück (beste \
        zuerst, höchstens 8). Keine Erklärung. Wenn nichts passt: [].";
    let user = format!("Suchwort: \"{query}\"\n\nKunden:\n{list}");
    let payload = json!({
        "systemInstruction": { "parts": [{ "text": system }] },
        "contents": [{ "parts": [{ "text": user }] }],
        "generationConfig": { "temperature": 0.0, "maxOutputTokens": 256 }
    });

    let (text, _usage) = auth.generate_content(&http, &model, payload).await.ok()?;
    let indices = parse_index_array(&text);
    let mut out = Vec::new();
    for n in indices.into_iter().take(limit) {
        if n >= 1 && n <= capped.len() {
            let mut d = order_to_destination(capped[n - 1]);
            if let Some(obj) = d.as_object_mut() {
                obj.insert("ai_matched".into(), json!(true));
            }
            out.push(d);
        }
    }
    Some(out)
}

/// Pull the first JSON integer array out of an LLM response (tolerates code fences).
fn parse_index_array(text: &str) -> Vec<usize> {
    if let (Some(s), Some(e)) = (text.find('['), text.rfind(']')) {
        if e > s {
            if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&text[s..=e]) {
                return arr.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect();
            }
        }
    }
    Vec::new()
}

/// Derive a city from the free-text customer name. The data has NO structured
/// city field; the city is embedded in `customerName` — usually after " in "
/// ("Get Impulse in Bad Oeynhausen"), otherwise the trailing token
/// ("Fitnesspark Stuhr" → Stuhr).
fn derive_city(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    if let Some(idx) = n.to_lowercase().rfind(" in ") {
        let city = n[idx + 4..].trim();
        if !city.is_empty() {
            return Some(city.to_string());
        }
    }
    n.split_whitespace().last().map(|w| w.to_string())
}

fn order_to_destination(o: &Value) -> Value {
    let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = o.get("customerName").and_then(|v| v.as_str()).unwrap_or("");
    json!({
        "purpose_ref": format!("order:{id}"),
        "label": name,
        "order_number": o.get("order_number").cloned().unwrap_or(Value::Null),
        "address": Value::Null,
        "city": derive_city(name),
        "lat": Value::Null,
        "lng": Value::Null,
        "kind": "ticket",
    })
}

/// GET /api/trips/destinations — the trip-mode console data source.
///   * `cities`: OPEN tickets grouped by a city derived from `customerName`
///     (busiest first) — the city buttons.
///   * `results`: tickets for the typed/dictated query (fuzzy substring over
///     ALL open tickets: name/order no./serial/ticket no.) OR for a selected
///     `city`. Each result's `purpose_ref` points to the order → verifiable
///     Geschäftspartner.
/// The order table is small (per-shop), so we fetch the open set once and group
/// in Rust (the city is not a queryable column).
pub async fn destinations(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DestinationQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as usize;

    let orders: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, customerName, order_number, serialNumber, ticketNumber, status \
             FROM order WHERE status IS NONE OR status NOT IN \
             ['done','closed','cancelled','delivered','repaired','shipped'] LIMIT 2000",
        )
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    // Cities grouped from the derived city
    let mut city_counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for o in &orders {
        if let Some(city) = o.get("customerName").and_then(|v| v.as_str()).and_then(derive_city) {
            *city_counts.entry(city).or_insert(0) += 1;
        }
    }
    let mut cities: Vec<Value> = city_counts
        .into_iter()
        .map(|(city, count)| json!({ "city": city, "count": count }))
        .collect();
    cities.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    cities.truncate(30);

    let lc = |o: &Value, k: &str| {
        o.get(k).and_then(|v| v.as_str()).map(|s| s.to_lowercase()).unwrap_or_default()
    };

    let results: Vec<Value> = if q.ai == Some(true) {
        // Explicit Gemini smart match (only on user request — cost-controlled)
        match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(query) => gemini_match_tickets(&orders, query, limit).await.unwrap_or_default(),
            None => Vec::new(),
        }
    } else if let Some(query) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let ql = query.to_lowercase();
        orders
            .iter()
            .filter(|o| {
                lc(o, "customerName").contains(&ql)
                    || lc(o, "order_number").contains(&ql)
                    || lc(o, "serialNumber").contains(&ql)
                    || lc(o, "ticketNumber").contains(&ql)
            })
            .take(limit)
            .map(order_to_destination)
            .collect()
    } else if let Some(city) = q.city.as_deref().filter(|s| !s.is_empty()) {
        orders
            .iter()
            .filter(|o| {
                o.get("customerName").and_then(|v| v.as_str()).and_then(derive_city).as_deref() == Some(city)
            })
            .take(limit)
            .map(order_to_destination)
            .collect()
    } else {
        Vec::new()
    };

    Ok(Json(json!({ "cities": cities, "results": results })))
}

// ─── GoBD seal verification ──────────────────────────────────────────────────

/// GET /api/trips/:id/verify — recompute the canonical `fahrtenbuch:v2` hash
/// from the STORED aggregate and confirm it matches the sealed value. Lets an
/// auditor independently verify that the closed entry was not altered after
/// sealing, and surfaces the evidence chain (Hedera anchor + odometer-photo CAS
/// refs + purpose reference) — the actionable "links to all checks". The
/// canonical builder is shared with `seal_trip()` so write and verify can't drift.
pub async fn verify_trip(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let trip: Option<Value> = state.db.select(("trip", id.as_str())).await.map_err(db_err)?;
    let Some(trip) = trip else {
        return Err((StatusCode::NOT_FOUND, format!("Trip '{id}' not found")));
    };

    let f = |k: &str| trip.get(k).and_then(|v| v.as_f64());
    let s = |k: &str| trip.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

    let stored_hash = s("seal_hash");
    let sealed = !stored_hash.is_empty();

    // Recompute under the version the trip was SEALED with (legacy trips have no
    // stored version → v2; newer trips seal v3, which folds in the vehicle plate).
    let version = {
        let v = s("seal_canonical_version");
        if v.is_empty() { "fahrtenbuch:v2".to_string() } else { v }
    };
    let canonical = crate::services::cell_resolver::fahrtenbuch_canonical(
        &version,
        &trip,
        &id,
        f("computed_distance_km"),
        f("odometer_gap_km"),
    );
    let recomputed_hash = hex::encode(Sha256::digest(canonical.as_bytes()));
    let hash_matches = sealed && recomputed_hash == stored_hash;

    // Latest Hedera anchor (each re-resolution appends a seal version).
    let seals = trip.get("hedera_seals").and_then(|v| v.as_array());
    let latest = seals.and_then(|a| a.last());
    let sequence = latest.and_then(|x| x.get("hedera_sequence")).cloned().unwrap_or(Value::Null);
    let timestamp = latest.and_then(|x| x.get("hedera_timestamp")).cloned().unwrap_or(Value::Null);

    Ok(Json(json!({
        "trip_id": id,
        "sealed": sealed,
        "hash_matches": hash_matches,
        "stored_hash": stored_hash,
        "recomputed_hash": recomputed_hash,
        "canonical_version": version,
        "seal_versions": seals.map(|a| a.len()).unwrap_or(0),
        "hedera": {
            "anchored": !sequence.is_null(),
            "sequence": sequence,
            "timestamp": timestamp,
        },
        "odometer_gap_km": f("odometer_gap_km"),
        "evidence": {
            "start_odometer_photo": trip.get("start_odometer_photo").cloned().unwrap_or(Value::Null),
            "end_odometer_photo": trip.get("end_odometer_photo").cloned().unwrap_or(Value::Null),
            "purpose": trip.get("purpose").cloned().unwrap_or(Value::Null),
            "purpose_ref": trip.get("purpose_ref").cloned().unwrap_or(Value::Null),
            "purpose_label": trip.get("purpose_label").cloned().unwrap_or(Value::Null),
            "purpose_declared_at": trip.get("purpose_declared_at").cloned().unwrap_or(Value::Null),
            "vehicle_id": trip.get("vehicle_id").cloned().unwrap_or(Value::Null),
            "vehicle_plate": trip.get("vehicle_plate").cloned().unwrap_or(Value::Null),
        },
    })))
}

// ─── Cell tower cache (on-device resolution) ─────────────────────────────────

#[derive(Deserialize)]
pub struct CellCacheQuery {
    /// Only towers updated/added after this RFC3339 timestamp (delta sync)
    pub since: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/cells/cache — download the resolved cell_tower cache so the PDA
/// can resolve known towers ON DEVICE (coordinates of known cells then never
/// leave the phone — the end-state of the privacy architecture). Towers are
/// mast positions, not personal data. Keyed `mcc-mnc-tac-cid`.
pub async fn cell_cache(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CellCacheQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let limit = q.limit.unwrap_or(5000).clamp(1, 50000);
    let query = if q.since.is_some() {
        "SELECT record::id(id) AS key, mcc, mnc, tac, cid, lat, lng, range_m \
         FROM cell_tower WHERE resolved_at > $since LIMIT $lim"
    } else {
        "SELECT record::id(id) AS key, mcc, mnc, tac, cid, lat, lng, range_m \
         FROM cell_tower LIMIT $lim"
    };
    let mut stmt = state.db.query(query).bind(("lim", limit));
    if let Some(since) = q.since {
        stmt = stmt.bind(("since", since));
    }
    let towers: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();
    Ok(Json(towers))
}

// ─── Fahrtenbuch monthly export ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ExportQuery {
    /// Month as YYYY-MM (default: current month)
    pub month: Option<String>,
    /// csv (default) | pdf
    pub format: Option<String>,
    pub device_id: Option<String>,
}

struct ExportRow {
    date: String,
    start_time: String,
    end_time: String,
    start_odo: String,
    end_odo: String,
    km_tacho: String,
    km_estimate: String,
    kennzeichen: String,
    purpose: String,
    driver: String,
    note: String,
    gap: String,
    seal: String,
}

/// GET /api/trips/export?month=YYYY-MM&format=csv|pdf
///
/// Finanzamt-oriented Fahrtenbuch export. Every row carries the GoBD seal
/// hash (+ Hedera sequence when anchored) so the auditor can verify that no
/// entry was altered after closing. Private trips appear with km only.
pub async fn export_trips(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ExportQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    use axum::response::IntoResponse;

    let month = q
        .month
        .unwrap_or_else(|| Utc::now().format("%Y-%m").to_string());
    if month.len() != 7 || !month.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Err((StatusCode::BAD_REQUEST, "month must be YYYY-MM".into()));
    }
    let from = format!("{month}-01");
    let to = format!("{month}-32"); // string compare upper bound within the month

    let mut conditions = vec!["started_at >= $from", "started_at < $to", "status != 'open'"];
    if q.device_id.is_some() {
        conditions.push("device_id = $dev");
    }
    let query = format!(
        "SELECT record::id(id) AS id, device_id, started_at, ended_at, purpose, \
         purpose_ref, purpose_label, purpose_declared_at, purpose_source, note, \
         vehicle_id, vehicle_plate, seal_canonical_version, \
         driver_user_id, start_odometer_km, start_odometer_source, end_odometer_km, \
         end_odometer_source, start_odometer_photo, end_odometer_photo, \
         computed_distance_km, odometer_gap_km, seal_hash, hedera_seals FROM trip \
         WHERE {} ORDER BY started_at ASC LIMIT 1000",
        conditions.join(" AND ")
    );

    let mut stmt = state
        .db
        .query(&query)
        .bind(("from", from))
        .bind(("to", to));
    if let Some(dev) = q.device_id {
        stmt = stmt.bind(("dev", dev));
    }
    let trips: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();

    let rows: Vec<ExportRow> = trips.iter().map(|t| build_row(t)).collect();

    match q.format.as_deref() {
        Some("pdf") => {
            let pdf = render_pdf(&month, &rows)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok((
                [
                    (axum::http::header::CONTENT_TYPE, "application/pdf".to_string()),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"fahrtenbuch_{month}.pdf\""),
                    ),
                ],
                pdf,
            )
                .into_response())
        }
        // GoBD-Z3 (Datenträgerüberlassung): the machine handover for a tax
        // audit — a ZIP with a GDPdU `index.xml` Beschreibungsstandard, a
        // headerless data file the auditor's tool (IDEA) ingests, and the
        // verification artifacts (per-trip seal hash recheck + Hedera anchor +
        // odometer-photo CAS refs). Sibling to the POS DSFinV-K export.
        Some("z3") => {
            let zip = build_z3_bundle(&month, &trips)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok((
                [
                    (axum::http::header::CONTENT_TYPE, "application/zip".to_string()),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"fahrtenbuch_z3_{month}.zip\""),
                    ),
                ],
                zip,
            )
                .into_response())
        }
        _ => {
            let csv = render_csv(&rows);
            Ok((
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "text/csv; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"fahrtenbuch_{month}.csv\""),
                    ),
                ],
                csv,
            )
                .into_response())
        }
    }
}

fn build_row(t: &Value) -> ExportRow {
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let f = |k: &str| t.get(k).and_then(|v| v.as_f64());

    let started = s("started_at");
    let ended = s("ended_at");
    let date = started.get(..10).unwrap_or("").to_string();
    let time_of = |ts: &str| ts.get(11..16).unwrap_or("").to_string();

    let km_tacho = match (f("start_odometer_km"), f("end_odometer_km")) {
        (Some(a), Some(b)) if b >= a => format!("{:.0}", b - a),
        _ => String::new(),
    };
    let hedera_seq = t
        .get("hedera_seals")
        .and_then(|v| v.as_array())
        .and_then(|a| a.last())
        .and_then(|s| s.get("hedera_sequence"))
        .and_then(|v| v.as_u64());
    let seal_hash = s("seal_hash");
    let seal = if seal_hash.is_empty() {
        "unversiegelt".to_string()
    } else {
        match hedera_seq {
            Some(seq) => format!("{}… (HCS #{seq})", &seal_hash[..12.min(seal_hash.len())]),
            None => format!("{}…", &seal_hash[..12.min(seal_hash.len())]),
        }
    };

    let purpose = match s("purpose").as_str() {
        "private" => "Privat".to_string(),
        "commute" => "Arbeitsweg".to_string(),
        _ => "Geschäftlich".to_string(),
    };

    // Ziel/Anlass = the GoBD "aufgesuchter Geschäftspartner": prefer the
    // declared destination label, append the free note if both exist.
    let label = s("purpose_label");
    let note = s("note").replace(';', ",");
    let destination = match (label.is_empty(), note.is_empty()) {
        (false, false) => format!("{label} — {note}"),
        (false, true) => label,
        (true, _) => note,
    };

    ExportRow {
        date,
        start_time: time_of(&started),
        end_time: time_of(&ended),
        start_odo: f("start_odometer_km").map(|v| format!("{v:.0}")).unwrap_or_default(),
        end_odo: f("end_odometer_km").map(|v| format!("{v:.0}")).unwrap_or_default(),
        km_tacho,
        km_estimate: f("computed_distance_km")
            .map(|v| format!("{v:.1}"))
            .unwrap_or_default(),
        kennzeichen: s("vehicle_plate"),
        purpose,
        driver: s("driver_user_id"),
        note: destination,
        gap: f("odometer_gap_km")
            .map(|v| format!("{v:+.1}"))
            .unwrap_or_default(),
        seal,
    }
}

fn render_csv(rows: &[ExportRow]) -> String {
    let mut out = String::from(
        "Datum;Beginn;Ende;Km-Stand Start;Km-Stand Ende;Km (Tacho);Km (geschätzt);\
         Kennzeichen;Zweck;Fahrer;Ziel/Anlass;Lücke (km);GoBD-Siegel\n",
    );
    for r in rows {
        out.push_str(&format!(
            "{};{};{};{};{};{};{};{};{};{};{};{};{}\n",
            r.date, r.start_time, r.end_time, r.start_odo, r.end_odo, r.km_tacho,
            r.km_estimate, r.kennzeichen, r.purpose, r.driver, r.note, r.gap, r.seal
        ));
    }
    out
}

/// Minimal tabular PDF (built-in Helvetica, fixed columns). Good enough for
/// audits; pretty layout can come later.
fn render_pdf(month: &str, rows: &[ExportRow]) -> Result<Vec<u8>, String> {
    use printpdf::*;

    let (doc, page1, layer1) = PdfDocument::new(
        format!("Fahrtenbuch {month}"),
        Mm(297.0),
        Mm(210.0), // A4 landscape
        "Layer 1",
    );
    let font = doc
        .add_builtin_font(BuiltinFont::Helvetica)
        .map_err(|e| e.to_string())?;
    let font_bold = doc
        .add_builtin_font(BuiltinFont::HelveticaBold)
        .map_err(|e| e.to_string())?;

    let headers = [
        "Datum", "Beginn", "Ende", "Km Start", "Km Ende", "Km", "Km (gesch.)",
        "Kennz.", "Zweck", "Fahrer", "Lücke", "Siegel",
    ];
    let col_x = [
        10.0, 32.0, 46.0, 59.0, 74.0, 90.0, 103.0, 122.0, 145.0, 170.0, 205.0, 222.0,
    ];

    let mut current_layer = doc.get_page(page1).get_layer(layer1);
    let mut y = 195.0;

    current_layer.use_text(
        format!("Fahrtenbuch {month} — GoBD-versiegelt (Hedera HCS)"),
        12.0,
        Mm(10.0),
        Mm(y),
        &font_bold,
    );
    y -= 8.0;
    for (i, h) in headers.iter().enumerate() {
        current_layer.use_text(*h, 8.0, Mm(col_x[i]), Mm(y), &font_bold);
    }
    y -= 5.0;

    for r in rows {
        if y < 12.0 {
            let (page, layer) = doc.add_page(Mm(297.0), Mm(210.0), "Layer");
            current_layer = doc.get_page(page).get_layer(layer);
            y = 195.0;
        }
        let cells = [
            r.date.as_str(), r.start_time.as_str(), r.end_time.as_str(),
            r.start_odo.as_str(), r.end_odo.as_str(), r.km_tacho.as_str(),
            r.km_estimate.as_str(), r.kennzeichen.as_str(), r.purpose.as_str(),
            r.driver.as_str(), r.gap.as_str(), r.seal.as_str(),
        ];
        for (i, c) in cells.iter().enumerate() {
            current_layer.use_text(*c, 7.0, Mm(col_x[i]), Mm(y), &font);
        }
        y -= 4.5;
    }

    let bytes = doc.save_to_bytes().map_err(|e| e.to_string())?;
    Ok(bytes)
}

// ─── GoBD-Z3 export bundle (Datenträgerüberlassung / GDPdU, for IDEA) ─────────
//
// The machine handover for a tax audit: a ZIP containing a GDPdU `index.xml`
// Beschreibungsstandard, a headerless data file (`fahrten.csv`) the auditor's
// tool ingests, the per-trip verification artifacts (`verification.json`), and a
// German `LIESMICH.txt`. The seal is over the canonical aggregate (not the raw
// points), so the bundle stays verifiable for the 10-year tax retention even
// after the DSGVO 14-day point pruning. Sibling to the POS DSFinV-K export.

/// Number of columns in `fahrten.csv` — kept in sync between the CSV writer and
/// the `index.xml` column descriptors by the `z3_tests` below.
const Z3_COL_COUNT: usize = 24;

/// Column descriptors for `index.xml`: (name, German description, kind, accuracy)
/// where kind: 0 = AlphaNumeric, 1 = Numeric, 2 = Date. ORDER MUST MATCH `z3_csv`.
fn z3_columns() -> Vec<(&'static str, &'static str, u8, u8)> {
    vec![
        ("trip_id", "Eindeutige Fahrt-Kennung (Datensatz-ID)", 0, 0),
        ("datum", "Datum der Fahrt", 2, 0),
        ("beginn", "Fahrtbeginn (ISO 8601, UTC)", 0, 0),
        ("ende", "Fahrtende (ISO 8601, UTC)", 0, 0),
        ("zweck_code", "Zweck-Schluessel: business | private | commute", 0, 0),
        ("zweck", "Zweck (Klartext)", 0, 0),
        ("geschaeftspartner", "Aufgesuchter Geschaeftspartner (verifizierbare Referenz, z. B. order:123); bei Privatfahrt leer", 0, 0),
        ("ziel_anlass", "Reiseziel / Anlass; bei Privatfahrt leer", 0, 0),
        ("zweck_quelle", "Herkunft der Zweckangabe: planned | text | voice | manual", 0, 0),
        ("zweck_erklaert_am", "Zeitpunkt der Zweckerklaerung (bei Fahrtbeginn deklariert)", 0, 0),
        ("fahrer", "Fahrer (Benutzer-ID)", 0, 0),
        ("kennzeichen", "Amtliches Kennzeichen des Fahrzeugs", 0, 0),
        ("km_start", "Kilometerstand bei Fahrtbeginn (Tacho)", 1, 0),
        ("km_ende", "Kilometerstand bei Fahrtende (Tacho)", 1, 0),
        ("km_tacho", "Gefahrene Kilometer laut Tacho (Ende minus Start)", 1, 0),
        ("km_geschaetzt", "Gefahrene Kilometer, geschaetzt aus dem Track (nur Hinweis)", 1, 1),
        ("luecke_km", "Differenz Tacho zu Vorfahrt (Lueckenkontrolle der Tacho-Kette)", 1, 1),
        ("siegel_hash", "GoBD-Siegel: SHA-256 ueber den kanonischen Aggregat-Datensatz (fahrtenbuch:v2)", 0, 0),
        ("siegel_version", "Anzahl Siegel-Versionen (Re-Versiegelungen werden angehaengt, nie ueberschrieben)", 1, 0),
        ("hedera_sequence", "Hedera-HCS-Sequenznummer des Ankers (sofern verankert)", 0, 0),
        ("hedera_timestamp", "Hedera-HCS-Konsens-Zeitstempel", 0, 0),
        ("beleg_km_start", "Beleg Anfangs-Kilometerstand (Foto, CAS-Referenz)", 0, 0),
        ("beleg_km_ende", "Beleg End-Kilometerstand (Foto, CAS-Referenz)", 0, 0),
        ("integritaet", "Integritaetspruefung beim Export: ja (Hash stimmt) / nein / n/a (unversiegelt)", 0, 0),
    ]
}

fn build_z3_bundle(month: &str, trips: &[Value]) -> Result<Vec<u8>, String> {
    let entries: Vec<(&str, Vec<u8>)> = vec![
        ("index.xml", z3_index_xml(month).into_bytes()),
        ("fahrten.csv", z3_csv(trips).into_bytes()),
        ("verification.json", z3_verification_json(month, trips).into_bytes()),
        ("LIESMICH.txt", z3_readme(month).into_bytes()),
    ];
    Ok(z3_zip(&entries))
}

fn z3_txt(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn z3_num(v: Option<f64>, decimals: usize) -> String {
    match v {
        Some(x) => format!("{:.*}", decimals, x).replace('.', ","),
        None => String::new(),
    }
}

fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn month_last_day(month: &str) -> String {
    let mut it = month.split('-');
    let y: i32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(2026);
    let m: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    let (ny, nm) = if m >= 12 { (y + 1, 1) } else { (y, m + 1) };
    chrono::NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("{month}-28"))
}

/// One headerless `;`-delimited record per trip; columns per `z3_columns()`.
fn z3_csv(trips: &[Value]) -> String {
    let mut out = String::new();
    for t in trips {
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let f = |k: &str| t.get(k).and_then(|v| v.as_f64());

        let id = s("id");
        let purpose = s("purpose");
        let is_private = purpose == "private";
        let started = s("started_at");
        let datum = started.get(..10).unwrap_or("").to_string();
        let zweck = match purpose.as_str() {
            "private" => "Privat",
            "commute" => "Arbeitsweg",
            _ => "Geschäftlich",
        };
        // Privatfahrt: km only — never the "where/who" (DSGVO mandatory). The
        // upload handler already strips ref/label server-side; blank here too.
        let partner = if is_private { String::new() } else { s("purpose_ref") };
        let ziel = if is_private {
            String::new()
        } else {
            let label = s("purpose_label");
            let note = s("note");
            match (label.is_empty(), note.is_empty()) {
                (false, false) => format!("{label} — {note}"),
                (false, true) => label,
                (true, _) => note,
            }
        };
        let km_tacho = match (f("start_odometer_km"), f("end_odometer_km")) {
            (Some(a), Some(b)) if b >= a => Some(b - a),
            _ => None,
        };

        let seals = t.get("hedera_seals").and_then(|v| v.as_array());
        let seal_versions = seals.map(|a| a.len()).unwrap_or(0);
        let latest = seals.and_then(|a| a.last());
        let hseq = match latest.and_then(|x| x.get("hedera_sequence")) {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(st)) => st.clone(),
            _ => String::new(),
        };
        let hts = latest
            .and_then(|x| x.get("hedera_timestamp"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let seal_hash = s("seal_hash");
        // Recompute the canonical hash at export time to prove integrity in the
        // file itself (shares the single-source-of-truth canonical builder).
        let integ = if seal_hash.is_empty() {
            "n/a".to_string()
        } else {
            let ver = {
                let v = s("seal_canonical_version");
                if v.is_empty() { "fahrtenbuch:v2".to_string() } else { v }
            };
            let canonical = crate::services::cell_resolver::fahrtenbuch_canonical(
                &ver,
                t,
                &id,
                f("computed_distance_km"),
                f("odometer_gap_km"),
            );
            let recomputed = hex::encode(Sha256::digest(canonical.as_bytes()));
            if recomputed == seal_hash { "ja".to_string() } else { "nein".to_string() }
        };

        let fields = [
            z3_txt(&id),
            z3_txt(&datum),
            z3_txt(&started),
            z3_txt(&s("ended_at")),
            z3_txt(&purpose),
            z3_txt(zweck),
            z3_txt(&partner),
            z3_txt(&ziel),
            z3_txt(&if is_private { String::new() } else { s("purpose_source") }),
            z3_txt(&if is_private { String::new() } else { s("purpose_declared_at") }),
            z3_txt(&s("driver_user_id")),
            z3_txt(&s("vehicle_plate")),
            z3_num(f("start_odometer_km"), 0),
            z3_num(f("end_odometer_km"), 0),
            z3_num(km_tacho, 0),
            z3_num(f("computed_distance_km"), 1),
            z3_num(f("odometer_gap_km"), 1),
            z3_txt(&seal_hash),
            seal_versions.to_string(),
            z3_txt(&hseq),
            z3_txt(&hts),
            z3_txt(&s("start_odometer_photo")),
            z3_txt(&s("end_odometer_photo")),
            z3_txt(&integ),
        ];
        debug_assert_eq!(fields.len(), Z3_COL_COUNT);
        out.push_str(&fields.join(";"));
        out.push_str("\r\n");
    }
    out
}

fn z3_index_xml(month: &str) -> String {
    let from = format!("{month}-01");
    let to = month_last_day(month);
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<!DOCTYPE DataSet SYSTEM \"gdpdu-01-09-2004.dtd\">\n");
    s.push_str("<DataSet>\n");
    s.push_str("  <Version>1.0</Version>\n");
    s.push_str("  <DataSupplier>\n");
    s.push_str("    <Name>9eck.com WMS - Elektronisches Fahrtenbuch</Name>\n");
    s.push_str("    <Location>9eck.com</Location>\n");
    s.push_str("    <Comment>GoBD-konformer Z3-Export (Datentraegerueberlassung) zur maschinellen Auswertung (z. B. IDEA).</Comment>\n");
    s.push_str("  </DataSupplier>\n");
    s.push_str("  <Media>\n");
    s.push_str(&format!("    <Name>Fahrtenbuch {month}</Name>\n"));
    s.push_str("    <Table>\n");
    s.push_str("      <URL><File>fahrten.csv</File></URL>\n");
    s.push_str("      <Name>Fahrten</Name>\n");
    s.push_str(&format!(
        "      <Description>Elektronisches Fahrtenbuch {month} - versiegelte Einzelfahrten (GoBD, unveraenderbar).</Description>\n"
    ));
    s.push_str(&format!(
        "      <Validity><Range><From>{from}</From><To>{to}</To></Range><Format>YYYY-MM-DD</Format></Validity>\n"
    ));
    s.push_str("      <DecimalSymbol>,</DecimalSymbol>\n");
    s.push_str("      <DigitGroupingSymbol>.</DigitGroupingSymbol>\n");
    s.push_str("      <VariableLength>\n");
    s.push_str("        <ColumnDelimiter>;</ColumnDelimiter>\n");
    s.push_str("        <RecordDelimiter>&#13;&#10;</RecordDelimiter>\n");
    s.push_str("        <TextEncapsulator>\"</TextEncapsulator>\n");
    for (name, desc, kind, acc) in z3_columns() {
        let typ = match kind {
            2 => "<Date><Format>YYYY-MM-DD</Format></Date>".to_string(),
            1 => format!("<Numeric><Accuracy>{acc}</Accuracy></Numeric>"),
            _ => "<AlphaNumeric/>".to_string(),
        };
        s.push_str("        <VariableColumn>\n");
        s.push_str(&format!("          <Name>{}</Name>\n", xml_esc(name)));
        s.push_str(&format!("          <Description>{}</Description>\n", xml_esc(desc)));
        s.push_str(&format!("          {typ}\n"));
        s.push_str("        </VariableColumn>\n");
    }
    s.push_str("      </VariableLength>\n");
    s.push_str("    </Table>\n");
    s.push_str("  </Media>\n");
    s.push_str("</DataSet>\n");
    s
}

fn z3_verification_json(month: &str, trips: &[Value]) -> String {
    let mut entries = Vec::new();
    let mut sealed_count = 0usize;
    for t in trips {
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let f = |k: &str| t.get(k).and_then(|v| v.as_f64());
        let id = s("id");
        let seal_hash = s("seal_hash");
        let sealed = !seal_hash.is_empty();
        if sealed {
            sealed_count += 1;
        }
        let canonical_version = {
            let v = s("seal_canonical_version");
            if v.is_empty() { "fahrtenbuch:v2".to_string() } else { v }
        };
        let hash_matches = if sealed {
            let canonical = crate::services::cell_resolver::fahrtenbuch_canonical(
                &canonical_version,
                t,
                &id,
                f("computed_distance_km"),
                f("odometer_gap_km"),
            );
            Some(hex::encode(Sha256::digest(canonical.as_bytes())) == seal_hash)
        } else {
            None
        };
        let seals = t.get("hedera_seals").and_then(|v| v.as_array());
        let latest = seals.and_then(|a| a.last());
        entries.push(json!({
            "trip_id": id,
            "sealed": sealed,
            "hash_matches": hash_matches,
            "canonical_version": canonical_version,
            "seal_hash": seal_hash,
            "seal_versions": seals.map(|a| a.len()).unwrap_or(0),
            "hedera_sequence": latest.and_then(|x| x.get("hedera_sequence")).cloned().unwrap_or(Value::Null),
            "hedera_timestamp": latest.and_then(|x| x.get("hedera_timestamp")).cloned().unwrap_or(Value::Null),
            "vehicle_id": t.get("vehicle_id").cloned().unwrap_or(Value::Null),
            "vehicle_plate": t.get("vehicle_plate").cloned().unwrap_or(Value::Null),
            "start_odometer_photo": t.get("start_odometer_photo").cloned().unwrap_or(Value::Null),
            "end_odometer_photo": t.get("end_odometer_photo").cloned().unwrap_or(Value::Null),
        }));
    }
    let doc = json!({
        "format": "9eck.fahrtenbuch.z3",
        "current_canonical_version": crate::services::cell_resolver::FAHRTENBUCH_CANONICAL_VERSION,
        "month": month,
        "generated_at": Utc::now().to_rfc3339(),
        "trip_count": trips.len(),
        "sealed_count": sealed_count,
        "note": "hash_matches recomputes SHA-256 over the canonical aggregate and compares it to the stored seal. Independently re-verifiable per trip via GET /api/trips/:id/verify and the Hedera HCS anchor. Raw GPS/cell points are intentionally absent (DSGVO 14-day pruning); the seal covers the aggregate, so it stays verifiable across the 10-year tax retention.",
        "trips": entries,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

fn z3_readme(month: &str) -> String {
    let mut s = String::new();
    s.push_str("Elektronisches Fahrtenbuch — GoBD-Z3-Export (Datenträgerüberlassung)\n");
    s.push_str(&format!("Zeitraum: {month}\n"));
    s.push_str("Zeichensatz: UTF-8\n\n");
    s.push_str("Inhalt dieses Archivs:\n");
    s.push_str("- index.xml          GDPdU-Beschreibungsstandard (Struktur der Datendatei, u. a. für IDEA).\n");
    s.push_str("- fahrten.csv        Datendatei, ein Datensatz je Fahrt, OHNE Kopfzeile.\n");
    s.push_str("                     Spaltentrenner ';', Textbegrenzer '\"', Dezimaltrennzeichen ','.\n");
    s.push_str("- verification.json  Prüfartefakte je Fahrt (Siegel, Hedera-Anker, Beleg-Referenzen).\n\n");
    s.push_str("Unveränderbarkeit (GoBD):\n");
    s.push_str("Jede abgeschlossene Fahrt ist über ihren Aggregat-Datensatz mit SHA-256 versiegelt\n");
    s.push_str("(Kanonik-Version 'fahrtenbuch:v2'). Korrekturen werden als neue Siegel-Version\n");
    s.push_str("angehängt, nie überschrieben. Das Siegel umfasst NICHT die Rohpunkte (GPS/Funkzellen);\n");
    s.push_str("diese werden aus Datenschutzgründen (DSGVO) nach 14 Tagen gelöscht. Da das Siegel über\n");
    s.push_str("das Aggregat gebildet wird, bleibt es über die 10-jährige steuerliche Aufbewahrung\n");
    s.push_str("hinweg prüfbar.\n");
    s.push_str("Unabhängige Prüfung je Fahrt: GET /api/trips/<id>/verify (Hash-Neuberechnung + Hedera).\n");
    s
}

// ── Minimal STORED (uncompressed) ZIP writer — no external dependency. ───────
// Deterministic (fixed DOS timestamp) and auditor-inspectable; the data files
// carry their own timestamps. Sufficient for a handful of small text entries.

fn z3_le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn z3_le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn z3_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let m = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & m);
        }
    }
    !crc
}

fn z3_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    const DOS_TIME: u16 = 0; // 00:00:00
    const DOS_DATE: u16 = 0x21; // 1980-01-01
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut meta: Vec<(u32, u32)> = Vec::new(); // (crc, local header offset)

    for (name, data) in entries {
        let crc = z3_crc32(data);
        let offset = out.len() as u32;
        meta.push((crc, offset));
        z3_le32(&mut out, 0x0403_4b50); // local file header signature
        z3_le16(&mut out, 20); // version needed
        z3_le16(&mut out, 0); // flags
        z3_le16(&mut out, 0); // method = stored
        z3_le16(&mut out, DOS_TIME);
        z3_le16(&mut out, DOS_DATE);
        z3_le32(&mut out, crc);
        z3_le32(&mut out, data.len() as u32); // compressed size
        z3_le32(&mut out, data.len() as u32); // uncompressed size
        z3_le16(&mut out, name.len() as u16);
        z3_le16(&mut out, 0); // extra len
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
    }

    let cd_offset = out.len() as u32;
    for (i, (name, data)) in entries.iter().enumerate() {
        let (crc, local_offset) = meta[i];
        z3_le32(&mut central, 0x0201_4b50); // central dir header signature
        z3_le16(&mut central, 20); // version made by
        z3_le16(&mut central, 20); // version needed
        z3_le16(&mut central, 0); // flags
        z3_le16(&mut central, 0); // method
        z3_le16(&mut central, DOS_TIME);
        z3_le16(&mut central, DOS_DATE);
        z3_le32(&mut central, crc);
        z3_le32(&mut central, data.len() as u32);
        z3_le32(&mut central, data.len() as u32);
        z3_le16(&mut central, name.len() as u16);
        z3_le16(&mut central, 0); // extra len
        z3_le16(&mut central, 0); // comment len
        z3_le16(&mut central, 0); // disk number start
        z3_le16(&mut central, 0); // internal attrs
        z3_le32(&mut central, 0); // external attrs
        z3_le32(&mut central, local_offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);

    // End of central directory record
    z3_le32(&mut out, 0x0605_4b50);
    z3_le16(&mut out, 0); // disk number
    z3_le16(&mut out, 0); // cd start disk
    z3_le16(&mut out, entries.len() as u16);
    z3_le16(&mut out, entries.len() as u16);
    z3_le32(&mut out, cd_size);
    z3_le32(&mut out, cd_offset);
    z3_le16(&mut out, 0); // comment len
    out
}

#[cfg(test)]
mod z3_tests {
    use super::*;

    fn sample_trip() -> Value {
        json!({
            "id": "trip123",
            "device_id": "devA",
            "driver_user_id": "user1",
            "started_at": "2026-06-02T08:00:00Z",
            "ended_at": "2026-06-02T08:42:00Z",
            "purpose": "business",
            "purpose_ref": "order:4711",
            "purpose_label": "Kunde Müller GmbH",
            "purpose_declared_at": "2026-06-02T08:00:00Z",
            "purpose_source": "planned",
            "vehicle_id": "veh-1",
            "vehicle_plate": "B X 123",
            "seal_canonical_version": "fahrtenbuch:v3",
            "note": "Reparatur vor Ort",
            "start_odometer_km": 12000.0,
            "start_odometer_source": "photo",
            "end_odometer_km": 12035.0,
            "end_odometer_source": "photo",
            "computed_distance_km": 31.4,
            "odometer_gap_km": 0.0,
            "seal_hash": "",
            "hedera_seals": []
        })
    }

    #[test]
    fn csv_row_has_declared_column_count() {
        let csv = z3_csv(std::slice::from_ref(&sample_trip()));
        let line = csv.lines().next().unwrap();
        // the sample fields contain no ';' so a plain split is exact here
        assert_eq!(line.split(';').count(), Z3_COL_COUNT);
        assert!(csv.ends_with("\r\n"));
        assert!(csv.contains("\"B X 123\""), "Kennzeichen column must be populated");
    }

    #[test]
    fn index_xml_column_count_matches_csv() {
        let xml = z3_index_xml("2026-06");
        assert_eq!(xml.matches("<VariableColumn>").count(), Z3_COL_COUNT);
        assert!(xml.contains("<File>fahrten.csv</File>"));
        assert!(xml.contains("<To>2026-06-30</To>"));
    }

    #[test]
    fn private_trip_omits_destination() {
        let mut t = sample_trip();
        t["purpose"] = json!("private");
        let csv = z3_csv(std::slice::from_ref(&t));
        assert!(!csv.contains("order:4711"));
        assert!(!csv.contains("Müller"));
    }

    #[test]
    fn integrity_is_ja_when_seal_matches() {
        let mut t = sample_trip(); // seal_canonical_version = fahrtenbuch:v3
        let canonical = crate::services::cell_resolver::fahrtenbuch_canonical(
            "fahrtenbuch:v3",
            &t,
            "trip123",
            t.get("computed_distance_km").and_then(|v| v.as_f64()),
            t.get("odometer_gap_km").and_then(|v| v.as_f64()),
        );
        t["seal_hash"] = json!(hex::encode(Sha256::digest(canonical.as_bytes())));
        let csv = z3_csv(std::slice::from_ref(&t));
        assert!(csv.contains("\"ja\""));
    }

    #[test]
    fn zip_has_valid_signatures_and_entries() {
        let zip = build_z3_bundle("2026-06", &[sample_trip()]).unwrap();
        assert_eq!(&zip[0..4], &[0x50, 0x4b, 0x03, 0x04]); // local file header
        let eocd = &zip[zip.len() - 22..]; // no comment → EOCD is the last 22 bytes
        assert_eq!(&eocd[0..4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 4); // 4 entries
        assert!(zip.windows(9).any(|w| w == b"index.xml"));
        assert!(zip.windows(11).any(|w| w == b"fahrten.csv"));
    }
}
