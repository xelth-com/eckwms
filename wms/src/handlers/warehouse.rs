use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ─── Warehouses / Locations ──────────────────────────────────────────────────

/// GET /api/warehouse — list warehouses/locations, ordered by complete_name.
/// Excludes learn-by-scan bins (`source = 'scan'`): those are shelves managed via
/// the Stocktake screen, not top-level warehouses — showing them here as cards
/// (with "RACKS 0") is misleading.
pub async fn list(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let locs: Vec<Value> = state
        .db
        .query("SELECT * FROM location WHERE source != 'scan' ORDER BY complete_name ASC")
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(locs))
}

/// POST /api/warehouse — create a warehouse/location
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let created: Option<Value> = state
        .db
        .create("location")
        .content(payload)
        .await
        .map_err(db_err)?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// GET /api/warehouse/:id — get a single warehouse with its racks
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let loc = state
        .get_synced_entity("location", &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Warehouse '{id}' not found")))?;

    // Fetch racks belonging to this warehouse
    let racks: Vec<Value> = state
        .db
        .query("SELECT * FROM rack WHERE warehouse_id = $wid ORDER BY name ASC")
        .bind(("wid", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let mut result = loc;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("racks".to_string(), json!(racks));
    }

    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct BinQuery {
    pub barcode: String,
}

/// GET /api/warehouse/bin?barcode=... — a shelf's current contents, for the
/// stocktake screen. Returns the `location` (may be absent until first scan)
/// and the quant lines sitting on that shelf.
pub async fn bin_contents(
    State(state): State<Arc<AppState>>,
    Query(q): Query<BinQuery>,
) -> ApiResult<Json<Value>> {
    let barcode = q.barcode.trim().to_string();
    let loc: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, barcode, complete_name, warehouse_code \
             FROM location WHERE barcode = $b LIMIT 1",
        )
        .bind(("b", barcode.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let lines: Vec<Value> = state
        .db
        .query(
            "SELECT product_id, default_code, product_name, qty, lot FROM quant \
             WHERE shelf_barcode = $b AND qty != NONE ORDER BY default_code ASC",
        )
        .bind(("b", barcode))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    Ok(Json(json!({ "location": loc.into_iter().next(), "lines": lines })))
}

#[derive(Deserialize)]
pub struct ReconcileQuery {
    pub warehouse: Option<String>,
}

/// GET /api/warehouse/inventory — a flat inventory table: one row per counted
/// (shelf, part), each carrying time · location · part · IST (counted) · SOLL
/// (Exact expected). Newest first. This is the "time · place · how-many-of-what
/// (is/should-be), all on one line" view.
pub async fn inventory(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    use std::collections::HashMap;

    let quants: Vec<Value> = state
        .db
        .query(
            "SELECT default_code, product_name, shelf_barcode, warehouse_code, qty, updated_at \
             FROM quant WHERE qty != NONE AND default_code != NONE ORDER BY updated_at DESC",
        )
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    // SOLL from Exact (tolerant of a missing table), keyed by normalized wh + code.
    let soll_rows: Vec<Value> = match state
        .db
        .query("SELECT item_code, warehouse_code, in_stock FROM stock_position WHERE item_code != NONE")
        .await
    {
        Ok(mut r) => r.take(0).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let mut soll: HashMap<String, f64> = HashMap::new();
    for r in &soll_rows {
        if let Some(code) = r.get("item_code").and_then(|v| v.as_str()) {
            let wh = r.get("warehouse_code").and_then(|v| v.as_str()).unwrap_or("");
            soll.insert(
                format!("{}|{}", norm_wh(wh), code),
                r.get("in_stock").and_then(|v| v.as_f64()).unwrap_or(0.0),
            );
        }
    }

    let lines: Vec<Value> = quants
        .iter()
        .map(|r| {
            let code = r.get("default_code").and_then(|v| v.as_str()).unwrap_or("");
            let wh = r.get("warehouse_code").and_then(|v| v.as_str()).unwrap_or("");
            let ist = r.get("qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let s = *soll.get(&format!("{}|{}", norm_wh(wh), code)).unwrap_or(&0.0);
            json!({
                "time": r.get("updated_at"),
                "location": r.get("shelf_barcode"),
                "warehouse": wh,
                "default_code": code,
                "name": r.get("product_name"),
                "ist": ist,
                "soll": s,
                "diff": ist - s,
            })
        })
        .collect();

    Ok(Json(json!({ "count": lines.len(), "lines": lines })))
}

/// Normalize a warehouse code for cross-system matching (Exact "WH_008" ↔ our
/// "WH008"): keep alphanumerics only, uppercase.
fn norm_wh(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase()
}

/// GET /api/warehouse/reconcile?warehouse=WH008 — SOLL (scraped Exact Online
/// `stock_position.in_stock`, the "should-be") vs IST (counted 9eck bins), per
/// product for the warehouse. Exact warehouse codes are matched to ours by
/// normalization. Discrepancies are sorted to the top.
pub async fn reconcile(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReconcileQuery>,
) -> ApiResult<Json<Value>> {
    use std::collections::{BTreeSet, HashMap};
    let wh = q.warehouse.unwrap_or_else(|| "WH008".to_string());
    let wh_norm = norm_wh(&wh);

    // IST — counted per product in this warehouse.
    let ist_rows: Vec<Value> = state
        .db
        .query(
            "SELECT default_code, product_name, math::sum(qty) AS ist FROM quant \
             WHERE warehouse_code = $wh AND qty != NONE AND default_code != NONE \
             GROUP BY default_code, product_name",
        )
        .bind(("wh", wh.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    // SOLL — scraped Exact stock positions (filter by normalized warehouse).
    // Tolerate a missing `stock_position` table (a peer that hasn't received the
    // Exact data yet): treat it as an empty soll instead of a 500.
    let soll_rows: Vec<Value> = match state
        .db
        .query("SELECT item_code, warehouse_code, in_stock FROM stock_position WHERE item_code != NONE")
        .await
    {
        Ok(mut r) => r.take(0).unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let mut soll: HashMap<String, f64> = HashMap::new();
    for r in &soll_rows {
        let whc = r.get("warehouse_code").and_then(|v| v.as_str()).unwrap_or_default();
        if norm_wh(whc) != wh_norm {
            continue;
        }
        if let Some(code) = r.get("item_code").and_then(|v| v.as_str()) {
            soll.insert(code.to_string(), r.get("in_stock").and_then(|x| x.as_f64()).unwrap_or(0.0));
        }
    }
    let mut ist: HashMap<String, f64> = HashMap::new();
    let mut names: HashMap<String, Value> = HashMap::new();
    for r in &ist_rows {
        if let Some(code) = r.get("default_code").and_then(|v| v.as_str()) {
            ist.insert(code.to_string(), r.get("ist").and_then(|x| x.as_f64()).unwrap_or(0.0));
            if let Some(n) = r.get("product_name") {
                names.insert(code.to_string(), n.clone());
            }
        }
    }

    let mut codes: BTreeSet<String> = BTreeSet::new();
    codes.extend(soll.keys().cloned());
    codes.extend(ist.keys().cloned());

    // Names for codes only present as SOLL (never counted).
    let need: Vec<String> = codes.iter().filter(|c| !names.contains_key(*c)).cloned().collect();
    if !need.is_empty() {
        let name_rows: Vec<Value> = state
            .db
            .query("SELECT default_code, name FROM product WHERE default_code IN $codes")
            .bind(("codes", need))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?;
        for r in &name_rows {
            if let Some(c) = r.get("default_code").and_then(|v| v.as_str()) {
                if let Some(n) = r.get("name") {
                    names.insert(c.to_string(), n.clone());
                }
            }
        }
    }

    let (mut counted, mut matched, mut short, mut over, mut uncounted) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut lines: Vec<Value> = Vec::with_capacity(codes.len());
    for code in &codes {
        let s = *soll.get(code).unwrap_or(&0.0);
        let has_ist = ist.contains_key(code);
        let i = *ist.get(code).unwrap_or(&0.0);
        let diff = i - s;
        let status = if !has_ist {
            uncounted += 1;
            "uncounted"
        } else if diff.abs() < 1e-9 {
            matched += 1;
            "match"
        } else if diff < 0.0 {
            short += 1;
            "short"
        } else {
            over += 1;
            "over"
        };
        if has_ist {
            counted += 1;
        }
        lines.push(json!({
            "default_code": code,
            "name": names.get(code).cloned().unwrap_or(Value::Null),
            "soll": s,
            "ist": i,
            "diff": diff,
            "status": status,
        }));
    }
    // Discrepancies first, then by |diff| desc, then code.
    lines.sort_by(|a, b| {
        let rank = |x: &Value| if x.get("status").and_then(|v| v.as_str()) == Some("match") { 1 } else { 0 };
        let da = a.get("diff").and_then(|v| v.as_f64()).unwrap_or(0.0).abs();
        let dbb = b.get("diff").and_then(|v| v.as_f64()).unwrap_or(0.0).abs();
        rank(a)
            .cmp(&rank(b))
            .then(dbb.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                a.get("default_code").and_then(|v| v.as_str()).unwrap_or("")
                    .cmp(b.get("default_code").and_then(|v| v.as_str()).unwrap_or(""))
            })
    });

    Ok(Json(json!({
        "warehouse": wh,
        "summary": { "total": codes.len(), "counted": counted, "match": matched, "short": short, "over": over, "uncounted": uncounted },
        "lines": lines,
    })))
}

/// POST /api/warehouse/put-away — learn-by-scan bin placement. Resolves the
/// scanned part against the catalog (strict), resolves or mints the scanned
/// shelf `location`, and records the on-hand `quant` at that bin. Both writes
/// land on the mesh via the live-watcher.
pub async fn put_away(
    State(state): State<Arc<AppState>>,
    Json(body): Json<crate::services::binmap::PutAway>,
) -> ApiResult<Json<Value>> {
    let res = crate::services::binmap::put_away(&state, body)
        .await
        .map_err(|e| {
            let code = if e.starts_with("unknown part") {
                StatusCode::NOT_FOUND
            } else if e.starts_with("ambiguous") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, e)
        })?;
    Ok(Json(json!({ "ok": true, "placed": res })))
}

// ─── Racks ───────────────────────────────────────────────────────────────────

/// GET /api/warehouse/racks — list all racks
pub async fn list_racks(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let racks: Vec<Value> = state
        .db
        .query("SELECT * FROM rack ORDER BY name ASC")
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(racks))
}

/// POST /api/warehouse/racks — create or upsert a rack
pub async fn create_rack(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let created: Option<Value> = state
        .db
        .create("rack")
        .content(payload)
        .await
        .map_err(db_err)?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/warehouse/racks/:id — update a rack
pub async fn update_rack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    // put_synced_entity advances `_vclock`; existence check keeps the 404.
    if state
        .get_synced_entity("rack", &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, format!("Rack '{id}' not found")));
    }
    let updated = state
        .put_synced_entity("rack", &id, payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(updated))
}

/// DELETE /api/warehouse/racks/:id — delete a rack
pub async fn delete_rack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let deleted: Option<Value> = state
        .db
        .delete(("rack", &*id))
        .await
        .map_err(db_err)?;

    match deleted {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Rack '{id}' not found"))),
    }
}
