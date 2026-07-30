//! Odoo external-API connector (JSON-RPC).
//!
//! The tenant's warehouse lives in a hosted Odoo 19 Enterprise we cannot extend
//! with custom modules — only the external API is available. This client speaks
//! Odoo JSON-RPC (`/jsonrpc`, service `object` → `execute_kw`) over reqwest, so
//! the 9eck mesh can pull master data (warehouses, locations, products, lots,
//! quants) and — later — push stock movements back as internal transfers /
//! inventory adjustments.
//!
//! Config is read from the environment (see [`OdooConfig::from_env`]):
//! ```text
//!   ODOO_URL      base URL, e.g. https://erp.example.com
//!   ODOO_DB       database name, e.g. companydb
//!   ODOO_LOGIN    user login, e.g. bot@example.com
//!   ODOO_API_KEY  an Odoo API key (My Profile → Account Security → API Keys)
//! ```
//! All operations here are READ-ONLY against Odoo. Writing to the live Odoo
//! instance is deliberately out of scope for this first cut — that path is to
//! be validated against the local sandbox first.

use std::collections::HashMap;
use std::time::Duration;

use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use tracing::info;

use crate::AppState;

#[derive(Clone, Debug)]
pub struct OdooConfig {
    pub url: String,
    pub db: String,
    pub login: String,
    pub api_key: String,
}

impl OdooConfig {
    /// Build from env. Returns `None` if any required var is missing/empty so
    /// callers can treat "no Odoo configured" as a soft no-op.
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Some(Self {
            url: get("ODOO_URL")?.trim_end_matches('/').to_string(),
            db: get("ODOO_DB")?,
            login: get("ODOO_LOGIN")?,
            api_key: get("ODOO_API_KEY")?,
        })
    }
}

/// A connected Odoo JSON-RPC client. `uid` is resolved once at connect time via
/// `common.authenticate` and reused for every subsequent `execute_kw`.
pub struct OdooClient {
    http: reqwest::Client,
    cfg: OdooConfig,
    uid: i64,
}

impl OdooClient {
    fn http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    }

    /// Authenticate and return a ready client. Errors surface as plain strings
    /// to match the crate's handler conventions.
    pub async fn connect(cfg: OdooConfig) -> Result<Self, String> {
        let http = Self::http_client();
        let uid = Self::authenticate(&http, &cfg).await?;
        Ok(Self { http, cfg, uid })
    }

    /// Connect using env config, or `Err` if unconfigured.
    pub async fn from_env() -> Result<Self, String> {
        let cfg = OdooConfig::from_env().ok_or_else(|| {
            "Odoo not configured (set ODOO_URL / ODOO_DB / ODOO_LOGIN / ODOO_API_KEY)".to_string()
        })?;
        Self::connect(cfg).await
    }

    pub fn uid(&self) -> i64 {
        self.uid
    }

    /// Low-level JSON-RPC call to `<url>/jsonrpc`. Returns the `result` value
    /// or an error built from Odoo's `error` object.
    async fn jsonrpc_on(
        http: &reqwest::Client,
        url: &str,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": "call",
            "params": { "service": service, "method": method, "args": args },
            "id": 1,
        });
        let resp = http
            .post(format!("{}/jsonrpc", url))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("odoo jsonrpc send: {}", e))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("odoo jsonrpc parse (HTTP {}): {}", status, e))?;
        if let Some(err) = v.get("error") {
            let msg = err
                .get("data")
                .and_then(|d| d.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| err.get("message").and_then(|m| m.as_str()))
                .unwrap_or("unknown odoo error");
            return Err(format!("odoo error: {}", msg));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn jsonrpc(&self, service: &str, method: &str, args: Value) -> Result<Value, String> {
        Self::jsonrpc_on(&self.http, &self.cfg.url, service, method, args).await
    }

    async fn authenticate(http: &reqwest::Client, cfg: &OdooConfig) -> Result<i64, String> {
        let res = Self::jsonrpc_on(
            http,
            &cfg.url,
            "common",
            "authenticate",
            json!([cfg.db, cfg.login, cfg.api_key, {}]),
        )
        .await?;
        match res.as_i64() {
            Some(uid) if uid > 0 => Ok(uid),
            _ => Err("odoo authenticate failed (check db / login / api_key)".to_string()),
        }
    }

    /// `version()` — unauthenticated; handy for a connectivity probe.
    pub async fn server_version(&self) -> Result<Value, String> {
        self.jsonrpc("common", "version", json!([])).await
    }

    /// Generic `execute_kw(model, method, args, kwargs)`.
    pub async fn execute_kw(
        &self,
        model: &str,
        method: &str,
        args: Value,
        kwargs: Value,
    ) -> Result<Value, String> {
        self.jsonrpc(
            "object",
            "execute_kw",
            json!([self.cfg.db, self.uid, self.cfg.api_key, model, method, args, kwargs]),
        )
        .await
    }

    /// `search_read(domain, fields, limit)`. `domain` is the full Odoo domain
    /// list (e.g. `json!([["quantity","!=",0]])`); `limit = 0` means no limit.
    pub async fn search_read(
        &self,
        model: &str,
        domain: Value,
        fields: &[&str],
        limit: i64,
    ) -> Result<Vec<Value>, String> {
        let kwargs = json!({ "fields": fields, "limit": limit });
        let res = self
            .execute_kw(model, "search_read", json!([domain]), kwargs)
            .await?;
        Ok(res.as_array().cloned().unwrap_or_default())
    }

    pub async fn search_count(&self, model: &str, domain: Value) -> Result<i64, String> {
        let res = self
            .execute_kw(model, "search_count", json!([domain]), json!({}))
            .await?;
        Ok(res.as_i64().unwrap_or(0))
    }

    // ─── write side (projection back into Odoo) ──────────────────────────────
    // All of these mutate Odoo. They are only reachable through the guarded
    // `/api/odoo/project/*` handlers (ODOO_WRITE_ENABLED), never from a sync.

    fn ctx_kwargs(context: &Value) -> Value {
        if context.is_null() {
            json!({})
        } else {
            json!({ "context": context })
        }
    }

    /// `create(model, values)` → new record id.
    pub async fn create_record(
        &self,
        model: &str,
        values: Value,
        context: Value,
    ) -> Result<i64, String> {
        let res = self
            .execute_kw(model, "create", json!([values]), Self::ctx_kwargs(&context))
            .await?;
        res.as_i64()
            .ok_or_else(|| format!("create {} returned non-int id: {}", model, res))
    }

    /// `write(model, ids, values)` → bool.
    pub async fn write_record(
        &self,
        model: &str,
        ids: Vec<i64>,
        values: Value,
        context: Value,
    ) -> Result<bool, String> {
        let res = self
            .execute_kw(model, "write", json!([ids, values]), Self::ctx_kwargs(&context))
            .await?;
        Ok(res.as_bool().unwrap_or(false))
    }

    /// Call an arbitrary model method on a recordset (e.g. a button action
    /// like `action_apply_inventory`).
    pub async fn call_method(
        &self,
        model: &str,
        method: &str,
        ids: Vec<i64>,
        context: Value,
    ) -> Result<Value, String> {
        self.execute_kw(model, method, json!([ids]), Self::ctx_kwargs(&context))
            .await
    }

    /// Resolve an Odoo product *variant* id by its internal reference
    /// (`default_code`). The tenant's catalogue carries `default_code`, not
    /// `barcode`, so this is the primary product key for the projection.
    pub async fn product_id_by_code(&self, default_code: &str) -> Result<Option<i64>, String> {
        let rows = self
            .search_read(
                "product.product",
                json!([["default_code", "=", default_code]]),
                &["id"],
                1,
            )
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("id"))
            .and_then(|v| v.as_i64()))
    }

    /// Project an on-hand quantity into Odoo as an **inventory adjustment**:
    /// set the counted quantity for (product, location[, lot]) and apply it,
    /// so Odoo books the corrective stock move. This is the "change the number"
    /// primitive of the 9eck → Odoo projection.
    ///
    /// Uses the `inventory_mode` context so `inventory_quantity` is writable,
    /// then `action_apply_inventory` to commit. NOTE: the exact incantation
    /// (inventory_mode create + apply) must be validated against a sandbox
    /// before trusting it on real data.
    pub async fn set_on_hand(
        &self,
        product_id: i64,
        location_id: i64,
        counted_qty: f64,
        lot_id: Option<i64>,
    ) -> Result<Value, String> {
        let ctx = json!({ "inventory_mode": true });
        let mut vals = json!({
            "product_id": product_id,
            "location_id": location_id,
            "inventory_quantity": counted_qty,
        });
        if let Some(lot) = lot_id {
            vals["lot_id"] = json!(lot);
        }
        let quant_id = self.create_record("stock.quant", vals, ctx.clone()).await?;
        self.call_method("stock.quant", "action_apply_inventory", vec![quant_id], ctx)
            .await?;
        Ok(json!({
            "quant_id": quant_id,
            "product_id": product_id,
            "location_id": location_id,
            "counted_qty": counted_qty,
        }))
    }
}

/// Upsert raw Odoo rows into a local mesh table keyed by the Odoo integer id
/// (`<table>:<odoo_id>`). Schemaless — the row is stored verbatim, including
/// its many2one `[id, "name"]` pairs, so callers can correlate later.
async fn upsert_all(state: &AppState, table: &str, rows: &[Value]) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    for row in rows {
        let id = row
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| format!("{}: row missing integer id", table))?;
        let rid = format!("{}:{}", table, id);
        // Read the existing `_vclock` and advance THIS node's component. CONTENT
        // replaces the whole record, so without re-stamping it the clock resets to
        // null every import — a null/stale clock can't dominate on a peer, so the
        // synced row is dropped as "local wins/equal" and the mesh churns re-pushing
        // it forever (the partner/product/file_resource non-convergence class).
        let existing: Option<Value> = state
            .db
            .query("SELECT _vclock FROM type::record($rid) LIMIT 1")
            .bind(("rid", rid.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .ok()
            .flatten();
        let vclock = eck_core::sync::conflict::next_local_vclock(
            existing.as_ref().and_then(|v| v.get("_vclock")),
            &state.instance_id,
        );
        // Stamp `updated_at` for the LWW tiebreaker on mesh re-sync. It is in
        // `merkle::IGNORED_FIELDS`, so it never changes the content hash (no
        // re-sync storm on identical content) but lets a genuinely changed
        // re-pull win over a peer's stale copy.
        let mut data = row.clone();
        if let Some(obj) = data.as_object_mut() {
            obj.insert("updated_at".to_string(), serde_json::json!(now));
            obj.insert("_vclock".to_string(), vclock);
        }
        // `.query().await` only reports transport errors; a per-statement DB
        // error surfaces on `.take()`, so take(0) to actually assert success.
        let mut resp = state
            .db
            .query("UPSERT type::record($rid) CONTENT $data")
            .bind(("rid", rid))
            .bind(("data", data))
            .await
            .map_err(|e| format!("{} upsert: {}", table, e))?;
        let _: Vec<Value> = resp
            .take(0)
            .map_err(|e| format!("{} upsert apply: {}", table, e))?;
    }
    Ok(())
}

/// Pull warehouses, locations, products and on-hand quants from Odoo and
/// upsert them into local `odoo_*` tables. Read-only against Odoo.
/// `product_limit` / `quant_limit` bound the first sync (`0` = unbounded).
///
/// NOTE: these tables are persisted locally only; wiring them into merkle mesh
/// propagation (vclock bumps like other entities) is a follow-up step.
pub async fn pull_master_data(
    state: &AppState,
    product_limit: i64,
    quant_limit: i64,
) -> Result<Value, String> {
    let client = OdooClient::from_env().await?;

    let warehouses = client
        .search_read(
            "stock.warehouse",
            json!([]),
            &["name", "code", "lot_stock_id", "view_location_id", "in_type_id", "out_type_id", "int_type_id"],
            0,
        )
        .await?;
    upsert_all(state, "odoo_warehouse", &warehouses).await?;

    let locations = client
        .search_read(
            "stock.location",
            json!([]),
            &["complete_name", "name", "usage", "location_id"],
            0,
        )
        .await?;
    upsert_all(state, "odoo_location", &locations).await?;

    let products = client
        .search_read(
            "product.product",
            json!([]),
            &[
                "default_code", "barcode", "name", "tracking", "uom_id",
                "categ_id", "type", "list_price", "active", "company_id",
            ],
            product_limit,
        )
        .await?;
    upsert_all(state, "odoo_product", &products).await?;

    let quants = client
        .search_read(
            "stock.quant",
            json!([["quantity", "!=", 0]]),
            &["product_id", "location_id", "lot_id", "quantity", "reserved_quantity"],
            quant_limit,
        )
        .await?;
    upsert_all(state, "odoo_quant", &quants).await?;

    let counts = json!({
        "warehouses": warehouses.len(),
        "locations": locations.len(),
        "products": products.len(),
        "quants": quants.len(),
    });
    info!("[odoo] pull_master_data synced {}", counts);
    Ok(counts)
}

/// A many2one comes back as `[id, "Display Name"]` (or `false`). Return the name.
fn many2one_name(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Array(a)) if a.len() == 2 => a[1].clone(),
        _ => Value::Null,
    }
}

/// Extract the integer leaf of a record-id string like `"odoo_product:123"`.
fn record_leaf_int(v: &Value) -> Option<i64> {
    v.as_str()?
        .rsplit(':')
        .next()?
        .trim_matches('`')
        .parse::<i64>()
        .ok()
}

/// Bridge the `odoo_product` mirror into 9eck's native `product` table.
///
/// Match an existing 9eck product by `default_code` (merge + link the Odoo id),
/// fall back to the `odoo_id` link for the few products without a code, and
/// create the rest as `product:odoo_<id>`. Idempotent: a re-run re-finds the
/// row by code / odoo_id and updates in place — no duplicates. Sets `updated_at`
/// for the mesh LWW tiebreaker (no manual vclock bump — matches the existing
/// `product` CRUD convention).
pub async fn bridge_products(state: &AppState) -> Result<Value, String> {
    let rows: Vec<Value> = state
        .db
        .query("SELECT * FROM odoo_product")
        .await
        .map_err(|e| format!("load odoo_product: {}", e))?
        .take(0)
        .map_err(|e| format!("load odoo_product: {}", e))?;

    let now = chrono::Utc::now().to_rfc3339();
    let (mut merged, mut created, mut skipped) = (0u64, 0u64, 0u64);

    for r in &rows {
        let Some(odoo_id) = r.get("id").and_then(record_leaf_int) else {
            skipped += 1;
            continue;
        };
        let default_code = r
            .get("default_code")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let name = r.get("name").cloned().unwrap_or(Value::Null);
        let list_price = r.get("list_price").cloned().unwrap_or(Value::Null);
        let active = r.get("active").cloned().unwrap_or(json!(true));
        let category = many2one_name(r.get("categ_id"));
        let ptype = r.get("type").cloned().unwrap_or(Value::Null);

        // Locate an existing 9eck product: by default_code, else by odoo_id link.
        let existing: Vec<Value> = if let Some(code) = &default_code {
            state
                .db
                .query("SELECT VALUE id FROM product WHERE default_code = $c LIMIT 1")
                .bind(("c", code.clone()))
                .await
                .map_err(|e| format!("match by code: {}", e))?
                .take(0)
                .map_err(|e| format!("match by code: {}", e))?
        } else {
            state
                .db
                .query("SELECT VALUE id FROM product WHERE odoo_id = $oid LIMIT 1")
                .bind(("oid", odoo_id))
                .await
                .map_err(|e| format!("match by odoo_id: {}", e))?
                .take(0)
                .map_err(|e| format!("match by odoo_id: {}", e))?
        };
        let existing_id = existing.into_iter().next().and_then(|v| v.as_str().map(String::from));

        if let Some(rid) = existing_id {
            // Merge the Odoo-sourced fields onto the existing 9eck product.
            let patch = json!({
                "odoo_id": odoo_id,
                "name": name,
                "list_price": list_price,
                "odoo_category": category,
                "odoo_type": ptype,
                "source": "odoo",
                "updated_at": now,
            });
            let mut resp = state
                .db
                .query("UPDATE type::record($rid) MERGE $patch")
                .bind(("rid", rid))
                .bind(("patch", patch))
                .await
                .map_err(|e| format!("merge product: {}", e))?;
            let _: Vec<Value> = resp.take(0).map_err(|e| format!("merge product apply: {}", e))?;
            merged += 1;
        } else {
            let rid = format!("product:odoo_{}", odoo_id);
            let content = json!({
                "default_code": default_code,
                "name": name,
                "list_price": list_price,
                "active": active,
                "odoo_id": odoo_id,
                "odoo_category": category,
                "odoo_type": ptype,
                "source": "odoo",
                "updated_at": now,
            });
            let mut resp = state
                .db
                .query("UPSERT type::record($rid) CONTENT $content")
                .bind(("rid", rid))
                .bind(("content", content))
                .await
                .map_err(|e| format!("create product: {}", e))?;
            let _: Vec<Value> = resp.take(0).map_err(|e| format!("create product apply: {}", e))?;
            created += 1;
        }
    }

    let summary = json!({
        "total": rows.len(),
        "merged": merged,
        "created": created,
        "skipped": skipped,
    });
    info!("[odoo] bridge_products {}", summary);
    Ok(summary)
}

// ─── projection back into Odoo (batched, dirty-diff) ─────────────────────────

/// The int id of a many2one `[id, "Name"]` pair (or None).
fn many2one_id(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Array(a)) if !a.is_empty() => a[0].as_i64(),
        _ => None,
    }
}

/// Sanitize a free-form string to a record-id-safe leaf.
fn id_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut us = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            us = false;
        } else if !us {
            out.push('_');
            us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Push changed on-hand totals into Odoo as inventory adjustments — **batched,
/// dirty-diff**. Rolls up `quant.qty` per (product, warehouse); for each group
/// whose total changed since the last projection, calls `set_on_hand` against
/// that warehouse's Odoo stock location. Bookkeeping of what was pushed lives in
/// the local (non-mesh) `odoo_projection` table so only genuine changes go out.
///
/// Self-guards on `ODOO_WRITE_ENABLED` so the hourly scheduler is a safe no-op
/// until the operator turns writes on. Only the node holding the flag + `ODOO_*`
/// config projects, so there's a single writer (no double-push across the mesh).
pub async fn project_dirty(db: &SurrealDb) -> Result<Value, String> {
    if std::env::var("ODOO_WRITE_ENABLED").ok().as_deref() != Some("true") {
        return Ok(json!({ "skipped": "ODOO_WRITE_ENABLED != true" }));
    }
    let client = OdooClient::from_env().await?;
    let now = chrono::Utc::now().to_rfc3339();

    // Ensure the local bookkeeping table exists (SurrealDB errors on a SELECT
    // from a table that was never defined/written).
    db.query("DEFINE TABLE IF NOT EXISTS odoo_projection SCHEMALESS")
        .await
        .map_err(|e| format!("define odoo_projection: {}", e))?;

    // warehouse code -> Odoo stock location id
    let whs: Vec<Value> = db
        .query("SELECT code, lot_stock_id FROM odoo_warehouse")
        .await
        .map_err(|e| format!("load warehouses: {}", e))?
        .take(0)
        .map_err(|e| format!("load warehouses: {}", e))?;
    let mut wh_map: HashMap<String, i64> = HashMap::new();
    for w in &whs {
        if let (Some(code), Some(loc)) =
            (w.get("code").and_then(|v| v.as_str()), many2one_id(w.get("lot_stock_id")))
        {
            wh_map.insert(code.to_string(), loc);
        }
    }

    // product leaf -> Odoo product id
    let prods: Vec<Value> = db
        .query("SELECT record::id(id) AS pl, odoo_id FROM product WHERE odoo_id != NONE")
        .await
        .map_err(|e| format!("load products: {}", e))?
        .take(0)
        .map_err(|e| format!("load products: {}", e))?;
    let mut prod_map: HashMap<String, i64> = HashMap::new();
    for p in &prods {
        if let (Some(pl), Some(oid)) =
            (p.get("pl").and_then(|v| v.as_str()), p.get("odoo_id").and_then(|v| v.as_i64()))
        {
            prod_map.insert(pl.to_string(), oid);
        }
    }

    // current on-hand per (product, warehouse)
    let groups: Vec<Value> = db
        .query(
            "SELECT product_id, warehouse_code, math::sum(qty) AS qty \
             FROM quant WHERE qty != NONE GROUP BY product_id, warehouse_code",
        )
        .await
        .map_err(|e| format!("rollup: {}", e))?
        .take(0)
        .map_err(|e| format!("rollup: {}", e))?;

    let (mut checked, mut pushed, mut skipped, mut errors) = (0u64, 0u64, 0u64, 0u64);
    let mut error_samples: Vec<Value> = Vec::new();

    for g in &groups {
        checked += 1;
        let product_leaf = g.get("product_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let wh_code = g.get("warehouse_code").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let qty = g.get("qty").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let (Some(&odoo_pid), Some(&odoo_loc)) =
            (prod_map.get(&product_leaf), wh_map.get(&wh_code))
        else {
            // no Odoo product link, or unknown/blank warehouse → can't project
            skipped += 1;
            continue;
        };

        let key = format!("odoo_projection:{}__{}", id_slug(&product_leaf), id_slug(&wh_code));
        let prev: Vec<Value> = db
            .query("SELECT projected_qty FROM type::record($rid)")
            .bind(("rid", key.clone()))
            .await
            .map_err(|e| format!("read projection: {}", e))?
            .take(0)
            .map_err(|e| format!("read projection: {}", e))?;
        let prev_qty = prev.first().and_then(|v| v.get("projected_qty")).and_then(|v| v.as_f64());
        if prev_qty == Some(qty) {
            skipped += 1; // unchanged since last projection
            continue;
        }

        match client.set_on_hand(odoo_pid, odoo_loc, qty, None).await {
            Ok(_) => {
                let content = json!({
                    "product_id": product_leaf,
                    "warehouse_code": wh_code,
                    "odoo_product_id": odoo_pid,
                    "odoo_location_id": odoo_loc,
                    "projected_qty": qty,
                    "projected_at": now,
                    "last_error": Value::Null,
                });
                let mut r = db
                    .query("UPSERT type::record($rid) CONTENT $c")
                    .bind(("rid", key))
                    .bind(("c", content))
                    .await
                    .map_err(|e| format!("write projection: {}", e))?;
                let _: Vec<Value> = r.take(0).map_err(|e| format!("write projection apply: {}", e))?;
                pushed += 1;
            }
            Err(e) => {
                errors += 1;
                if error_samples.len() < 5 {
                    error_samples.push(json!({ "product": product_leaf, "wh": wh_code, "error": e }));
                }
                // Record the error but leave projected_qty stale so we retry next cycle.
                let _ = db
                    .query("UPSERT type::record($rid) MERGE $c")
                    .bind(("rid", key))
                    .bind(("c", json!({ "last_error": e, "last_error_at": now })))
                    .await;
            }
        }
    }

    let summary = json!({
        "checked": checked,
        "pushed": pushed,
        "skipped": skipped,
        "errors": errors,
        "error_samples": error_samples,
    });
    info!("[odoo] project_dirty {}", summary);
    Ok(summary)
}
