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

        let _: Result<Option<Value>, _> = state.db
            .upsert(("product", ext_id))
            .merge(json!({
                "source_system": "exact_online",
                "external_id": ext_id,
                "name": name,
                "default_code": default_code,
                "barcode": barcode,
                "payload": item,
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
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .await;
        updated += 1;
    }

    Ok(Json(json!({ "success": true, "updated": updated, "skipped": skipped })))
}
