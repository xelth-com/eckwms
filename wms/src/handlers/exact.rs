//! Manual import handlers for Exact Online data.
//! Called from the UI's Scrapers page. Each handler accepts a JSON array,
//! extracts top-level fields from the raw payload, and upserts via `.merge()`
//! to avoid overwriting user-modified fields.

use axum::{extract::State, http::StatusCode, Json};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

/// Extract a string from the first matching key in a JSON object.
fn pick_str<'a>(obj: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() { return s; }
        }
    }
    ""
}

/// Next `_vclock` for a synced record, advancing THIS node's counter from
/// whatever it currently holds. These tables (product/partner/stock_position/
/// quotation/sales_order) are in SYNC_ENTITY_TYPES, so a merge-import that
/// changes content but leaves `_vclock` null/stale never propagates — the peer
/// drops it as "local wins/equal" and the two nodes churn forever re-pushing the
/// same rows (the partner-churn mesh incident). Setting an advancing vclock makes
/// the import dominate and converge. One extra read per row — fine for a batch.
async fn next_vclock(state: &AppState, table: &str, id: &str) -> Value {
    // NB: `type::thing($tb, $id)` does NOT match all-digit string ids (they're
    // stored backtick-quoted, e.g. `partner:`00193``), so read via `type::record`
    // with the explicit backtick-quoted thing — same as import_ticket.
    let rid = format!("{}:`{}`", table, id);
    let existing: Option<Value> = state
        .db
        .query("SELECT _vclock FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|v| v.get("_vclock")),
        &state.instance_id,
    )
}

/// POST /api/exact/backfill-vclocks — one-shot: stamp an advancing `_vclock` on
/// every Exact-imported synced row that currently has none. Rows imported before
/// the vclock fix are null-clocked, so a peer can't tell this node's enriched
/// version dominates and drops the update as "local wins/equal" — the mesh then
/// churns re-pushing the same rows forever (the partner-churn incident). Stamping
/// a vclock lets this node's version win and the two nodes converge. Idempotent.
pub async fn backfill_vclocks(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let tables = ["product", "partner", "stock_position", "quotation", "sales_order"];
    let now = Utc::now().to_rfc3339();
    let mut per_table = serde_json::Map::new();
    let mut total = 0i64;
    for table in tables {
        let ids: Vec<Value> = state
            .db
            .query("SELECT record::id(id) AS id FROM type::table($tb) WHERE _vclock = NONE")
            .bind(("tb", table.to_string()))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mut n = 0i64;
        for row in &ids {
            let id = match row.get("id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let vclock = next_vclock(&state, table, &id).await;
            let rid = format!("{}:`{}`", table, id);
            let _: Result<Option<Value>, _> = state
                .db
                .query("UPDATE type::record($rid) MERGE { _vclock: $vc, updated_at: $now }")
                .bind(("rid", rid))
                .bind(("vc", vclock))
                .bind(("now", now.clone()))
                .await
                .map(|_| None);
            n += 1;
        }
        per_table.insert(table.to_string(), json!(n));
        total += n;
    }
    Ok(Json(json!({ "success": true, "stamped": total, "per_table": per_table })))
}

/// POST /api/exact/import-items — import products from Exact Online
pub async fn import_items(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let items = body.get("items").and_then(|v| v.as_array())
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default();

    let mut updated = 0i64;
    let mut skipped = 0i64;

    for item in &items {
        let ext_id = pick_str(item, &["code", "Code"]);
        if ext_id.is_empty() { skipped += 1; continue; }

        let name = pick_str(item, &["description", "Description", "name", "Name"]);
        let barcode = pick_str(item, &["barcode", "Barcode"]);
        let default_code = pick_str(item, &["code", "Code"]);
        let vclock = next_vclock(&state, "product", ext_id).await;

        let _: Result<Option<Value>, _> = state.db
            .upsert(("product", ext_id))
            .merge(json!({
                "source_system": "exact_online",
                "external_id": ext_id,
                "name": name,
                "default_code": default_code,
                "barcode": barcode,
                "payload": item,
                "_vclock": vclock,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .await;
        updated += 1;
    }

    Ok(Json(json!({ "success": true, "updated": updated, "skipped": skipped })))
}

/// POST /api/exact/import-customers — import partners/customers from Exact Online
pub async fn import_customers(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let items = body.get("customers").and_then(|v| v.as_array())
        .or_else(|| body.get("items").and_then(|v| v.as_array()))
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default();

    let mut updated = 0i64;
    let mut skipped = 0i64;

    for item in &items {
        let ext_id = pick_str(item, &["code", "Code"]);
        if ext_id.is_empty() { skipped += 1; continue; }

        let name = pick_str(item, &["name", "Name", "accountName", "AccountName"]);
        let email = pick_str(item, &["email", "Email"]);
        let phone = pick_str(item, &["phone", "Phone"]);
        let city = pick_str(item, &["city", "City"]);
        let country = pick_str(item, &["country", "Country"]);
        let vclock = next_vclock(&state, "partner", ext_id).await;

        let _: Result<Option<Value>, _> = state.db
            .upsert(("partner", ext_id))
            .merge(json!({
                "source_system": "exact_online",
                "external_id": ext_id,
                "name": name,
                "email": email,
                "phone": phone,
                "city": city,
                "country": country,
                "payload": item,
                "_vclock": vclock,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .await;
        updated += 1;
    }

    Ok(Json(json!({ "success": true, "updated": updated, "skipped": skipped })))
}

/// POST /api/exact/import-stock-positions — import stock positions from Exact Online
pub async fn import_stock_positions(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let items = body.get("positions").and_then(|v| v.as_array())
        .or_else(|| body.get("items").and_then(|v| v.as_array()))
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default();

    let mut updated = 0i64;
    let mut skipped = 0i64;

    for item in &items {
        let item_code = pick_str(item, &["item_code", "ItemCode"]);
        let wh_code = pick_str(item, &["warehouse_code", "WarehouseCode"]);
        if item_code.is_empty() { skipped += 1; continue; }

        let ext_id = format!("{}_{}", item_code, wh_code);
        let vclock = next_vclock(&state, "stock_position", &ext_id).await;
        let _: Result<Option<Value>, _> = state.db
            .upsert(("stock_position", ext_id.as_str()))
            .merge(json!({
                "source_system": "exact_online",
                "item_code": item_code,
                "warehouse_code": wh_code,
                "in_stock": item.get("in_stock").or(item.get("InStock")),
                "planned_in": item.get("planned_in").or(item.get("PlannedIn")),
                "planned_out": item.get("planned_out").or(item.get("PlannedOut")),
                "projected_stock": item.get("projected_stock").or(item.get("ProjectedStock")),
                "reorder_point": item.get("reorder_point").or(item.get("ReorderPoint")),
                "payload": item,
                "_vclock": vclock,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .await;
        updated += 1;
    }

    Ok(Json(json!({ "success": true, "updated": updated, "skipped": skipped })))
}

/// POST /api/exact/import-quotations — import quotations from Exact Online
pub async fn import_quotations(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let items = body.get("quotations").and_then(|v| v.as_array())
        .or_else(|| body.get("items").and_then(|v| v.as_array()))
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default();

    let mut updated = 0i64;
    let mut skipped = 0i64;

    for item in &items {
        let number = pick_str(item, &["number_version", "NumberVersion"]);
        if number.is_empty() { skipped += 1; continue; }

        let ext_id = number.replace(['/', ' '], "_");
        let vclock = next_vclock(&state, "quotation", &ext_id).await;
        let _: Result<Option<Value>, _> = state.db
            .upsert(("quotation", ext_id.as_str()))
            .merge(json!({
                "source_system": "exact_online",
                "number_version": number,
                "ordered_by_code": item.get("ordered_by_code").or(item.get("OrderedByCode")),
                "ordered_by_name": item.get("ordered_by_name").or(item.get("OrderedByName")),
                "amount": item.get("amount").or(item.get("Amount")),
                "currency": item.get("currency").or(item.get("Currency")),
                "status": item.get("status").or(item.get("Status")),
                "quotation_date": item.get("quotation_date").or(item.get("QuotationDate")),
                "description": item.get("description").or(item.get("Description")),
                "payload": item,
                "_vclock": vclock,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .await;
        updated += 1;
    }

    Ok(Json(json!({ "success": true, "updated": updated, "skipped": skipped })))
}

/// POST /api/exact/import-sales-orders — import sales orders from Exact Online
pub async fn import_sales_orders(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let items = body.get("orders").and_then(|v| v.as_array())
        .or_else(|| body.get("items").and_then(|v| v.as_array()))
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default();

    let mut updated = 0i64;
    let mut skipped = 0i64;

    for item in &items {
        let order_number = pick_str(item, &["order_number", "OrderNumber", "number"]);
        if order_number.is_empty() { skipped += 1; continue; }

        let customer_name = pick_str(item, &["deliver_to_name", "DeliverToName", "ordered_by_name", "OrderedByName"]);
        let status = pick_str(item, &["status", "Status"]);
        let vclock = next_vclock(&state, "sales_order", order_number).await;

        let _: Result<Option<Value>, _> = state.db
            .upsert(("sales_order", order_number))
            .merge(json!({
                "source_system": "exact_online",
                "order_number": order_number,
                "customer_name": customer_name,
                "status": status,
                "amount": item.get("amount").or(item.get("Amount")),
                "currency": item.get("currency").or(item.get("Currency")),
                "order_date": item.get("order_date").or(item.get("OrderDate")),
                "payload": item,
                "_vclock": vclock,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .await;
        updated += 1;
    }

    Ok(Json(json!({ "success": true, "updated": updated, "skipped": skipped })))
}
