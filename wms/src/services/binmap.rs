//! Learn-by-scan bin map (put-away).
//!
//! Odoo has no bin-level addressing for the customer warehouse (0 putaway rules,
//! 22 coarse virtual locations). 9eck builds the "where things lie" map
//! organically: a technician scans a *part* (its barcode encodes the Odoo
//! `default_code`) then the *shelf* (its pre-existing physical barcode), and we
//! record the on-hand `quant` at that `location`. Unknown shelves are minted on
//! first scan — that is the "learn" in learn-by-scan.
//!
//! Parts are NOT invented here: they must already exist in the catalog (bridged
//! from Odoo), so the projection back to Odoo stays anchored to real product
//! ids. `location` and `quant` are both in `SYNC_ENTITY_TYPES`, so every
//! placement propagates across the mesh via the live-watcher.

use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::AppState;

fn one() -> f64 {
    1.0
}

#[derive(Deserialize)]
pub struct PutAway {
    /// Barcode read off the part — matched against `product.barcode` /
    /// `default_code`. Must already exist in the catalog.
    pub item_barcode: String,
    /// Barcode read off the shelf/bin — matched against `location.barcode`;
    /// minted as a new `location` if unseen (the "learn" step).
    pub shelf_barcode: String,
    /// Quantity, interpreted per `op`.
    #[serde(default = "one")]
    pub qty: f64,
    /// `"set"` (default) = the counted on-hand at this bin; `"add"` = a delta
    /// (+/-). Default is `set` so retries are idempotent and the absolute
    /// on-hand stays safe to project into Odoo; the PDA can send `add` for an
    /// increment-per-scan UX.
    #[serde(default)]
    pub op: Option<String>,
    /// Optional warehouse code (e.g. `"WH008"`) stamped on a freshly-minted bin.
    #[serde(default)]
    pub warehouse: Option<String>,
    /// Optional serial/lot for serialized parts (own quant per lot).
    #[serde(default)]
    pub lot: Option<String>,
}

/// Keep `[A-Za-z0-9_]`, collapse every other run to a single `_`. Builds a
/// deterministic, record-id-safe leaf from a free-form barcode.
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_us = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Resolve part + shelf and record the on-hand `quant` at that bin.
pub async fn put_away(state: &AppState, req: PutAway) -> Result<Value, String> {
    let item = req.item_barcode.trim().to_string();
    let shelf = req.shelf_barcode.trim().to_string();
    if item.is_empty() || shelf.is_empty() {
        return Err("item_barcode and shelf_barcode are required".into());
    }
    let now = chrono::Utc::now().to_rfc3339();

    // 1) Resolve the part — strictly. It must already be in the catalog so the
    //    Odoo projection has a real product id to hang the number on.
    let prods: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS rid, name, default_code \
             FROM product WHERE barcode = $b OR default_code = $b LIMIT 2",
        )
        .bind(("b", item.clone()))
        .await
        .map_err(|e| format!("product lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("product lookup: {}", e))?;
    if prods.is_empty() {
        return Err(format!(
            "unknown part '{}' — not in catalog; run /api/odoo/sync + bridge-products first",
            item
        ));
    }
    if prods.len() > 1 {
        return Err(format!("ambiguous part '{}' — {} catalog matches", item, prods.len()));
    }
    let prod = &prods[0];
    let prod_leaf = prod.get("rid").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let prod_name = prod.get("name").cloned().unwrap_or(Value::Null);
    let prod_code = prod.get("default_code").cloned().unwrap_or(Value::Null);

    // 2) Resolve the shelf by its barcode, or mint it (learn-by-scan).
    let existing_loc: Vec<Value> = state
        .db
        .query("SELECT record::id(id) AS rid FROM location WHERE barcode = $b LIMIT 1")
        .bind(("b", shelf.clone()))
        .await
        .map_err(|e| format!("location lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("location lookup: {}", e))?;

    let wh = req.warehouse.clone().unwrap_or_default();
    let (loc_leaf, loc_created) = if let Some(l) = existing_loc.first() {
        (l.get("rid").and_then(|v| v.as_str()).unwrap_or_default().to_string(), false)
    } else {
        let leaf = format!("bin_{}", slug(&shelf));
        let complete_name = if wh.is_empty() {
            shelf.clone()
        } else {
            format!("{}/{}", wh, shelf)
        };
        let content = json!({
            "barcode": shelf,
            "name": shelf,
            "complete_name": complete_name,
            "warehouse_code": wh,
            "usage": "internal",
            "kind": "bin",
            "source": "scan",
            "created_at": now,
            "updated_at": now,
        });
        let mut resp = state
            .db
            .query("UPSERT type::record($rid) CONTENT $content")
            .bind(("rid", format!("location:{}", leaf)))
            .bind(("content", content))
            .await
            .map_err(|e| format!("mint location: {}", e))?;
        let _: Vec<Value> = resp.take(0).map_err(|e| format!("mint location apply: {}", e))?;
        (leaf, true)
    };

    // 3) Upsert the on-hand quant at (bin, part[, lot]) under a deterministic id
    //    so a re-scan updates in place instead of duplicating.
    let op = req.op.as_deref().unwrap_or("set");
    let lot_slug = req
        .lot
        .as_deref()
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!("__{}", slug(l)))
        .unwrap_or_default();
    let quant_leaf = format!("{}__{}{}", slug(&loc_leaf), slug(&prod_leaf), lot_slug);
    let quant_rid = format!("quant:{}", quant_leaf);

    let prev: Vec<Value> = state
        .db
        .query("SELECT qty FROM type::record($rid)")
        .bind(("rid", quant_rid.clone()))
        .await
        .map_err(|e| format!("read quant: {}", e))?
        .take(0)
        .map_err(|e| format!("read quant: {}", e))?;
    let existed = !prev.is_empty();
    let prev_qty = prev.first().and_then(|v| v.get("qty")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let new_qty = if op == "add" { prev_qty + req.qty } else { req.qty };

    let mut quant = json!({
        "product_id": prod_leaf,
        "location_id": loc_leaf,
        "default_code": prod_code,
        "product_name": prod_name,
        "shelf_barcode": shelf,
        "warehouse_code": wh,
        "lot": req.lot,
        "qty": new_qty,
        "source": "scan",
        "updated_at": now,
    });
    if !existed {
        quant["created_at"] = json!(now);
    }
    let mut resp = state
        .db
        .query("UPSERT type::record($rid) MERGE $q")
        .bind(("rid", quant_rid))
        .bind(("q", quant))
        .await
        .map_err(|e| format!("upsert quant: {}", e))?;
    let _: Vec<Value> = resp.take(0).map_err(|e| format!("upsert quant apply: {}", e))?;

    info!(
        "[binmap] put_away part={:?} bin={} op={} {}->{}",
        prod_code, shelf, op, prev_qty, new_qty
    );
    // Best-effort push so open dashboards (Inventory table) refresh live —
    // no subscribers is not an error.
    let _ = state.ws_tx.send(
        json!({
            "type": "INVENTORY_UPDATED",
            "source": "put_away",
            "location_id": loc_leaf,
            "product_id": prod_leaf,
            "default_code": prod_code.clone(),
            "qty": new_qty,
        })
        .to_string(),
    );
    Ok(json!({
        "product": { "id": prod_leaf, "name": prod_name, "default_code": prod_code },
        "location": { "id": loc_leaf, "barcode": shelf, "created": loc_created },
        "quant": { "id": quant_leaf, "op": op, "qty_before": prev_qty, "qty": new_qty },
    }))
}

// ─── take / consume (part removal) ───────────────────────────────────────────

/// Take `qty` of a part out of a bin — the mirror of [`put_away`], used by
/// repair consumption (and later, picking). Purely local + mesh; the Odoo
/// projection is a separate, guarded step. Never drives the bin negative: the
/// amount actually removed is clamped to what's on the shelf and any remainder
/// is reported as `shortfall` so a data gap surfaces instead of a lie.
#[derive(Deserialize)]
pub struct TakeReq {
    pub item_barcode: String,
    /// Optional — when omitted, the bin is auto-resolved if the part sits in
    /// exactly one bin (else the caller must disambiguate by scanning the shelf).
    #[serde(default)]
    pub shelf_barcode: Option<String>,
    #[serde(default = "one")]
    pub qty: f64,
}

/// Structured outcome of a [`take`].
pub struct Taken {
    pub product_id: String,
    pub product_name: Value,
    pub default_code: Value,
    pub location_id: String,
    pub shelf_barcode: Value,
    pub qty_before: f64,
    pub qty_taken: f64,
    pub shortfall: f64,
    pub bin_qty_after: f64,
    pub product_onhand_after: f64,
}

/// Failure modes a caller maps to HTTP status codes.
pub enum TakeError {
    UnknownPart(String),
    AmbiguousPart(String),
    NoStock(String),
    /// The part is in several bins — payload is the candidate bin list.
    AmbiguousBin(Vec<Value>),
    Bad(String),
    Db(String),
}

/// Resolve a part strictly against the catalog (never invented).
async fn resolve_part(state: &AppState, item: &str) -> Result<(String, Value, Value), TakeError> {
    let prods: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS rid, name, default_code \
             FROM product WHERE barcode = $b OR default_code = $b LIMIT 2",
        )
        .bind(("b", item.to_string()))
        .await
        .map_err(|e| TakeError::Db(format!("product lookup: {}", e)))?
        .take(0)
        .map_err(|e| TakeError::Db(format!("product lookup: {}", e)))?;
    if prods.is_empty() {
        return Err(TakeError::UnknownPart(format!(
            "unknown part '{}' — not in catalog",
            item
        )));
    }
    if prods.len() > 1 {
        return Err(TakeError::AmbiguousPart(format!(
            "ambiguous part '{}' — {} catalog matches",
            item,
            prods.len()
        )));
    }
    let p = &prods[0];
    Ok((
        p.get("rid").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        p.get("name").cloned().unwrap_or(Value::Null),
        p.get("default_code").cloned().unwrap_or(Value::Null),
    ))
}

/// Sum the on-hand across every bin holding this part.
async fn product_onhand(state: &AppState, product_leaf: &str) -> Result<f64, TakeError> {
    let qtys: Vec<f64> = state
        .db
        .query("SELECT VALUE qty FROM quant WHERE product_id = $p AND qty != NONE")
        .bind(("p", product_leaf.to_string()))
        .await
        .map_err(|e| TakeError::Db(format!("onhand sum: {}", e)))?
        .take(0)
        .map_err(|e| TakeError::Db(format!("onhand sum: {}", e)))?;
    Ok(qtys.iter().sum())
}

pub async fn take(state: &AppState, req: TakeReq) -> Result<Taken, TakeError> {
    let item = req.item_barcode.trim().to_string();
    if item.is_empty() {
        return Err(TakeError::Bad("item_barcode is required".into()));
    }
    if req.qty <= 0.0 {
        return Err(TakeError::Bad("qty must be positive".into()));
    }
    let now = chrono::Utc::now().to_rfc3339();

    let (prod_leaf, prod_name, prod_code) = resolve_part(state, &item).await?;

    // Resolve the source bin: explicit shelf, else the part's sole bin.
    let (loc_leaf, mut shelf_bc) = if let Some(shelf) =
        req.shelf_barcode.as_deref().map(str::trim).filter(|s| !s.is_empty())
    {
        let rows: Vec<Value> = state
            .db
            .query("SELECT record::id(id) AS rid FROM location WHERE barcode = $b LIMIT 1")
            .bind(("b", shelf.to_string()))
            .await
            .map_err(|e| TakeError::Db(format!("location lookup: {}", e)))?
            .take(0)
            .map_err(|e| TakeError::Db(format!("location lookup: {}", e)))?;
        let l = rows
            .into_iter()
            .next()
            .ok_or_else(|| TakeError::NoStock(format!("unknown shelf '{}'", shelf)))?;
        (
            l.get("rid").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            json!(shelf),
        )
    } else {
        let bins: Vec<Value> = state
            .db
            .query("SELECT location_id, shelf_barcode, qty FROM quant WHERE product_id = $p AND qty > 0")
            .bind(("p", prod_leaf.clone()))
            .await
            .map_err(|e| TakeError::Db(format!("bins lookup: {}", e)))?
            .take(0)
            .map_err(|e| TakeError::Db(format!("bins lookup: {}", e)))?;
        match bins.len() {
            0 => {
                return Err(TakeError::NoStock(format!(
                    "no stock recorded for '{}' in any bin — put it away first",
                    item
                )))
            }
            1 => {
                let b = &bins[0];
                (
                    b.get("location_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    b.get("shelf_barcode").cloned().unwrap_or(Value::Null),
                )
            }
            _ => return Err(TakeError::AmbiguousBin(bins)),
        }
    };

    let quant_leaf = format!("{}__{}", slug(&loc_leaf), slug(&prod_leaf));
    let quant_rid = format!("quant:{}", quant_leaf);

    let prev: Vec<Value> = state
        .db
        .query("SELECT qty, shelf_barcode FROM type::record($rid)")
        .bind(("rid", quant_rid.clone()))
        .await
        .map_err(|e| TakeError::Db(format!("read quant: {}", e)))?
        .take(0)
        .map_err(|e| TakeError::Db(format!("read quant: {}", e)))?;
    let existed = !prev.is_empty();
    let qty_before = prev.first().and_then(|v| v.get("qty")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    if let Some(bc) = prev.first().and_then(|v| v.get("shelf_barcode")).filter(|v| !v.is_null()) {
        shelf_bc = bc.clone();
    }

    let available = qty_before.max(0.0);
    let qty_taken = req.qty.min(available);
    let shortfall = req.qty - qty_taken;
    let bin_qty_after = qty_before - qty_taken;

    // Only touch the quant if there was something there (or a row already
    // exists) — don't mint an empty 0-qty placement just to record a miss.
    if existed || qty_taken > 0.0 {
        let patch = json!({ "qty": bin_qty_after, "updated_at": now, "source": "scan" });
        let mut resp = state
            .db
            .query("UPSERT type::record($rid) MERGE $p")
            .bind(("rid", quant_rid))
            .bind(("p", patch))
            .await
            .map_err(|e| TakeError::Db(format!("decrement quant: {}", e)))?;
        let _: Vec<Value> = resp.take(0).map_err(|e| TakeError::Db(format!("decrement quant apply: {}", e)))?;
    }

    let product_onhand_after = product_onhand(state, &prod_leaf).await?;

    info!(
        "[binmap] take part={:?} bin={} req={} taken={} shortfall={} bin_after={} onhand={}",
        prod_code, loc_leaf, req.qty, qty_taken, shortfall, bin_qty_after, product_onhand_after
    );

    let _ = state.ws_tx.send(
        json!({
            "type": "INVENTORY_UPDATED",
            "source": "take",
            "location_id": loc_leaf,
            "product_id": prod_leaf,
            "default_code": prod_code.clone(),
            "qty": bin_qty_after,
        })
        .to_string(),
    );

    Ok(Taken {
        product_id: prod_leaf,
        product_name: prod_name,
        default_code: prod_code,
        location_id: loc_leaf,
        shelf_barcode: shelf_bc,
        qty_before,
        qty_taken,
        shortfall,
        bin_qty_after,
        product_onhand_after,
    })
}
