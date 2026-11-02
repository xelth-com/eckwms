//! EntityView — one read model behind the object page and the scan tray
//! (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §2), the representative-photo picker
//! (§4), and the compact chat-context rendering (§6).
//!
//! `kind` ∈ order|receipt|unit|place|item|product maps onto a storage table.
//! `id` is the record key without the table prefix. All ids are backtick-quoted
//! into `type::record($rid)` so all-digit keys stay STRING ids (SurrealDB 3.2 has
//! no `type::thing`), matching `services::receiving` / `services::repair`.

use eck_core::db::SurrealDb;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use crate::services::events::{self, EventOut};

/// Map an entity `kind` to its storage table. `None` = unknown kind (→ 400).
pub fn table_for(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "order" => "order",
        "receipt" => "receipt",
        "unit" => "receipt_unit",
        "place" => "location",
        "item" => "item",
        "product" => "product",
        "partner" => "partner",
        _ => return None,
    })
}

/// Record id for `type::record($rid)`.
fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

// ─── Response shapes (§2) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Photo {
    pub file_id: String,
    pub label: Option<String>,
    pub image_summary: Option<String>,
    pub device_score: Option<i64>,
    pub attached_at: Option<String>,
    /// Plan/capture evidence label carried on the `has_attachment` edge
    /// (§8.2) — absent for legacy photos. Additive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Link {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub icon_file_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    pub kind: String,
    pub id: String,
    pub table: String,
    pub title: String,
    pub subtitle: String,
    pub status: Option<String>,
    pub icon_file_id: Option<String>,
    pub photos: Vec<Photo>,
    pub up: Vec<Link>,
    pub down: Vec<Link>,
    pub events: Vec<EventOut>,
    pub fields: Map<String, Value>,
    /// The derived plan (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §8) — the order
    /// plan for an `order`, absent for every other kind. Additive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Value>,
    /// Clear-tier requester facts for an `order` — `{company, city, zip,
    /// ticket_number}` resolved from its support ticket (§9). NEVER the
    /// requester person. Absent for every other kind / when nothing resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer: Option<Value>,
    /// Non-image (PDF/protocol) attachments of an `order` and its unit/receipt/
    /// ticket (§9). Omitted when empty. Additive.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<Value>,
    /// The order's linked device unit (`{unit_id, serial, serial_source,
    /// ai_facts, ocr_verify}`), resolved through the UUID link ONLY
    /// (`order.unit_id` or `receipt_unit.order_uuid`; never the OCR-prone serial
    /// fallback). Present for an `order` with a linked unit, omitted otherwise
    /// and for every other kind. Additive (§12 Task B).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<Value>,
}

// ─── Small value helpers ─────────────────────────────────────────────────────

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// First non-empty string among `keys`.
fn pick(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| s(v, k))
}

/// RFC3339 millis, epoch on failure (SurrealDB serialises datetime to a string
/// over the SDK; legacy rows may hold a plain string — same as repair.rs).
fn ts_key(at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

// ─── Representative photo (§4) ───────────────────────────────────────────────

/// A candidate image for the icon picker.
#[derive(Debug, Clone)]
pub struct PhotoRow {
    pub file_id: String,
    pub device_score: Option<i64>,
    pub attached_at: String,
}

/// Choose the icon: highest `device_score` wins, ties → newest; if nothing is
/// scored yet → newest image; none → `None`. Pure → unit-tested.
pub fn pick_icon(rows: &[PhotoRow]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let any_scored = rows.iter().any(|r| r.device_score.is_some());
    let best = if any_scored {
        rows.iter().max_by_key(|r| {
            (
                r.device_score.unwrap_or(i64::MIN),
                ts_key(&r.attached_at),
            )
        })
    } else {
        rows.iter().max_by_key(|r| ts_key(&r.attached_at))
    };
    best.map(|r| r.file_id.clone())
}

fn is_image(mime: Option<&str>) -> bool {
    // A missing mime (legacy row) is treated as an image rather than dropped —
    // the warehouse's attachments are overwhelmingly photos.
    mime.map(|m| m.starts_with("image") || m.is_empty()).unwrap_or(true)
}

/// Image attachment edges on ONE record (as `PhotoRow`s, newest first).
async fn own_photo_rows(db: &SurrealDb, table: &str, id: &str) -> Vec<PhotoRow> {
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(out.id) AS file_id, out.device_score AS device_score, \
                    out.mime_type AS mime_type, created_at AS attached_at \
             FROM has_attachment WHERE in = type::record($tb, $id) \
             ORDER BY created_at DESC LIMIT 100",
        )
        .bind(("tb", json!(table)))
        .bind(("id", json!(id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    rows.into_iter()
        .filter(|r| is_image(r.get("mime_type").and_then(|v| v.as_str())))
        .filter_map(|r| {
            s(&r, "file_id").map(|file_id| PhotoRow {
                file_id,
                device_score: r.get("device_score").and_then(|v| v.as_i64()),
                attached_at: s(&r, "attached_at").unwrap_or_default(),
            })
        })
        .collect()
}

/// Full photo shape (with summaries) for ONE record.
async fn own_photos(db: &SurrealDb, table: &str, id: &str, origin: Option<&str>) -> Vec<Photo> {
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(out.id) AS file_id, label, created_at AS attached_at, \
                    evidence_type, \
                    out.image_summary AS image_summary, out.device_score AS device_score, \
                    out.mime_type AS mime_type \
             FROM has_attachment WHERE in = type::record($tb, $id) \
             ORDER BY created_at DESC LIMIT 100",
        )
        .bind(("tb", json!(table)))
        .bind(("id", json!(id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    rows.into_iter()
        .filter(|r| is_image(r.get("mime_type").and_then(|v| v.as_str())))
        .filter_map(|r| {
            s(&r, "file_id").map(|file_id| {
                // Label the photo by origin when it is not the entity's own
                // (§4: order's unit/receipt photos are "labelled by origin").
                let label = match origin {
                    Some(o) => Some(o.to_string()),
                    None => s(&r, "label"),
                };
                Photo {
                    file_id,
                    label,
                    image_summary: s(&r, "image_summary"),
                    device_score: r.get("device_score").and_then(|v| v.as_i64()),
                    attached_at: s(&r, "attached_at"),
                    evidence_type: s(&r, "evidence_type"),
                }
            })
        })
        .collect()
}

/// The icon for ONE entity (§4). For `order` the candidate set includes the
/// photos of its units and its receipt. Not an N+1 concern: one entity.
pub async fn icon_file_id(db: &SurrealDb, kind: &str, id: &str) -> Option<String> {
    let table = table_for(kind)?;
    let mut rows = own_photo_rows(db, table, id).await;
    if kind == "order" {
        let order: Option<Value> = db
            .query("SELECT serial_number, unit_id, metadata.receipt_uuid AS receipt_uuid FROM type::record($rid) LIMIT 1")
            .bind(("rid", rid(table, id)))
            .await
            .and_then(|mut r| r.take(0))
            .ok()
            .flatten();
        if let Some(o) = order {
            // Linked units (UUID link preferred, serial fallback) feed the icon.
            for u in units_of_order(db, id, s(&o, "unit_id").as_deref(), s(&o, "serial_number").as_deref()).await {
                if let Some(uid) = s(&u, "id") {
                    rows.extend(own_photo_rows(db, "receipt_unit", &uid).await);
                }
            }
            if let Some(ruid) = s(&o, "receipt_uuid") {
                rows.extend(own_photo_rows(db, "receipt", &ruid).await);
            }
        }
    }
    pick_icon(&rows)
}

/// Representative photo per order id, in ONE batched query over `has_attachment`
/// (no N+1 — §4, the kiosk is a 3.3 GiB box). The candidate set for the LIST is
/// each order's OWN attachments (the repair-photo scan attaches the device photo
/// straight onto the order — see `handlers::files::upload`) PLUS the photos of the
/// receipt it was opened from (`metadata.receipt_uuid`, one extra batched query);
/// the per-unit fan-out stays in `icon_file_id` for the single object page.
pub async fn icons_for_orders(db: &SurrealDb, order_ids: &[String]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    if order_ids.is_empty() {
        return out;
    }

    // One batched lookup over the given orders for their two off-record photo
    // sources: the RECEIPT they were opened from (`metadata.receipt_uuid`) and
    // the UNIT they point at (`order.unit_id`). Intake photos hang off those,
    // not off the order row.
    let links: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS order_id, metadata.receipt_uuid AS receipt_uuid, unit_id \
             FROM order WHERE record::id(id) IN $ids",
        )
        .bind(("ids", json!(order_ids)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let mut receipt_orders: HashMap<String, Vec<String>> = HashMap::new();
    let mut unit_orders: HashMap<String, Vec<String>> = HashMap::new();
    for l in &links {
        let Some(oid) = s(l, "order_id") else { continue };
        if let Some(ruid) = s(l, "receipt_uuid") {
            receipt_orders.entry(ruid).or_default().push(oid.clone());
        }
        if let Some(uid) = s(l, "unit_id") {
            unit_orders.entry(uid).or_default().push(oid.clone());
        }
    }

    // Units linked the OTHER way (`receipt_unit.order_uuid` → one of our orders),
    // one more batched query. Dedup on the UUID link, never on serial.
    let linked_units: Vec<Value> = db
        .query("SELECT record::id(id) AS unit_id, order_uuid FROM receipt_unit WHERE order_uuid IN $ids")
        .bind(("ids", json!(order_ids)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    for u in &linked_units {
        if let (Some(uid), Some(oid)) = (s(u, "unit_id"), s(u, "order_uuid")) {
            let e = unit_orders.entry(uid).or_default();
            if !e.contains(&oid) {
                e.push(oid);
            }
        }
    }

    let mut owner_ids: Vec<String> = order_ids.to_vec();
    owner_ids.extend(receipt_orders.keys().cloned());
    owner_ids.extend(unit_orders.keys().cloned());
    owner_ids.sort();
    owner_ids.dedup();

    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(in) AS owner_id, record::id(out.id) AS file_id,                     out.device_score AS device_score, out.mime_type AS mime_type,                     created_at AS attached_at              FROM has_attachment              WHERE record::id(in) IN $ids              ORDER BY created_at DESC",
        )
        .bind(("ids", json!(owner_ids)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();

    // Bucket candidates per order (direct edges + its receipt's + its unit's),
    // then reuse the single-entity picker.
    let mut buckets: HashMap<String, Vec<PhotoRow>> = HashMap::new();
    for r in rows {
        if !is_image(r.get("mime_type").and_then(|v| v.as_str())) {
            continue;
        }
        let (Some(owner), Some(fid)) = (s(&r, "owner_id"), s(&r, "file_id")) else {
            continue;
        };
        let row = PhotoRow {
            file_id: fid,
            device_score: r.get("device_score").and_then(|v| v.as_i64()),
            attached_at: s(&r, "attached_at").unwrap_or_default(),
        };
        if let Some(oids) = receipt_orders.get(&owner) {
            for oid in oids {
                buckets.entry(oid.clone()).or_default().push(row.clone());
            }
        }
        if let Some(oids) = unit_orders.get(&owner) {
            for oid in oids {
                buckets.entry(oid.clone()).or_default().push(row.clone());
            }
        }
        if order_ids.iter().any(|o| o == &owner) {
            buckets.entry(owner).or_default().push(row);
        }
    }
    for (oid, rows) in buckets {
        if let Some(icon) = pick_icon(&rows) {
            out.insert(oid, icon);
        }
    }
    out
}

/// Representative photo per UNIT id, in ONE batched query over `has_attachment`
/// (no N+1). Used for the repairs board's "awaiting triage" cards — each card is
/// a `receipt_unit`, and its icon is its own attached photo.
pub async fn icons_for_units(db: &SurrealDb, unit_ids: &[String]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    if unit_ids.is_empty() {
        return out;
    }
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(in) AS owner_id, record::id(out.id) AS file_id,                     out.device_score AS device_score, out.mime_type AS mime_type,                     created_at AS attached_at              FROM has_attachment              WHERE record::id(in) IN $ids              ORDER BY created_at DESC",
        )
        .bind(("ids", json!(unit_ids)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let mut buckets: HashMap<String, Vec<PhotoRow>> = HashMap::new();
    for r in rows {
        if !is_image(r.get("mime_type").and_then(|v| v.as_str())) {
            continue;
        }
        let (Some(owner), Some(fid)) = (s(&r, "owner_id"), s(&r, "file_id")) else {
            continue;
        };
        if !unit_ids.iter().any(|u| u == &owner) {
            continue;
        }
        buckets.entry(owner).or_default().push(PhotoRow {
            file_id: fid,
            device_score: r.get("device_score").and_then(|v| v.as_i64()),
            attached_at: s(&r, "attached_at").unwrap_or_default(),
        });
    }
    for (uid, rows) in buckets {
        if let Some(icon) = pick_icon(&rows) {
            out.insert(uid, icon);
        }
    }
    out
}

// ─── Link / relation helpers ─────────────────────────────────────────────────

/// CRM sub-kind of a `partner` record: `company`|`person`|`opp`. Prefers the
/// explicit `crm_type` written by `pda::crm_update` / the scan smart-code letter,
/// falling back to the Odoo `is_company` boolean (→ company, else person).
fn partner_kind(rec: &Value) -> &'static str {
    match rec.get("crm_type").and_then(|v| v.as_str()).map(|s| s.trim().to_lowercase()) {
        Some(ref t) if t == "c" || t == "company" => "company",
        Some(ref t) if t == "h" || t == "person" => "person",
        Some(ref t) if t == "d" || t == "opp" || t == "opportunity" => "opp",
        _ => {
            if rec.get("is_company").and_then(|v| v.as_bool()).unwrap_or(false) {
                "company"
            } else {
                "person"
            }
        }
    }
}

/// Display label for a partner sub-kind (`opp` → `opportunity`).
fn partner_kind_label(k: &str) -> &'static str {
    match k {
        "company" => "company",
        "opp" => "opportunity",
        _ => "person",
    }
}

/// Human (title, subtitle, status) for a record of the given kind.
fn titles(kind: &str, rec: &Value) -> (String, String, Option<String>) {
    let status = s(rec, "status");
    match kind {
        "order" => {
            let num = pick(rec, &["order_number"]).unwrap_or_else(|| "Order".into());
            let model = pick(rec, &["product_name", "product_sku"]);
            let title = match model {
                Some(m) => format!("{num} · {m}"),
                None => num,
            };
            let serial = s(rec, "serial_number");
            let subtitle = match (serial, status.clone()) {
                (Some(sn), Some(st)) => format!("{sn} · {st}"),
                (Some(sn), None) => sn,
                (None, Some(st)) => st,
                (None, None) => String::new(),
            };
            (title, subtitle, status)
        }
        "unit" => {
            let title = pick(rec, &["code", "serial", "barcode"]).unwrap_or_else(|| "Unit".into());
            let k = s(rec, "kind");
            let subtitle = match (k, status.clone()) {
                (Some(k), Some(st)) => format!("{k} · {st}"),
                (Some(k), None) => k,
                (None, Some(st)) => st,
                (None, None) => String::new(),
            };
            (title, subtitle, status)
        }
        "receipt" => {
            let title = pick(
                rec,
                &["tracking_number", "postal_barcode", "client_name", "label"],
            )
            .unwrap_or_else(|| "Receipt".into());
            let subtitle = pick(rec, &["note", "packaging"]).unwrap_or_default();
            (title, subtitle, status)
        }
        "place" => {
            let title =
                pick(rec, &["complete_name", "name", "barcode"]).unwrap_or_else(|| "Place".into());
            let subtitle = pick(rec, &["barcode"]).unwrap_or_default();
            (title, subtitle, status)
        }
        "item" | "product" => {
            let title = pick(rec, &["complete_name", "name"]).unwrap_or_else(|| "Item".into());
            let subtitle = pick(rec, &["default_code", "product_sku", "sku"]).unwrap_or_default();
            (title, subtitle, status)
        }
        "partner" => {
            // Title = company/person name; subtitle = "kind · city".
            let title =
                pick(rec, &["name", "company", "complete_name"]).unwrap_or_else(|| "Partner".into());
            let klabel = partner_kind_label(partner_kind(rec));
            let city = s(rec, "city");
            let subtitle = match city {
                Some(c) => format!("{klabel} · {c}"),
                None => klabel.to_string(),
            };
            (title, subtitle, status)
        }
        _ => (
            pick(rec, &["name", "complete_name"]).unwrap_or_else(|| kind.to_string()),
            String::new(),
            status,
        ),
    }
}

fn link_from(kind: &str, rec: &Value) -> Option<Link> {
    let id = pick(rec, &["id", "unit_id", "receipt_id"])?;
    let (title, subtitle, _) = titles(kind, rec);
    Some(Link {
        kind: kind.to_string(),
        id,
        title,
        subtitle,
        icon_file_id: None,
    })
}

/// Units that belong to a serial (`receipt_unit.serial`).
async fn units_of_serial(db: &SurrealDb, serial: &str) -> Vec<Value> {
    db.query("SELECT record::id(id) AS id, * FROM receipt_unit WHERE serial = $sn LIMIT 50")
        .bind(("sn", json!(serial)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default()
}

/// Units that belong to an order, PREFERRING the UUID link
/// (`receipt_unit.order_uuid = order_id`, plus the `order.unit_id` pointer) and
/// only falling back to the OCR-prone serial join when the order carries no
/// linked unit at all (logged). Dedup is on the unit id, never on serial.
async fn units_of_order(
    db: &SurrealDb,
    order_id: &str,
    order_unit_id: Option<&str>,
    serial: Option<&str>,
) -> Vec<Value> {
    let mut units: Vec<Value> = db
        .query("SELECT record::id(id) AS id, * FROM receipt_unit WHERE order_uuid = $oid LIMIT 50")
        .bind(("oid", json!(order_id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    // The order→unit pointer, if it is not already in the set.
    if let Some(uid) = order_unit_id.map(str::trim).filter(|s| !s.is_empty()) {
        if !units.iter().any(|u| s(u, "id").as_deref() == Some(uid)) {
            if let Some(u) = fetch_one(db, "receipt_unit", uid).await {
                units.push(u);
            }
        }
    }
    if units.is_empty() {
        if let Some(sn) = serial.map(str::trim).filter(|s| !s.is_empty()) {
            let by_serial = units_of_serial(db, sn).await;
            if !by_serial.is_empty() {
                tracing::warn!(
                    "[link] serial fallback: order {} units resolved by serial {} (no UUID link)",
                    order_id,
                    sn
                );
            }
            return by_serial;
        }
    }
    units
}

/// The order's linked device unit as the compact `unit` card (§12 Task B).
/// Resolved through the UUID link ONLY — `order.unit_id` first, then the reverse
/// `receipt_unit.order_uuid = order_id` link — never the serial fallback. `None`
/// when the order carries no linked unit. Returns `{unit_id, serial,
/// serial_source, ai_facts, ocr_verify}` with absent fields omitted.
async fn linked_order_unit(db: &SurrealDb, order_id: &str, order_unit_id: Option<&str>) -> Option<Value> {
    const PROJ: &str =
        "record::id(id) AS id, serial, serial_source, ai_facts, ocr_verify";
    // order.unit_id pointer first.
    if let Some(uid) = order_unit_id.map(str::trim).filter(|s| !s.is_empty()) {
        let row: Option<Value> = db
            .query(format!("SELECT {PROJ} FROM type::record($rid) LIMIT 1"))
            .bind(("rid", rid("receipt_unit", uid)))
            .await
            .and_then(|mut r| r.take(0))
            .ok()
            .flatten();
        if let Some(r) = row {
            return Some(unit_card(&r));
        }
    }
    // Reverse link: the unit that points at this order.
    let row: Option<Value> = db
        .query(format!(
            "SELECT {PROJ} FROM receipt_unit WHERE order_uuid = $oid ORDER BY created_at ASC LIMIT 1"
        ))
        .bind(("oid", json!(order_id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    row.map(|r| unit_card(&r))
}

/// Shape the `unit` card, omitting absent/null fields.
fn unit_card(row: &Value) -> Value {
    let mut m = Map::new();
    if let Some(id) = s(row, "id") {
        m.insert("unit_id".into(), json!(id));
    }
    if let Some(sn) = s(row, "serial") {
        m.insert("serial".into(), json!(sn));
    }
    if let Some(src) = s(row, "serial_source") {
        m.insert("serial_source".into(), json!(src));
    }
    if let Some(f) = row.get("ai_facts").filter(|v| !v.is_null()) {
        m.insert("ai_facts".into(), f.clone());
    }
    if let Some(ov) = row.get("ocr_verify").filter(|v| !v.is_null()) {
        m.insert("ocr_verify".into(), ov.clone());
    }
    Value::Object(m)
}

// ─── The read model (§2) ─────────────────────────────────────────────────────

/// Load the full EntityView for `kind`/`id`, or `None` when the record does not
/// exist. Unknown kinds are rejected by the handler before this is called.
pub async fn load(db: &SurrealDb, kind: &str, id: &str) -> anyhow::Result<Option<EntityView>> {
    let Some(table) = table_for(kind) else {
        return Ok(None);
    };

    let rec: Option<Value> = db
        .query("SELECT record::id(id) AS id, * FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid(table, id)))
        .await?
        .take(0)?;
    let Some(rec) = rec else {
        return Ok(None);
    };

    let (title, subtitle, status) = titles(kind, &rec);

    let mut photos: Vec<Photo> = own_photos(db, table, id, None).await;
    let mut up: Vec<Link> = Vec::new();
    let mut down: Vec<Link> = Vec::new();
    let mut fields: Map<String, Value> = Map::new();
    // Codes the legacy audit tables (scan_log) matched this entity on.
    let mut codes: Vec<String> = Vec::new();
    // Linked unit ids gathered for an order (feed the §9 documents list).
    let mut order_unit_ids: Vec<String> = Vec::new();
    let serial = s(&rec, "serial_number").or_else(|| s(&rec, "serial"));

    let put = |fields: &mut Map<String, Value>, rec: &Value, keys: &[&str]| {
        for k in keys {
            if let Some(v) = s(rec, k) {
                fields.insert(k.to_string(), json!(v));
            }
        }
    };

    match kind {
        "order" => {
            put(&mut fields, &rec, &[
                "customer_name", "product_name", "product_sku", "issue_description",
                "priority", "assigned_to", "resolution",
            ]);
            if let Some(meta) = rec.get("metadata") {
                for k in ["zoho_ticket", "ticket_id", "receipt_uuid"] {
                    if let Some(v) = s(meta, k) {
                        fields.insert(k.to_string(), json!(v));
                    }
                }
            }
            if let Some(n) = s(&rec, "order_number") {
                codes.push(n);
            }
            if let Some(sn) = serial.clone() {
                codes.push(sn.clone());
            }
            // down: the order's units (UUID link preferred, serial fallback),
            // each followed by the place it sits on.
            {
                let units =
                    units_of_order(db, id, s(&rec, "unit_id").as_deref(), serial.as_deref()).await;
                let mut seen_places: Vec<String> = Vec::new();
                for u in &units {
                    if let Some(l) = link_from("unit", u) {
                        down.push(l);
                    }
                    // unit photos feed the order's candidate/photo set (origin-labelled)
                    if let Some(uid) = s(u, "id") {
                        photos.extend(own_photos(db, "receipt_unit", &uid, Some("unit")).await);
                        order_unit_ids.push(uid);
                    }
                    if let Some(loc) = s(u, "location_id") {
                        if !seen_places.contains(&loc) {
                            seen_places.push(loc.clone());
                            if let Some(place) = fetch_one(db, "location", &loc).await {
                                if let Some(l) = link_from("place", &place) {
                                    down.push(l);
                                }
                            }
                        }
                    }
                }
            }
            // up: receipt via metadata.receipt_uuid.
            if let Some(ruid) = rec
                .get("metadata")
                .and_then(|m| s(m, "receipt_uuid"))
            {
                if let Some(r) = fetch_one(db, "receipt", &ruid).await {
                    if let Some(l) = link_from("receipt", &r) {
                        up.push(l);
                    }
                    photos.extend(own_photos(db, "receipt", &ruid, Some("receipt")).await);
                }
            }
        }
        "unit" => {
            put(&mut fields, &rec, &["kind", "note", "location_barcode", "receipt_id"]);
            for k in ["code", "barcode", "serial"] {
                if let Some(v) = s(&rec, k) {
                    codes.push(v);
                }
            }
            // up: receipt, order (by serial), place.
            if let Some(ruid) = s(&rec, "receipt_id") {
                if let Some(r) = fetch_one(db, "receipt", &ruid).await {
                    if let Some(l) = link_from("receipt", &r) {
                        up.push(l);
                    }
                }
            }
            if let Some(o) = find_live_order_record_for_unit(db, &rec, serial.as_deref()).await {
                if let Some(l) = link_from("order", &o) {
                    up.push(l);
                }
            }
            if let Some(loc) = s(&rec, "location_id") {
                if let Some(p) = fetch_one(db, "location", &loc).await {
                    if let Some(l) = link_from("place", &p) {
                        up.push(l);
                    }
                }
            }
            // down: sibling units of the same receipt of kind part/box.
            if let Some(ruid) = s(&rec, "receipt_id") {
                let sibs: Vec<Value> = db
                    .query(
                        "SELECT record::id(id) AS id, * FROM receipt_unit \
                         WHERE receipt_id = $r AND kind IN ['part','box'] \
                         AND record::id(id) != $self LIMIT 50",
                    )
                    .bind(("r", json!(ruid)))
                    .bind(("self", json!(id)))
                    .await
                    .and_then(|mut r| r.take(0))
                    .unwrap_or_default();
                for sib in &sibs {
                    if let Some(l) = link_from("unit", sib) {
                        down.push(l);
                    }
                }
            }
        }
        "receipt" => {
            put(&mut fields, &rec, &["client_name", "note", "packaging", "zoho_ticket", "ticket_id"]);
            let units: Vec<Value> = db
                .query("SELECT record::id(id) AS id, * FROM receipt_unit WHERE receipt_id = $r LIMIT 100")
                .bind(("r", json!(id)))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            for u in &units {
                if let Some(l) = link_from("unit", u) {
                    down.push(l);
                }
            }
        }
        "place" => {
            put(&mut fields, &rec, &["barcode", "usage"]);
            if let Some(b) = s(&rec, "barcode") {
                codes.push(b);
            }
            // up: parent location.
            if let Some(parent) = s(&rec, "parent_id") {
                if let Some(p) = fetch_one(db, "location", &parent).await {
                    if let Some(l) = link_from("place", &p) {
                        up.push(l);
                    }
                }
            }
            // down: units sitting here + child locations + quants (product qty).
            let units: Vec<Value> = db
                .query("SELECT record::id(id) AS id, * FROM receipt_unit WHERE location_id = $loc LIMIT 100")
                .bind(("loc", json!(id)))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            for u in &units {
                if let Some(l) = link_from("unit", u) {
                    down.push(l);
                }
            }
            let children: Vec<Value> = db
                .query("SELECT record::id(id) AS id, * FROM location WHERE parent_id = $loc LIMIT 100")
                .bind(("loc", json!(id)))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            for c in &children {
                if let Some(l) = link_from("place", c) {
                    down.push(l);
                }
            }
        }
        "item" | "product" => {
            put(&mut fields, &rec, &["default_code", "product_sku", "sku", "barcode"]);
            for k in ["default_code", "barcode"] {
                if let Some(v) = s(&rec, k) {
                    codes.push(v);
                }
            }
        }
        "partner" => {
            // Business facts + contact PII. The signed-in dashboard already shows
            // customer email/phone/address in clear on the support + repairs pages
            // (`support/[id]`, `repairs/[id]`), so showing them here for the same
            // audience is consistent — see §PII note in the task report.
            let pk = partner_kind(&rec);
            fields.insert("kind".into(), json!(partner_kind_label(pk)));
            if let Some(name) = s(&rec, "name") {
                fields.insert("company".into(), json!(name));
            }
            put(&mut fields, &rec, &[
                "city", "status", "priority", "origin", "country", "vat",
                "email", "phone", "mobile", "street", "zip",
            ]);

            let name = s(&rec, "name");
            // person/opp: up = parent company partner (matched by name).
            if pk != "company" {
                if let Some(parent) = s(&rec, "parent_name").or_else(|| s(&rec, "company")) {
                    let comp: Option<Value> = db
                        .query(
                            "SELECT record::id(id) AS id, * FROM partner \
                             WHERE string::lowercase(name ?? '') = string::lowercase($n) LIMIT 1",
                        )
                        .bind(("n", json!(parent)))
                        .await
                        .and_then(|mut r| r.take(0))
                        .ok()
                        .flatten();
                    if let Some(c) = comp {
                        if let Some(l) = link_from("partner", &c) {
                            up.push(l);
                        }
                    }
                }
            } else if let Some(n) = name.clone() {
                // company: down = its persons / opportunities.
                let members: Vec<Value> = db
                    .query(
                        "SELECT record::id(id) AS id, * FROM partner \
                         WHERE (string::lowercase(parent_name ?? '') = string::lowercase($n) \
                                OR string::lowercase(company ?? '') = string::lowercase($n)) \
                         AND record::id(id) != $self LIMIT 50",
                    )
                    .bind(("n", json!(n)))
                    .bind(("self", json!(id)))
                    .await
                    .and_then(|mut r| r.take(0))
                    .unwrap_or_default();
                for m in &members {
                    if let Some(l) = link_from("partner", m) {
                        down.push(l);
                    }
                }
            }

            // down: orders whose customer matches this partner — email (exact,
            // case-insensitive) preferred, else the name. Orders carry no partner
            // id, only `customer_email` / `customer_name` (see services::repair).
            let orders: Vec<Value> = if let Some(em) = s(&rec, "email") {
                db.query(
                    "SELECT record::id(id) AS id, * FROM order \
                     WHERE string::lowercase(customer_email ?? '') = string::lowercase($e) \
                     ORDER BY created_at DESC LIMIT 50",
                )
                .bind(("e", json!(em)))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default()
            } else if let Some(n) = name.clone() {
                db.query(
                    "SELECT record::id(id) AS id, * FROM order \
                     WHERE string::lowercase(customer_name ?? '') = string::lowercase($n) \
                     ORDER BY created_at DESC LIMIT 50",
                )
                .bind(("n", json!(n)))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default()
            } else {
                Vec::new()
            };
            for o in &orders {
                if let Some(l) = link_from("order", o) {
                    down.push(l);
                }
            }
        }
        _ => {}
    }

    // Additive `plan` field (§8): the derived order plan for an `order`.
    let plan = if kind == "order" {
        match crate::services::plan::order_plan_value(db, id).await {
            Value::Null => None,
            v => Some(v),
        }
    } else {
        None
    };

    // Order enrichment (§9): clear-tier requester company/city/zip/ticket_number
    // (NEVER the requester person) and the non-image repair documents of the
    // order + its units + receipt + ticket. Additive; absent for other kinds.
    let (customer, documents) = if kind == "order" {
        let contact = crate::services::order_enrich::ticket_for_order(db, &rec).await;
        let order_city = rec.get("metadata").and_then(|m| s(m, "city"));
        let order_customer = s(&rec, "customer_name");
        let receipt_uuid = rec.get("metadata").and_then(|m| s(m, "receipt_uuid"));
        // Receipt is the LAST fallback for company/city (owner, 2026-11-01) when
        // the ticket link gives nothing — one batched lookup for this one order.
        let receipts = crate::services::order_enrich::resolve_receipts(
            db,
            receipt_uuid.as_ref().map(std::slice::from_ref).unwrap_or(&[]),
        )
        .await;
        let receipt_facts = receipt_uuid.as_deref().and_then(|r| receipts.get(r));
        let customer = crate::services::order_enrich::customer_json(
            contact.as_ref(),
            order_customer.as_deref(),
            order_city.as_deref(),
            receipt_facts,
        );
        let ticket_id = contact.as_ref().and_then(|c| c.ticket_id.clone());
        let documents = crate::services::order_enrich::documents_for_order(
            db,
            id,
            &order_unit_ids,
            receipt_uuid.as_deref(),
            ticket_id.as_deref(),
        )
        .await;
        (customer, documents)
    } else {
        (None, Vec::new())
    };

    // Additive `unit` card (§12 Task B): the order's linked device unit, via the
    // UUID link only. Absent for other kinds / when no unit is linked.
    let unit = if kind == "order" {
        linked_order_unit(db, id, s(&rec, "unit_id").as_deref()).await
    } else {
        None
    };

    let events = events::timeline(db, kind, id, serial.as_deref(), &codes).await;
    let icon_file_id = pick_icon(
        &photos
            .iter()
            .map(|p| PhotoRow {
                file_id: p.file_id.clone(),
                device_score: p.device_score,
                attached_at: p.attached_at.clone().unwrap_or_default(),
            })
            .collect::<Vec<_>>(),
    );

    Ok(Some(EntityView {
        kind: kind.to_string(),
        id: id.to_string(),
        table: table.to_string(),
        title,
        subtitle,
        status,
        icon_file_id,
        photos,
        up,
        down,
        events,
        fields,
        plan,
        customer,
        documents,
        unit,
    }))
}

/// One record by table + leaf, with its id projected as `id`.
async fn fetch_one(db: &SurrealDb, table: &str, id: &str) -> Option<Value> {
    db.query("SELECT record::id(id) AS id, * FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid(table, id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten()
}

/// Newest live order for a UNIT, preferring the UUID link (`order_uuid`) and
/// only falling back to the serial join (logged) for legacy rows. `rec` is the
/// `receipt_unit` row being viewed.
async fn find_live_order_record_for_unit(
    db: &SurrealDb,
    rec: &Value,
    serial: Option<&str>,
) -> Option<Value> {
    if let Some(ouid) = s(rec, "order_uuid") {
        if let Some(o) = fetch_one(db, "order", &ouid).await {
            let st = s(&o, "status").unwrap_or_default();
            if !["completed", "closed", "cancelled", "done"].contains(&st.as_str()) {
                return Some(o);
            }
        }
    }
    if let Some(sn) = serial {
        if let Some(o) = find_live_order_record(db, sn).await {
            tracing::warn!(
                "[link] serial fallback: unit up-link resolved order by serial {} (no live UUID link)",
                sn
            );
            return Some(o);
        }
    }
    None
}

/// Newest live (non-terminal) order for a serial, id projected as `id`.
async fn find_live_order_record(db: &SurrealDb, serial: &str) -> Option<Value> {
    db.query(
        "SELECT record::id(id) AS id, * FROM order \
         WHERE serial_number = $sn AND status NOT IN ['completed','closed','cancelled','done'] \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(("sn", json!(serial)))
    .await
    .and_then(|mut r| r.take(0))
    .ok()
    .flatten()
}

// ─── Chat context (§6) ───────────────────────────────────────────────────────

/// Compact text rendering of an EntityView for the chat system prompt (§6):
/// title, fields, up/down titles, last 20 events, photo summaries. Capped at
/// ~2000 chars. Pure → unit-tested.
pub fn render_for_prompt(v: &EntityView) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} [{}:{}]\n", v.title, v.kind, v.id));
    if !v.subtitle.is_empty() {
        out.push_str(&v.subtitle);
        out.push('\n');
    }
    if let Some(st) = &v.status {
        out.push_str(&format!("Status: {st}\n"));
    }
    if !v.fields.is_empty() {
        let facts: Vec<String> = v
            .fields
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| format!("{k}={s}")))
            .collect();
        if !facts.is_empty() {
            out.push_str("Facts: ");
            out.push_str(&facts.join("; "));
            out.push('\n');
        }
    }
    if !v.up.is_empty() {
        let links: Vec<String> = v.up.iter().map(|l| format!("{} {}", l.kind, l.title)).collect();
        out.push_str(&format!("Up: {}\n", links.join("; ")));
    }
    if !v.down.is_empty() {
        let links: Vec<String> = v
            .down
            .iter()
            .map(|l| format!("{} {}", l.kind, l.title))
            .collect();
        out.push_str(&format!("Down: {}\n", links.join("; ")));
    }
    let summaries: Vec<String> = v
        .photos
        .iter()
        .filter_map(|p| p.image_summary.as_ref().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    if !summaries.is_empty() {
        out.push_str("Photos: ");
        out.push_str(&summaries.join(" | "));
        out.push('\n');
    }
    if !v.events.is_empty() {
        out.push_str("Recent events:\n");
        for e in v.events.iter().take(20) {
            let actor = e.actor.as_deref().map(|a| format!(" ({a})")).unwrap_or_default();
            out.push_str(&format!("- {} {} {}{}\n", e.at, e.kind, e.text, actor));
        }
    }

    // Hard cap (§6): keep the head, mark the cut on a char boundary.
    const CAP: usize = 2000;
    if out.chars().count() > CAP {
        let kept: String = out.chars().take(CAP).collect();
        format!("{kept}…[truncated]")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(file_id: &str, score: Option<i64>, at: &str) -> PhotoRow {
        PhotoRow {
            file_id: file_id.into(),
            device_score: score,
            attached_at: at.into(),
        }
    }

    #[test]
    fn icon_highest_score_wins() {
        let rows = vec![
            row("a", Some(3), "2026-10-31T10:00:00Z"),
            row("b", Some(9), "2026-10-01T10:00:00Z"),
            row("c", Some(7), "2026-10-31T23:00:00Z"),
        ];
        assert_eq!(pick_icon(&rows), Some("b".into()));
    }

    #[test]
    fn icon_score_tie_breaks_on_newest() {
        let rows = vec![
            row("old", Some(5), "2026-10-01T10:00:00Z"),
            row("new", Some(5), "2026-10-31T10:00:00Z"),
        ];
        assert_eq!(pick_icon(&rows), Some("new".into()));
    }

    #[test]
    fn icon_no_scores_falls_back_to_newest() {
        let rows = vec![
            row("old", None, "2026-10-01T10:00:00Z"),
            row("new", None, "2026-10-31T10:00:00Z"),
        ];
        assert_eq!(pick_icon(&rows), Some("new".into()));
    }

    #[test]
    fn icon_scored_beats_unscored_newer() {
        let rows = vec![
            row("scored", Some(2), "2026-01-01T00:00:00Z"),
            row("unscored_newer", None, "2026-10-31T00:00:00Z"),
        ];
        assert_eq!(pick_icon(&rows), Some("scored".into()));
    }

    #[test]
    fn icon_empty_is_none() {
        assert_eq!(pick_icon(&[]), None);
    }

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        db.query(
            "DEFINE TABLE order SCHEMALESS; DEFINE TABLE file_resource SCHEMALESS; \
             DEFINE TABLE has_attachment TYPE RELATION SCHEMALESS; \
             DEFINE TABLE receipt_unit SCHEMALESS; DEFINE TABLE location SCHEMALESS; \
             DEFINE TABLE receipt SCHEMALESS; DEFINE TABLE event SCHEMALESS; \
             DEFINE TABLE scan_log SCHEMALESS; DEFINE TABLE repair_event SCHEMALESS; \
             DEFINE TABLE partner SCHEMALESS; DEFINE TABLE crm_update_log SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    /// The batched list icon: `record::id(in) IN $ids` + relation traversal must
    /// bucket per order and pick the highest device_score.
    #[tokio::test]
    async fn icons_for_orders_batched_picks_best() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('order','o1') SET order_number='REP-1', created_at=time::now(); \
             CREATE type::record('file_resource','f1') SET cas_uuid='f1', mime_type='image/jpeg', device_score=3; \
             CREATE type::record('file_resource','f2') SET cas_uuid='f2', mime_type='image/jpeg', device_score=8; \
             RELATE (type::record('order','o1'))->has_attachment->(type::record('file_resource','f1')) SET created_at=time::now(), label='x'; \
             RELATE (type::record('order','o1'))->has_attachment->(type::record('file_resource','f2')) SET created_at=time::now(), label='x';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let icons = icons_for_orders(&db, &["o1".into()]).await;
        assert_eq!(icons.get("o1"), Some(&"f2".to_string()));
    }

    #[tokio::test]
    async fn load_order_builds_view_with_icon_photos_and_created_event() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('order','o1') SET order_number='REP-1', product_name='Acme 770', \
                serial_number='SN1', status='processing', customer_name='ACME', created_at=time::now(); \
             CREATE type::record('file_resource','f1') SET cas_uuid='f1', mime_type='image/jpeg', \
                device_score=8, image_summary='white analyzer'; \
             RELATE (type::record('order','o1'))->has_attachment->(type::record('file_resource','f1')) \
                SET created_at=time::now(), label='repair_photo';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let view = load(&db, "order", "o1").await.unwrap().unwrap();
        assert_eq!(view.title, "REP-1 · Acme 770");
        assert_eq!(view.table, "order");
        assert_eq!(view.icon_file_id, Some("f1".into()));
        assert_eq!(view.photos.len(), 1);
        assert_eq!(view.fields.get("customer_name").and_then(|v| v.as_str()), Some("ACME"));
        assert!(view.events.iter().any(|e| e.kind == "created"));
        assert!(view.events.iter().any(|e| e.kind == "photo"));
    }

    /// A `company` partner builds a view whose title is the company name,
    /// subtitle is "kind · city", `fields` carry the business facts + contact
    /// PII, `down` links the company's person AND a matching repair order, and
    /// `events` fold in a `crm_update_log` row via the extended timeline.
    #[tokio::test]
    async fn load_partner_company_builds_view_with_links_and_crm_event() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('partner','p1') SET name='Acme Europe', crm_type='c', \
                is_company=true, city='Beispielstadt', email='info@acme.example', phone='+49 6196', \
                status='active', created_at=time::now(); \
             CREATE type::record('partner','p2') SET name='Dr. Weber', crm_type='h', \
                parent_name='Acme Europe', city='Beispielstadt', created_at=time::now(); \
             CREATE type::record('order','o9') SET order_number='GER2610-001', product_name='Acme 770', \
                customer_email='INFO@acme.example', status='processing', created_at=time::now(); \
             CREATE crm_update_log SET entity_type='c', entity_id='p1', \
                changes={ status: 'won', notes: 'Follow up' }, device_id='PDA-7', created_at=time::now();",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let view = load(&db, "partner", "p1").await.unwrap().unwrap();
        assert_eq!(view.title, "Acme Europe");
        assert_eq!(view.table, "partner");
        assert_eq!(view.subtitle, "company · Beispielstadt");
        assert_eq!(view.fields.get("kind").and_then(|v| v.as_str()), Some("company"));
        assert_eq!(view.fields.get("company").and_then(|v| v.as_str()), Some("Acme Europe"));
        assert_eq!(view.fields.get("email").and_then(|v| v.as_str()), Some("info@acme.example"));
        // down: the person (matched by parent_name) and the order (matched by email).
        assert!(view.down.iter().any(|l| l.kind == "partner" && l.id == "p2"));
        assert!(view.down.iter().any(|l| l.kind == "order" && l.id == "o9"));
        // events: the crm_update_log row surfaced as a `crm_update` event.
        assert!(view.events.iter().any(|e| e.kind == "crm_update" && e.text.contains("won")));
        assert!(view.events.iter().any(|e| e.kind == "created"));
    }

    /// A `person` partner links UP to its parent company and keeps the person
    /// name as title.
    #[tokio::test]
    async fn load_partner_person_links_up_to_company() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('partner','c1') SET name='ACME GmbH', crm_type='c', \
                is_company=true, created_at=time::now(); \
             CREATE type::record('partner','h1') SET name='Jane Doe', crm_type='h', \
                parent_name='ACME GmbH', city='Berlin', created_at=time::now();",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let view = load(&db, "partner", "h1").await.unwrap().unwrap();
        assert_eq!(view.title, "Jane Doe");
        assert_eq!(view.subtitle, "person · Berlin");
        assert!(view.up.iter().any(|l| l.kind == "partner" && l.id == "c1"));
    }

    /// §12 Task B: an order with a UUID-linked unit surfaces the compact `unit`
    /// card (serial/serial_source/ai_facts/ocr_verify); an order with no linked
    /// unit omits it entirely.
    #[tokio::test]
    async fn load_order_surfaces_linked_unit_card() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('order','o1') SET order_number='REP-1', status='processing', \
                unit_id='u1', created_at=time::now(); \
             CREATE type::record('receipt_unit','u1') SET serial='I81800562', \
                serial_source='ocr', ai_facts={ model: { value: 'Acme 770', confidence: 0.9 } }, \
                ocr_verify={ status: 'suggested' }, created_at=time::now(); \
             CREATE type::record('order','o2') SET order_number='REP-2', status='processing', \
                created_at=time::now();",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let view = load(&db, "order", "o1").await.unwrap().unwrap();
        let unit = view.unit.expect("linked unit card present");
        assert_eq!(unit.get("unit_id").and_then(|v| v.as_str()), Some("u1"));
        assert_eq!(unit.get("serial").and_then(|v| v.as_str()), Some("I81800562"));
        assert_eq!(unit.get("serial_source").and_then(|v| v.as_str()), Some("ocr"));
        assert_eq!(
            unit.get("ocr_verify").and_then(|v| v.get("status")).and_then(|v| v.as_str()),
            Some("suggested")
        );
        assert!(unit.get("ai_facts").and_then(|v| v.get("model")).is_some());

        // No linked unit → field omitted.
        let view2 = load(&db, "order", "o2").await.unwrap().unwrap();
        assert!(view2.unit.is_none());
    }

    #[tokio::test]
    async fn load_unknown_record_is_none() {
        let db = test_db().await;
        assert!(load(&db, "order", "missing").await.unwrap().is_none());
    }

    #[test]
    fn table_mapping() {
        assert_eq!(table_for("unit"), Some("receipt_unit"));
        assert_eq!(table_for("place"), Some("location"));
        assert_eq!(table_for("order"), Some("order"));
        assert_eq!(table_for("bogus"), None);
    }

    #[test]
    fn render_caps_and_includes_core_fields() {
        let mut fields = Map::new();
        fields.insert("customer_name".into(), json!("ACME GmbH"));
        let v = EntityView {
            kind: "order".into(),
            id: "o1".into(),
            table: "order".into(),
            title: "REP-1 · Acme 770".into(),
            subtitle: "SN-9 · processing".into(),
            status: Some("processing".into()),
            icon_file_id: None,
            photos: vec![Photo {
                file_id: "f1".into(),
                label: Some("repair_photo".into()),
                image_summary: Some("A white Acme 770 analyzer".into()),
                device_score: Some(8),
                attached_at: Some("2026-10-31T10:00:00Z".into()),
                evidence_type: None,
            }],
            up: vec![Link {
                kind: "receipt".into(),
                id: "r1".into(),
                title: "Parcel".into(),
                subtitle: String::new(),
                icon_file_id: None,
            }],
            down: vec![],
            events: vec![EventOut {
                at: "2026-10-31T10:00:00Z".into(),
                kind: "status".into(),
                text: "processing".into(),
                actor: Some("dimi".into()),
                ref_: None,
            }],
            fields,
            plan: None,
            customer: None,
            documents: vec![],
            unit: None,
        };
        let r = render_for_prompt(&v);
        assert!(r.contains("REP-1 · Acme 770"));
        assert!(r.contains("customer_name=ACME GmbH"));
        assert!(r.contains("Up: receipt Parcel"));
        assert!(r.contains("A white Acme 770 analyzer"));
        assert!(r.contains("status"));
        assert!(r.chars().count() <= 2000 + 16);
    }
}
