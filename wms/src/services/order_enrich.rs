//! Order enrichment — resolve the Zoho **support ticket** behind a repair order
//! (or an inbound receipt) and project the *clear-tier* customer facts the
//! repair views are allowed to show: **company, city, zip, ticket_number**, plus
//! the **documents** (repair-protocol PDFs etc.) attached to the order, its unit,
//! its receipt and its ticket.
//!
//! PII rule (owner, 2026-11-01): the **requester as a person** (name, email,
//! phone, address) stays hidden — this module NEVER returns any of those. The
//! **company** and the **city** are clear-tier and shown to everyone who can see
//! the repairs page (the kiosk observer included). Company is stored in
//! `document.meta.company` which, on a vault node, holds a `Company_<hex>` PPRL
//! token; it is resolved back to clear through the SAME vault read path the
//! support ticket page uses (`eck_core::pii_vault` — `clear_or_token` +
//! `get_many`), falling back to the plain value on a legacy/clear row. A token
//! that cannot be resolved is dropped (never surfaced as `Company_<hex>`).
//!
//! The GER26… claim number is deliberately NOT part of this surface.

use std::collections::HashMap;

use eck_core::db::SurrealDb;
use eck_core::pii_vault::{clear_or_token, Field};
use serde_json::{json, Value};

/// A pointer from an order/receipt to its ticket: a `document` leaf
/// (`metadata.ticket_id`) and/or a Zoho ticket NUMBER (`metadata.zoho_ticket`).
#[derive(Debug, Clone, Default)]
pub struct TicketRef {
    pub ticket_id: Option<String>,
    pub ticket_number: Option<String>,
}

impl TicketRef {
    pub fn is_empty(&self) -> bool {
        self.ticket_id.is_none() && self.ticket_number.is_none()
    }
}

/// The clear-tier contact facts resolved from a ticket. NOTHING about the
/// requester person (name/email/phone/address) is ever carried here.
#[derive(Debug, Clone, Default)]
pub struct ReqContact {
    /// The resolved ticket's `document` leaf.
    pub ticket_id: Option<String>,
    pub ticket_number: Option<String>,
    pub company: Option<String>,
    pub city: Option<String>,
    pub zip: Option<String>,
}

/// Trimmed, non-empty string field of a JSON object.
fn sv(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Build a [`TicketRef`] from an order's `metadata` (or a receipt row): the
/// `ticket_id` document leaf and the `zoho_ticket` number.
pub fn ticket_ref_from_meta(meta: &Value) -> TicketRef {
    TicketRef {
        ticket_id: sv(meta, "ticket_id"),
        ticket_number: sv(meta, "zoho_ticket"),
    }
}

/// `document.meta.company` resolved to clear: a legacy clear value passes
/// through, a PPRL token is looked up in `revealed` (the vault batch), and an
/// unresolved token is dropped — the UI never shows `Company_<hex>`.
fn company_of(meta: &Value, revealed: &HashMap<String, String>) -> Option<String> {
    let raw = meta.get("company").and_then(|v| v.as_str()).unwrap_or("");
    match clear_or_token(raw) {
        Field::Clear(s) => Some(s.to_string()),
        Field::Token(t) => revealed.get(t).cloned(),
        Field::Empty => None,
    }
}

/// Resolve a batch of ticket references in ONE ticket query (no N+1) plus ONE
/// vault batch for the company tokens. Returns a map `key -> ReqContact` for
/// every key whose ticket was found; keys with no matching ticket are absent.
///
/// Resolution per key: the `ticket_id` document leaf wins; else the
/// `zoho_ticket` number is matched against `document.meta.ticket_number`.
pub async fn resolve_tickets(
    db: &SurrealDb,
    refs: &HashMap<String, TicketRef>,
) -> HashMap<String, ReqContact> {
    let mut out: HashMap<String, ReqContact> = HashMap::new();
    if refs.is_empty() {
        return out;
    }

    let mut leaf_ids: Vec<String> = Vec::new();
    let mut numbers: Vec<String> = Vec::new();
    for r in refs.values() {
        if let Some(t) = &r.ticket_id {
            leaf_ids.push(t.clone());
        }
        if let Some(n) = &r.ticket_number {
            numbers.push(n.clone());
        }
    }
    leaf_ids.sort();
    leaf_ids.dedup();
    numbers.sort();
    numbers.dedup();
    if leaf_ids.is_empty() && numbers.is_empty() {
        return out;
    }

    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, meta FROM document \
             WHERE type = 'support_ticket' \
               AND (record::id(id) IN $ids OR meta.ticket_number IN $nums)",
        )
        .bind(("ids", json!(leaf_ids)))
        .bind(("nums", json!(numbers)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();

    // `(leaf, meta)` indexed both ways so a key can match on either pointer.
    let mut by_leaf: HashMap<String, (String, Value)> = HashMap::new();
    let mut by_number: HashMap<String, (String, Value)> = HashMap::new();
    for row in rows {
        let Some(leaf) = row.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()) else {
            continue;
        };
        let meta = row.get("meta").cloned().unwrap_or(Value::Null);
        if let Some(num) = sv(&meta, "ticket_number") {
            by_number.entry(num).or_insert((leaf.clone(), meta.clone()));
        }
        by_leaf.insert(leaf.clone(), (leaf, meta));
    }

    // One vault batch for every company token across the resolved tickets
    // (reuses the support-page reveal path). Clear rows need no lookup.
    let mut tokens: Vec<String> = Vec::new();
    for (_, meta) in by_leaf.values().chain(by_number.values()) {
        if let Some(c) = meta.get("company").and_then(|v| v.as_str()) {
            if let Field::Token(t) = clear_or_token(c) {
                tokens.push(t.to_string());
            }
        }
    }
    tokens.sort();
    tokens.dedup();
    let revealed = eck_core::pii_vault::get_many(db, &tokens).await;

    for (key, r) in refs {
        let resolved = r
            .ticket_id
            .as_ref()
            .and_then(|t| by_leaf.get(t))
            .or_else(|| r.ticket_number.as_ref().and_then(|n| by_number.get(n)));
        let Some((leaf, meta)) = resolved else { continue };
        out.insert(
            key.clone(),
            ReqContact {
                ticket_id: Some(leaf.clone()),
                ticket_number: sv(meta, "ticket_number"),
                company: company_of(meta, &revealed),
                city: sv(meta, "city"),
                zip: sv(meta, "zip"),
            },
        );
    }
    out
}

/// Resolve the ticket behind a single order record (`order.metadata`).
pub async fn ticket_for_order(db: &SurrealDb, order: &Value) -> Option<ReqContact> {
    let meta = order.get("metadata")?;
    let r = ticket_ref_from_meta(meta);
    if r.is_empty() {
        return None;
    }
    let mut m = HashMap::new();
    m.insert("k".to_string(), r);
    resolve_tickets(db, &m).await.remove("k")
}

/// Clear-tier facts carried by an order's inbound **receipt** itself: its own
/// `client_name` and a city derived from `client_address`. These are the LAST
/// fallback for orders whose ticket (or whole ticket link) is missing. Nothing
/// about the requester person is ever read here.
#[derive(Debug, Clone, Default)]
pub struct ReceiptFacts {
    pub client_name: Option<String>,
    pub city: Option<String>,
}

/// Batched receipt lookup for the fallback ladder: `receipt_uuid -> ReceiptFacts`
/// for every receipt found. ONE query, no N+1. City is derived from the
/// receipt's `client_address` with the same parser the intake path uses.
pub async fn resolve_receipts(
    db: &SurrealDb,
    receipt_uuids: &[String],
) -> HashMap<String, ReceiptFacts> {
    let mut out: HashMap<String, ReceiptFacts> = HashMap::new();
    let mut ids: Vec<String> = receipt_uuids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return out;
    }

    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS id, client_name, client_address \
             FROM receipt WHERE record::id(id) IN $ids",
        )
        .bind(("ids", json!(ids)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();

    for row in rows {
        let Some(id) = sv(&row, "id") else { continue };
        let client_name = sv(&row, "client_name");
        let city = sv(&row, "client_address")
            .and_then(|a| crate::services::claimlist::city_from_address(&a));
        out.insert(id, ReceiptFacts { client_name, city });
    }
    out
}

/// Final clear-tier `(company, city)` with the full fallback ladder
/// (owner, 2026-11-01):
/// * company = ticket company → order `customer_name` → receipt `client_name`;
/// * city    = order `metadata.city` → ticket city → receipt city.
///
/// The requester person is never part of any of these sources.
pub fn company_city(
    contact: Option<&ReqContact>,
    order_customer_name: Option<&str>,
    order_city: Option<&str>,
    receipt: Option<&ReceiptFacts>,
) -> (Option<String>, Option<String>) {
    let norm = |s: &str| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    let company = contact
        .and_then(|c| c.company.clone())
        .or_else(|| order_customer_name.and_then(norm))
        .or_else(|| receipt.and_then(|r| r.client_name.clone()));
    let city = order_city
        .and_then(norm)
        .or_else(|| contact.and_then(|c| c.city.clone()))
        .or_else(|| receipt.and_then(|r| r.city.clone()));
    (company, city)
}

/// The clear-tier `customer` block for the entity view: `{company, city, zip,
/// ticket_number}` and NOTHING about the requester person. `company`/`city`
/// follow the full [`company_city`] fallback ladder; `zip`/`ticket_number` come
/// from the ticket alone. `None` when there is nothing to show.
pub fn customer_json(
    c: Option<&ReqContact>,
    order_customer_name: Option<&str>,
    city_override: Option<&str>,
    receipt: Option<&ReceiptFacts>,
) -> Option<Value> {
    let (company, city) = company_city(c, order_customer_name, city_override, receipt);
    let zip = c.and_then(|c| c.zip.clone());
    let ticket_number = c.and_then(|c| c.ticket_number.clone());
    if company.is_none() && city.is_none() && zip.is_none() && ticket_number.is_none() {
        return None;
    }
    Some(json!({
        "company": company,
        "city": city,
        "zip": zip,
        "ticket_number": ticket_number,
    }))
}

/// Classify a repair document by its file name (§9). `/reparatur/i` →
/// `repair_order`, `/vereinbarung|datenverarbeitung/i` → `dsgvo`, else
/// `attachment`.
pub fn doc_kind(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("reparatur") {
        "repair_order"
    } else if n.contains("vereinbarung") || n.contains("datenverarbeitung") {
        "dsgvo"
    } else {
        "attachment"
    }
}

/// NON-IMAGE (PDF etc.) attachments of an order and everything it hangs off —
/// its linked units, its receipt and its ticket — as
/// `{file_id, name, mime, source, kind, attached_at}`. ONE batched query over
/// `has_attachment`, deduped on `file_id`, capped at 50. An empty vec is normal.
///
/// A missing/empty mime (legacy photo rows) is treated as an image and dropped,
/// matching `services::entity::is_image`.
pub async fn documents_for_order(
    db: &SurrealDb,
    order_id: &str,
    unit_ids: &[String],
    receipt_uuid: Option<&str>,
    ticket_id: Option<&str>,
) -> Vec<Value> {
    let mut owners: Vec<String> = vec![order_id.to_string()];
    owners.extend(unit_ids.iter().cloned());
    if let Some(r) = receipt_uuid {
        owners.push(r.to_string());
    }
    if let Some(t) = ticket_id {
        owners.push(t.to_string());
    }
    owners.sort();
    owners.dedup();
    owners.retain(|s| !s.is_empty());
    if owners.is_empty() {
        return Vec::new();
    }

    let rows: Vec<Value> = db
        .query(
            "SELECT record::tb(in) AS tb, record::id(out.id) AS file_id, \
                    out.original_name AS name, out.mime_type AS mime, \
                    created_at AS attached_at \
             FROM has_attachment WHERE record::id(in) IN $ids \
             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(("ids", json!(owners)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();

    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    for r in rows {
        let mime = r.get("mime").and_then(|v| v.as_str()).unwrap_or("");
        if mime.is_empty() || mime.starts_with("image") {
            continue;
        }
        let Some(file_id) = sv(&r, "file_id") else { continue };
        if seen.contains(&file_id) {
            continue;
        }
        seen.push(file_id.clone());
        let name = sv(&r, "name").unwrap_or_default();
        let source = match r.get("tb").and_then(|v| v.as_str()).unwrap_or("") {
            "order" => "order",
            "receipt_unit" => "unit",
            "receipt" => "receipt",
            "document" => "ticket",
            _ => "attachment",
        };
        out.push(json!({
            "file_id": file_id,
            "name": name,
            "mime": mime,
            "source": source,
            "kind": doc_kind(&name),
            "attached_at": r.get("attached_at").cloned().unwrap_or(Value::Null),
        }));
        if out.len() >= 50 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        db.query(
            "DEFINE TABLE document SCHEMALESS; DEFINE TABLE order SCHEMALESS; \
             DEFINE TABLE receipt SCHEMALESS; DEFINE TABLE receipt_unit SCHEMALESS; \
             DEFINE TABLE file_resource SCHEMALESS; \
             DEFINE TABLE has_attachment TYPE RELATION SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    /// Seed a support ticket whose meta carries requester PII (customer/email/
    /// phone) alongside the clear-tier company/city/zip.
    async fn seed_ticket(db: &SurrealDb, leaf: &str, number: &str, company: &str) {
        db.query(
            "UPSERT type::record('document', $leaf) MERGE { \
                type: 'support_ticket', \
                meta: { ticket_number: $num, company: $company, city: 'Dornbirn', \
                        zip: '6850', customer: 'Max Mustermann', \
                        email: 'max@example.com', phone: '+43 5572 123456' } }",
        )
        .bind(("leaf", leaf.to_string()))
        .bind(("num", number.to_string()))
        .bind(("company", company.to_string()))
        .await
        .unwrap()
        .check()
        .unwrap();
    }

    #[tokio::test]
    async fn resolves_by_ticket_id_leaf() {
        let db = test_db().await;
        seed_ticket(&db, "32834", "32834", "Magic Fit Dornbirn").await;
        let mut refs = HashMap::new();
        refs.insert(
            "o1".to_string(),
            TicketRef { ticket_id: Some("32834".into()), ticket_number: None },
        );
        let out = resolve_tickets(&db, &refs).await;
        let c = out.get("o1").expect("resolved");
        assert_eq!(c.company.as_deref(), Some("Magic Fit Dornbirn"));
        assert_eq!(c.city.as_deref(), Some("Dornbirn"));
        assert_eq!(c.zip.as_deref(), Some("6850"));
        assert_eq!(c.ticket_number.as_deref(), Some("32834"));
    }

    #[tokio::test]
    async fn resolves_by_zoho_ticket_number() {
        let db = test_db().await;
        // Document leaf differs from the ticket NUMBER — the order points only
        // at the number (metadata.zoho_ticket).
        seed_ticket(&db, "doc-abc", "30654", "Joy Fitness").await;
        let mut refs = HashMap::new();
        refs.insert(
            "o2".to_string(),
            TicketRef { ticket_id: None, ticket_number: Some("30654".into()) },
        );
        let out = resolve_tickets(&db, &refs).await;
        let c = out.get("o2").expect("resolved");
        assert_eq!(c.ticket_id.as_deref(), Some("doc-abc"));
        assert_eq!(c.company.as_deref(), Some("Joy Fitness"));
        assert_eq!(c.city.as_deref(), Some("Dornbirn"));
    }

    #[tokio::test]
    async fn batched_resolution_one_query_many_orders() {
        let db = test_db().await;
        seed_ticket(&db, "100", "100", "Alpha GmbH").await;
        seed_ticket(&db, "doc-b", "200", "Beta GmbH").await;
        let mut refs = HashMap::new();
        refs.insert("a".into(), TicketRef { ticket_id: Some("100".into()), ticket_number: None });
        refs.insert("b".into(), TicketRef { ticket_id: None, ticket_number: Some("200".into()) });
        refs.insert("c".into(), TicketRef { ticket_id: Some("missing".into()), ticket_number: None });
        let out = resolve_tickets(&db, &refs).await;
        assert_eq!(out.get("a").and_then(|c| c.company.clone()).as_deref(), Some("Alpha GmbH"));
        assert_eq!(out.get("b").and_then(|c| c.company.clone()).as_deref(), Some("Beta GmbH"));
        // An order whose ticket does not exist is simply absent.
        assert!(out.get("c").is_none());
    }

    /// The projected `customer` block must never carry the requester person.
    #[tokio::test]
    async fn customer_json_omits_requester_pii() {
        let db = test_db().await;
        seed_ticket(&db, "32834", "32834", "Magic Fit Dornbirn").await;
        let mut refs = HashMap::new();
        refs.insert("o1".into(), TicketRef { ticket_id: Some("32834".into()), ticket_number: None });
        let out = resolve_tickets(&db, &refs).await;
        let c = out.get("o1").unwrap();
        let cust = customer_json(Some(c), None, None, None).expect("some");
        let obj = cust.as_object().unwrap();
        // Only the four clear-tier keys.
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["city", "company", "ticket_number", "zip"]);
        // And none of the requester VALUES leaked through.
        let s = serde_json::to_string(&cust).unwrap();
        assert!(!s.contains("Mustermann"), "{s}");
        assert!(!s.contains("example.com"), "{s}");
        assert!(!s.contains("123456"), "{s}");
    }

    #[test]
    fn customer_city_override_wins() {
        let c = ReqContact { city: Some("TicketCity".into()), ..Default::default() };
        let v = customer_json(Some(&c), None, Some("OrderCity"), None).unwrap();
        assert_eq!(v["city"], json!("OrderCity"));
        // Nothing to show at all → None.
        assert!(customer_json(None, None, None, None).is_none());
        assert!(customer_json(Some(&ReqContact::default()), None, None, None).is_none());
    }

    /// The company/city fallback ladder (owner, 2026-11-01):
    /// company = ticket > order customer_name > receipt client_name;
    /// city    = order metadata.city > ticket city > receipt city.
    #[test]
    fn company_city_fallback_ladder() {
        let ticket = ReqContact {
            company: Some("TicketCo".into()),
            city: Some("TicketCity".into()),
            ..Default::default()
        };
        let receipt = ReceiptFacts {
            client_name: Some("ReceiptCo".into()),
            city: Some("ReceiptCity".into()),
        };

        // Ticket company wins; order city wins.
        let (co, ci) = company_city(Some(&ticket), Some("OrderCust"), Some("OrderCity"), Some(&receipt));
        assert_eq!(co.as_deref(), Some("TicketCo"));
        assert_eq!(ci.as_deref(), Some("OrderCity"));

        // No ticket company → order customer_name; no order city → ticket city.
        let no_company = ReqContact { city: Some("TicketCity".into()), ..Default::default() };
        let (co, ci) = company_city(Some(&no_company), Some("OrderCust"), None, Some(&receipt));
        assert_eq!(co.as_deref(), Some("OrderCust"));
        assert_eq!(ci.as_deref(), Some("TicketCity"));

        // No ticket link, no order customer → receipt client_name; receipt city last.
        let (co, ci) = company_city(None, None, None, Some(&receipt));
        assert_eq!(co.as_deref(), Some("ReceiptCo"));
        assert_eq!(ci.as_deref(), Some("ReceiptCity"));

        // A blank order customer_name is skipped, not surfaced.
        let (co, _) = company_city(None, Some("   "), None, Some(&receipt));
        assert_eq!(co.as_deref(), Some("ReceiptCo"));

        // Nothing anywhere.
        assert_eq!(company_city(None, None, None, None), (None, None));
    }

    #[tokio::test]
    async fn resolve_receipts_client_name_and_city() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('receipt','rr1') SET \
                client_name='Fit Studio GmbH', \
                client_address='Hauptstr. 5, 6850 Dornbirn';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let out = resolve_receipts(&db, &["rr1".to_string(), "missing".to_string()]).await;
        let r = out.get("rr1").expect("receipt found");
        assert_eq!(r.client_name.as_deref(), Some("Fit Studio GmbH"));
        assert_eq!(r.city.as_deref(), Some("Dornbirn"));
        // A receipt that does not exist is simply absent.
        assert!(out.get("missing").is_none());
    }

    #[test]
    fn doc_kind_heuristic() {
        assert_eq!(doc_kind("Reparatur-_und_Wartungsbeauftragung.pdf"), "repair_order");
        assert_eq!(doc_kind("Vereinbarung zur Datenverarbeitung.pdf"), "dsgvo");
        assert_eq!(doc_kind("Datenverarbeitung.pdf"), "dsgvo");
        assert_eq!(doc_kind("Anhang 1.pdf"), "attachment");
    }

    #[tokio::test]
    async fn documents_dedupe_filter_and_source() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('order','o1') SET order_number='REP-1'; \
             CREATE type::record('receipt','r1'); \
             CREATE type::record('document','t1') SET type='support_ticket'; \
             CREATE type::record('file_resource','pdf1') SET original_name='Reparaturauftrag.pdf', mime_type='application/pdf'; \
             CREATE type::record('file_resource','pdf2') SET original_name='Anhang 1.pdf', mime_type='application/pdf'; \
             CREATE type::record('file_resource','img1') SET original_name='device.jpg', mime_type='image/jpeg'; \
             RELATE (type::record('order','o1'))->has_attachment->(type::record('file_resource','pdf1')) SET created_at=time::now(); \
             RELATE (type::record('receipt','r1'))->has_attachment->(type::record('file_resource','pdf1')) SET created_at=time::now(); \
             RELATE (type::record('document','t1'))->has_attachment->(type::record('file_resource','pdf2')) SET created_at=time::now(); \
             RELATE (type::record('order','o1'))->has_attachment->(type::record('file_resource','img1')) SET created_at=time::now();",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let docs = documents_for_order(&db, "o1", &[], Some("r1"), Some("t1")).await;
        // pdf1 appears on BOTH order and receipt → deduped to one; img1 filtered out.
        assert_eq!(docs.len(), 2, "{docs:?}");
        let names: Vec<&str> = docs.iter().filter_map(|d| d["name"].as_str()).collect();
        assert!(names.contains(&"Reparaturauftrag.pdf"));
        assert!(names.contains(&"Anhang 1.pdf"));
        assert!(!names.iter().any(|n| n.contains("device")));
        // kind heuristic surfaced.
        let rep = docs.iter().find(|d| d["name"] == json!("Reparaturauftrag.pdf")).unwrap();
        assert_eq!(rep["kind"], json!("repair_order"));
        // the ticket PDF carries source = "ticket".
        let anh = docs.iter().find(|d| d["name"] == json!("Anhang 1.pdf")).unwrap();
        assert_eq!(anh["source"], json!("ticket"));
    }
}
