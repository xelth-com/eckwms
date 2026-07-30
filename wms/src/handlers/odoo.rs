//! Admin HTTP surface for the Odoo connector.
//!
//! - `GET  /api/odoo/ping` — authenticate + live counts (no persistence).
//! - `POST /api/odoo/sync` — pull master data into local `odoo_*` tables.
//!
//! Both are mounted behind the JWT `auth_middleware` (see `main.rs`). They are
//! read-only against Odoo; `sync` only writes to the local mesh DB.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};

use crate::services::odoo::{pull_master_data, OdooClient};
use crate::AppState;

type ApiResult = Result<Json<Value>, (StatusCode, String)>;

fn upstream(e: String) -> (StatusCode, String) {
    (StatusCode::BAD_GATEWAY, e)
}

/// GET /api/odoo/ping — connectivity probe. Authenticates against Odoo and
/// returns the server version, our uid, and live counts. Persists nothing.
pub async fn ping(State(_state): State<Arc<AppState>>) -> ApiResult {
    let client = OdooClient::from_env().await.map_err(upstream)?;
    let version = client.server_version().await.unwrap_or(Value::Null);
    let warehouses = client
        .search_count("stock.warehouse", json!([]))
        .await
        .unwrap_or(0);
    let products = client
        .search_count("product.product", json!([]))
        .await
        .unwrap_or(0);
    let quants = client
        .search_count("stock.quant", json!([["quantity", "!=", 0]]))
        .await
        .unwrap_or(0);
    Ok(Json(json!({
        "ok": true,
        "uid": client.uid(),
        "server_version": version.get("server_version"),
        "counts": { "warehouses": warehouses, "products": products, "quants": quants },
    })))
}

/// POST /api/odoo/sync — pull warehouses/locations/products/quants from Odoo
/// into local `odoo_*` tables. Optional JSON body:
/// `{ "product_limit": <int>, "quant_limit": <int> }` (`0`/absent = unbounded).
pub async fn sync(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> ApiResult {
    let product_limit = body.get("product_limit").and_then(|v| v.as_i64()).unwrap_or(0);
    let quant_limit = body.get("quant_limit").and_then(|v| v.as_i64()).unwrap_or(0);
    let counts = pull_master_data(&state, product_limit, quant_limit)
        .await
        .map_err(upstream)?;
    Ok(Json(json!({ "ok": true, "synced": counts })))
}

/// POST /api/odoo/bridge-products — map the odoo_product mirror into 9eck's
/// native `product` table (merge by default_code, create the rest). Idempotent;
/// writes only to the local mesh DB. Run a `/sync` first so the mirror is fresh.
pub async fn bridge_products(State(state): State<Arc<AppState>>) -> ApiResult {
    let res = crate::services::odoo::bridge_products(&state)
        .await
        .map_err(upstream)?;
    Ok(Json(json!({ "ok": true, "bridged": res })))
}

/// POST /api/odoo/project/run — roll up 9eck on-hand per (product, warehouse)
/// and push the changed totals into Odoo (batched, dirty-diff). Manual trigger
/// for the same job the hourly scheduler runs. Self-guards on
/// `ODOO_WRITE_ENABLED` (returns `{skipped}` when off).
pub async fn project_run(State(state): State<Arc<AppState>>) -> ApiResult {
    let res = crate::services::odoo::project_dirty(&state.db)
        .await
        .map_err(upstream)?;
    Ok(Json(json!({ "ok": true, "projection": res })))
}

/// POST /api/odoo/project/set-onhand — push a counted on-hand quantity into
/// Odoo as an inventory adjustment ("change the number"). GUARDED by env
/// `ODOO_WRITE_ENABLED=true`, else 403 — so it can't fire against a production
/// Odoo by accident while `ODOO_*` points there.
///
/// Body: `{ "product_id": <int> | "default_code": "<ref>", "location_id": <int>,
///          "qty": <float>, "lot_id": <int>? }`.
pub async fn set_onhand(State(_state): State<Arc<AppState>>, Json(body): Json<Value>) -> ApiResult {
    if std::env::var("ODOO_WRITE_ENABLED").ok().as_deref() != Some("true") {
        return Err((
            StatusCode::FORBIDDEN,
            "Odoo writes disabled — set ODOO_WRITE_ENABLED=true to enable".into(),
        ));
    }
    let client = OdooClient::from_env().await.map_err(upstream)?;
    let product_id = if let Some(pid) = body.get("product_id").and_then(|v| v.as_i64()) {
        pid
    } else if let Some(code) = body.get("default_code").and_then(|v| v.as_str()) {
        client
            .product_id_by_code(code)
            .await
            .map_err(upstream)?
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no product with default_code {}", code)))?
    } else {
        return Err((StatusCode::BAD_REQUEST, "need product_id or default_code".into()));
    };
    let location_id = body
        .get("location_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "need location_id".into()))?;
    let qty = body
        .get("qty")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "need qty".into()))?;
    let lot_id = body.get("lot_id").and_then(|v| v.as_i64());
    let applied = client
        .set_on_hand(product_id, location_id, qty, lot_id)
        .await
        .map_err(upstream)?;
    Ok(Json(json!({ "ok": true, "applied": applied })))
}
