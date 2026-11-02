//! Admin/JWT HTTP surface for the claim-list legacy bridge (see
//! `services::claimlist` + the claim-list spec under `.eck/`).
//!
//! - `GET  /api/claimlist/status`          — bookkeeping row + row counts.
//! - `POST /api/claimlist/check`           — refresh + ingest, then diff.
//! - `GET  /api/claimlist/diff`            — diff the snapshot vs. our orders.
//! - `POST /api/claimlist/assign/:order_id`— mint + store the next GER number.
//! - `GET  /api/claimlist/export`          — the claim export pack (v2).
//! - `POST /api/claimlist/from-receipt/:id`— mint a repair order from a receipt.
//!
//! All are mounted behind the JWT `auth_middleware` (see `main.rs`), like the
//! Odoo handlers. The reader path is a soft no-op unless `CLAIMLIST_TOKEN_FILE`
//! is configured.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde_json::{json, Value};

use crate::services::claimlist;
use crate::AppState;

type ApiResult = Result<Json<Value>, (StatusCode, String)>;

fn upstream(e: String) -> (StatusCode, String) {
    (StatusCode::BAD_GATEWAY, e)
}

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Default product-group label for a minted claim when the ticket carries none.
/// Deployment data (the tenant's product line) — `ECK_CLAIM_PRODUCT_GROUP`,
/// empty by default so the neutral build leaves the column blank.
fn default_product_group() -> String {
    std::env::var("ECK_CLAIM_PRODUCT_GROUP")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Root of the legacy NAS document tree used ONLY for the presentation-only
/// `nas_folder` projection label (CAS stays the source of truth). Deployment
/// data — `ECK_NAS_DOCS_ROOT` (e.g. a UNC share); empty disables the label.
fn nas_docs_root() -> String {
    std::env::var("ECK_NAS_DOCS_ROOT")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// GET /api/claimlist/status — the `claimlist_state` bookkeeping row plus a live
/// count of snapshotted rows. Persists nothing.
pub async fn status(State(state): State<Arc<AppState>>) -> ApiResult {
    // Define first so the SELECTs don't error on a fresh node.
    state
        .db
        .query(
            "DEFINE TABLE IF NOT EXISTS claimlist_state SCHEMALESS; \
             DEFINE TABLE IF NOT EXISTS claimlist_row SCHEMALESS;",
        )
        .await
        .map_err(db_err)?;

    let st: Option<Value> = state
        .db
        .query("SELECT * FROM claimlist_state:main")
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let count: Option<Value> = state
        .db
        .query("SELECT count() AS c FROM claimlist_row GROUP ALL")
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let row_count = count
        .and_then(|v| v.get("c").and_then(|c| c.as_u64()))
        .unwrap_or(0);

    Ok(Json(json!({
        "configured": std::env::var("CLAIMLIST_TOKEN_FILE").ok().filter(|s| !s.trim().is_empty()).is_some(),
        "state": st.unwrap_or(Value::Null),
        "row_count": row_count,
    })))
}

/// POST /api/claimlist/check — run the reader (refresh token → metadata poll →
/// download+parse only on change), then attach a fresh diff. Self-guards on
/// `CLAIMLIST_TOKEN_FILE` (returns `{skipped}` when unset).
pub async fn check(State(state): State<Arc<AppState>>) -> ApiResult {
    let ingest = claimlist::check_and_ingest(&state.db)
        .await
        .map_err(upstream)?;
    // Diff is only meaningful once a snapshot exists; skip it when dormant.
    let diff = if ingest.get("skipped").is_some() {
        Value::Null
    } else {
        claimlist::diff_report(&state.db).await.map_err(db_err)?
    };
    Ok(Json(json!({ "ok": true, "ingest": ingest, "diff": diff })))
}

/// GET /api/claimlist/diff — diff the last snapshot against our orders (no fetch).
pub async fn diff(State(state): State<Arc<AppState>>) -> ApiResult {
    let res = claimlist::diff_report(&state.db).await.map_err(db_err)?;
    Ok(Json(res))
}

/// POST /api/claimlist/assign/:order_id — mint and store the next optimistic
/// `GERyymm-nnn` for an order. Body: `{ "claim_date"?: "<YYYY-MM-DD>" }`
/// (default = today UTC). Idempotent: if the order already has a `claim_no`,
/// it is returned unchanged.
pub async fn assign(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult {
    let order = state
        .get_synced_entity("order", &order_id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Order '{order_id}' not found")))?;

    // Idempotent: keep an already-assigned number.
    if let Some(existing) = order
        .get("claim_no")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(Json(json!({
            "ok": true,
            "claim_no": existing,
            "changed": false,
        })));
    }

    let claim_date = body
        .as_ref()
        .and_then(|b| b.get("claim_date"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());

    let claim_no = claimlist::next_claim_no(&state.db, &claim_date)
        .await
        .map_err(db_err)?;

    // `order` is a synced table — route through put_synced_entity so the write
    // advances _vclock (a raw UPDATE would leave the clock stale and peers would
    // drop the edit as "local wins/equal").
    let updated = state
        .put_synced_entity("order", &order_id, json!({ "claim_no": claim_no }))
        .await
        .map_err(db_err)?;

    Ok(Json(json!({
        "ok": true,
        "claim_no": claim_no,
        "changed": true,
        "order": updated,
    })))
}

/// GET /api/claimlist/export — the export pack the Claude-in-Excel agent turns
/// into rows (`spec: claimlist-v2`). EVERY repair carrying a GER number, not
/// just the finished ones: a "new claim" row (white) is entered as soon as the
/// device arrives and only later completed, so `stage` tells the agent which
/// lifecycle step a row is at. Raw pass-through — vocabulary mapping (Werte
/// lists) is the agent lane's job, not ours.
pub async fn export(State(state): State<Arc<AppState>>) -> ApiResult {
    let orders: Vec<Value> = state
        .db
        .query(
            "SELECT * FROM order \
             WHERE order_type = 'repair' \
             AND claim_no != NONE",
        )
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let meta_str = |o: &Value, k: &str| -> Value {
        o.get("metadata")
            .and_then(|m| m.get(k))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let field = |o: &Value, k: &str| -> Value { o.get(k).cloned().unwrap_or(Value::Null) };

    let items: Vec<Value> = orders
        .iter()
        .map(|o| {
            let status = o
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let stage = if matches!(status.as_str(), "completed" | "resolved" | "done") {
                "processed"
            } else {
                "new"
            };
            json!({
                "zoho_ticket": meta_str(o, "zoho_ticket"),
                "country": meta_str(o, "country").as_str().map(|s| s.to_string())
                    .unwrap_or_else(|| "Germany".to_string()),
                "city": meta_str(o, "city"),
                "claim_date": field(o, "claim_date"),
                "claim_no": field(o, "claim_no"),
                "cs_de_no": meta_str(o, "cs_de_no"),
                "customer": field(o, "customer_name"),
                "product_group": meta_str(o, "product_group").as_str().map(|s| s.to_string())
                    .unwrap_or_else(default_product_group),
                "product_name": field(o, "product_name"),
                "serial": field(o, "serial_number"),
                "manufacture_date": meta_str(o, "manufacture_date"),
                "purchase_date": meta_str(o, "purchase_date"),
                "defect_major": meta_str(o, "defect_major"),
                "defect_minor": meta_str(o, "defect_minor"),
                "issue": field(o, "issue_description"),
                "resolution_major": meta_str(o, "resolution_major"),
                "resolution_minor": meta_str(o, "resolution_minor"),
                "resolution_text": o.get("resolution").cloned()
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| field(o, "repair_notes")),
                "parts": field(o, "parts_used"),
                "warranty": meta_str(o, "warranty"),
                "stage": stage,
                "order_uuid": field(o, "uuid"),
                "receipt_uuid": meta_str(o, "receipt_uuid"),
                "nas_folder": meta_str(o, "nas_folder"),
                "received_date": meta_str(o, "received_date"),
                "completed_date": meta_str(o, "completed_date"),
            })
        })
        .collect();

    Ok(Json(json!({
        "generated_at": Utc::now().to_rfc3339(),
        "count": items.len(),
        "spec": "claimlist-v2",
        "rows": items,
    })))
}

// ─── POST /api/claimlist/from-receipt/:receipt_id ─────────────────────────────

/// Everything the caller may override on the order minted from a receipt. All
/// optional — whatever is absent is derived from the receipt (or left unset).
#[derive(Debug, Default, serde::Deserialize)]
pub struct FromReceiptBody {
    pub claim_no: Option<String>,
    pub claim_date: Option<String>,
    pub zoho_ticket: Option<Value>,
    pub product_name: Option<String>,
    pub product_group: Option<String>,
    pub manufacture_date: Option<String>,
    pub purchase_date: Option<String>,
    pub issue: Option<String>,
    pub defect_major: Option<String>,
    pub defect_minor: Option<String>,
    pub warranty: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub customer: Option<String>,
    pub serial: Option<String>,
    /// Which unit of the receipt this order is for (`receipt_unit` record-id
    /// leaf). When given, idempotency is keyed on the (receipt, unit) pair so a
    /// multi-device parcel can mint one order per device. When absent, the first
    /// serial-like unit is used (today's behaviour).
    pub unit_id: Option<String>,
}

/// Render a `datetime`/string field as `YYYY-MM-DD`. SurrealDB datetimes come
/// back through serde_json either as an RFC3339 string or as a single-key
/// wrapper object; anything else yields `None` (never a fabricated date).
fn as_date_string(v: Option<&Value>) -> Option<String> {
    let raw = match v? {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.values().find_map(|x| x.as_str()).map(|s| s.to_string())?,
        _ => return None,
    };
    let head: String = raw.chars().take(10).collect();
    let ok = head.len() == 10
        && head.chars().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        });
    ok.then_some(head)
}

fn trimmed(o: &Option<String>) -> Option<String> {
    o.as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The support ticket a receipt points at, if any: by `receipt.ticket_id` (the
/// `document` record-id leaf the walk-in lookup stored) or, failing that, by
/// `receipt.zoho_ticket` against the indexed `meta.ticket_number`.
///
/// A missing ticket is NOT an error — a parcel receipt has none, and a walk-in
/// whose ticket the receiver could not find still has to become a claim.
async fn ticket_meta_for_receipt(state: &AppState, receipt: &Value) -> Option<Value> {
    if let Some(tid) = str_field(receipt, "ticket_id") {
        let row: Option<Value> = state
            .db
            .query(
                "SELECT meta FROM document \
                 WHERE type = 'support_ticket' AND record::id(id) = $tid LIMIT 1",
            )
            .bind(("tid", tid))
            .await
            .ok()?
            .take(0)
            .ok()?;
        if let Some(meta) = row.and_then(|r| r.get("meta").cloned()).filter(|m| m.is_object()) {
            return Some(meta);
        }
    }
    let tn = str_field(receipt, "zoho_ticket")?;
    let tn = tn.trim_start_matches('#').trim().to_string();
    let row: Option<Value> = state
        .db
        .query(
            "SELECT meta FROM document \
             WHERE type = 'support_ticket' AND meta.ticket_number = $tn LIMIT 1",
        )
        .bind(("tn", tn))
        .await
        .ok()?
        .take(0)
        .ok()?;
    row.and_then(|r| r.get("meta").cloned())
        .filter(|m| m.is_object())
}

/// "Magic Fit Dornbirn in Dornbirn" — the customer string the legacy claim
/// sheet (and the NAS folder name) carries: firm, then the town it sits in.
/// Falls back to whichever half exists.
fn customer_in_city(company: Option<&str>, city: Option<&str>) -> Option<String> {
    match (
        company.map(str::trim).filter(|s| !s.is_empty()),
        city.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        (Some(co), Some(ci)) => Some(format!("{co} in {ci}")),
        (Some(co), None) => Some(co.to_string()),
        (None, Some(ci)) => Some(ci.to_string()),
        (None, None) => None,
    }
}

/// Country from the postcode SHAPE: Austrian zips are 4 digits, German 5.
/// Crude, and right for every claim the pilot tenant has ever filed — the two
/// countries in the sheet are Germany and Austria.
fn country_from_zip(zip: Option<&str>) -> Option<&'static str> {
    let z = zip?.trim();
    if !z.chars().all(|c| c.is_ascii_digit()) || z.is_empty() {
        return None;
    }
    match z.len() {
        4 => Some("Austria"),
        5 => Some("Germany"),
        _ => None,
    }
}

/// POST /api/claimlist/from-receipt/:receipt_id — mint a repair `order` out of a
/// `receipt` taken on the PDA, so the claim can be numbered and exported to the
/// tenant's weekly report before any repair work starts.
///
/// IDEMPOTENT on `metadata.receipt_uuid`: a second call returns the order that
/// already exists (`created: false`) instead of a duplicate claim.
///
/// Every body field is optional. What the caller omits is derived, in order,
/// from the receipt and from the SUPPORT TICKET the receipt points at
/// (`receipt.ticket_id`, else `receipt.zoho_ticket` → `meta.ticket_number`):
/// serial, device model → product name, manufacturing date, company + city →
/// `"Firm in Town"`, country from the zip's length, and the HTML-stripped
/// ticket description → the issue text. Matching the Reparaturauftrag at
/// intake is what buys this — nobody retypes an address downstream.
pub async fn from_receipt(
    State(state): State<Arc<AppState>>,
    Path(receipt_id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult {
    let body: FromReceiptBody = match body {
        Some(Json(v)) => serde_json::from_value(v)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad body: {e}")))?,
        None => FromReceiptBody::default(),
    };

    // ── the receipt ──
    let receipt: Option<Value> = state
        .db
        .query("SELECT * FROM type::record('receipt', $id)")
        .bind(("id", receipt_id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let receipt = receipt.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("Receipt '{receipt_id}' not found"),
        )
    })?;

    // ── units of the receipt (needed to choose the unit + key idempotency) ──
    let units: Vec<Value> = state
        .db
        .query(
            "SELECT record::id(id) AS unit_id, barcode, serial, code, created_at \
             FROM receipt_unit WHERE receipt_id = $r ORDER BY created_at ASC",
        )
        .bind(("r", receipt_id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    // The unit this order is for: an explicit body `unit_id`, else the first
    // serial-like unit (today's behaviour). This is the UUID link's anchor.
    let chosen_unit_id: Option<String> = match trimmed(&body.unit_id) {
        Some(uid) => {
            if !units
                .iter()
                .any(|u| str_field(u, "unit_id").as_deref() == Some(uid.as_str()))
            {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("unit '{uid}' does not belong to receipt '{receipt_id}'"),
                ));
            }
            Some(uid)
        }
        None => units.iter().find_map(|u| {
            ["serial", "barcode"]
                .iter()
                .any(|k| str_field(u, k).map(|s| claimlist::looks_like_serial(&s)).unwrap_or(false))
                .then(|| str_field(u, "unit_id"))
                .flatten()
        }),
    };

    // ── idempotency ──
    // Per (receipt, unit) when a unit is pinned (explicit body `unit_id`, or the
    // receipt holds several units — so the 2nd device of a parcel can get its
    // own order); otherwise the legacy one-order-per-receipt rule. Same response
    // shape in both cases.
    let per_unit = body.unit_id.is_some() || units.len() > 1;
    let existing: Option<Value> = match (per_unit, chosen_unit_id.as_deref()) {
        (true, Some(uid)) => state
            .db
            .query("SELECT * FROM order WHERE unit_id = $u LIMIT 1")
            .bind(("u", uid.to_string()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?,
        // Single-unit receipt, or a multi-unit receipt with no serial-like unit
        // to anchor on: fall back to the receipt-level rule (never mint an
        // unanchored duplicate).
        _ => state
            .db
            .query("SELECT * FROM order WHERE metadata.receipt_uuid = $r LIMIT 1")
            .bind(("r", receipt_id.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?,
    };
    if let Some(order) = existing {
        return Ok(Json(json!({
            "ok": true,
            "created": false,
            "order": order,
        })));
    }

    // ── derived defaults ──
    let chosen_unit: Option<&Value> = chosen_unit_id.as_deref().and_then(|uid| {
        units
            .iter()
            .find(|u| str_field(u, "unit_id").as_deref() == Some(uid))
    });

    // The Zoho ticket the receiver attached at the counter. Everything the
    // caller left out is filled from it — the whole point of matching the
    // Reparaturauftrag at intake is that nobody retypes the address afterwards.
    let ticket = ticket_meta_for_receipt(&state, &receipt).await;
    let tmeta = |k: &str| ticket.as_ref().and_then(|m| str_field(m, k));

    let zoho_ticket: Value = match body.zoho_ticket.clone() {
        Some(v) if !v.is_null() => v,
        _ => str_field(&receipt, "zoho_ticket")
            .or_else(|| tmeta("ticket_number"))
            .map(Value::String)
            .unwrap_or(Value::Null),
    };

    let serial = trimmed(&body.serial)
        .or_else(|| {
            // Prefer the chosen unit's serial, then any serial-like unit.
            chosen_unit.and_then(|u| {
                ["serial", "barcode"]
                    .iter()
                    .find_map(|k| str_field(u, k).filter(|s| claimlist::looks_like_serial(s)))
            })
        })
        .or_else(|| {
            units.iter().find_map(|u| {
                ["serial", "barcode"]
                    .iter()
                    .find_map(|k| str_field(u, k).filter(|s| claimlist::looks_like_serial(s)))
            })
        })
        .or_else(|| tmeta("serial_number"));

    let city = trimmed(&body.city)
        .or_else(|| tmeta("city"))
        .or_else(|| {
            str_field(&receipt, "client_address").and_then(|a| claimlist::city_from_address(&a))
        });
    // "Firm in Town" — the shape the legacy sheet and the NAS folders use.
    let customer = trimmed(&body.customer)
        .or_else(|| {
            customer_in_city(
                tmeta("company").or_else(|| tmeta("customer")).as_deref(),
                city.as_deref(),
            )
        })
        .or_else(|| str_field(&receipt, "client_name"));
    let country = trimmed(&body.country)
        .or_else(|| country_from_zip(tmeta("zip").as_deref()).map(|c| c.to_string()))
        .unwrap_or_else(|| "Germany".to_string());
    let product_name = trimmed(&body.product_name).or_else(|| tmeta("device_model"));
    let manufacture_date = trimmed(&body.manufacture_date).or_else(|| tmeta("manufacturing_date"));
    // The ticket description is Zoho HTML; the sheet wants a sentence.
    let issue = trimmed(&body.issue).or_else(|| {
        tmeta("description")
            .map(|d| crate::ai::summarization::strip_html(&d))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });

    let received_date = as_date_string(receipt.get("created_at"));
    let claim_date = trimmed(&body.claim_date)
        .or_else(|| received_date.clone())
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let claim_no = trimmed(&body.claim_no);
    // Legacy NAS projection path (presentation only — CAS stays the truth).
    // The real folders on the NAS carry the customer suffix
    // (`GER2609-004_Firm in Town`), so a claim minted here lands on the same
    // name a human would have typed. Root is deployment data (`ECK_NAS_DOCS_ROOT`);
    // an unset root leaves the label empty.
    let nas_folder = claim_no.as_ref().and_then(|cn| {
        let root = nas_docs_root();
        if root.is_empty() {
            return None;
        }
        let leaf = match customer.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(c) => format!("{cn}_{c}"),
            None => cn.clone(),
        };
        Some(format!(
            "{}\\{}\\{}",
            root,
            &claim_date[..4.min(claim_date.len())],
            leaf
        ))
    });

    let uuid = uuid::Uuid::new_v4().to_string();
    let order_number = format!(
        "{}{}-{:04}",
        std::env::var("REPAIR_ORDER_PREFIX").unwrap_or_else(|_| "REP-".into()),
        Utc::now().format("%Y%m%d"),
        rand::random::<u16>() % 10000
    );

    let fields = json!({
        "uuid": uuid,
        "order_number": order_number,
        "order_type": "repair",
        "status": "received",
        "priority": "normal",
        "claim_no": claim_no,
        "claim_date": claim_date,
        "customer_name": customer,
        "product_name": product_name,
        "serial_number": serial,
        "issue_description": issue,
        "parts_used": [],
        // Top-level UUID link (order → unit); `null` when no unit could be
        // anchored. The unit → order side is stamped right after the mint below.
        "unit_id": chosen_unit_id,
        "metadata": {
            "receipt_uuid": receipt_id,
            "source": "claimlist/from-receipt",
            "zoho_ticket": zoho_ticket,
            "ticket_id": str_field(&receipt, "ticket_id"),
            "intake": str_field(&receipt, "intake").unwrap_or_else(|| "parcel".to_string()),
            "country": country,
            "city": city,
            "product_group": trimmed(&body.product_group).unwrap_or_else(default_product_group),
            "manufacture_date": manufacture_date,
            "purchase_date": trimmed(&body.purchase_date),
            "defect_major": trimmed(&body.defect_major),
            "defect_minor": trimmed(&body.defect_minor),
            "warranty": trimmed(&body.warranty),
            "received_date": received_date,
            "nas_folder": nas_folder,
        },
    });

    // `order` is a synced table — route through put_synced_entity so the write
    // advances _vclock and reaches the mesh (a raw CREATE would not).
    let created = state
        .put_synced_entity("order", &uuid, fields)
        .await
        .map_err(db_err)?;

    // Stamp the unit → order side of the UUID link (the order → unit side is the
    // top-level `unit_id` written above). Through `put_synced_entity` so the
    // `receipt_unit` write advances `_vclock` and reaches the mesh.
    if let Some(uid) = chosen_unit_id.as_deref() {
        state
            .put_synced_entity("receipt_unit", uid, json!({ "order_uuid": uuid }))
            .await
            .map_err(db_err)?;
    }

    // `created_at` must be a real `datetime`, so it is stamped server-side — an
    // RFC3339 string bound through serde would poison ORDER BY / arithmetic.
    let stamped: Option<Value> = state
        .db
        .query("UPDATE type::record('order', $id) SET created_at = time::now() RETURN AFTER")
        .bind(("id", uuid.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(json!({
        "ok": true,
        "created": true,
        "order": stamped.unwrap_or(created),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customer_is_firm_in_town() {
        assert_eq!(
            customer_in_city(Some("Magic Fit Dornbirn"), Some("Dornbirn")).as_deref(),
            Some("Magic Fit Dornbirn in Dornbirn")
        );
        // Half a pair still yields the half we have — never "in" with a hole.
        assert_eq!(
            customer_in_city(Some("Joy Fitness"), Some("  ")).as_deref(),
            Some("Joy Fitness")
        );
        assert_eq!(
            customer_in_city(None, Some("Buxtehude")).as_deref(),
            Some("Buxtehude")
        );
        assert_eq!(customer_in_city(None, None), None);
    }

    /// AT zips are 4 digits, DE 5 — the only two countries the claim sheet has
    /// ever carried. Anything else must decline rather than guess.
    #[test]
    fn country_follows_the_zip_shape() {
        assert_eq!(country_from_zip(Some("6850")), Some("Austria"));
        assert_eq!(country_from_zip(Some("21614")), Some("Germany"));
        assert_eq!(country_from_zip(Some(" 21614 ")), Some("Germany"));
        assert_eq!(country_from_zip(Some("D-21614")), None);
        assert_eq!(country_from_zip(Some("")), None);
        assert_eq!(country_from_zip(None), None);
    }

    #[test]
    fn as_date_string_reads_surreal_datetimes_and_refuses_junk() {
        assert_eq!(
            as_date_string(Some(&json!("2026-09-10T08:15:00Z"))).as_deref(),
            Some("2026-09-10")
        );
        assert_eq!(
            as_date_string(Some(&json!({ "$datetime": "2026-09-10T08:15:00Z" }))).as_deref(),
            Some("2026-09-10")
        );
        assert_eq!(as_date_string(Some(&json!("gestern"))), None);
        assert_eq!(as_date_string(None), None);
    }
}
