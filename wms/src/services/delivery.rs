//! Delivery / shipping distillation.
//!
//! DHL and Opal parcel data used to be dumped one-blob-per-parcel into a bespoke
//! `shipment` table (`provider` + a `raw_response` JSON string) that was NOT in
//! `SYNC_ENTITY_TYPES` — so it never left the scraper node and the office master
//! showed no shipping lines. This module routes the scraper payloads into the
//! Odoo-shaped models that already exist and already mesh:
//!
//!   delivery_carrier          — the carrier catalog (DHL, Opal)          [synced]
//!   stock_picking_delivery    — the shipment (tracking, recipient, dims) [synced, Zone-1: recipient PII]
//!   delivery_tracking         — one row per (shipment, status) event     [synced]
//!   shipment_raw              — the raw scraper JSON, LOCAL staging only  [NOT synced]
//!
//! The raw payload stays local (like `document_raw`) — kept only so a better
//! parser can re-distill later. See `.eck/DOMAIN_MODEL.md` §Delivery.

use chrono::Utc;
use eck_core::db::SurrealDb;
use serde_json::{json, Value};

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

/// First non-empty of the given keys (for DHL's recipient_* / delivered_to_* pairs).
fn first<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        let val = s(v, k);
        if !val.is_empty() {
            return val;
        }
    }
    ""
}

/// Lowercase alphanumeric slug, for composing a stable record-key suffix.
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

/// Provider-independent distilled shipment.
pub struct Distilled {
    pub carrier_key: String,
    pub carrier_name: String,
    pub carrier_type: String,
    pub status: String,
    pub status_date: String,
    pub recipient_name: String,
    pub recipient_street: String,
    pub recipient_city: String,
    pub recipient_zip: String,
    pub recipient_country: String,
    // The pickup (sender) end — for INBOUND parcels this is the client, the
    // one name the warehouse worker actually needs to see. Parcel carriers
    // (DHL tracking) reveal no sender, so these stay empty there.
    pub pickup_name: String,
    pub pickup_street: String,
    pub pickup_city: String,
    pub pickup_zip: String,
    pub pickup_country: String,
    pub dimensions: String,
    pub weight: String,
    pub reference: String,
    pub description: String,
    pub created_ext: String,
    pub delivery_date: String,
}

/// Map a provider's raw scraper payload onto the common shape. Opal is a courier
/// with pickup+delivery ends (recipient = the delivery end); DHL is a parcel
/// carrier with recipient_* (falling back to delivered_to_*).
pub fn distill(provider: &str, raw: &Value) -> Distilled {
    match provider {
        "opal" => Distilled {
            carrier_key: "opal".into(),
            carrier_name: "Opal".into(),
            carrier_type: "courier".into(),
            status: s(raw, "status").into(),
            status_date: format!("{} {}", s(raw, "status_date"), s(raw, "status_time"))
                .trim()
                .to_string(),
            recipient_name: s(raw, "delivery_name").into(),
            recipient_street: s(raw, "delivery_street").into(),
            recipient_city: s(raw, "delivery_city").into(),
            recipient_zip: s(raw, "delivery_zip").into(),
            recipient_country: s(raw, "delivery_country").into(),
            pickup_name: format!("{} {}", s(raw, "pickup_name"), s(raw, "pickup_name2"))
                .trim()
                .to_string(),
            pickup_street: s(raw, "pickup_street").into(),
            pickup_city: s(raw, "pickup_city").into(),
            pickup_zip: s(raw, "pickup_zip").into(),
            pickup_country: s(raw, "pickup_country").into(),
            dimensions: s(raw, "dimensions").into(),
            weight: s(raw, "weight").into(),
            reference: s(raw, "reference").into(),
            description: s(raw, "description").into(),
            created_ext: s(raw, "created_at").into(),
            delivery_date: s(raw, "delivery_date").into(),
        },
        // dhl (and any future parcel carrier defaulting to the DHL shape)
        _ => Distilled {
            carrier_key: "dhl".into(),
            carrier_name: "DHL".into(),
            carrier_type: "parcel".into(),
            status: s(raw, "status").into(),
            status_date: s(raw, "status_date").into(),
            recipient_name: first(raw, &["recipient_name", "delivered_to_name"]).into(),
            recipient_street: first(raw, &["recipient_street", "delivered_to_street"]).into(),
            recipient_city: first(raw, &["recipient_city", "delivered_to_city"]).into(),
            recipient_zip: first(raw, &["recipient_zip", "delivered_to_zip"]).into(),
            recipient_country: first(raw, &["recipient_country", "delivered_to_country"]).into(),
            pickup_name: String::new(),
            pickup_street: String::new(),
            pickup_city: String::new(),
            pickup_zip: String::new(),
            pickup_country: String::new(),
            dimensions: String::new(),
            weight: String::new(),
            reference: s(raw, "reference").into(),
            description: s(raw, "product").into(),
            created_ext: String::new(),
            delivery_date: String::new(),
        },
    }
}

/// Persist one shipment into the meshed models + local raw staging. Idempotent:
/// the shipment record is keyed by tracking number, so re-syncs update in place.
pub async fn persist(db: &SurrealDb, provider: &str, tracking: &str, raw: &Value) {
    let d = distill(provider, raw);
    let now = Utc::now().to_rfc3339();

    // 1. carrier catalog (keyed by carrier key).
    let _: Result<Option<Value>, _> = db
        .upsert(("delivery_carrier", d.carrier_key.clone()))
        .merge(json!({
            "name": d.carrier_name,
            "carrier_type": d.carrier_type,
            "active": true,
            "updated_at": now,
        }))
        .await;

    // 2. the shipment record — SYNCED, Zone-1 (recipient PII). Keyed by tracking.
    let _: Result<Option<Value>, _> = db
        .upsert(("stock_picking_delivery", tracking))
        .merge(json!({
            "tracking_number": tracking,
            "provider": d.carrier_key,
            "status": d.status,
            "status_date": d.status_date,
            "recipient_name": d.recipient_name,
            "recipient_street": d.recipient_street,
            "recipient_city": d.recipient_city,
            "recipient_zip": d.recipient_zip,
            "recipient_country": d.recipient_country,
            "pickup_name": d.pickup_name,
            "pickup_street": d.pickup_street,
            "pickup_city": d.pickup_city,
            "pickup_zip": d.pickup_zip,
            "pickup_country": d.pickup_country,
            "dimensions": d.dimensions,
            "weight": d.weight,
            "reference": d.reference,
            "description": d.description,
            "created_ext": d.created_ext,
            "delivery_date": d.delivery_date,
            "updated_at": now,
        }))
        .await;

    // 3. tracking event — keyed by (tracking, status) so the timeline accumulates
    //    as the status advances across syncs.
    if !d.status.is_empty() {
        let ev_key = format!("{}__{}", tracking, slug(&d.status));
        let _: Result<Option<Value>, _> = db
            .upsert(("delivery_tracking", ev_key))
            .merge(json!({
                "delivery_id": tracking,
                "status": d.status,
                "location": d.recipient_city,
                "description": d.description,
                "event_time": d.status_date,
                "created_at": now,
            }))
            .await;
    }

    // 4. raw scraper payload — LOCAL staging only (never in SYNC_ENTITY_TYPES).
    let _: Result<Option<Value>, _> = db
        .upsert(("shipment_raw", tracking))
        .merge(json!({
            "provider": provider,
            "raw_response": serde_json::to_string(raw).unwrap_or_default(),
            "updated_at": now,
        }))
        .await;
}

/// One-shot migration: re-distill every legacy `shipment` row into the models.
/// Runs at startup only when the new table is empty AND the old one has rows, so
/// it fires once on the scraper node and no-ops everywhere else. Does not drop
/// `shipment` (kept as a safety net; drop manually once confident).
pub async fn migrate_from_shipment_if_needed(db: &SurrealDb) {
    let spd: Option<Value> = db
        .query("SELECT count() AS c FROM stock_picking_delivery GROUP ALL")
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    let spd_count = spd.as_ref().and_then(|v| v.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);
    if spd_count > 0 {
        return; // already migrated / already populated
    }

    let rows: Vec<Value> = match db
        .query("SELECT provider, tracking_number, raw_response FROM shipment")
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(v) => v,
        Err(_) => return,
    };
    if rows.is_empty() {
        return;
    }

    let mut migrated = 0usize;
    for row in &rows {
        let tracking = s(row, "tracking_number");
        if tracking.is_empty() {
            continue;
        }
        let provider = s(row, "provider");
        let raw: Value = serde_json::from_str(s(row, "raw_response")).unwrap_or(json!({}));
        persist(db, provider, tracking, &raw).await;
        migrated += 1;
    }
    tracing::info!(
        "[delivery] migrated {} legacy shipment rows into stock_picking_delivery/delivery_tracking/delivery_carrier",
        migrated
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dhl_recipient_fallback() {
        let raw = json!({
            "status": "Zugestellt", "status_date": "2026-04-02T14:57:59",
            "recipient_name": "Adrian Terzija", "recipient_city": "Hagen",
            "recipient_zip": "58095", "recipient_street": "Kampstr. 13",
            "recipient_country": "DE", "product": "DHL Paket",
        });
        let d = distill("dhl", &raw);
        assert_eq!(d.carrier_name, "DHL");
        assert_eq!(d.recipient_city, "Hagen");
        assert_eq!(d.description, "DHL Paket");
        assert_eq!(d.status, "Zugestellt");
    }

    #[test]
    fn dhl_delivered_to_fallback_when_recipient_empty() {
        let raw = json!({ "delivered_to_city": "Berlin", "delivered_to_zip": "10115" });
        let d = distill("dhl", &raw);
        assert_eq!(d.recipient_city, "Berlin");
        assert_eq!(d.recipient_zip, "10115");
    }

    #[test]
    fn opal_maps_delivery_end() {
        let raw = json!({
            "status": "Zugestellt", "status_date": "14.01.2026", "status_time": "10:30",
            "delivery_name": "ZetaBody Deutschland", "delivery_city": "Beispielstadt",
            "delivery_zip": "12345", "dimensions": "90,00x43,00x36,00", "weight": "12",
            "created_at": "12.01.2026 - 13:53 Uhr", "delivery_date": "14.01.2026",
        });
        let d = distill("opal", &raw);
        assert_eq!(d.carrier_name, "Opal");
        assert_eq!(d.recipient_city, "Beispielstadt");
        assert_eq!(d.dimensions, "90,00x43,00x36,00");
        assert_eq!(d.status_date, "14.01.2026 10:30");
    }

    #[test]
    fn slug_is_stable() {
        assert_eq!(slug("Zugestellt"), "zugestellt");
        assert_eq!(slug("In Zustellung"), "in_zustellung");
        assert_eq!(slug("Delivered!"), "delivered");
    }
}
