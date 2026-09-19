//! Warehouse receiving — the durable record of "a parcel arrived and its
//! contents are now lying somewhere in the warehouse".
//!
//! Two tables, both in `SYNC_ENTITY_TYPES` (so a receipt taken on the PDA's
//! kiosk converges to the office master) and both on the Zone-1 ops denylist
//! (they carry `client_name` / `client_address`):
//!
//!   receipt       — the header: WHAT arrived (postal barcode, OCR of the
//!                   label, optional link to a scraped `stock_picking_delivery`
//!                   tracking number, client, packaging, note, who received it).
//!   receipt_unit  — one row per THING now sitting on a place. Owner doctrine:
//!                   receiving must always end with >= 1 unit on a place, even
//!                   when nothing about the parcel is known — so a unit is
//!                   created with a server-assigned `RCV-YYYYMMDD-NNNN` code
//!                   even if the receiver scanned no smart code at all.
//!
//! The two reserved zones are ORDINARY `location` rows (no new table, no
//! special-casing in the explorer): barcode `ZONE-INBOUND` ("Eingang") and
//! `ZONE-OUTBOUND` ("Ausgang"), ensured at boot by [`ensure_zone_locations`].
//! A unit's zone is derived from its `location_barcode`, so moving a unit onto
//! a normal shelf simply drops it out of both zone lists ("other").
//!
//! Ids are CLIENT-MINTED UUIDs: the PDA records offline and retries, so every
//! write here is an idempotent UPSERT keyed by the id the client already chose
//! — a re-POST after a flaky retry must never duplicate a receipt or a unit.

use crate::handlers::files::SCAN_FRAME_CONTEXT;
use eck_core::db::SurrealDb;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Reserved inbound zone ("Eingang") — a `location.barcode`, not a table.
pub const ZONE_INBOUND: &str = "ZONE-INBOUND";
/// Reserved outbound zone ("Ausgang") — a `location.barcode`, not a table.
pub const ZONE_OUTBOUND: &str = "ZONE-OUTBOUND";

/// Run one statement and surface the PER-STATEMENT SurrealQL error.
/// `db.query(..).await` is `Ok` even when the statement itself failed — only
/// `Response::check()` reveals it (same trap as `services::delivery`).
macro_rules! run_stmt {
    ($label:expr, $q:expr) => {
        match $q.await {
            Ok(resp) => {
                resp.check().map_err(|e| format!("{}: {}", $label, e))?;
            }
            Err(e) => return Err(format!("{}: {}", $label, e)),
        }
    };
}

/// Record id for `type::record($rid)`: the key is backtick-quoted so all-digit
/// keys stay STRING ids. `type::thing` does NOT exist in SurrealDB 3.2 (parse
/// error → silent no-op); see `handlers/exact.rs` / `services/delivery.rs`.
fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

/// Lowercase alphanumeric slug — same shape `services::binmap` uses to mint a
/// location leaf, so a zone minted here and a shelf minted by `put_away` for
/// the same barcode land on the SAME record id instead of racing into two rows.
fn slug(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c.is_whitespace() {
                Some('_')
            } else {
                None
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// Next `_vclock` for a synced record, advancing THIS node's component from
/// whatever the row currently holds. `receipt`, `receipt_unit` and `location`
/// are all in `SYNC_ENTITY_TYPES`, so a write that leaves `_vclock` stale never
/// propagates — the peer drops it as "local wins/equal" and the mesh churns.
async fn next_vclock(db: &SurrealDb, table: &str, key: &str, instance_id: &str) -> Value {
    let existing: Option<Value> = db
        .query("SELECT _vclock FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid(table, key)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|v| v.get("_vclock")),
        instance_id,
    )
}

// ─── Zones ───────────────────────────────────────────────────────────────────

/// Derive the coarse zone of a place from its barcode. Anything that is not one
/// of the two reserved zones is "other" — a normal shelf, a workbench, a van.
pub fn zone_for(location_barcode: &str) -> &'static str {
    match location_barcode.trim().to_ascii_uppercase().as_str() {
        ZONE_INBOUND => "inbound",
        ZONE_OUTBOUND => "outbound",
        _ => "other",
    }
}

/// Map a `?zone=` filter onto the location barcode it selects. `all` (and any
/// unknown value) means no filter.
fn zone_filter_barcode(zone: &str) -> Option<&'static str> {
    match zone.trim().to_ascii_lowercase().as_str() {
        "inbound" => Some(ZONE_INBOUND),
        "outbound" => Some(ZONE_OUTBOUND),
        _ => None,
    }
}

/// Boot-time, idempotent: make sure the two reserved zones exist as ordinary
/// `location` rows. Called once at startup next to the other one-shot
/// migrations; safe to call on every boot and on every node.
pub async fn ensure_zone_locations(db: &SurrealDb, instance_id: &str) {
    for (barcode, name) in [(ZONE_INBOUND, "Eingang"), (ZONE_OUTBOUND, "Ausgang")] {
        match resolve_or_create_location(db, barcode, Some(name), instance_id).await {
            Ok(leaf) => tracing::debug!("[receiving] zone {} -> location:{}", barcode, leaf),
            Err(e) => tracing::warn!("[receiving] ensure zone {}: {}", barcode, e),
        }
    }
}

/// Resolve a place by its barcode, minting it if unseen (learn-by-scan, the
/// same contract as `binmap::put_away`). Returns the location's record-id leaf.
pub async fn resolve_or_create_location(
    db: &SurrealDb,
    barcode: &str,
    name: Option<&str>,
    instance_id: &str,
) -> Result<String, String> {
    let barcode = barcode.trim();
    if barcode.is_empty() {
        return Err("empty location barcode".into());
    }

    let existing: Vec<Value> = db
        .query("SELECT record::id(id) AS rid FROM location WHERE barcode = $b LIMIT 1")
        .bind(("b", barcode.to_string()))
        .await
        .map_err(|e| format!("location lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("location lookup: {}", e))?;

    if let Some(l) = existing.first() {
        if let Some(leaf) = l.get("rid").and_then(|v| v.as_str()) {
            return Ok(leaf.to_string());
        }
    }

    let leaf = format!("bin_{}", slug(barcode));
    let display = name.unwrap_or(barcode);
    let vclock = next_vclock(db, "location", &leaf, instance_id).await;

    run_stmt!(
        "mint location",
        db.query(
            "UPSERT type::record($rid) MERGE { \
                barcode: $barcode, name: $name, complete_name: $name, \
                usage: 'internal', active: true, source: 'receiving', \
                _vclock: $vclock, created_at: time::now(), updated_at: time::now() \
             } RETURN NONE",
        )
        .bind(("rid", rid("location", &leaf)))
        .bind(("barcode", barcode.to_string()))
        .bind(("name", display.to_string()))
        .bind(("vclock", vclock))
    );

    Ok(leaf)
}

// ─── Input / output shapes ───────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct UnitInput {
    /// Client-minted UUID — the record key, so an offline retry converges.
    pub unit_id: String,
    /// item | box | part. Anything else is normalised to "item".
    #[serde(default)]
    pub kind: Option<String>,
    /// The code actually scanned off the thing (may be a bare serial).
    #[serde(default)]
    pub barcode: Option<String>,
    /// OUR human code. When absent the server assigns `RCV-YYYYMMDD-NNNN`.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReceiptInput {
    /// Client-minted UUID — the record key.
    pub receipt_id: String,
    #[serde(default)]
    pub postal_barcode: Option<String>,
    #[serde(default)]
    pub ocr_text: Option<String>,
    /// `stock_picking_delivery.tracking_number`, when the receiver matched the
    /// parcel against a scraped shipment. Nullable — most parcels aren't.
    #[serde(default)]
    pub shipment_tracking: Option<String>,
    /// How the thing got here: `parcel` (a carrier delivered it) or `walk_in`
    /// (a customer carried it to the counter). A walk-in has NO postal barcode,
    /// so it is the one receipt shape where `postal_barcode` is legitimately
    /// absent — everything downstream must cope. Absent → `parcel`.
    #[serde(default)]
    pub intake: Option<String>,
    /// The Zoho ticket number the receiver picked off
    /// `/api/receiving/tickets/lookup` (the human-facing "#32834").
    #[serde(default)]
    pub zoho_ticket: Option<String>,
    /// The `document` record-id leaf of that same ticket — the durable link;
    /// `zoho_ticket` is what a human reads.
    #[serde(default)]
    pub ticket_id: Option<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub client_address: Option<String>,
    #[serde(default)]
    pub packaging: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Acting warehouse user (id string), as the PDA knows them.
    #[serde(default)]
    pub received_by: Option<String>,
    /// Where the units land. Defaults to `ZONE-INBOUND`.
    #[serde(default)]
    pub location_barcode: Option<String>,
    #[serde(default)]
    pub units: Vec<UnitInput>,
    /// CAS uuids of photos already uploaded via `/api/upload/image`.
    #[serde(default)]
    pub photo_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredUnit {
    pub unit_id: String,
    pub code: String,
    pub location_barcode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredReceipt {
    pub receipt_id: String,
    /// Normalised `parcel` | `walk_in` — echoed so the PDA sees what was stored
    /// rather than what it hoped it sent.
    pub intake: String,
    pub zoho_ticket: Option<String>,
    pub ticket_id: Option<String>,
    pub units: Vec<StoredUnit>,
}

/// `walk_in` for anything the receiver flagged as a counter hand-over (the PDA
/// sends `walk_in`; be generous about the spelling), `parcel` otherwise —
/// including an absent value, so every pre-existing receipt reads as a parcel.
pub fn normalize_intake(intake: Option<&str>) -> &'static str {
    match intake
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
        .as_str()
    {
        "walk_in" | "walkin" | "counter" => "walk_in",
        _ => "parcel",
    }
}

/// Server-assigned human code for a unit nobody labelled: `RCV-YYYYMMDD-NNNN`,
/// the same shape (and the same random-suffix trick) as the repair order number
/// minted in `handlers::pda`.
pub fn generate_unit_code() -> String {
    format!(
        "RCV-{}-{:04}",
        chrono::Utc::now().format("%Y%m%d"),
        rand::random::<u16>() % 10000
    )
}

fn normalize_kind(kind: Option<&str>) -> &'static str {
    match kind.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "box" => "box",
        "part" => "part",
        _ => "item",
    }
}

/// `None`/empty → SurrealDB NONE, so an absent optional field stays absent
/// instead of being stored as an empty string the UI then has to special-case.
fn opt(v: Option<&String>) -> Value {
    match v.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => json!(s),
        None => Value::Null,
    }
}

// ─── Write path ──────────────────────────────────────────────────────────────

/// Idempotent upsert of a receipt and its units.
///
/// Everything is keyed by the client-minted ids, so POSTing the same body twice
/// (the offline-retry case) yields ONE receipt and ONE unit per `unit_id`:
///  * `created_at` is only stamped on the first write (read-before-write tells
///    us whether the row is new), `updated_at` on every write;
///  * a `code` already assigned to a unit is NEVER regenerated — otherwise the
///    label printed off the first response would stop matching the row;
///  * photo edges are checked for existence before RELATE, so a retry does not
///    fan out duplicate `has_attachment` edges.
///
/// Doctrine: a receipt with no units still produces one — the parcel is
/// physically on the floor whether or not anyone knew what was in it.
pub async fn upsert_receipt(
    db: &SurrealDb,
    instance_id: &str,
    device_uuid: &str,
    input: &ReceiptInput,
) -> Result<StoredReceipt, String> {
    let receipt_id = input.receipt_id.trim().to_string();
    if receipt_id.is_empty() {
        return Err("receipt_id is required".into());
    }

    let location_barcode = input
        .location_barcode
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(ZONE_INBOUND)
        .to_string();
    let location_leaf =
        resolve_or_create_location(db, &location_barcode, None, instance_id).await?;

    // ── header ──
    let existing_receipt: Option<Value> = db
        .query("SELECT created_at FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt", &receipt_id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    let receipt_is_new = existing_receipt.is_none();
    let vclock = next_vclock(db, "receipt", &receipt_id, instance_id).await;

    // `time::now()` must be produced IN SurrealQL: the SDK stopped coercing
    // RFC3339 strings into `datetime`, and a mixed-type column makes
    // `ORDER BY created_at DESC` sort by TYPE and bury the freshest rows.
    let ts = if receipt_is_new {
        "created_at: time::now(), updated_at: time::now()"
    } else {
        "updated_at: time::now()"
    };
    let intake = normalize_intake(input.intake.as_deref());
    let zoho_ticket = input
        .zoho_ticket
        .as_deref()
        .map(|s| s.trim().trim_start_matches('#').trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let ticket_id = input
        .ticket_id
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let q = format!(
        "UPSERT type::record($rid) MERGE {{ \
            postal_barcode: $postal_barcode, ocr_text: $ocr_text, \
            shipment_tracking: $shipment_tracking, intake: $intake, \
            zoho_ticket: $zoho_ticket, ticket_id: $ticket_id, \
            client_name: $client_name, client_address: $client_address, \
            packaging: $packaging, note: $note, \
            received_by: $received_by, received_by_device: $received_by_device, \
            location_barcode: $location_barcode, \
            status: 'received', _vclock: $vclock, {} \
         }} RETURN NONE",
        ts
    );
    run_stmt!(
        "receipt",
        db.query(&q)
            .bind(("rid", rid("receipt", &receipt_id)))
            .bind(("postal_barcode", opt(input.postal_barcode.as_ref())))
            .bind(("ocr_text", opt(input.ocr_text.as_ref())))
            .bind(("shipment_tracking", opt(input.shipment_tracking.as_ref())))
            .bind(("intake", json!(intake)))
            .bind(("zoho_ticket", opt(zoho_ticket.as_ref())))
            .bind(("ticket_id", opt(ticket_id.as_ref())))
            .bind(("client_name", opt(input.client_name.as_ref())))
            .bind(("client_address", opt(input.client_address.as_ref())))
            .bind(("packaging", opt(input.packaging.as_ref())))
            .bind(("note", opt(input.note.as_ref())))
            .bind(("received_by", opt(input.received_by.as_ref())))
            .bind(("received_by_device", json!(device_uuid)))
            .bind(("location_barcode", json!(location_barcode.clone())))
            .bind(("vclock", vclock))
    );

    // ── units ──
    // Owner doctrine: receiving ALWAYS ends with at least one unit on a place.
    // With nothing declared we still mint one anonymous unit off the receipt id
    // so the parcel is findable in the zone list.
    let mut declared: Vec<UnitInput> = input.units.clone();
    if declared.is_empty() {
        declared.push(UnitInput {
            unit_id: receipt_id.clone(),
            kind: Some("box".into()),
            barcode: input.postal_barcode.clone(),
            code: None,
            serial: None,
            note: None,
        });
    }

    let mut stored = Vec::with_capacity(declared.len());
    for u in &declared {
        if u.unit_id.trim().is_empty() {
            tracing::warn!("[receiving] receipt {}: unit without unit_id, skipped", receipt_id);
            continue;
        }
        stored.push(
            upsert_unit(
                db,
                instance_id,
                &receipt_id,
                u,
                &location_leaf,
                &location_barcode,
            )
            .await?,
        );
    }

    // ── photos ──
    for cas_id in &input.photo_ids {
        if let Err(e) = attach_photo(db, &receipt_id, cas_id).await {
            // Non-fatal on purpose: the parcel is already recorded, and losing
            // the record over a missing CAS id would be the worse failure.
            tracing::warn!("[receiving] attach photo {} to receipt {}: {}", cas_id, receipt_id, e);
        }
    }

    Ok(StoredReceipt {
        receipt_id,
        intake: intake.to_string(),
        zoho_ticket,
        ticket_id,
        units: stored,
    })
}

/// Write ONE unit onto a place, idempotently by its client-minted id. The only
/// place a `receipt_unit` row is created — `upsert_receipt` loops over it and
/// [`add_unit`] calls it for a unit the receiver adds to an ALREADY active
/// receipt, so the code-assignment rules can never drift apart between the two.
///
/// Precedence for the human code: the code the row already carries > the one
/// the client scanned (a `b-…` smart code) > a freshly assigned `RCV-` code.
async fn upsert_unit(
    db: &SurrealDb,
    instance_id: &str,
    receipt_id: &str,
    u: &UnitInput,
    location_leaf: &str,
    location_barcode: &str,
) -> Result<StoredUnit, String> {
    let unit_id = u.unit_id.trim().to_string();
    if unit_id.is_empty() {
        return Err("unit_id is required".into());
    }

    let prior: Option<Value> = db
        .query("SELECT code, created_at FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt_unit", &unit_id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    let unit_is_new = prior.is_none();
    let prior_code = prior
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let code = prior_code
        .or_else(|| {
            u.code
                .as_deref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(generate_unit_code);

    let vclock = next_vclock(db, "receipt_unit", &unit_id, instance_id).await;
    let ts = if unit_is_new {
        "created_at: time::now(), updated_at: time::now()"
    } else {
        "updated_at: time::now()"
    };
    // `status` is only asserted when the row is NEW: a unit already moved onto a
    // shelf (or put into repair) must not fall back to 'received' because the
    // PDA re-POSTed the receipt it belongs to.
    let status = if unit_is_new { "status: 'received', " } else { "" };
    let q = format!(
        "UPSERT type::record($rid) MERGE {{ \
            receipt_id: $receipt_id, kind: $kind, barcode: $barcode, \
            code: $code, serial: $serial, note: $note, \
            location_id: $location_id, location_barcode: $location_barcode, \
            {}_vclock: $vclock, {} \
         }} RETURN NONE",
        status, ts
    );
    run_stmt!(
        "receipt_unit",
        db.query(&q)
            .bind(("rid", rid("receipt_unit", &unit_id)))
            .bind(("receipt_id", json!(receipt_id.to_string())))
            .bind(("kind", json!(normalize_kind(u.kind.as_deref()))))
            .bind(("barcode", opt(u.barcode.as_ref())))
            .bind(("code", json!(code.clone())))
            .bind(("serial", opt(u.serial.as_ref())))
            .bind(("note", opt(u.note.as_ref())))
            .bind(("location_id", json!(location_leaf.to_string())))
            .bind(("location_barcode", json!(location_barcode.to_string())))
            .bind(("vclock", vclock))
    );

    Ok(StoredUnit {
        unit_id,
        code,
        location_barcode: location_barcode.to_string(),
    })
}

// ─── The active receipt (Beleg) — point operations on ONE receipt ────────────
//
// Owner doctrine: on the PDA the receiver ACTIVATES one receipt in the Lager
// list, and from then on everything the camera produces — a barcode, a photo,
// a page of OCR — is attached to THAT receipt, and the receipt's properties are
// changed from that same context (put a device into repair, take it back out).
// So the server side is not a second receiving workflow; it is a handful of
// small point operations on a receipt that already exists.

/// Cap for the two free-text columns a receiver can grow without bound
/// (`note`, `ocr_text`). 20 kB each: enough for a day of counter notes, small
/// enough that a runaway OCR loop cannot bloat a synced row into the mesh.
const TEXT_CAP: usize = 20_000;
const TRUNCATION_MARKER: &str = "[… older text truncated …]\n---\n";

/// Append `entry` to a bounded text column, dropping the OLDEST content when
/// the result would exceed [`TEXT_CAP`]. The newest text is what the receiver
/// is looking at, so the tail is what survives.
fn append_bounded(existing: Option<&str>, entry: &str) -> String {
    let combined = match existing.map(str::trim).filter(|s| !s.is_empty()) {
        Some(prev) => format!("{}\n---\n{}", prev, entry),
        None => entry.to_string(),
    };
    if combined.len() <= TEXT_CAP {
        return combined;
    }
    // Keep the tail, cut on a char boundary, and say so.
    let keep = TEXT_CAP.saturating_sub(TRUNCATION_MARKER.len());
    let mut cut = combined.len() - keep;
    while cut < combined.len() && !combined.is_char_boundary(cut) {
        cut += 1;
    }
    format!("{}{}", TRUNCATION_MARKER, &combined[cut..])
}

/// Read one receipt row (all stored fields, with the record-id leaf projected
/// as `receipt_id`).
async fn load_receipt(db: &SurrealDb, receipt_id: &str) -> Result<Value, String> {
    let row: Option<Value> = db
        .query("SELECT record::id(id) AS receipt_id, * FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt", receipt_id)))
        .await
        .map_err(|e| format!("receipt lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("receipt lookup: {}", e))?;
    row.ok_or_else(|| format!("receipt {} not found", receipt_id))
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The serial a repair order for this unit would be keyed on: the serial the
/// receiver captured, else the raw barcode scanned off the device.
fn repair_key_for(unit: &Value) -> Option<String> {
    str_field(unit, "serial").or_else(|| str_field(unit, "barcode"))
}

async fn load_unit(db: &SurrealDb, unit_id: &str) -> Result<Value, String> {
    let row: Option<Value> = db
        .query("SELECT record::id(id) AS unit_id, * FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt_unit", unit_id)))
        .await
        .map_err(|e| format!("unit lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("unit lookup: {}", e))?;
    row.ok_or_else(|| format!("unit {} not found", unit_id))
}

/// One unit of the active receipt, in the shape the PDA renders, with the live
/// repair order (if any) folded in.
async fn unit_view(db: &SurrealDb, unit: &Value) -> Value {
    let loc = unit
        .get("location_barcode")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let repair = match repair_key_for(unit) {
        Some(serial) => crate::services::repair::find_live_order(db, &serial)
            .await
            .unwrap_or(None)
            .map(|o| {
                json!({
                    "order_uuid": o.get("order_uuid").cloned().unwrap_or(Value::Null),
                    "order_number": o.get("order_number").cloned().unwrap_or(Value::Null),
                    "status": o.get("status").cloned().unwrap_or(Value::Null),
                })
            })
            .unwrap_or(Value::Null),
        None => Value::Null,
    };
    json!({
        "unit_id": unit.get("unit_id").cloned().unwrap_or(Value::Null),
        "code": unit.get("code").cloned().unwrap_or(Value::Null),
        "kind": unit.get("kind").cloned().unwrap_or(Value::Null),
        "barcode": unit.get("barcode").cloned().unwrap_or(Value::Null),
        "serial": unit.get("serial").cloned().unwrap_or(Value::Null),
        "note": unit.get("note").cloned().unwrap_or(Value::Null),
        "location_id": unit.get("location_id").cloned().unwrap_or(Value::Null),
        "location_barcode": unit.get("location_barcode").cloned().unwrap_or(Value::Null),
        "zone": zone_for(loc),
        "status": unit.get("status").cloned().unwrap_or(Value::Null),
        "created_at": unit.get("created_at").cloned().unwrap_or(Value::Null),
        "repair": repair,
    })
}

/// Everything the phone needs to render the ACTIVE receipt: the header as
/// stored, its units (each with the live repair order folded in) and the photos
/// hanging off it.
pub async fn get_receipt_context(db: &SurrealDb, receipt_id: &str) -> Result<Value, String> {
    let receipt = load_receipt(db, receipt_id).await?;

    let units: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS unit_id, * FROM receipt_unit \
             WHERE receipt_id = $r ORDER BY created_at ASC",
        )
        .bind(("r", receipt_id.to_string()))
        .await
        .map_err(|e| format!("units lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("units lookup: {}", e))?;

    let mut unit_views = Vec::with_capacity(units.len());
    for u in &units {
        unit_views.push(unit_view(db, u).await);
    }

    // Same `has_attachment` edge `handlers::files` walks, projected down to what
    // the phone's gallery needs.
    let photos: Vec<Value> = db
        .query(
            "SELECT out.cas_uuid AS cas_uuid, out.mime_type AS mime_type, \
                    out.context AS context, out.original_name AS original_name, \
                    out.expires_at AS expires_at, out.recognized_text AS recognized_text, \
                    label, created_at \
             FROM has_attachment \
             WHERE in = type::record('receipt', $r) \
             ORDER BY created_at DESC",
        )
        .bind(("r", receipt_id.to_string()))
        .await
        .map_err(|e| format!("photos lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("photos lookup: {}", e))?;

    // Scan frames (the camera frame a barcode/OCR came from) ride along in
    // `photos` — carrying `context`/`expires_at`/`recognized_text` so the phone
    // can dim them — but they are evidence, not photos: they never inflate
    // `photo_count`, which is what the UI badges and the "has photos" checks use.
    let scan_frame_count = photos
        .iter()
        .filter(|p| p.get("context").and_then(|v| v.as_str()) == Some(SCAN_FRAME_CONTEXT))
        .count();

    Ok(json!({
        "receipt": receipt,
        "units": unit_views,
        "photos": photos,
        "photo_count": photos.len() - scan_frame_count,
        "scan_frame_count": scan_frame_count,
    }))
}

/// Add ONE unit to an already active receipt — the camera found a second device
/// in the same parcel. Idempotent by `unit_id`; the unit lands on the receipt's
/// own place, and gets an `RCV-` code by the same rules as the receipt upsert.
pub async fn add_unit(
    db: &SurrealDb,
    instance_id: &str,
    receipt_id: &str,
    unit: &UnitInput,
) -> Result<Value, String> {
    let receipt = load_receipt(db, receipt_id).await?;
    let location_barcode = str_field(&receipt, "location_barcode")
        .unwrap_or_else(|| ZONE_INBOUND.to_string());
    let location_leaf =
        resolve_or_create_location(db, &location_barcode, None, instance_id).await?;

    let stored = upsert_unit(
        db,
        instance_id,
        receipt_id,
        unit,
        &location_leaf,
        &location_barcode,
    )
    .await?;

    // The receipt itself changed shape — bump it so the mesh carries the new
    // unit's parent along with it.
    touch_receipt(db, instance_id, receipt_id).await;

    let row = load_unit(db, &stored.unit_id).await?;
    Ok(json!({
        "ok": true,
        "unit": unit_view(db, &row).await,
    }))
}

/// `updated_at` + `_vclock` bump on the receipt header, for changes that happen
/// on its children. Non-fatal: the child write is the truth.
async fn touch_receipt(db: &SurrealDb, instance_id: &str, receipt_id: &str) {
    let vclock = next_vclock(db, "receipt", receipt_id, instance_id).await;
    if let Err(e) = db
        .query("UPDATE type::record($rid) SET _vclock = $vclock, updated_at = time::now() RETURN NONE")
        .bind(("rid", rid("receipt", receipt_id)))
        .bind(("vclock", vclock))
        .await
        .and_then(|r| r.check())
    {
        tracing::warn!("[receiving] touch receipt {}: {}", receipt_id, e);
    }
}

/// Append a line to the receipt's note. `source = "ocr"` ALSO appends to
/// `ocr_text` — the OCR is kept as its own column (the fuzzy ticket matcher
/// reads it) as well as in the human log the receiver actually looks at.
pub async fn append_note(
    db: &SurrealDb,
    instance_id: &str,
    receipt_id: &str,
    text: &str,
    source: &str,
) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("text is required".into());
    }
    let source = match source.trim().to_ascii_lowercase().as_str() {
        "ocr" => "ocr",
        _ => "manual",
    };

    let receipt = load_receipt(db, receipt_id).await?;
    let entry = format!(
        "{} [{}] {}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
        source,
        text
    );
    let note = append_bounded(receipt.get("note").and_then(|v| v.as_str()), &entry);
    let ocr_text = if source == "ocr" {
        append_bounded(receipt.get("ocr_text").and_then(|v| v.as_str()), &entry)
    } else {
        receipt
            .get("ocr_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let vclock = next_vclock(db, "receipt", receipt_id, instance_id).await;
    run_stmt!(
        "append note",
        db.query(
            "UPDATE type::record($rid) SET note = $note, ocr_text = $ocr_text, \
                _vclock = $vclock, updated_at = time::now() RETURN NONE",
        )
        .bind(("rid", rid("receipt", receipt_id)))
        .bind(("note", json!(note)))
        .bind(("ocr_text", if ocr_text.is_empty() { Value::Null } else { json!(ocr_text) }))
        .bind(("vclock", vclock))
    );

    Ok(json!({ "ok": true, "receipt": load_receipt(db, receipt_id).await? }))
}

/// Put one unit of the active receipt into repair, or take it back out.
///
/// `add` opens (or finds) the ONE live repair order for the unit's serial —
/// exactly the order the PDA's `device_bound` slot bind would open, through the
/// same `services::repair` function — and flags the unit `in_repair`.
/// `remove` cancels that order and puts the unit back to `received`. Both
/// directions are idempotent: repeating either is a no-op that reports the
/// state it arrived at.
pub async fn set_repair(
    db: &SurrealDb,
    instance_id: &str,
    receipt_id: &str,
    unit_id: &str,
    action: &str,
) -> Result<Value, String> {
    let receipt = load_receipt(db, receipt_id).await?;
    let unit = load_unit(db, unit_id).await?;
    if str_field(&unit, "receipt_id").as_deref() != Some(receipt_id) {
        return Err(format!(
            "unit {} does not belong to receipt {}",
            unit_id, receipt_id
        ));
    }
    let serial = repair_key_for(&unit)
        .ok_or_else(|| format!("unit {} has neither serial nor barcode", unit_id))?;

    let (repair, created, new_status) = match action.trim().to_ascii_lowercase().as_str() {
        "add" => {
            let (order, created) = crate::services::repair::open_repair_order(
                db,
                instance_id,
                &crate::services::repair::OpenRepair {
                    serial,
                    // Deliberately no issue text: the receipt note is the
                    // receiver's log, not the customer's complaint.
                    issue: None,
                    customer_name: str_field(&receipt, "client_name"),
                    receipt_uuid: Some(receipt_id.to_string()),
                    zoho_ticket: str_field(&receipt, "zoho_ticket"),
                    ticket_id: str_field(&receipt, "ticket_id"),
                    source: "receiving/context".into(),
                    created_by_device: str_field(&receipt, "received_by_device"),
                    acting_user_id: str_field(&receipt, "received_by"),
                    owner_user_id: None,
                },
            )
            .await?;
            (Some(order), created, "in_repair")
        }
        "remove" => {
            let live = crate::services::repair::find_live_order(db, &serial).await?;
            let cancelled = match live {
                Some(o) => {
                    let uuid = o
                        .get("order_uuid")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    crate::services::repair::cancel_repair_order(
                        db,
                        instance_id,
                        &uuid,
                        "removed via receiving context",
                    )
                    .await?
                }
                None => None,
            };
            (cancelled, false, "received")
        }
        other => return Err(format!("unknown action '{}' (add|remove)", other)),
    };

    let vclock = next_vclock(db, "receipt_unit", unit_id, instance_id).await;
    run_stmt!(
        "unit repair status",
        db.query(
            "UPDATE type::record($rid) SET status = $status, \
                _vclock = $vclock, updated_at = time::now() RETURN NONE",
        )
        .bind(("rid", rid("receipt_unit", unit_id)))
        .bind(("status", json!(new_status)))
        .bind(("vclock", vclock))
    );
    touch_receipt(db, instance_id, receipt_id).await;

    let row = load_unit(db, unit_id).await?;
    Ok(json!({
        "ok": true,
        "unit": unit_view(db, &row).await,
        "repair": repair.map(|o| json!({
            "order_uuid": o.get("order_uuid").cloned().unwrap_or(Value::Null),
            "order_number": o.get("order_number").cloned().unwrap_or(Value::Null),
            "status": o.get("status").cloned().unwrap_or(Value::Null),
        })).unwrap_or(Value::Null),
        "created": created,
    }))
}

/// `SELECT count() AS c … GROUP ALL` -> the count (0 when no rows / on error).
/// Never use `SELECT VALUE count()` here: it comes back as an object.
async fn count_rows(db: &SurrealDb, sql: &str, bind: (&'static str, String)) -> i64 {
    let rows: Vec<Value> = db
        .query(sql)
        .bind(bind)
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    rows.first()
        .and_then(|r| r.get("c"))
        .and_then(|c| c.as_i64())
        .unwrap_or(0)
}

/// RELATE one already-uploaded CAS file onto the receipt, mirroring the generic
/// `handlers::files::upload` edge. Idempotent: a retry finds the edge and skips.
async fn attach_photo(db: &SurrealDb, receipt_id: &str, cas_id: &str) -> Result<(), String> {
    let cas_id = cas_id.trim();
    if cas_id.is_empty() {
        return Err("empty photo id".into());
    }
    let rows: Vec<Value> = db
        .query("SELECT record::id(id) AS rid FROM file_resource WHERE cas_uuid = $fid LIMIT 1")
        .bind(("fid", cas_id.to_string()))
        .await
        .map_err(|e| format!("file lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("file lookup: {}", e))?;
    let file_leaf = rows
        .first()
        .and_then(|r| r.get("rid"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "file_resource row not found".to_string())?
        .to_string();

    let dupe: Vec<Value> = db
        .query(
            "SELECT id FROM has_attachment \
             WHERE in = type::record('receipt', $r) AND out = type::record('file_resource', $f) \
             LIMIT 1",
        )
        .bind(("r", receipt_id.to_string()))
        .bind(("f", file_leaf.clone()))
        .await
        .map_err(|e| format!("edge lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("edge lookup: {}", e))?;
    if !dupe.is_empty() {
        return Ok(());
    }

    run_stmt!(
        "relate photo",
        db.query(
            "RELATE (type::record('receipt', $r)) -> has_attachment -> \
             (type::record('file_resource', $f)) \
             SET created_at = time::now(), label = 'receiving'",
        )
        .bind(("r", receipt_id.to_string()))
        .bind(("f", file_leaf))
    );
    Ok(())
}

/// Move a unit onto another place (lookup-or-create), marking it "moved".
/// Status stays "moved" even when the target is the outbound zone — "shipped"
/// is a later, separate act (the parcel actually leaving), not a shelf change.
pub async fn move_unit(
    db: &SurrealDb,
    instance_id: &str,
    unit_id: &str,
    location_barcode: &str,
) -> Result<Value, String> {
    let unit_id = unit_id.trim();
    let location_barcode = location_barcode.trim();
    if unit_id.is_empty() {
        return Err("unit_id is required".into());
    }
    if location_barcode.is_empty() {
        return Err("location_barcode is required".into());
    }

    let existing: Option<Value> = db
        .query("SELECT code FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt_unit", unit_id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    if existing.is_none() {
        return Err(format!("unit {} not found", unit_id));
    }

    let location_leaf =
        resolve_or_create_location(db, location_barcode, None, instance_id).await?;
    let vclock = next_vclock(db, "receipt_unit", unit_id, instance_id).await;

    run_stmt!(
        "move unit",
        db.query(
            "UPSERT type::record($rid) MERGE { \
                location_id: $location_id, location_barcode: $location_barcode, \
                status: 'moved', _vclock: $vclock, updated_at: time::now() \
             } RETURN NONE",
        )
        .bind(("rid", rid("receipt_unit", unit_id)))
        .bind(("location_id", json!(location_leaf)))
        .bind(("location_barcode", json!(location_barcode)))
        .bind(("vclock", vclock))
    );

    Ok(json!({
        "success": true,
        "unit_id": unit_id,
        "location_barcode": location_barcode,
        "zone": zone_for(location_barcode),
    }))
}

// ─── Read path ───────────────────────────────────────────────────────────────

/// Units currently sitting in a zone, newest first, joined with their receipt
/// header. `zone` is inbound | outbound | all.
pub async fn list_units(db: &SurrealDb, zone: &str, limit: usize) -> Result<Vec<Value>, String> {
    // LIMIT takes a literal (clamped) rather than a bind param — the zone
    // filter is the only variable part and it is a bound value, not text.
    let limit = limit.clamp(1, 1000);
    let (where_clause, filter_barcode) = match zone_filter_barcode(zone) {
        Some(b) => ("WHERE location_barcode = $zone_barcode ", Some(b)),
        None => ("", None),
    };
    let q = format!(
        "SELECT record::id(id) AS unit_id, code, kind, barcode, serial, status, \
                location_barcode, receipt_id, created_at \
         FROM receipt_unit {}ORDER BY created_at DESC LIMIT {}",
        where_clause, limit
    );

    let mut query = db.query(&q);
    if let Some(b) = filter_barcode {
        query = query.bind(("zone_barcode", b.to_string()));
    }
    let units: Vec<Value> = query
        .await
        .map_err(|e| format!("list units: {}", e))?
        .take(0)
        .map_err(|e| format!("list units: {}", e))?;

    // Receipt headers + photo counts, memoised per receipt — several units
    // normally share one parcel, so this is far fewer round-trips than N.
    let mut headers: std::collections::HashMap<String, (Value, i64)> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(units.len());
    for u in units {
        let receipt_id = u
            .get("receipt_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !receipt_id.is_empty() && !headers.contains_key(&receipt_id) {
            let header: Value = db
                .query(
                    "SELECT postal_barcode, shipment_tracking, client_name, \
                            intake, zoho_ticket, ticket_id, \
                            received_by, created_at \
                     FROM type::record($rid) LIMIT 1",
                )
                .bind(("rid", rid("receipt", &receipt_id)))
                .await
                .and_then(|mut r| r.take(0))
                .ok()
                .flatten()
                .unwrap_or(Value::Null);
            // `SELECT VALUE count() … GROUP ALL` yields an OBJECT on this
            // SurrealDB ("Expected int, got object"), so project it as a field.
            let photo_count = count_rows(
                db,
                // Scan-frame evidence is not a photo — it must not light up the
                // camera badge in the receiving list.
                "SELECT count() AS c FROM has_attachment \
                 WHERE in = type::record('receipt', $r) \
                 AND out.context != 'scan_frame' GROUP ALL",
                ("r", receipt_id.clone()),
            )
            .await;
            headers.insert(receipt_id.clone(), (header, photo_count));
        }

        let (header, photo_count) = headers
            .get(&receipt_id)
            .cloned()
            .unwrap_or((Value::Null, 0));
        let h = |k: &str| header.get(k).cloned().unwrap_or(Value::Null);
        let loc = u
            .get("location_barcode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        out.push(json!({
            "unit_id": u.get("unit_id").cloned().unwrap_or(Value::Null),
            "code": u.get("code").cloned().unwrap_or(Value::Null),
            "kind": u.get("kind").cloned().unwrap_or(Value::Null),
            "barcode": u.get("barcode").cloned().unwrap_or(Value::Null),
            "serial": u.get("serial").cloned().unwrap_or(Value::Null),
            "status": u.get("status").cloned().unwrap_or(Value::Null),
            "location_barcode": u.get("location_barcode").cloned().unwrap_or(Value::Null),
            "zone": zone_for(&loc),
            "receipt_id": receipt_id,
            "postal_barcode": h("postal_barcode"),
            "shipment_tracking": h("shipment_tracking"),
            // Receipts written before the walk-in lane existed carry no
            // `intake` — they were all parcels, so read the absence that way
            // instead of handing the UI a null it has to special-case.
            "intake": match h("intake") {
                Value::Null => json!("parcel"),
                v => v,
            },
            "zoho_ticket": h("zoho_ticket"),
            "ticket_id": h("ticket_id"),
            "client_name": h("client_name"),
            "received_by": h("received_by"),
            "received_at": u.get("created_at").cloned().unwrap_or(Value::Null),
            "photo_count": photo_count,
        }));
    }

    Ok(out)
}

/// Case-insensitive exact lookup of a unit by the code scanned off the thing OR
/// by our own `RCV-`/smart code, with its receipt header folded in. Used by the
/// raw-barcode leg of `/api/scan` — BEFORE the unknown-barcode stub-product
/// auto-create, so scanning a parcel we received does not mint a bogus product.
pub async fn lookup_unit_by_code(db: &SurrealDb, code: &str) -> Option<Value> {
    let needle = code.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS unit_id, * FROM receipt_unit \
             WHERE string::lowercase(barcode ?? '') = $b \
                OR string::lowercase(code ?? '') = $b \
             LIMIT 1",
        )
        .bind(("b", needle))
        .await
        .ok()?
        .take(0)
        .ok()?;
    let unit = rows.into_iter().next()?;

    let receipt_id = unit.get("receipt_id").and_then(|v| v.as_str()).unwrap_or("");
    let receipt: Value = if receipt_id.is_empty() {
        Value::Null
    } else {
        db.query("SELECT * FROM type::record($rid) LIMIT 1")
            .bind(("rid", rid("receipt", receipt_id)))
            .await
            .and_then(|mut r| r.take::<Option<Value>>(0))
            .ok()
            .flatten()
            .unwrap_or(Value::Null)
    };

    let loc = unit
        .get("location_barcode")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut out = unit.clone();
    if let Some(o) = out.as_object_mut() {
        o.insert("zone".into(), json!(zone_for(loc)));
        o.insert("receipt".into(), receipt);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tables the receiving path touches, DEFINEd exactly as `main.rs` does at
    /// boot — a `mem://` store has none of them and the writes would no-op.
    async fn test_db() -> SurrealDb {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();
        db.query(
            "DEFINE TABLE IF NOT EXISTS location SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS receipt SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS receipt_unit SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS file_resource SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS order SCHEMALESS;
             DEFINE TABLE OVERWRITE has_attachment TYPE RELATION SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    fn sample(receipt_id: &str, unit_id: &str) -> ReceiptInput {
        ReceiptInput {
            receipt_id: receipt_id.into(),
            postal_barcode: Some("00340434161094123456".into()),
            ocr_text: Some("ZetaBody Deutschland".into()),
            shipment_tracking: None,
            intake: None,
            zoho_ticket: None,
            ticket_id: None,
            client_name: Some("ZetaBody".into()),
            client_address: None,
            packaging: Some("Karton".into()),
            note: None,
            received_by: Some("dmytro".into()),
            location_barcode: None,
            units: vec![UnitInput {
                unit_id: unit_id.into(),
                kind: Some("box".into()),
                barcode: Some("SN-4711".into()),
                code: None,
                serial: Some("4711".into()),
                note: None,
            }],
            photo_ids: vec![],
        }
    }

    #[test]
    fn zone_derivation() {
        assert_eq!(zone_for("ZONE-INBOUND"), "inbound");
        assert_eq!(zone_for(" zone-inbound "), "inbound");
        assert_eq!(zone_for("ZONE-OUTBOUND"), "outbound");
        assert_eq!(zone_for("A-01-03"), "other");
        assert_eq!(zone_for(""), "other");
    }

    #[tokio::test]
    async fn ensure_zone_locations_is_idempotent() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;
        ensure_zone_locations(&db, "node-a").await;

        let n: Option<i64> = Some(count_rows(&db, "SELECT count() AS c FROM location GROUP ALL", ("x", String::new())).await);
        assert_eq!(n, Some(2));

        let names: Vec<String> = db
            .query("SELECT VALUE name FROM location ORDER BY name")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(names, vec!["Ausgang".to_string(), "Eingang".to_string()]);
    }

    /// The offline-retry case: the PDA POSTs, the ack is lost, it POSTs again.
    /// One receipt, one unit — and the unit keeps the code it was first given.
    #[tokio::test]
    async fn receipt_upsert_is_idempotent() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;
        let input = sample("r-1", "u-1");

        let first = upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        let second = upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();

        let receipts: Option<i64> = Some(count_rows(&db, "SELECT count() AS c FROM receipt GROUP ALL", ("x", String::new())).await);
        let units: Option<i64> = Some(count_rows(&db, "SELECT count() AS c FROM receipt_unit GROUP ALL", ("x", String::new())).await);
        assert_eq!(receipts, Some(1));
        assert_eq!(units, Some(1));
        assert_eq!(first.units[0].code, second.units[0].code);
        assert_eq!(first.units[0].location_barcode, ZONE_INBOUND);
    }

    /// A unit with no code of its own gets `RCV-YYYYMMDD-NNNN`; one that scanned
    /// its own smart code keeps it.
    #[tokio::test]
    async fn code_is_assigned_when_absent_and_preserved_when_given() {
        let db = test_db().await;
        let stored = upsert_receipt(&db, "node-a", "dev-1", &sample("r-2", "u-2"))
            .await
            .unwrap();
        assert!(
            stored.units[0].code.starts_with("RCV-"),
            "expected an RCV- code, got {}",
            stored.units[0].code
        );
        assert_eq!(stored.units[0].code.len(), "RCV-20260907-0001".len());

        let mut with_code = sample("r-3", "u-3");
        with_code.units[0].code = Some("b-abcdef".into());
        let stored = upsert_receipt(&db, "node-a", "dev-1", &with_code).await.unwrap();
        assert_eq!(stored.units[0].code, "b-abcdef");
    }

    /// Doctrine: receiving must always end with at least one unit on a place,
    /// even when the receiver declared nothing about the parcel.
    #[tokio::test]
    async fn receipt_without_units_still_produces_one() {
        let db = test_db().await;
        let mut input = sample("r-4", "unused");
        input.units.clear();
        let stored = upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        assert_eq!(stored.units.len(), 1);
        assert_eq!(stored.units[0].location_barcode, ZONE_INBOUND);
    }

    #[test]
    fn intake_normalisation() {
        assert_eq!(normalize_intake(Some("walk_in")), "walk_in");
        assert_eq!(normalize_intake(Some(" Walk-In ")), "walk_in");
        assert_eq!(normalize_intake(Some("walk in")), "walk_in");
        assert_eq!(normalize_intake(Some("parcel")), "parcel");
        // Absent = the pre-walk-in world: everything arrived by carrier.
        assert_eq!(normalize_intake(None), "parcel");
        assert_eq!(normalize_intake(Some("nonsense")), "parcel");
    }

    /// The walk-in lane end to end: a customer carries a device to the counter,
    /// so there is NO postal barcode at all — only the ticket the receiver
    /// picked off `/receiving/tickets/lookup`. Upsert, list and the `/scan`
    /// raw-barcode lookup must all survive that absence.
    #[tokio::test]
    async fn walk_in_receipt_without_postal_barcode() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;

        let mut input = sample("r-walk", "u-walk");
        input.postal_barcode = None;
        input.ocr_text = Some(
            "Reparaturauftrag Magic Fit Dornbirn Bildgasse 18 6850 S/N 181700403".into(),
        );
        input.intake = Some("walk_in".into());
        // The receiver may paste the number with the hash a human would write.
        input.zoho_ticket = Some("#32834".into());
        input.ticket_id = Some("doc-32834".into());
        input.client_name = Some("Magic Fit Dornbirn".into());
        input.units[0].barcode = Some("181700403".into());
        input.units[0].serial = Some("181700403".into());

        let stored = upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        assert_eq!(stored.intake, "walk_in");
        assert_eq!(stored.zoho_ticket.as_deref(), Some("32834"));
        assert_eq!(stored.ticket_id.as_deref(), Some("doc-32834"));
        assert_eq!(stored.units.len(), 1);

        // Idempotent retry, as for a parcel.
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        let receipts = count_rows(&db, "SELECT count() AS c FROM receipt GROUP ALL", ("x", String::new())).await;
        assert_eq!(receipts, 1);

        let rows = list_units(&db, "inbound", 200).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["intake"], json!("walk_in"));
        assert_eq!(rows[0]["zoho_ticket"], json!("32834"));
        assert_eq!(rows[0]["ticket_id"], json!("doc-32834"));
        assert_eq!(rows[0]["postal_barcode"], Value::Null);
        assert_eq!(rows[0]["client_name"], json!("Magic Fit Dornbirn"));

        // `/api/scan`: the device serial resolves to the unit, not a stub product.
        let hit = lookup_unit_by_code(&db, "181700403").await.expect("by serial");
        assert_eq!(hit["unit_id"], json!("u-walk"));
        assert_eq!(hit["receipt"]["intake"], json!("walk_in"));
        assert_eq!(hit["receipt"]["zoho_ticket"], json!("32834"));

        // A parcel receipt still reads as `parcel` in the same list.
        upsert_receipt(&db, "node-a", "dev-1", &sample("r-parcel", "u-parcel"))
            .await
            .unwrap();
        let rows = list_units(&db, "inbound", 200).await.unwrap();
        let parcel = rows
            .iter()
            .find(|r| r["unit_id"] == json!("u-parcel"))
            .expect("parcel row");
        assert_eq!(parcel["intake"], json!("parcel"));
    }

    /// A walk-in with nothing scanned at all: no postal barcode, no units. The
    /// anonymous unit is still minted (owner doctrine) and still findable.
    #[tokio::test]
    async fn walk_in_without_units_or_barcode_still_lands_on_a_place() {
        let db = test_db().await;
        let mut input = sample("r-walk-bare", "unused");
        input.postal_barcode = None;
        input.intake = Some("walk_in".into());
        input.units.clear();

        let stored = upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        assert_eq!(stored.units.len(), 1);
        assert!(stored.units[0].code.starts_with("RCV-"));
        let hit = lookup_unit_by_code(&db, &stored.units[0].code)
            .await
            .expect("by minted code");
        assert_eq!(hit["receipt"]["intake"], json!("walk_in"));
    }

    #[tokio::test]
    async fn move_changes_zone() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;
        upsert_receipt(&db, "node-a", "dev-1", &sample("r-5", "u-5"))
            .await
            .unwrap();

        let inbound = list_units(&db, "inbound", 200).await.unwrap();
        assert_eq!(inbound.len(), 1);
        assert_eq!(inbound[0]["zone"], json!("inbound"));

        let moved = move_unit(&db, "node-a", "u-5", ZONE_OUTBOUND).await.unwrap();
        assert_eq!(moved["zone"], json!("outbound"));

        assert!(list_units(&db, "inbound", 200).await.unwrap().is_empty());
        let outbound = list_units(&db, "outbound", 200).await.unwrap();
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0]["status"], json!("moved"));
        assert_eq!(outbound[0]["zone"], json!("outbound"));

        // A normal shelf is neither zone, and drops out of both lists.
        move_unit(&db, "node-a", "u-5", "A-01-03").await.unwrap();
        assert!(list_units(&db, "outbound", 200).await.unwrap().is_empty());
        let all = list_units(&db, "all", 200).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["zone"], json!("other"));
    }

    #[tokio::test]
    async fn move_unknown_unit_is_an_error() {
        let db = test_db().await;
        assert!(move_unit(&db, "node-a", "nope", ZONE_OUTBOUND).await.is_err());
    }

    /// The `/api/scan` leg: a received unit must be findable by the code scanned
    /// off it AND by our own code, case-insensitively, header folded in.
    #[tokio::test]
    async fn lookup_matches_barcode_and_code_case_insensitively() {
        let db = test_db().await;
        let stored = upsert_receipt(&db, "node-a", "dev-1", &sample("r-6", "u-6"))
            .await
            .unwrap();

        let hit = lookup_unit_by_code(&db, "sn-4711").await.expect("by barcode");
        assert_eq!(hit["unit_id"], json!("u-6"));
        assert_eq!(hit["zone"], json!("inbound"));
        assert_eq!(hit["receipt"]["client_name"], json!("ZetaBody"));

        let hit = lookup_unit_by_code(&db, &stored.units[0].code.to_lowercase())
            .await
            .expect("by code");
        assert_eq!(hit["unit_id"], json!("u-6"));

        assert!(lookup_unit_by_code(&db, "nothing-here").await.is_none());
    }

    /// `list_units` carries the receipt header, and `photo_count` counts the
    /// `has_attachment` edges hanging on the receipt (zero without photos).
    #[tokio::test]
    async fn list_carries_receipt_header_and_photo_count() {
        let db = test_db().await;
        db.query("CREATE file_resource SET cas_uuid = 'cas-7', filename = 'box.webp'")
            .await
            .unwrap()
            .check()
            .unwrap();
        let mut input = sample("r-7", "u-7");
        input.photo_ids = vec!["cas-7".into(), "missing-cas".into()];
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        // retry must not duplicate the edge
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        let rows = list_units(&db, "all", 200).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["postal_barcode"], json!("00340434161094123456"));
        assert_eq!(rows[0]["client_name"], json!("ZetaBody"));
        assert_eq!(rows[0]["received_by"], json!("dmytro"));
        assert_eq!(rows[0]["photo_count"], json!(1));
        assert_eq!(rows[0]["kind"], json!("box"));
    }

    /// Scan frames are evidence, not photos: they hang on the receipt like any
    /// attachment but must not inflate `photo_count` in either the list or the
    /// receipt context — the context reports them separately as
    /// `scan_frame_count` and hands the phone what it needs to dim them.
    #[tokio::test]
    async fn photo_count_excludes_scan_frames() {
        let db = test_db().await;
        db.query(
            "CREATE file_resource SET cas_uuid = 'cas-real', mime_type = 'image/webp', \
             context = 'receiving'; \
             CREATE file_resource SET cas_uuid = 'cas-frame', mime_type = 'image/webp', \
             context = 'scan_frame', temp = true, priority = 'low', \
             recognized_text = '00340434161094123456', expires_at = time::now() + 14d",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let mut input = sample("r-sf", "u-sf");
        input.photo_ids = vec!["cas-real".into(), "cas-frame".into()];
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();

        let rows = list_units(&db, "all", 200).await.unwrap();
        assert_eq!(rows[0]["photo_count"], json!(1), "list counts the real photo only");

        let ctx = get_receipt_context(&db, "r-sf").await.unwrap();
        assert_eq!(ctx["photo_count"], json!(1));
        assert_eq!(ctx["scan_frame_count"], json!(1));
        assert_eq!(ctx["photos"].as_array().unwrap().len(), 2, "frames still ride along");
        let frame = ctx["photos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["cas_uuid"] == json!("cas-frame"))
            .expect("frame in photos");
        assert_eq!(frame["context"], json!("scan_frame"));
        assert_eq!(frame["recognized_text"], json!("00340434161094123456"));
        assert!(!frame["expires_at"].is_null(), "phone needs the TTL to dim it");
    }

    // ─── The active receipt (Beleg) ──────────────────────────────────────────

    /// The one round-trip the phone makes when the receiver activates a
    /// receipt: header, units, photos — and the live repair order folded into
    /// the unit that has one.
    #[tokio::test]
    async fn context_carries_units_photos_and_repair_status() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;
        db.query("CREATE file_resource SET cas_uuid = 'cas-ctx', mime_type = 'image/webp', context = 'receiving'")
            .await
            .unwrap()
            .check()
            .unwrap();
        let mut input = sample("r-ctx", "u-ctx");
        input.photo_ids = vec!["cas-ctx".into()];
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();

        let ctx = get_receipt_context(&db, "r-ctx").await.unwrap();
        assert_eq!(ctx["receipt"]["client_name"], json!("ZetaBody"));
        assert_eq!(ctx["receipt"]["packaging"], json!("Karton"));
        assert_eq!(ctx["units"].as_array().unwrap().len(), 1);
        assert_eq!(ctx["units"][0]["unit_id"], json!("u-ctx"));
        assert_eq!(ctx["units"][0]["zone"], json!("inbound"));
        assert_eq!(ctx["units"][0]["repair"], Value::Null);
        assert_eq!(ctx["photo_count"], json!(1));
        assert_eq!(ctx["photos"][0]["cas_uuid"], json!("cas-ctx"));
        assert_eq!(ctx["photos"][0]["mime_type"], json!("image/webp"));

        // Put it into repair from the receipt context — the order shows up on
        // the unit the next time the phone reads the context.
        let res = set_repair(&db, "node-a", "r-ctx", "u-ctx", "add").await.unwrap();
        assert_eq!(res["created"], json!(true));
        let ctx = get_receipt_context(&db, "r-ctx").await.unwrap();
        assert_eq!(ctx["units"][0]["status"], json!("in_repair"));
        let repair = &ctx["units"][0]["repair"];
        assert_eq!(repair["status"], json!("pending"));
        assert!(repair["order_number"].as_str().unwrap().starts_with("REP-"));

        assert!(get_receipt_context(&db, "nope").await.is_err());
    }

    /// A second device found in the same parcel: added onto the receipt's own
    /// place, given an RCV- code, and idempotent by the client-minted id.
    #[tokio::test]
    async fn add_unit_is_idempotent_and_lands_on_the_receipt_place() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;
        upsert_receipt(&db, "node-a", "dev-1", &sample("r-add", "u-add"))
            .await
            .unwrap();

        let extra = UnitInput {
            unit_id: "u-add-2".into(),
            kind: Some("item".into()),
            barcode: Some("SN-9999".into()),
            code: None,
            serial: Some("9999".into()),
            note: None,
        };
        let first = add_unit(&db, "node-a", "r-add", &extra).await.unwrap();
        let code = first["unit"]["code"].as_str().unwrap().to_string();
        assert!(code.starts_with("RCV-"), "expected an RCV- code, got {}", code);
        assert_eq!(first["unit"]["location_barcode"], json!(ZONE_INBOUND));
        assert_eq!(first["unit"]["zone"], json!("inbound"));

        let second = add_unit(&db, "node-a", "r-add", &extra).await.unwrap();
        assert_eq!(second["unit"]["code"], json!(code));
        let ctx = get_receipt_context(&db, "r-add").await.unwrap();
        assert_eq!(ctx["units"].as_array().unwrap().len(), 2);

        // Onto a receipt that does not exist there is nothing to add.
        assert!(add_unit(&db, "node-a", "nope", &extra).await.is_err());
    }

    /// The note grows by appends, OCR lands in BOTH columns, and neither column
    /// can grow without bound.
    #[tokio::test]
    async fn note_appends_and_is_capped() {
        let db = test_db().await;
        let mut input = sample("r-note", "u-note");
        input.note = None;
        input.ocr_text = None;
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();

        let r = append_note(&db, "node-a", "r-note", "Karton nass", "manual")
            .await
            .unwrap();
        let note = r["receipt"]["note"].as_str().unwrap();
        assert!(note.contains("[manual] Karton nass"));
        assert_eq!(r["receipt"]["ocr_text"], Value::Null);

        let r = append_note(&db, "node-a", "r-note", "ZetaBody 770 S/N 181700403", "ocr")
            .await
            .unwrap();
        let note = r["receipt"]["note"].as_str().unwrap();
        assert!(note.contains("Karton nass"), "the earlier note survives");
        assert!(note.contains("\n---\n"), "entries are separated");
        assert!(note.contains("[ocr] ZetaBody 770"));
        // OCR is kept as its own column too — the ticket matcher reads it.
        assert!(r["receipt"]["ocr_text"].as_str().unwrap().contains("181700403"));

        assert!(append_note(&db, "node-a", "r-note", "   ", "manual").await.is_err());

        // Bound: a runaway OCR loop truncates the OLDEST text, never the newest.
        let page = "x".repeat(5000);
        for _ in 0..6 {
            append_note(&db, "node-a", "r-note", &page, "ocr").await.unwrap();
        }
        append_note(&db, "node-a", "r-note", "LETZTE ZEILE", "ocr")
            .await
            .unwrap();
        let r = get_receipt_context(&db, "r-note").await.unwrap();
        let note = r["receipt"]["note"].as_str().unwrap();
        let ocr = r["receipt"]["ocr_text"].as_str().unwrap();
        assert!(note.len() <= TEXT_CAP, "note is {} bytes", note.len());
        assert!(ocr.len() <= TEXT_CAP, "ocr_text is {} bytes", ocr.len());
        assert!(note.starts_with(TRUNCATION_MARKER));
        assert!(note.ends_with("LETZTE ZEILE"));
        assert!(!note.contains("Karton nass"), "the oldest text is what goes");
    }

    /// The owner's toggle: into repair, out again, and back in — idempotent in
    /// both directions, with ONE live order at a time.
    #[tokio::test]
    async fn repair_toggle_is_idempotent_both_ways() {
        let db = test_db().await;
        ensure_zone_locations(&db, "node-a").await;
        let mut input = sample("r-rep", "u-rep");
        input.client_name = Some("Magic Fit Dornbirn".into());
        input.zoho_ticket = Some("#32834".into());
        input.units[0].serial = Some("181700403".into());
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();

        let first = set_repair(&db, "node-a", "r-rep", "u-rep", "add").await.unwrap();
        assert_eq!(first["created"], json!(true));
        assert_eq!(first["unit"]["status"], json!("in_repair"));
        let order = first["repair"]["order_uuid"].as_str().unwrap().to_string();

        // A second "add" finds the live order instead of minting a second one.
        let again = set_repair(&db, "node-a", "r-rep", "u-rep", "add").await.unwrap();
        assert_eq!(again["created"], json!(false));
        assert_eq!(again["repair"]["order_uuid"], json!(order));

        // The receipt's customer + ticket rode along onto the order.
        let stored = crate::services::repair::get_order(&db, &order)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored["customer_name"], json!("Magic Fit Dornbirn"));
        assert_eq!(stored["serial_number"], json!("181700403"));

        let removed = set_repair(&db, "node-a", "r-rep", "u-rep", "remove").await.unwrap();
        assert_eq!(removed["unit"]["status"], json!("received"));
        assert_eq!(removed["repair"]["status"], json!("cancelled"));
        assert_eq!(removed["unit"]["repair"], Value::Null);

        // Removing twice is a no-op, not an error.
        let noop = set_repair(&db, "node-a", "r-rep", "u-rep", "remove").await.unwrap();
        assert_eq!(noop["unit"]["status"], json!("received"));

        // Back in: a NEW order, because the old one is cancelled.
        let back = set_repair(&db, "node-a", "r-rep", "u-rep", "add").await.unwrap();
        assert_eq!(back["created"], json!(true));
        assert_ne!(back["repair"]["order_uuid"], json!(order));

        // A re-POST of the whole receipt must NOT reset the repair flag.
        upsert_receipt(&db, "node-a", "dev-1", &input).await.unwrap();
        let ctx = get_receipt_context(&db, "r-rep").await.unwrap();
        assert_eq!(ctx["units"][0]["status"], json!("in_repair"));

        assert!(set_repair(&db, "node-a", "r-rep", "u-rep", "sideways").await.is_err());
        assert!(set_repair(&db, "node-a", "r-rep", "nope", "add").await.is_err());
    }
}
