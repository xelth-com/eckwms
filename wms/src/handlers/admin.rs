use std::sync::Arc;

use axum::{extract::{State, Query}, http::StatusCode, Extension, Json};
use eck_core::auth::Claims;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::{services::scheduler, AppState};

#[derive(Deserialize)]
pub struct QueryRequest {
    pub query: String,
}

/// POST /api/admin/query — execute arbitrary SurrealQL for diagnostics.
/// Admin-only: this is a full read/write SQL surface (SELECT … FROM user, DELETE,
/// REMOVE TABLE …). The route is JWT-gated; this is the role check on top.
pub async fn query(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<QueryRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if claims.role != "admin" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "admin only" })),
        ));
    }
    let result: Result<Vec<Value>, _> = state
        .db
        .query(&body.query)
        .await
        .and_then(|mut r| r.take(0));

    match result {
        Ok(rows) => Ok(Json(json!({ "ok": true, "result": rows }))),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )),
    }
}

/// POST /api/admin/force-sync — manually trigger every scraper provider in
/// sequence (OPAL, DHL, Zoho Desk, Exact Online, Excel). Bypasses the cron
/// schedule so the operator can drive a sync on demand and watch the logs.
/// Zoho runs in incremental mode (limit 100) to keep the run bounded —
/// switch to full_sync=true via the scheduler's daily path if a full
/// re-import is needed.
#[derive(Deserialize, Default)]
pub struct ForceSyncParams {
    /// `?full=true` → run a FULL Zoho re-import (all tickets, not just the
    /// recent 100). It can take a long time under Zoho's rate limit, so it is
    /// spawned in the background and this endpoint returns immediately.
    #[serde(default)]
    pub full: bool,
}

pub async fn force_sync(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ForceSyncParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    info!("[admin] force-sync triggered (full={})", params.full);

    // Full Zoho re-import: long-running (rate-limited), so spawn + return now.
    if params.full {
        let db = state.db.clone();
        let iid = state.instance_id.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("client build");
            info!("[admin] force-sync: Zoho Desk FULL re-import (background) started");
            // Guarded: two of these died silently mid-run on Zoho session
            // flaps (2026-07-22/23) — a panic now lands as a 'died' row.
            let (db2, c2, i2) = (db.clone(), client.clone(), iid.clone());
            scheduler::run_guarded(&db, &iid, "zoho_desk", async move {
                scheduler::sync_zoho(&db2, &c2, &i2, true).await;
            })
            .await;
            info!("[admin] force-sync: Zoho Desk FULL re-import done");
        });
        return Ok(Json(json!({
            "ok": true,
            "mode": "zoho_full_background",
            "note": "Full Zoho re-import running detached. Watch wms.log / sync_history / observer.",
        })));
    }

    let started = std::time::Instant::now();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("client build: {}", e) })),
        ))?;

    let db = &state.db;
    let iid = state.instance_id.as_str();

    info!("[admin] force-sync: OPAL");
    scheduler::sync_opal(db, &client, iid).await;
    info!("[admin] force-sync: DHL");
    scheduler::sync_dhl(db, &client, iid).await;
    info!("[admin] force-sync: Zoho Desk (incremental)");
    scheduler::sync_zoho(db, &client, iid, false).await;
    info!("[admin] force-sync: Exact Online");
    scheduler::sync_exact_online(db, &client, iid).await;
    info!("[admin] force-sync: Excel");
    scheduler::sync_excel(db, &client, iid).await;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    info!("[admin] force-sync done in {} ms", elapsed_ms);

    Ok(Json(json!({
        "ok": true,
        "providers": ["opal", "dhl", "zoho_desk", "exact_online", "excel"],
        "elapsed_ms": elapsed_ms,
        "note": "See sync_history table and wms.log for per-provider details.",
    })))
}

/// POST /api/admin/backfill/unit-order-links?apply=0|1 — one-shot migration
/// that fills the new UUID link (`order.unit_id` ↔ `receipt_unit.order_uuid`)
/// for repair orders opened before the link existed. DRY-RUN by default
/// (`apply` absent or `0`): it only reports what it WOULD do.
///
/// A pair is linked only when it is UNAMBIGUOUS: the order's
/// `metadata.receipt_uuid` equals the unit's `receipt_id`, AND either the
/// serials match exactly, OR the receipt holds exactly one unit and one
/// order, OR the receipt holds exactly one `kind == "item"` unit (alongside
/// any number of `box`/`part` units) and exactly one order — that device
/// unit is linked even when serials differ or are absent. A `box`/`part`
/// unit is never linked by that last rule; a receipt with no `item` unit at
/// all stays `unmatched`. Anything else (several serial matches, several
/// `item` units, or a mismatch on a multi-unit / multi-order receipt) goes
/// to `ambiguous[]` / `unmatched[]` with both serial spellings and is NEVER
/// auto-linked. `apply=1` writes only the clean links, each advancing
/// `_vclock` through `put_synced_entity`.
#[derive(Deserialize, Default)]
pub struct BackfillParams {
    #[serde(default)]
    pub apply: u8,
}

pub async fn backfill_unit_order_links(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<BackfillParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if claims.role != "admin" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "admin only" })),
        ));
    }
    let apply = params.apply == 1;

    let db_err_json = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e })),
        )
    };

    // Repair orders that carry a receipt link. `unit_id` tells us which are
    // already linked.
    let orders: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS order_id, serial_number, unit_id, \
                    metadata.receipt_uuid AS receipt_uuid \
             FROM order WHERE order_type = 'repair' AND metadata.receipt_uuid != NONE",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(|e| db_err_json(e.to_string()))?;

    // Units of every receipt in play.
    let receipts: Vec<String> = orders
        .iter()
        .filter_map(|o| backfill_str(o, "receipt_uuid"))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let units: Vec<Value> = if receipts.is_empty() {
        Vec::new()
    } else {
        state
            .db
            .query(
                "SELECT record::id(id) AS unit_id, serial, barcode, order_uuid, receipt_id, kind \
                 FROM receipt_unit WHERE receipt_id IN $rids",
            )
            .bind(("rids", json!(receipts)))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(|e| db_err_json(e.to_string()))?
    };

    let plan = classify_unit_order_links(&orders, &units);

    let mut linked = 0usize;
    if apply {
        for (order_id, unit_id) in &plan.to_link {
            state
                .put_synced_entity("order", order_id, json!({ "unit_id": unit_id }))
                .await
                .map_err(|e| db_err_json(e.to_string()))?;
            state
                .put_synced_entity("receipt_unit", unit_id, json!({ "order_uuid": order_id }))
                .await
                .map_err(|e| db_err_json(e.to_string()))?;
            linked += 1;
        }
    } else {
        linked = plan.to_link.len();
    }

    info!(
        "[admin] backfill unit-order-links: apply={} linked={} already_linked={} ambiguous={} unmatched={}",
        apply, linked, plan.already_linked, plan.ambiguous.len(), plan.unmatched.len()
    );

    Ok(Json(json!({
        "ok": true,
        "applied": apply,
        "linked": linked,
        "already_linked": plan.already_linked,
        "ambiguous": plan.ambiguous.len(),
        "unmatched": plan.unmatched.len(),
        "ambiguous_detail": plan.ambiguous,
        "unmatched_detail": plan.unmatched,
    })))
}

fn backfill_str(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The outcome of the unit↔order backfill match — pure, so it is unit-tested
/// without a DB. `to_link` holds ONLY the unambiguous (order_id, unit_id) pairs;
/// everything doubtful lands in `ambiguous` / `unmatched` and is never linked.
pub struct BackfillPlan {
    pub to_link: Vec<(String, String)>,
    pub ambiguous: Vec<Value>,
    pub unmatched: Vec<Value>,
    pub already_linked: usize,
}

/// Decide which orders can be safely linked to a unit. Inputs are the projected
/// rows from the handler (`orders`: order_id, serial_number, unit_id,
/// receipt_uuid; `units`: unit_id, serial, barcode, order_uuid, receipt_id).
pub fn classify_unit_order_links(orders: &[Value], units: &[Value]) -> BackfillPlan {
    let mut already_linked = 0usize;
    let mut by_receipt: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    let mut receipt_order_count: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for o in orders {
        let Some(ruid) = backfill_str(o, "receipt_uuid") else { continue };
        *receipt_order_count.entry(ruid.clone()).or_default() += 1;
        if backfill_str(o, "unit_id").is_some() {
            already_linked += 1;
            continue;
        }
        by_receipt.entry(ruid).or_default().push(o);
    }

    let mut units_by_receipt: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    for u in units {
        if let Some(rid) = backfill_str(u, "receipt_id") {
            units_by_receipt.entry(rid).or_default().push(u);
        }
    }

    let mut to_link: Vec<(String, String)> = Vec::new();
    let mut ambiguous: Vec<Value> = Vec::new();
    let mut unmatched: Vec<Value> = Vec::new();

    for (ruid, orders_r) in &by_receipt {
        let empty = Vec::new();
        let units_r = units_by_receipt.get(ruid).unwrap_or(&empty);
        let one_unit_one_order =
            units_r.len() == 1 && receipt_order_count.get(ruid).copied().unwrap_or(0) == 1;

        for o in orders_r {
            let Some(order_id) = backfill_str(o, "order_id") else { continue };
            let order_serial = backfill_str(o, "serial_number");

            // Exact serial match against the units of this receipt.
            let exact: Vec<&&Value> = match order_serial.as_deref() {
                Some(sn) => units_r
                    .iter()
                    .filter(|u| backfill_str(u, "serial").as_deref() == Some(sn))
                    .collect(),
                None => Vec::new(),
            };

            if exact.len() == 1 {
                if let Some(uid) = backfill_str(exact[0], "unit_id") {
                    to_link.push((order_id, uid));
                }
            } else if exact.len() > 1 {
                ambiguous.push(json!({
                    "order_id": order_id,
                    "receipt_uuid": ruid,
                    "order_serial": order_serial,
                    "reason": "several units match the serial exactly",
                    "units": exact.iter().map(|u| json!({
                        "unit_id": backfill_str(u, "unit_id"),
                        "unit_serial": backfill_str(u, "serial"),
                    })).collect::<Vec<_>>(),
                }));
            } else if one_unit_one_order {
                // No serial match, but the receipt is a clean 1-unit/1-order
                // parcel — link them (covers the OCR-typo case, e.g. GER2610-028).
                if let Some(uid) = backfill_str(units_r[0], "unit_id") {
                    to_link.push((order_id, uid));
                }
            } else {
                // No serial match and not a clean single-unit receipt (it may
                // hold box/part units alongside a device, e.g. the receipt was
                // logged as one item + one part). Fall back to the device
                // (kind == "item") unit specifically — box/part units are
                // never link targets.
                let item_units: Vec<&&Value> = units_r
                    .iter()
                    .filter(|u| backfill_str(u, "kind").as_deref() == Some("item"))
                    .collect();
                let order_count_for_receipt =
                    receipt_order_count.get(ruid).copied().unwrap_or(0);

                if item_units.len() == 1 && order_count_for_receipt == 1 {
                    // single item unit on receipt — link to the device even
                    // though other box/part units exist and serials differ.
                    if let Some(uid) = backfill_str(item_units[0], "unit_id") {
                        to_link.push((order_id, uid));
                    }
                } else if item_units.len() > 1 {
                    ambiguous.push(json!({
                        "order_id": order_id,
                        "receipt_uuid": ruid,
                        "order_serial": order_serial,
                        "reason": "several item units on receipt",
                        "units": item_units.iter().map(|u| json!({
                            "unit_id": backfill_str(u, "unit_id"),
                            "unit_serial": backfill_str(u, "serial"),
                        })).collect::<Vec<_>>(),
                    }));
                } else if item_units.is_empty() {
                    unmatched.push(json!({
                        "order_id": order_id,
                        "receipt_uuid": ruid,
                        "order_serial": order_serial,
                        "reason": "no device (item) unit on receipt (box/part only)",
                        "unit_serials": units_r.iter().map(|u| json!({
                            "unit_id": backfill_str(u, "unit_id"),
                            "unit_serial": backfill_str(u, "serial"),
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    // Exactly one item unit, but more than one order on this
                    // receipt — which order owns it is ambiguous without a
                    // serial match.
                    unmatched.push(json!({
                        "order_id": order_id,
                        "receipt_uuid": ruid,
                        "order_serial": order_serial,
                        "reason": "no exact serial match and receipt is not a single unit+order",
                        "unit_serials": units_r.iter().map(|u| json!({
                            "unit_id": backfill_str(u, "unit_id"),
                            "unit_serial": backfill_str(u, "serial"),
                        })).collect::<Vec<_>>(),
                    }));
                }
            }
        }
    }

    BackfillPlan {
        to_link,
        ambiguous,
        unmatched,
        already_linked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(id: &str, serial: &str, receipt: &str, unit_id: Option<&str>) -> Value {
        let mut o = json!({
            "order_id": id,
            "serial_number": serial,
            "receipt_uuid": receipt,
        });
        if let Some(u) = unit_id {
            o["unit_id"] = json!(u);
        }
        o
    }
    fn unit(id: &str, serial: &str, receipt: &str) -> Value {
        json!({ "unit_id": id, "serial": serial, "receipt_id": receipt })
    }
    fn unit_kind(id: &str, serial: Option<&str>, receipt: &str, kind: &str) -> Value {
        json!({ "unit_id": id, "serial": serial, "receipt_id": receipt, "kind": kind })
    }

    #[test]
    fn exact_serial_links_single_match() {
        let orders = [order("o1", "SN-A", "r1", None)];
        let units = [unit("u1", "SN-A", "r1")];
        let plan = classify_unit_order_links(&orders, &units);
        assert_eq!(plan.to_link, vec![("o1".into(), "u1".into())]);
        assert!(plan.ambiguous.is_empty() && plan.unmatched.is_empty());
    }

    /// The GER2610-028 class: serials differ by one char, but the receipt is a
    /// clean 1-unit/1-order parcel → linked.
    #[test]
    fn single_unit_single_order_links_despite_serial_typo() {
        let orders = [order("o1", "I82400067", "r1", None)];
        let units = [unit("u1", "I824000067", "r1")];
        let plan = classify_unit_order_links(&orders, &units);
        assert_eq!(plan.to_link, vec![("o1".into(), "u1".into())]);
    }

    /// Serial mismatch on a MULTI-unit receipt is never auto-linked.
    #[test]
    fn multi_unit_mismatch_is_unmatched() {
        let orders = [order("o1", "SN-X", "r1", None)];
        let units = [unit("u1", "SN-A", "r1"), unit("u2", "SN-B", "r1")];
        let plan = classify_unit_order_links(&orders, &units);
        assert!(plan.to_link.is_empty());
        assert_eq!(plan.unmatched.len(), 1);
    }

    #[test]
    fn two_units_same_serial_is_ambiguous() {
        let orders = [order("o1", "SN-A", "r1", None)];
        let units = [unit("u1", "SN-A", "r1"), unit("u2", "SN-A", "r1")];
        let plan = classify_unit_order_links(&orders, &units);
        assert!(plan.to_link.is_empty());
        assert_eq!(plan.ambiguous.len(), 1);
    }

    #[test]
    fn already_linked_is_counted_and_skipped() {
        let orders = [order("o1", "SN-A", "r1", Some("u1"))];
        let units = [unit("u1", "SN-A", "r1")];
        let plan = classify_unit_order_links(&orders, &units);
        assert_eq!(plan.already_linked, 1);
        assert!(plan.to_link.is_empty());
    }

    /// Single item unit + a part on the same receipt, serial mismatch: the
    /// order still links to the item unit, never the part.
    #[test]
    fn single_item_unit_links_despite_part_and_serial_mismatch() {
        let orders = [order("o1", "SN-DIFFERENT", "r1", None)];
        let units = [
            unit_kind("u-item", Some("SN-A"), "r1", "item"),
            unit_kind("u-part", None, "r1", "part"),
        ];
        let plan = classify_unit_order_links(&orders, &units);
        assert_eq!(plan.to_link, vec![("o1".into(), "u-item".into())]);
        assert!(plan.ambiguous.is_empty() && plan.unmatched.is_empty());
    }

    /// Same, but the order has no serial at all.
    #[test]
    fn single_item_unit_links_when_order_has_no_serial() {
        let mut o = json!({ "order_id": "o1", "receipt_uuid": "r1" });
        o["serial_number"] = Value::Null;
        let units = [
            unit_kind("u-item", Some("SN-A"), "r1", "item"),
            unit_kind("u-part", None, "r1", "part"),
        ];
        let plan = classify_unit_order_links(&[o], &units);
        assert_eq!(plan.to_link, vec![("o1".into(), "u-item".into())]);
    }

    /// Box + part only (no item unit at all) → unmatched with the dedicated
    /// reason, never linked to a box/part unit.
    #[test]
    fn box_and_part_only_is_unmatched_with_no_item_reason() {
        let orders = [order("o1", "SN-X", "r1", None)];
        let units = [
            unit_kind("u-box", None, "r1", "box"),
            unit_kind("u-part", None, "r1", "part"),
        ];
        let plan = classify_unit_order_links(&orders, &units);
        assert!(plan.to_link.is_empty());
        assert_eq!(plan.unmatched.len(), 1);
        assert_eq!(
            plan.unmatched[0]["reason"],
            json!("no device (item) unit on receipt (box/part only)")
        );
    }

    /// Two item units on the receipt, no serial match → ambiguous, not linked.
    #[test]
    fn two_item_units_no_serial_match_is_ambiguous() {
        let orders = [order("o1", "SN-X", "r1", None)];
        let units = [
            unit_kind("u1", Some("SN-A"), "r1", "item"),
            unit_kind("u2", Some("SN-B"), "r1", "item"),
        ];
        let plan = classify_unit_order_links(&orders, &units);
        assert!(plan.to_link.is_empty());
        assert_eq!(plan.ambiguous.len(), 1);
        assert_eq!(plan.ambiguous[0]["reason"], json!("several item units on receipt"));
    }
}

/// POST /api/admin/mesh-replay/:entity_type — one-shot backfill: scan every
/// row of the given table on this node and queue a `push` task on the relay
/// for each other online peer. Useful after adding a new entity_type to
/// SYNC_ENTITY_TYPES (e.g. when a peer joins fresh and needs the existing
/// dataset, but cross-NAT direct merkle pull can't reach it).
///
/// Batches into chunks of `BATCH_SIZE` so a single 6k-row table doesn't post
/// a 50 MB envelope to the relay. The mesh_relay_poller on the receiving
/// side applies each batch via the same conflict-aware upsert as direct push.
///
/// Idempotent: running twice just re-asserts the same data; conflict
/// resolution treats matching VectorClock entries as no-ops.
pub async fn mesh_replay(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(entity_type): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    const BATCH_SIZE: usize = 50;

    // Cheap safety: allow-list against a hard-coded set of synced tables to
    // avoid an operator typo (`POST /mesh-replay/user`) dumping Zone 1 PII
    // into the relay queue. Mirrors SYNC_ENTITY_TYPES in core/engine.rs but
    // is intentionally re-declared so a future divergence on either side
    // doesn't silently widen this surface.
    const ALLOWED: &[&str] = &[
        "item", "order", "product", "partner", "file_resource", "location",
        "quant", "picking", "move_line", "rack", "action_proof",
        "delivery_carrier", "delivery_tracking", "device_intake",
        "inventory_discrepancy", "product_alias", "category", "menu_item",
        "transaction", "storno_log",
        "ai_sop", "registered_device", "document",
        "device", "device_event",
    ];
    if !ALLOWED.contains(&entity_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("entity_type '{}' not in mesh-replay allow-list", entity_type) })),
        ));
    }

    info!("[admin] mesh-replay: scanning table '{}'", entity_type);

    let query = format!(
        "SELECT *, record::id(id) AS id FROM {}",
        entity_type
    );
    let rows: Vec<Value> = match state.db.query(&query).await.and_then(|mut r| r.take(0)) {
        Ok(rows) => rows,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("scan failed: {}", e) })),
            ));
        }
    };
    let total_rows = rows.len();

    // Discover peers via the relay (skips ourselves + offline nodes).
    let peers = match state.sync_engine.relay().get_mesh_status().await {
        Ok(n) => n,
        Err(e) => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false, "error": format!("relay unreachable: {}", e) })),
            ));
        }
    };
    let target_uuids: Vec<String> = peers
        .into_iter()
        .filter(|n| n.instance_id != state.instance_id && n.status == "online")
        .filter(|n| n.node_role.as_deref() != Some("cache"))
        .map(|n| n.instance_id)
        .collect();

    if target_uuids.is_empty() {
        return Ok(Json(json!({
            "ok": true,
            "entity_type": entity_type,
            "rows": total_rows,
            "peers": 0,
            "note": "No online full peers — nothing to dispatch.",
        })));
    }

    let relay = state.sync_engine.relay();
    let mut tasks_queued = 0usize;
    let mut errors = 0usize;
    for chunk in rows.chunks(BATCH_SIZE) {
        for target in &target_uuids {
            let payload = json!({
                "entity_type": entity_type,
                "entities": chunk,
                "source_instance": state.instance_id,
            });
            match relay.mesh_dispatch(target, "push", payload).await {
                Ok(_) => tasks_queued += 1,
                Err(e) => {
                    errors += 1;
                    tracing::warn!(
                        "mesh-replay: dispatch to {} failed: {}",
                        target, e
                    );
                }
            }
        }
    }

    info!(
        "[admin] mesh-replay '{}': {} rows -> {} peers, {} tasks queued ({} errors)",
        entity_type, total_rows, target_uuids.len(), tasks_queued, errors
    );

    Ok(Json(json!({
        "ok": true,
        "entity_type": entity_type,
        "rows": total_rows,
        "peers": target_uuids.len(),
        "batch_size": BATCH_SIZE,
        "tasks_queued": tasks_queued,
        "errors": errors,
    })))
}
