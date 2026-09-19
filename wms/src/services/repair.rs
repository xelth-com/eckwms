//! Repair orders as small point operations.
//!
//! The PDA has always been able to open a repair order by binding a device to a
//! slot (`POST /api/repair/event` with `device_bound`). The receiving context
//! needs the SAME act from a different direction — the receiver has an active
//! receipt (Beleg) on screen and puts one of its units into repair, or takes it
//! back out — so the order-minting logic lives here instead of inline in one
//! handler, and both callers share it.
//!
//! Doctrine: ONE live order per serial. "Live" is every status that is not a
//! terminal one, so a second bind (or a second "put into repair") finds the
//! existing order and returns it rather than fanning out duplicates for the
//! same physical device.
//!
//! `order` is a SYNCED table, so every write here advances this node's `_vclock`
//! and stamps its timestamps with SurrealQL `time::now()` — an RFC3339 string
//! bound through serde would be a STRING in a datetime column and poison
//! `ORDER BY created_at DESC` (it sorts by TYPE first).

use eck_core::db::SurrealDb;
use serde_json::{json, Value};

/// Statuses that mean "this order is finished" — anything else is live.
pub const TERMINAL_STATUSES: [&str; 4] = ["completed", "closed", "cancelled", "done"];

/// Same per-statement error surfacing as `services::receiving`: `db.query(..)`
/// is `Ok` even when the statement failed; only `check()` reveals it.
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

/// Record id for `type::record($rid)` — backtick-quoted so all-digit keys stay
/// STRING ids (`type::thing` does not exist on SurrealDB 3.2).
fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

/// Next `_vclock` for a synced `order` row, advancing THIS node's component.
async fn next_vclock(db: &SurrealDb, key: &str, instance_id: &str) -> Value {
    let existing: Option<Value> = db
        .query("SELECT _vclock FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("order", key)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|v| v.get("_vclock")),
        instance_id,
    )
}

/// `REP-YYYYMMDD-NNNN` (prefix overridable per tenant), the same shape the
/// `device_bound` path has minted since the Go era.
pub fn generate_order_number() -> String {
    format!(
        "{}{}-{:04}",
        std::env::var("REPAIR_ORDER_PREFIX").unwrap_or_else(|_| "REP-".into()),
        chrono::Utc::now().format("%Y%m%d"),
        rand::random::<u16>() % 10000
    )
}

/// The projection every caller gets back — the leaf record id under the name
/// the PDA knows it by (`order_uuid`), plus what a human reads.
const ORDER_PROJECTION: &str = "record::id(id) AS order_uuid, order_number, status, \
     serial_number, customer_name, created_at, updated_at";

/// `order.created_at` is a MIXED column on the live master: rows minted before
/// the timestamps were stamped server-side carry an RFC3339 STRING, newer ones
/// a real `datetime`. SurrealDB sorts by TYPE first, so `ORDER BY created_at
/// DESC` would rank by which kind of value a row happens to hold rather than by
/// time (the `shipments`/`updated_at` trap). Parse instead, and treat anything
/// unparseable as the epoch.
fn created_at_key(row: &Value) -> i64 {
    row.get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// The newest live (non-terminal) repair order for a serial, if any.
///
/// Doctrine is ONE live order per serial, so this normally has a single row to
/// choose from; the ordering only matters where history left two behind (a
/// slot bind and a claim minted from the same receipt, say).
pub async fn find_live_order(db: &SurrealDb, serial: &str) -> Result<Option<Value>, String> {
    let serial = serial.trim();
    if serial.is_empty() {
        return Ok(None);
    }
    let q = format!(
        "SELECT {} FROM order \
         WHERE serial_number = $sn AND status NOT IN $terminal LIMIT 25",
        ORDER_PROJECTION
    );
    let rows: Vec<Value> = db
        .query(&q)
        .bind(("sn", serial.to_string()))
        .bind(("terminal", json!(TERMINAL_STATUSES)))
        .await
        .map_err(|e| format!("live order lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("live order lookup: {}", e))?;
    Ok(rows.into_iter().max_by_key(created_at_key))
}

/// One order by its record-id leaf, in the same projection.
pub async fn get_order(db: &SurrealDb, order_uuid: &str) -> Result<Option<Value>, String> {
    let q = format!("SELECT {} FROM type::record($rid) LIMIT 1", ORDER_PROJECTION);
    let row: Option<Value> = db
        .query(&q)
        .bind(("rid", rid("order", order_uuid)))
        .await
        .map_err(|e| format!("order lookup: {}", e))?
        .take(0)
        .map_err(|e| format!("order lookup: {}", e))?;
    Ok(row)
}

/// Everything a caller may know about the device it is opening an order for.
/// All of it is optional except the serial — the receiving lane knows the
/// customer and the ticket, the PDA's slot bind knows neither.
#[derive(Debug, Default, Clone)]
pub struct OpenRepair {
    /// The device serial the order is keyed on. Required.
    pub serial: String,
    /// Free text. The receiving lane deliberately leaves this empty: the
    /// receipt note is the receiver's log, not the customer's complaint.
    pub issue: Option<String>,
    pub customer_name: Option<String>,
    /// `receipt` record-id leaf, when the order was opened from a Beleg.
    pub receipt_uuid: Option<String>,
    pub zoho_ticket: Option<String>,
    pub ticket_id: Option<String>,
    /// Where the act came from, for the audit trail
    /// (`repair/device_bound`, `receiving/context`).
    pub source: String,
    pub created_by_device: Option<String>,
    pub acting_user_id: Option<String>,
    pub owner_user_id: Option<String>,
}

fn opt(v: Option<&String>) -> Value {
    match v.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => json!(s),
        None => Value::Null,
    }
}

/// Open a repair order for a serial, or hand back the live one that already
/// exists. Returns `(order, created)`.
pub async fn open_repair_order(
    db: &SurrealDb,
    instance_id: &str,
    input: &OpenRepair,
) -> Result<(Value, bool), String> {
    let serial = input.serial.trim().to_string();
    if serial.is_empty() {
        return Err("serial is required to open a repair order".into());
    }

    if let Some(existing) = find_live_order(db, &serial).await? {
        return Ok((existing, false));
    }

    let order_uuid = uuid::Uuid::new_v4().to_string();
    let order_number = generate_order_number();
    let vclock = next_vclock(db, &order_uuid, instance_id).await;

    run_stmt!(
        "open repair order",
        db.query(
            "UPSERT type::record($rid) MERGE { \
                uuid: $uuid, order_number: $order_number, order_type: 'repair', \
                serial_number: $serial, status: 'pending', priority: 'normal', \
                customer_name: $customer_name, issue_description: $issue, \
                parts_used: [], \
                metadata: { \
                    source: $source, receipt_uuid: $receipt_uuid, \
                    zoho_ticket: $zoho_ticket, ticket_id: $ticket_id, \
                    created_by_device: $device, acting_user_id: $acting_user_id, \
                    owner_user_id: $owner_user_id \
                }, \
                _vclock: $vclock, created_at: time::now(), updated_at: time::now() \
             } RETURN NONE",
        )
        .bind(("rid", rid("order", &order_uuid)))
        .bind(("uuid", json!(order_uuid.clone())))
        .bind(("order_number", json!(order_number.clone())))
        .bind(("serial", json!(serial.clone())))
        .bind(("customer_name", opt(input.customer_name.as_ref())))
        .bind(("issue", opt(input.issue.as_ref())))
        .bind(("source", json!(input.source.clone())))
        .bind(("receipt_uuid", opt(input.receipt_uuid.as_ref())))
        .bind(("zoho_ticket", opt(input.zoho_ticket.as_ref())))
        .bind(("ticket_id", opt(input.ticket_id.as_ref())))
        .bind(("device", opt(input.created_by_device.as_ref())))
        .bind(("acting_user_id", opt(input.acting_user_id.as_ref())))
        .bind(("owner_user_id", opt(input.owner_user_id.as_ref())))
        .bind(("vclock", vclock))
    );

    tracing::info!(
        "[repair] opened {} ({}) for serial {} via {}",
        order_number,
        order_uuid,
        serial,
        input.source
    );

    let stored = get_order(db, &order_uuid)
        .await?
        .ok_or_else(|| "order vanished right after write".to_string())?;
    Ok((stored, true))
}

/// Cancel one order — the "take it back out of repair" half. Idempotent: an
/// order that is already terminal is returned unchanged.
pub async fn cancel_repair_order(
    db: &SurrealDb,
    instance_id: &str,
    order_uuid: &str,
    reason: &str,
) -> Result<Option<Value>, String> {
    let Some(current) = get_order(db, order_uuid).await? else {
        return Ok(None);
    };
    let status = current.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if TERMINAL_STATUSES.contains(&status) {
        return Ok(Some(current));
    }

    let vclock = next_vclock(db, order_uuid, instance_id).await;
    // SET (not MERGE): `metadata` is a nested object and MERGE would replace the
    // whole of it, dropping the receipt link the order was opened with.
    run_stmt!(
        "cancel repair order",
        db.query(
            "UPDATE type::record($rid) SET \
                status = 'cancelled', cancelled_at = time::now(), \
                metadata.cancelled_reason = $reason, \
                _vclock = $vclock, updated_at = time::now() \
             RETURN NONE",
        )
        .bind(("rid", rid("order", order_uuid)))
        .bind(("reason", json!(reason)))
        .bind(("vclock", vclock))
    );

    tracing::info!("[repair] cancelled order {} ({})", order_uuid, reason);
    get_order(db, order_uuid).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("test").use_db("test").await.unwrap();
        db.query("DEFINE TABLE IF NOT EXISTS order SCHEMALESS;")
            .await
            .unwrap()
            .check()
            .unwrap();
        db
    }

    fn req(serial: &str) -> OpenRepair {
        OpenRepair {
            serial: serial.into(),
            source: "test".into(),
            ..Default::default()
        }
    }

    /// One live order per serial, whoever asks and however often.
    #[tokio::test]
    async fn open_is_idempotent_per_serial() {
        let db = test_db().await;
        let (first, created) = open_repair_order(&db, "node-a", &req("SN-1")).await.unwrap();
        assert!(created);
        assert!(first["order_number"].as_str().unwrap().starts_with("REP-"));

        let (second, created) = open_repair_order(&db, "node-a", &req("SN-1")).await.unwrap();
        assert!(!created);
        assert_eq!(first["order_uuid"], second["order_uuid"]);
    }

    /// Cancel frees the serial: the next open mints a NEW order rather than
    /// resurrecting the cancelled one.
    #[tokio::test]
    async fn cancel_then_open_mints_a_new_order() {
        let db = test_db().await;
        let (order, _) = open_repair_order(&db, "node-a", &req("SN-2")).await.unwrap();
        let uuid = order["order_uuid"].as_str().unwrap().to_string();

        let cancelled = cancel_repair_order(&db, "node-a", &uuid, "test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled["status"], json!("cancelled"));
        assert!(find_live_order(&db, "SN-2").await.unwrap().is_none());

        // Cancelling twice is a no-op, not an error.
        cancel_repair_order(&db, "node-a", &uuid, "test").await.unwrap();

        let (again, created) = open_repair_order(&db, "node-a", &req("SN-2")).await.unwrap();
        assert!(created);
        assert_ne!(again["order_uuid"], json!(uuid));
    }

    /// History left two live orders on one serial, one of them with a STRING
    /// `created_at` (the pre-server-stamp era). The newest must win on TIME,
    /// not on which SurrealDB type the row happens to hold.
    #[tokio::test]
    async fn newest_live_order_wins_across_mixed_timestamp_types() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('order', 'legacy') SET serial_number = 'SN-3', \
                order_number = 'REP-OLD', status = 'pending', \
                created_at = '2026-09-10T15:38:29.763834287+00:00'; \
             CREATE type::record('order', 'fresh') SET serial_number = 'SN-3', \
                order_number = 'REP-NEW', status = 'received', \
                created_at = <datetime>'2026-09-10T15:44:33.773881620Z';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let live = find_live_order(&db, "SN-3").await.unwrap().unwrap();
        assert_eq!(live["order_number"], json!("REP-NEW"));

        // …and the older one still wins when IT is the later of the two.
        db.query(
            "UPDATE type::record('order', 'legacy') \
             SET created_at = '2026-09-10T16:00:00+00:00'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let live = find_live_order(&db, "SN-3").await.unwrap().unwrap();
        assert_eq!(live["order_number"], json!("REP-OLD"));
    }

    #[tokio::test]
    async fn empty_serial_is_rejected() {
        let db = test_db().await;
        assert!(open_repair_order(&db, "node-a", &req("  ")).await.is_err());
        assert!(find_live_order(&db, "").await.unwrap().is_none());
    }
}
