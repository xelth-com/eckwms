//! PDA-facing endpoints (eckwms-movFast Android client).
//!
//! Ported from the legacy eckwmsgo/eckwmsr servers. Request/response
//! contracts mirror the Android client's `ScanApiService.kt`: the PDA
//! decrypts SmartTag V2 QR codes locally and sends plaintext
//! `{prefix}-{uuid}` codes; raw EAN/SKU barcodes are resolved against
//! product/order/location/item tables; unknown barcodes create a stub
//! product (PDA is source of truth, same as legacy).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::Utc;
use eck_core::auth::Claims;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// In-memory msgId dedup with a 10-minute TTL (matches legacy IsDuplicate).
fn is_duplicate(msg_id: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    if msg_id.is_empty() {
        return false;
    }
    let map = SEEN.get_or_init(|| Mutex::new(HashMap::new()));
    let mut seen = map.lock().unwrap();
    let now = Instant::now();
    seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(600));
    seen.insert(msg_id.to_string(), now).is_some()
}

async fn device_status(state: &AppState, device_id: &str) -> Option<String> {
    let dev: Option<super::device::DeviceRecord> = state
        .db
        .select(("registered_device", device_id))
        .await
        .ok()
        .flatten();
    dev.filter(|d| d.deleted_at.is_none()).map(|d| d.status)
}

/// Reject scans/events from unknown or non-active devices.
pub(crate) async fn require_active_device(state: &AppState, device_id: &str) -> Result<(), (StatusCode, String)> {
    if device_id.is_empty() {
        return Ok(());
    }
    match device_status(state, device_id).await {
        Some(s) if s == "active" => Ok(()),
        Some(s) => Err((StatusCode::FORBIDDEN, format!("Device is {}", s))),
        None => Err((StatusCode::FORBIDDEN, "Device not registered".into())),
    }
}

fn qr_prefixes_from_env() -> Vec<String> {
    std::env::var("QR_PREFIXES")
        .unwrap_or_else(|_| "9eck.com/,xelth.com/".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn repair_order_prefix() -> String {
    std::env::var("REPAIR_ORDER_PREFIX").unwrap_or_else(|_| "REP-".into())
}

// ─── GET /api/status — device heartbeat ──────────────────────────────────────
//
// The PDA polls this to learn its own status (active/pending/blocked) and to
// receive rotating config: enc_key (SmartTag AES key), repair_order_prefix,
// qr_prefixes + qr_tenant_suffix (trusted-link anti-spoofing list).

pub async fn status(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let mut device_state = "active".to_string();

    if claims.auth_method == "ed25519_signature" {
        match device_status(&state, &claims.sub).await {
            Some(s) if s == "blocked" => {
                return Err((StatusCode::FORBIDDEN, "Device is blocked".into()))
            }
            Some(s) => {
                device_state = s;
                let _ = state
                    .db
                    .query("UPDATE registered_device SET last_seen_at = $now WHERE device_id = $id")
                    .bind(("id", claims.sub.clone()))
                    .bind(("now", Utc::now().to_rfc3339()))
                    .await;
            }
            None => return Err((StatusCode::FORBIDDEN, "Device not registered".into())),
        }
    }

    let enc_key = if device_state == "active" {
        std::env::var("ENC_KEY").ok().filter(|k| !k.is_empty())
    } else {
        None
    };

    Ok(Json(json!({
        "status": device_state,
        "server": "wms",
        "version": env!("CARGO_PKG_VERSION"),
        "instance_id": state.instance_id,
        // Mesh identity + tier — the device orders the eckN default nodes by
        // mod3(mesh_id) for its failover list (no single node is the default).
        "mesh_id": state.mesh_id,
        "tier": if std::env::var("ECK_LICENSE_TOKEN").ok().filter(|t| !t.is_empty()).is_some() { "paid" } else { "free" },
        "enc_key": enc_key,
        "repair_order_prefix": repair_order_prefix(),
        "qr_prefixes": qr_prefixes_from_env(),
        "qr_tenant_suffix": std::env::var("QR_TENANT_SUFFIX").unwrap_or_default(),
    })))
}

// ─── POST /api/scan — universal barcode entry point ──────────────────────────

#[derive(Deserialize)]
pub struct ScanRequest {
    #[serde(rename = "deviceId", default)]
    pub device_id: String,
    pub barcode: String,
    #[serde(rename = "type", default)]
    pub barcode_type: String,
    #[serde(default)]
    pub checksum: String,
    #[serde(rename = "msgId", default)]
    pub msg_id: String,
    #[serde(rename = "orderId", default)]
    pub order_id: Option<String>,
}

fn scan_response(
    rtype: &str,
    action: &str,
    message: String,
    data: Value,
    req: &ScanRequest,
) -> Value {
    json!({
        "type": rtype,
        "action": action,
        "message": message,
        "data": data,
        "checksum": req.checksum,
        "msgId": req.msg_id,
    })
}

/// Plaintext SmartTag route codes the PDA produces after local decryption:
/// `i-<uuid>` (item), `p-<uuid>` (place), `b-<uuid>` (box), `l-<uuid>` (label),
/// `company-/person-/opp-<uuid>` (CRM). Returns (table, response_type, uuid).
fn parse_typed_code(barcode: &str) -> Option<(&'static str, &'static str, String)> {
    let (prefix, id) = barcode.split_once('-')?;
    // UUID part: 36 chars with dashes re-joined
    let id = id.to_lowercase();
    let is_uuid = id.len() == 36
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && id.chars().filter(|c| *c == '-').count() == 4;
    if !is_uuid {
        return None;
    }
    let mapped = match prefix.to_lowercase().as_str() {
        "i" | "item" => ("item", "item"),
        "b" | "box" => ("item", "box"),
        "p" | "place" | "loc" => ("location", "place"),
        "l" | "label" => ("item", "label"),
        "company" => ("partner", "company"),
        "person" => ("partner", "person"),
        "opp" => ("partner", "opp"),
        _ => return None,
    };
    Some((mapped.0, mapped.1, id))
}

pub async fn handle_scan(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ScanRequest>,
) -> ApiResult<Json<Value>> {
    let barcode = body.barcode.trim().to_string();
    if barcode.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Empty barcode".into()));
    }

    if is_duplicate(&body.msg_id) {
        return Ok(Json(json!({
            "type": "duplicate",
            "action": "ignore",
            "message": "Message already processed",
            "msgId": body.msg_id,
            "duplicate": true,
            "checksum": body.checksum,
        })));
    }

    require_active_device(&state, &body.device_id).await?;

    // Audit log (fire-and-forget, schemaless)
    {
        let _ = state
            .db
            .query("CREATE scan_log SET device_id = $d, barcode = $b, msg_id = $m, order_id = $o, created_at = $now")
            .bind(("d", body.device_id.clone()))
            .bind(("b", barcode.clone()))
            .bind(("m", body.msg_id.clone()))
            .bind(("o", body.order_id.clone()))
            .bind(("now", Utc::now().to_rfc3339()))
            .await;
    }

    // 1. Typed smart code ({prefix}-{uuid}) — decrypted locally on the PDA
    if let Some((table, rtype, id)) = parse_typed_code(&barcode) {
        let found = state.get_synced_entity(table, &id).await.map_err(|e| db_err(e))?;

        if let Some(record) = found {
            let message = record
                .get("complete_name")
                .or_else(|| record.get("name"))
                .or_else(|| record.get("serial_number"))
                .and_then(|v| v.as_str())
                .unwrap_or(rtype)
                .to_string();
            return Ok(Json(scan_response(rtype, "found", message, record, &body)));
        }

        // PDA is source of truth: lazily create items and places
        let now = Utc::now().to_rfc3339();
        let stub = match table {
            "item" => Some(json!({
                "name": format!("Item {}", &id[..8]),
                "smart_code": barcode,
                "source_system": "pda_scan",
                "created_at": now, "updated_at": now,
            })),
            "location" => Some(json!({
                "name": format!("LOC-{}", &id[..8]),
                "complete_name": format!("LOC-{}", &id[..8]),
                "barcode": barcode,
                "usage": "internal",
                "active": true,
                "source_system": "pda_scan",
                "created_at": now, "updated_at": now,
            })),
            _ => None, // CRM entities are never auto-created from scans
        };

        if let Some(content) = stub {
            let created: Option<Value> = state
                .db
                .create((table, id.as_str()))
                .content(content)
                .await
                .map_err(db_err)?;
            let created = created.unwrap_or(Value::Null);
            let message = created
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Registered")
                .to_string();
            info!("Scan: lazily created {} {} for device {}", table, id, body.device_id);
            return Ok(Json(scan_response(rtype, "created", message, created, &body)));
        }

        return Ok(Json(scan_response(
            rtype,
            "unknown",
            format!("{} not found locally", rtype),
            Value::Null,
            &body,
        )));
    }

    // 2. Raw barcode lookups — collect candidates across entity types
    let products: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS id, * FROM product WHERE barcode = $b OR default_code = $b LIMIT 5")
        .bind(("b", barcode.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    let orders: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS id, * FROM order WHERE order_number = $b OR serial_number = $b LIMIT 5")
        .bind(("b", barcode.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    let locations: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS id, * FROM location WHERE barcode = $b LIMIT 2")
        .bind(("b", barcode.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    let mut candidates: Vec<(String, Value)> = Vec::new();
    for p in products {
        candidates.push(("product".into(), p));
    }
    for o in orders {
        candidates.push(("order".into(), o));
    }
    for l in locations {
        candidates.push(("place".into(), l));
    }

    match candidates.len() {
        1 => {
            let (rtype, record) = candidates.pop().unwrap();
            let message = record
                .get("name")
                .or_else(|| record.get("order_number"))
                .or_else(|| record.get("complete_name"))
                .and_then(|v| v.as_str())
                .unwrap_or(&rtype)
                .to_string();
            Ok(Json(scan_response(&rtype, "found", message, record, &body)))
        }
        0 => {
            // Nothing matched: register a stub product (legacy behavior)
            let now = Utc::now().to_rfc3339();
            let created: Option<Value> = state
                .db
                .create("product")
                .content(json!({
                    "name": format!("Item {}", barcode),
                    "barcode": barcode,
                    "default_code": barcode,
                    "active": true,
                    "type": "product",
                    "source_system": "pda_scan",
                    "created_at": now, "updated_at": now,
                }))
                .await
                .map_err(db_err)?;
            let created = created.unwrap_or(Value::Null);
            let message = format!("Item {} registered", barcode);
            info!("Scan: created stub product for unknown barcode {}", barcode);
            Ok(Json(scan_response("product", "created", message, created, &body)))
        }
        _ => {
            // Collision — the PDA renders a picker from data.candidates
            let cand_json: Vec<Value> = candidates
                .iter()
                .map(|(t, r)| {
                    json!({
                        "title": r.get("name")
                            .or_else(|| r.get("order_number"))
                            .or_else(|| r.get("complete_name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown"),
                        "type": t,
                        "id": r.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "barcode": r.get("barcode")
                            .or_else(|| r.get("order_number"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                    })
                })
                .collect();
            Ok(Json(json!({
                "type": "ambiguous",
                "action": "interaction",
                "message": format!("Multiple matches for {}. Select one:", barcode),
                "data": { "candidates": cand_json },
                "checksum": body.checksum,
                "msgId": body.msg_id,
            })))
        }
    }
}

// ─── POST /api/repair/event — repair-mode workflow events ────────────────────
//
// `device_bound` auto-creates a pending repair order for the scanned serial
// (REP-YYYYMMDD-XXXX) unless an open one already exists. Every event is kept
// in `repair_event` for the audit trail.

#[derive(Deserialize)]
pub struct RepairEventRequest {
    #[serde(default)]
    pub source_device_id: String,
    pub target_device_id: String,
    pub event_type: String,
    #[serde(default)]
    pub data: String,
    #[serde(default)]
    pub acting_user_id: Option<String>,
    #[serde(default)]
    pub owner_user_id: Option<String>,
}

pub async fn repair_event(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RepairEventRequest>,
) -> ApiResult<Json<Value>> {
    require_active_device(&state, &body.source_device_id).await?;

    let now = Utc::now().to_rfc3339();
    let _ = state
        .db
        .query(
            "CREATE repair_event SET source_device_id = $src, target_device_id = $tgt, \
             event_type = $et, data = $data, acting_user_id = $au, owner_user_id = $ou, created_at = $now",
        )
        .bind(("src", body.source_device_id.clone()))
        .bind(("tgt", body.target_device_id.clone()))
        .bind(("et", body.event_type.clone()))
        .bind(("data", body.data.clone()))
        .bind(("au", body.acting_user_id.clone()))
        .bind(("ou", body.owner_user_id.clone()))
        .await
        .map_err(db_err)?;

    if body.event_type == "device_bound" {
        // Reuse an open repair order for this serial if one exists
        let open: Vec<Value> = state
            .db
            .query(
                "SELECT record::id(id) AS id, order_number FROM order \
                 WHERE serial_number = $sn AND status NOT IN ['completed', 'closed', 'cancelled', 'done'] \
                 LIMIT 1",
            )
            .bind(("sn", body.target_device_id.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .unwrap_or_default();

        if let Some(existing) = open.first() {
            return Ok(Json(json!({
                "success": true,
                "created": false,
                "order_id": existing.get("id"),
                "order_number": existing.get("order_number"),
            })));
        }

        let prefix = repair_order_prefix();
        let order_number = format!(
            "{}{}-{:04}",
            prefix,
            Utc::now().format("%Y%m%d"),
            rand::random::<u16>() % 10000
        );

        let created: Option<Value> = state
            .db
            .create("order")
            .content(json!({
                "uuid": uuid::Uuid::new_v4().to_string(),
                "order_number": order_number,
                "order_type": "repair",
                "serial_number": body.target_device_id,
                "status": "pending",
                "priority": "normal",
                "issue_description": body.data,
                "metadata": {
                    "created_by_device": body.source_device_id,
                    "acting_user_id": body.acting_user_id,
                    "owner_user_id": body.owner_user_id,
                },
                "created_at": now, "updated_at": now,
            }))
            .await
            .map_err(db_err)?;

        let created = created.unwrap_or(Value::Null);
        info!(
            "Repair: auto-created order {} for serial {}",
            order_number,
            created.get("serial_number").and_then(|v| v.as_str()).unwrap_or("?")
        );
        return Ok(Json(json!({
            "success": true,
            "created": true,
            "order_number": order_number,
            "order": created,
        })));
    }

    Ok(Json(json!({ "success": true })))
}

// ─── Multi-user (PIN switching on the PDA) ───────────────────────────────────

/// GET /api/users/active — users available on the PDA user-switch pad
pub async fn active_users(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let users: Vec<Value> = state
        .users_db
        .query(
            "SELECT record::id(id) AS id, username, name, role FROM user \
             WHERE isActive = true AND deleted_at IS NONE ORDER BY username ASC",
        )
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(users))
}

#[derive(Deserialize)]
pub struct VerifyPinRequest {
    #[serde(rename = "userId")]
    pub user_id: String,
    pub pin: String,
}

/// POST /api/users/verify-pin — bcrypt PIN check; 200 on success
pub async fn verify_pin(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyPinRequest>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<Value> = state
        .users_db
        .query("SELECT pin FROM user WHERE record::id(id) = $uid AND isActive = true AND deleted_at IS NONE LIMIT 1")
        .bind(("uid", body.user_id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let hash = rows
        .first()
        .and_then(|r| r.get("pin"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if hash.is_empty() {
        return Err((StatusCode::FORBIDDEN, "User has no PIN configured".into()));
    }

    match eck_core::auth::verify_password(&body.pin, hash) {
        Ok(true) => Ok(Json(json!({ "success": true }))),
        _ => Err((StatusCode::UNAUTHORIZED, "Invalid PIN".into())),
    }
}

// ─── Picking execution (route + confirm + validate) ──────────────────────────

/// GET /api/pickings/active — assigned/in-progress pickings with line counts
pub async fn active_pickings(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
    let mut pickings: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, name, state, partner_name, origin, priority, \
             scheduled_date, location_id, location_dest_id, created_at FROM picking \
             WHERE state IN ['assigned', 'confirmed', 'in_progress'] \
             ORDER BY priority DESC, created_at ASC",
        )
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    let totals: Vec<Value> = state
        .db
        .query("SELECT picking_id, count() AS total FROM move_line GROUP BY picking_id")
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();
    let done: Vec<Value> = state
        .db
        .query("SELECT picking_id, count() AS done FROM move_line WHERE state = 'done' GROUP BY picking_id")
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    let total_map: HashMap<String, i64> = totals
        .iter()
        .filter_map(|r| {
            Some((
                r.get("picking_id")?.as_str()?.to_string(),
                r.get("total")?.as_i64()?,
            ))
        })
        .collect();
    let done_map: HashMap<String, i64> = done
        .iter()
        .filter_map(|r| {
            Some((
                r.get("picking_id")?.as_str()?.to_string(),
                r.get("done")?.as_i64()?,
            ))
        })
        .collect();

    for p in pickings.iter_mut() {
        let pid = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if let Some(obj) = p.as_object_mut() {
            obj.insert("line_count".into(), json!(total_map.get(&pid).copied().unwrap_or(0)));
            obj.insert("picked_count".into(), json!(done_map.get(&pid).copied().unwrap_or(0)));
        }
    }

    Ok(Json(pickings))
}

/// GET /api/pickings/:id/route — ordered pick lines + map path points
pub async fn picking_route(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let mut lines: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS id, picking_id, product_id, product_name, product_barcode, \
             product_code, qty_demand, qty_done, location_id, location_name, location_barcode, \
             rack_id, rack_name, rack_x, rack_y, rack_width, rack_height, state, sequence \
             FROM move_line WHERE picking_id = $pid ORDER BY sequence ASC",
        )
        .bind(("pid", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    // Best-effort enrichment for lines missing denormalized product/location data
    for line in lines.iter_mut() {
        let needs_product = line.get("product_name").and_then(|v| v.as_str()).unwrap_or("").is_empty();
        if needs_product {
            if let Some(pid) = line.get("product_id").and_then(|v| v.as_str()).map(String::from) {
                if let Ok(Some(prod)) = state.get_synced_entity("product", &pid).await {
                    if let Some(obj) = line.as_object_mut() {
                        if let Some(n) = prod.get("name") {
                            obj.insert("product_name".into(), n.clone());
                        }
                        if let Some(b) = prod.get("barcode") {
                            obj.insert("product_barcode".into(), b.clone());
                        }
                        if let Some(c) = prod.get("default_code") {
                            obj.insert("product_code".into(), c.clone());
                        }
                    }
                }
            }
        }
        let needs_location = line.get("location_name").and_then(|v| v.as_str()).unwrap_or("").is_empty();
        if needs_location {
            if let Some(lid) = line.get("location_id").and_then(|v| v.as_str()).map(String::from) {
                if let Ok(Some(loc)) = state.get_synced_entity("location", &lid).await {
                    if let Some(obj) = line.as_object_mut() {
                        if let Some(n) = loc.get("complete_name").or_else(|| loc.get("name")) {
                            obj.insert("location_name".into(), n.clone());
                        }
                        if let Some(b) = loc.get("barcode") {
                            obj.insert("location_barcode".into(), b.clone());
                        }
                    }
                }
            }
        }
    }

    // Route path: walk rack coordinates in line order, dropping repeats
    let mut path: Vec<Value> = Vec::new();
    let mut last: Option<(i64, i64)> = None;
    for line in &lines {
        let x = line.get("rack_x").and_then(|v| v.as_i64());
        let y = line.get("rack_y").and_then(|v| v.as_i64());
        if let (Some(x), Some(y)) = (x, y) {
            if last != Some((x, y)) {
                path.push(json!({ "x": x, "y": y }));
                last = Some((x, y));
            }
        }
    }

    Ok(Json(json!({ "lines": lines, "route": { "path": path } })))
}

#[derive(Deserialize)]
pub struct ConfirmLineRequest {
    pub qty_done: f64,
    #[serde(default)]
    pub scanned_product_barcode: String,
    #[serde(default)]
    pub scanned_location_barcode: String,
}

/// POST /api/pickings/:id/lines/:line_id/confirm
pub async fn confirm_pick_line(
    State(state): State<Arc<AppState>>,
    Path((picking_id, line_id)): Path<(String, String)>,
    Json(body): Json<ConfirmLineRequest>,
) -> ApiResult<Json<Value>> {
    let now = Utc::now().to_rfc3339();
    let updated: Vec<Value> = state
        .db
        .query(
            "UPDATE move_line SET qty_done = $q, state = 'done', \
             scanned_product_barcode = $spb, scanned_location_barcode = $slb, \
             confirmed_at = $now, updated_at = $now \
             WHERE record::id(id) = $lid AND picking_id = $pid",
        )
        .bind(("q", body.qty_done))
        .bind(("spb", body.scanned_product_barcode))
        .bind(("slb", body.scanned_location_barcode))
        .bind(("now", now.clone()))
        .bind(("lid", line_id.clone()))
        .bind(("pid", picking_id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    if updated.is_empty() {
        return Err((StatusCode::NOT_FOUND, format!("Pick line '{line_id}' not found")));
    }

    // First confirmation moves the picking to in_progress
    let _ = state
        .db
        .query("UPDATE picking SET state = 'in_progress', updated_at = $now WHERE record::id(id) = $pid AND state IN ['assigned', 'confirmed']")
        .bind(("now", now))
        .bind(("pid", picking_id))
        .await;

    Ok(Json(json!({ "success": true })))
}

/// POST /api/pickings/:id/validate — close out a picking
pub async fn validate_picking(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let updated: Vec<Value> = state
        .db
        .query("UPDATE picking SET state = 'done', updated_at = $now WHERE record::id(id) = $pid")
        .bind(("now", Utc::now().to_rfc3339()))
        .bind(("pid", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    if updated.is_empty() {
        return Err((StatusCode::NOT_FOUND, format!("Picking '{id}' not found")));
    }

    Ok(Json(json!({ "success": true })))
}

// ─── Explorer (browse locations/products from the PDA) ───────────────────────

#[derive(Deserialize)]
pub struct ExplorerLocationsQuery {
    pub parent_id: Option<String>,
}

/// GET /api/explorer/locations?parent_id=
pub async fn explorer_locations(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ExplorerLocationsQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let query = if q.parent_id.is_some() {
        "SELECT record::id(id) AS id, name, complete_name, barcode, usage, parent_id, active \
         FROM location WHERE parent_id = $pid ORDER BY name ASC"
    } else {
        "SELECT record::id(id) AS id, name, complete_name, barcode, usage, parent_id, active \
         FROM location WHERE parent_id IS NONE OR parent_id = '' ORDER BY name ASC"
    };

    let mut stmt = state.db.query(query);
    if let Some(pid) = q.parent_id {
        stmt = stmt.bind(("pid", pid));
    }

    let locations: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();
    Ok(Json(locations))
}

/// GET /api/explorer/locations/:id/contents — quants in a location
pub async fn explorer_location_contents(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    let mut quants: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS id, * FROM quant WHERE location_id = $lid")
        .bind(("lid", id))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    for q in quants.iter_mut() {
        let has_name = q.get("product_name").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        if !has_name {
            if let Some(pid) = q.get("product_id").and_then(|v| v.as_str()).map(String::from) {
                if let Ok(Some(prod)) = state.get_synced_entity("product", &pid).await {
                    if let (Some(obj), Some(name)) = (q.as_object_mut(), prod.get("name")) {
                        obj.insert("product_name".into(), name.clone());
                    }
                }
            }
        }
    }

    Ok(Json(quants))
}

#[derive(Deserialize)]
pub struct ExplorerProductsQuery {
    #[serde(default)]
    pub q: String,
    pub limit: Option<i64>,
}

/// GET /api/explorer/products?q=&limit=
pub async fn explorer_products(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExplorerProductsQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let needle = params.q.trim().to_lowercase();

    let products: Vec<Value> = if needle.is_empty() {
        state
            .db
            .query("SELECT record::id(id) AS id, name, barcode, default_code, qty_available, list_price, active FROM product ORDER BY name ASC LIMIT $l")
            .bind(("l", limit))
            .await
            .map_err(db_err)?
            .take(0)
            .unwrap_or_default()
    } else {
        state
            .db
            .query(
                "SELECT record::id(id) AS id, name, barcode, default_code, qty_available, list_price, active \
                 FROM product WHERE string::lowercase(name ?? '') CONTAINS $q \
                 OR barcode = $raw OR default_code = $raw \
                 ORDER BY name ASC LIMIT $l",
            )
            .bind(("q", needle))
            .bind(("raw", params.q.trim().to_string()))
            .bind(("l", limit))
            .await
            .map_err(db_err)?
            .take(0)
            .unwrap_or_default()
    };

    Ok(Json(products))
}

/// GET /api/explorer/products/:id/locations — where a product is stocked
pub async fn explorer_product_locations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    let mut quants: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS id, * FROM quant WHERE product_id = $pid")
        .bind(("pid", id))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();

    for q in quants.iter_mut() {
        let has_name = q.get("location_name").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        if !has_name {
            if let Some(lid) = q.get("location_id").and_then(|v| v.as_str()).map(String::from) {
                if let Ok(Some(loc)) = state.get_synced_entity("location", &lid).await {
                    if let (Some(obj), Some(name)) = (
                        q.as_object_mut(),
                        loc.get("complete_name").or_else(|| loc.get("name")),
                    ) {
                        obj.insert("location_name".into(), name.clone());
                    }
                }
            }
        }
    }

    Ok(Json(quants))
}

// ─── CRM (PDA offline CRM workflow) ──────────────────────────────────────────
//
// SmartTag V2 CRM codes (`company-/person-/opp-<uuid>`) route to the PDA's
// CrmEntityScreen. The screen fetches current entity data here and pushes
// queued offline edits through /api/crm/update.

fn crm_table(entity_type: &str) -> Option<&'static str> {
    match entity_type {
        "company" | "person" => Some("partner"),
        "opp" | "opportunity" => Some("opportunity"),
        _ => None,
    }
}

/// GET /api/crm/:entity_type/:id — current entity data for the PDA screen
pub async fn crm_get(
    State(state): State<Arc<AppState>>,
    Path((entity_type, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let table = crm_table(&entity_type)
        .ok_or((StatusCode::BAD_REQUEST, format!("Unknown CRM entity type '{entity_type}'")))?;

    let record = state
        .get_synced_entity(table, &id)
        .await
        .map_err(|e| db_err(e))?
        .ok_or((StatusCode::NOT_FOUND, format!("{entity_type} '{id}' not found")))?;

    let mut out = record;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("entity_type".into(), json!(entity_type));
        obj.insert("entity_id".into(), json!(id));
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CrmUpdateRequest {
    pub entity_type: String,
    pub entity_id: String,
    #[serde(default)]
    pub changes: Value,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(rename = "deviceId", default)]
    pub device_id: String,
}

/// POST /api/crm/update — apply a queued offline CRM edit
///
/// Body matches WarehouseRepository.queueCrmUpdate():
/// `{entity_type, entity_id, changes: {notes?, status?}, timestamp}`.
/// Status overwrites; notes append to `pda_notes` (never clobber CRM notes).
/// Unknown entities are UPSERTed (offline-first: the printed QR is proof the
/// entity exists upstream even if this node hasn't synced it yet).
pub async fn crm_update(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CrmUpdateRequest>,
) -> ApiResult<Json<Value>> {
    let table = crm_table(&body.entity_type)
        .ok_or((StatusCode::BAD_REQUEST, format!("Unknown CRM entity type '{}'", body.entity_type)))?;

    require_active_device(&state, &body.device_id).await?;

    let now = Utc::now().to_rfc3339();
    let status = body.changes.get("status").and_then(|v| v.as_str()).map(String::from);
    let note = body.changes.get("notes").and_then(|v| v.as_str()).map(String::from);

    // Audit trail first — the log row survives even if the apply fails
    let _ = state
        .db
        .query(
            "CREATE crm_update_log SET entity_type = $et, entity_id = $eid, changes = $ch, \
             device_id = $dev, client_timestamp = $ts, created_at = $now",
        )
        .bind(("et", body.entity_type.clone()))
        .bind(("eid", body.entity_id.clone()))
        .bind(("ch", body.changes.clone()))
        .bind(("dev", body.device_id.clone()))
        .bind(("ts", body.timestamp))
        .bind(("now", now.clone()))
        .await
        .map_err(db_err)?;

    let mut set_clauses = vec![
        "updated_at = $now".to_string(),
        "crm_type = crm_type ?? $ctype".to_string(),
        "source_system = source_system ?? 'pda_crm'".to_string(),
    ];
    if status.is_some() {
        set_clauses.push("status = $status".to_string());
    }
    if note.is_some() {
        set_clauses.push(
            "pda_notes = array::concat(pda_notes ?? [], [{ note: $note, device_id: $dev, at: $now }])"
                .to_string(),
        );
    }

    let query = format!(
        "UPSERT type::record($tbl, $eid) SET {}",
        set_clauses.join(", ")
    );

    let mut stmt = state
        .db
        .query(&query)
        .bind(("tbl", table))
        .bind(("eid", body.entity_id.clone()))
        .bind(("ctype", body.entity_type.clone()))
        .bind(("dev", body.device_id.clone()))
        .bind(("now", now));
    if let Some(s) = status {
        stmt = stmt.bind(("status", s));
    }
    if let Some(n) = note {
        stmt = stmt.bind(("note", n));
    }

    let updated: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();

    info!(
        "CRM update applied: {} {} from device {}",
        body.entity_type, body.entity_id, body.device_id
    );

    Ok(Json(json!({
        "success": true,
        "entity": updated.first().cloned().unwrap_or(Value::Null),
    })))
}

// ─── POST /api/sync/pull — PDA metadata cache sync ───────────────────────────
//
// The SyncWorker pulls file_resources + attachments to mirror photo metadata
// into the on-device Room cache.

#[derive(Deserialize)]
pub struct SyncPullRequest {
    #[serde(default)]
    pub entity_types: Vec<String>,
    #[serde(default)]
    pub since: Option<String>,
}

pub async fn sync_pull(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SyncPullRequest>,
) -> ApiResult<Json<Value>> {
    let mut out = serde_json::Map::new();

    if body.entity_types.iter().any(|t| t == "file_resources") {
        let query = if body.since.is_some() {
            "SELECT cas_uuid AS id, hash, original_name AS originalName, mime_type AS mimeType, \
             size_bytes AS sizeBytes, storage_path AS storagePath, avatar_b64 AS avatar_data, \
             updated_at FROM file_resource WHERE updated_at > $since ORDER BY updated_at DESC LIMIT 500"
        } else {
            "SELECT cas_uuid AS id, hash, original_name AS originalName, mime_type AS mimeType, \
             size_bytes AS sizeBytes, storage_path AS storagePath, avatar_b64 AS avatar_data, \
             updated_at FROM file_resource ORDER BY updated_at DESC LIMIT 500"
        };
        let mut stmt = state.db.query(query);
        if let Some(since) = body.since.clone() {
            stmt = stmt.bind(("since", since));
        }
        let files: Vec<Value> = stmt.await.map_err(db_err)?.take(0).unwrap_or_default();
        out.insert("file_resources".into(), json!(files));
    }

    if body.entity_types.iter().any(|t| t == "attachments") {
        let attachments: Vec<Value> = state
            .db
            .query(
                "SELECT record::id(id) AS id, record::tb(in) AS res_model, record::id(in) AS res_id, \
                 out.cas_uuid AS file_resource_id, label AS tags FROM has_attachment LIMIT 1000",
            )
            .await
            .map_err(db_err)?
            .take(0)
            .unwrap_or_default();
        out.insert("attachments".into(), json!(attachments));
    }

    if out.is_empty() {
        warn!("sync_pull: no recognized entity_types in {:?}", body.entity_types);
    }

    Ok(Json(Value::Object(out)))
}
