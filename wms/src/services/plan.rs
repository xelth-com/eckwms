//! Plan panel + capture — the DERIVED, STATELESS plan the PDA renders
//! (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §8, joint contract with the movFast
//! session `eckwms-movFast/.eck/UI_CONVENTIONS.md` "PLAN PANEL + capture rails").
//!
//! There is NO plan table. A plan is computed on demand from ticket + receipt +
//! units + attachments (receiving) or from `order.status` + evidence edges
//! (repair). The PDA keeps its own `receiving.json` as the offline fallback and
//! overlays the server plan by step `key`, so this server never has to be the
//! source of truth for the flow — only for what is EXPECTED and what is DONE.
//!
//! Builders are PURE (`build_receiving_plan`, `build_order_plan`): they take
//! already-loaded data and return a `Plan`/`OrderPlan`, so the step-state and
//! expected-vs-booked logic is unit-tested without a DB. The async loaders
//! (`load_receiving_plan`, `load_order_plan`) do the queries and feed them.

use eck_core::db::SurrealDb;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Record id for `type::record($rid)` — backtick-quoted so all-digit keys stay
/// STRING ids (`type::thing` does not exist on this SurrealDB), the house idiom.
fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ─── Shared shapes (§8.1) ─────────────────────────────────────────────────────

/// Record pointer in the plan (`{kind,id}`), serialised null when absent.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EntityRef {
    pub kind: String,
    pub id: String,
}

/// The thing the plan is about (`{kind,id,title}`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Subject {
    pub kind: String,
    pub id: String,
    pub title: String,
}

/// One device fact surfaced on a plan item (from `receipt_unit.ai_facts`,
/// `.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §11). Additive.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanFact {
    pub kind: String,
    pub value: String,
    pub confidence: f64,
}

/// One item under a step (an expected serial, a booked unit, …).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    pub key: String,
    pub label: String,
    /// `expected | done | extra`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// `{kind,id}` when the item already IS an entity, `null` while only expected.
    pub entity: Option<EntityRef>,
    /// `fuzzy` when a `done` item matched its expected serial only after OCR
    /// confusable folding (see [`serial_match`]); omitted on an exact match so
    /// older PDA builds that never heard of it keep working unchanged.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_kind: Option<String>,
    /// OCR-verify device facts for this booked unit (§11). Omitted when empty —
    /// only `units`-step items of a booked unit ever carry them.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub facts: Vec<PlanFact>,
    /// OCR-verify status (`pending|ok|suggested`) for this booked unit (§11).
    /// Omitted when the unit has no review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// Why OCR-verify flagged this unit — `ocr_verify.suggestion.reason` — set
    /// ONLY when `verify == "suggested"` (§12 Task B). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_reason: Option<String>,
}

/// One receiving step.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub key: String,
    pub label: String,
    /// `done | active | todo | skipped`.
    pub state: String,
    /// Subset of `scan photo ocr note` — drives the capture rails.
    pub accepts: Vec<String>,
    /// The label a photo captured in this step is tagged with (CAS edge
    /// `evidence_type`). Absent for steps that take no photo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_type: Option<String>,
    pub expected: i64,
    pub done: i64,
    pub items: Vec<Item>,
}

/// The receiving plan (`{subject,active,steps}`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Plan {
    pub subject: Subject,
    /// Root HINT only (`{kind,id}|null`): the PDA keeps the active row locally.
    pub active: Option<EntityRef>,
    pub steps: Vec<Step>,
}

impl Plan {
    /// The empty plan served when no source matched (HTTP 200, never 404): the
    /// subject carries the query kind/id but an EMPTY title and no steps.
    fn empty(kind: &str, id: &str) -> Plan {
        Plan {
            subject: Subject {
                kind: kind.to_string(),
                id: id.to_string(),
                title: String::new(),
            },
            active: None,
            steps: Vec::new(),
        }
    }
}

// ─── Pure receiving builder (§8.1) ────────────────────────────────────────────

/// An expected serial taken from the matched ticket (data that EXISTS only —
/// no accessory/box expectations in v1).
#[derive(Debug, Clone)]
pub struct ExpectedSerial {
    pub serial: String,
    pub label: String,
}

/// A unit actually booked onto the receipt.
#[derive(Debug, Clone, Default)]
pub struct BookedUnit {
    pub unit_id: String,
    pub serial: Option<String>,
    /// Does this unit carry >= 1 (non-scan-frame) image attachment?
    pub has_device_photo: bool,
    /// OCR-verify device facts (`receipt_unit.ai_facts`, §11).
    pub facts: Vec<PlanFact>,
    /// OCR-verify status (`receipt_unit.ocr_verify.status`, §11).
    pub verify: Option<String>,
    /// OCR-verify suggestion reason (`receipt_unit.ocr_verify.suggestion.reason`),
    /// surfaced on the `units` item only when `verify == "suggested"` (§12 Task B).
    pub verify_reason: Option<String>,
}

/// Everything the pure receiving builder needs, already loaded.
#[derive(Debug, Clone)]
pub struct ReceivingPlanInput {
    pub subject: Subject,
    /// `parcel | walk_in` (normalised).
    pub intake: String,
    pub postal_done: bool,
    /// Receipt carries a `zoho_ticket` / `ticket_id` / `shipment_tracking`.
    pub has_shipment_link: bool,
    pub box_photo_done: bool,
    pub expected: Vec<ExpectedSerial>,
    pub units: Vec<BookedUnit>,
}

/// Normalise a serial for the expected-vs-booked compare: drop ALL whitespace,
/// uppercase. Case + whitespace only, per the contract (confusable folding is
/// the ticket matcher's job, not the plan's).
fn norm_serial(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase()
}

/// How an expected serial matched a booked one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialMatch {
    Exact,
    Fuzzy,
}

/// Fold the classic OCR confusables so a scanner misread doesn't block the
/// match: O↔0, I↔1, L↔1, S↔5, B↔8, Z↔2. Applied to both sides, so it folds
/// consistently whichever side was mis-scanned.
fn fold_ocr_confusables(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'O' => '0',
            'I' => '1',
            'L' => '1',
            'S' => '5',
            'B' => '8',
            'Z' => '2',
            other => other,
        })
        .collect()
}

/// OCR-tolerant serial compare (§ receiving plan `units` step, `serial_source:
/// "ocr"` units). `Exact` = equal after upper-casing and stripping whitespace;
/// `Fuzzy` = equal only after additionally folding OCR confusables on both
/// sides. Deliberately NOT edit-distance: a dropped or extra character must
/// still fail — that case needs a human, not an auto-match. Pure, unit-tested.
pub fn serial_match(a: &str, b: &str) -> Option<SerialMatch> {
    let na = norm_serial(a);
    let nb = norm_serial(b);
    if na == nb {
        return Some(SerialMatch::Exact);
    }
    if fold_ocr_confusables(&na) == fold_ocr_confusables(&nb) {
        return Some(SerialMatch::Fuzzy);
    }
    None
}

fn accepts(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A step computed before the active/todo pass is applied.
struct RawStep {
    key: &'static str,
    label: &'static str,
    accepts: Vec<String>,
    evidence_type: Option<&'static str>,
    expected: i64,
    done: i64,
    items: Vec<Item>,
    satisfied: bool,
    skipped: bool,
}

/// The `units` step (expected serials from the ticket; done = booked units that
/// carry one of them). `.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §8.1.
fn units_step(input: &ReceivingPlanInput) -> RawStep {
    let mut items: Vec<Item> = Vec::new();
    let expected_serials = &input.expected;

    if !expected_serials.is_empty() {
        // Track which booked units got matched to an expected serial.
        let mut matched_units: Vec<bool> = vec![false; input.units.len()];
        let mut done = 0i64;
        for (i, es) in expected_serials.iter().enumerate() {
            // Prefer an exact hit over a fuzzy one among the still-unmatched
            // booked units; `serial_match` itself never crosses an edit (a
            // dropped/extra char stays unmatched on purpose).
            let hit = input
                .units
                .iter()
                .enumerate()
                .filter(|(ui, _)| !matched_units[*ui])
                .filter_map(|(ui, u)| {
                    u.serial
                        .as_deref()
                        .and_then(|s| serial_match(&es.serial, s))
                        .map(|m| (ui, u, m))
                })
                .min_by_key(|(_, _, m)| if *m == SerialMatch::Exact { 0 } else { 1 });
            match hit {
                Some((ui, u, m)) => {
                    matched_units[ui] = true;
                    done += 1;
                    items.push(Item {
                        key: format!("u{}", i + 1),
                        label: es.label.clone(),
                        state: "done".into(),
                        serial: Some(es.serial.clone()),
                        entity: Some(EntityRef { kind: "unit".into(), id: u.unit_id.clone() }),
                        match_kind: (m == SerialMatch::Fuzzy).then(|| "fuzzy".to_string()),
                        facts: u.facts.clone(),
                        verify: u.verify.clone(),
                        verify_reason: reason_if_suggested(u),
                    });
                }
                None => items.push(Item {
                    key: format!("u{}", i + 1),
                    label: es.label.clone(),
                    state: "expected".into(),
                    serial: Some(es.serial.clone()),
                    entity: None,
                    match_kind: None,
                    facts: Vec::new(),
                    verify: None,
                    verify_reason: None,
                }),
            }
        }
        // Booked units matching no expected serial → extra.
        for (ui, u) in input.units.iter().enumerate() {
            if matched_units[ui] {
                continue;
            }
            items.push(Item {
                key: u.unit_id.clone(),
                label: unit_label(u),
                state: "extra".into(),
                serial: u.serial.clone(),
                entity: Some(EntityRef { kind: "unit".into(), id: u.unit_id.clone() }),
                match_kind: None,
                facts: u.facts.clone(),
                verify: u.verify.clone(),
                verify_reason: reason_if_suggested(u),
            });
        }
        let expected = expected_serials.len() as i64;
        RawStep {
            key: "units",
            label: "Units",
            accepts: accepts(&["scan", "photo", "ocr", "note"]),
            evidence_type: None,
            expected,
            done,
            items,
            satisfied: expected > 0 && done >= expected,
            skipped: false,
        }
    } else {
        // No ticket matched: the booked units ARE the expectation. expected =
        // max(1, booked) so an empty receipt is still "one unit wanted".
        for u in &input.units {
            items.push(Item {
                key: u.unit_id.clone(),
                label: unit_label(u),
                state: "done".into(),
                serial: u.serial.clone(),
                entity: Some(EntityRef { kind: "unit".into(), id: u.unit_id.clone() }),
                match_kind: None,
                facts: u.facts.clone(),
                verify: u.verify.clone(),
                verify_reason: reason_if_suggested(u),
            });
        }
        let booked = input.units.len() as i64;
        let expected = booked.max(1);
        RawStep {
            key: "units",
            label: "Units",
            accepts: accepts(&["scan", "photo", "ocr", "note"]),
            evidence_type: None,
            expected,
            done: booked,
            items,
            satisfied: booked >= expected,
            skipped: false,
        }
    }
}

/// The `device_photo` step — one item per booked unit, done iff that unit has a
/// (non-scan-frame) image attachment of its own.
fn device_photo_step(input: &ReceivingPlanInput) -> RawStep {
    let mut items = Vec::with_capacity(input.units.len());
    let mut done = 0i64;
    for u in &input.units {
        let state = if u.has_device_photo {
            done += 1;
            "done"
        } else {
            "expected"
        };
        items.push(Item {
            key: u.unit_id.clone(),
            label: unit_label(u),
            state: state.into(),
            serial: u.serial.clone(),
            entity: Some(EntityRef { kind: "unit".into(), id: u.unit_id.clone() }),
            match_kind: None,
            // device_photo items do not carry OCR-verify facts (§11 scopes
            // them to the `units` step).
            facts: Vec::new(),
            verify: None,
            verify_reason: None,
        });
    }
    let expected = input.units.len() as i64;
    RawStep {
        key: "device_photo",
        label: "Device photo",
        accepts: accepts(&["photo"]),
        evidence_type: Some("device_photo"),
        expected,
        done,
        items,
        satisfied: expected > 0 && done >= expected,
        skipped: false,
    }
}

fn unit_label(u: &BookedUnit) -> String {
    u.serial.clone().unwrap_or_else(|| format!("Unit {}", &u.unit_id))
}

/// The `units` item's `verify_reason` — present ONLY when the unit's OCR-verify
/// status is `suggested` (§12 Task B).
fn reason_if_suggested(u: &BookedUnit) -> Option<String> {
    if u.verify.as_deref() == Some("suggested") {
        u.verify_reason.clone()
    } else {
        None
    }
}

/// The root `active` hint: the first booked unit without a device photo, else
/// the receipt itself, else null. HINT only (the PDA keeps its own active row).
fn active_hint(input: &ReceivingPlanInput) -> Option<EntityRef> {
    if let Some(u) = input.units.iter().find(|u| !u.has_device_photo) {
        return Some(EntityRef { kind: "unit".into(), id: u.unit_id.clone() });
    }
    if input.subject.kind == "receipt" && !input.subject.id.is_empty() {
        return Some(EntityRef { kind: "receipt".into(), id: input.subject.id.clone() });
    }
    None
}

/// Build the receiving plan. Pure → unit-tested. Step order is the v1 contract:
/// `postal → shipment → box_photo → units → device_photo`. Exactly one step is
/// `active` (the first non-done, non-skipped one); every later step is `todo`.
pub fn build_receiving_plan(input: &ReceivingPlanInput) -> Plan {
    let walk_in = input.intake == "walk_in";

    let postal = RawStep {
        key: "postal",
        label: "Postal barcode",
        accepts: accepts(&["scan"]),
        evidence_type: None,
        expected: if walk_in { 0 } else { 1 },
        done: if input.postal_done { 1 } else { 0 },
        items: Vec::new(),
        satisfied: input.postal_done,
        skipped: walk_in,
    };
    let shipment = RawStep {
        key: "shipment",
        label: "Ticket / shipment",
        accepts: accepts(&["scan", "ocr", "note"]),
        evidence_type: None,
        expected: 1,
        done: if input.has_shipment_link { 1 } else { 0 },
        items: Vec::new(),
        satisfied: input.has_shipment_link,
        skipped: false,
    };
    let box_photo = RawStep {
        key: "box_photo",
        label: "Box photo",
        accepts: accepts(&["photo"]),
        evidence_type: Some("box_photo"),
        expected: 1,
        done: if input.box_photo_done { 1 } else { 0 },
        items: Vec::new(),
        satisfied: input.box_photo_done,
        skipped: false,
    };

    let raws = vec![postal, shipment, box_photo, units_step(input), device_photo_step(input)];

    // Second pass: exactly one `active` (first non-done, non-skipped).
    let mut active_assigned = false;
    let mut steps = Vec::with_capacity(raws.len());
    for r in raws {
        let state = if r.skipped {
            "skipped"
        } else if !active_assigned && !r.satisfied {
            active_assigned = true;
            "active"
        } else if !active_assigned && r.satisfied {
            "done"
        } else {
            "todo"
        };
        steps.push(Step {
            key: r.key.to_string(),
            label: r.label.to_string(),
            state: state.to_string(),
            accepts: r.accepts,
            evidence_type: r.evidence_type.map(|s| s.to_string()),
            expected: r.expected,
            done: r.done,
            items: r.items,
        });
    }

    Plan {
        subject: input.subject.clone(),
        active: active_hint(input),
        steps,
    }
}

// ─── Receiving loaders (§8.1) ─────────────────────────────────────────────────

/// What the plan is being asked for. Exactly one source per request; the handler
/// picks the first query parameter present, in this order.
#[derive(Debug, Clone)]
pub enum PlanQuery {
    Receipt(String),
    Ticket(String),
    Shipment(String),
    Serial(String),
}

/// Does `table:id` carry >= 1 non-scan-frame image attachment edge?
async fn has_image_attachment(db: &SurrealDb, table: &str, id: &str) -> bool {
    let rows: Vec<Value> = db
        .query(
            "SELECT count() AS c FROM has_attachment \
             WHERE in = type::record($tb, $id) AND out.context != 'scan_frame' GROUP ALL",
        )
        .bind(("tb", json!(table)))
        .bind(("id", json!(id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    rows.first()
        .and_then(|r| r.get("c").and_then(|v| v.as_i64()))
        .unwrap_or(0)
        > 0
}

/// The ticket's device facts (serial, model, customer, city, number) from its
/// `document.meta`, looked up by record-id leaf first, then by ticket number.
async fn ticket_facts(
    db: &SurrealDb,
    ticket_id: Option<&str>,
    zoho_ticket: Option<&str>,
) -> Option<Value> {
    if let Some(tid) = ticket_id.map(str::trim).filter(|s| !s.is_empty()) {
        let row: Option<Value> = db
            .query("SELECT meta FROM type::record('document', $id) LIMIT 1")
            .bind(("id", json!(tid)))
            .await
            .and_then(|mut r| r.take(0))
            .ok()
            .flatten();
        if let Some(meta) = row.and_then(|r| r.get("meta").cloned()).filter(Value::is_object) {
            return Some(meta);
        }
    }
    if let Some(num) = zoho_ticket
        .map(|s| s.trim().trim_start_matches('#').trim())
        .filter(|s| !s.is_empty())
    {
        let row: Option<Value> = db
            .query(
                "SELECT meta FROM document \
                 WHERE type = 'support_ticket' AND meta.ticket_number = $n LIMIT 1",
            )
            .bind(("n", json!(num)))
            .await
            .and_then(|mut r| r.take(0))
            .ok()
            .flatten();
        if let Some(meta) = row.and_then(|r| r.get("meta").cloned()).filter(Value::is_object) {
            return Some(meta);
        }
    }
    None
}

/// One expected serial from a ticket's `meta` (serial + model label), if the
/// ticket carries a serial at all.
fn expected_from_ticket(meta: &Value) -> Vec<ExpectedSerial> {
    let Some(serial) = s(meta, "serial_number") else {
        return Vec::new();
    };
    let label = match s(meta, "device_model") {
        Some(m) => format!("{m} · {serial}"),
        None => serial.clone(),
    };
    vec![ExpectedSerial { serial, label }]
}

/// Parse `receipt_unit.ai_facts` (`{ "<kind>": {value,confidence,…} }`) into the
/// plan's `facts[]` shape. Pure → unit-tested. Order is stable (sorted by kind).
pub fn facts_from_ai_facts(ai_facts: &Value) -> Vec<PlanFact> {
    let Some(obj) = ai_facts.as_object() else { return Vec::new() };
    let mut out: Vec<PlanFact> = obj
        .iter()
        .filter_map(|(kind, v)| {
            let value = v.get("value").and_then(|x| x.as_str())?.to_string();
            let confidence = v.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0);
            Some(PlanFact { kind: kind.clone(), value, confidence })
        })
        .collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind));
    out
}

/// Pull the OCR-verify status (`ocr_verify.status`) if any. Pure.
pub fn verify_from_ocr_verify(ocr_verify: &Value) -> Option<String> {
    ocr_verify
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Pull the OCR-verify suggestion reason (`ocr_verify.suggestion.reason`) if
/// any. Pure. The caller surfaces it only when the status is `suggested`.
pub fn verify_reason_from_ocr_verify(ocr_verify: &Value) -> Option<String> {
    ocr_verify
        .get("suggestion")
        .and_then(|s| s.get("reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Booked units of a receipt → `BookedUnit`s with their device-photo flag and
/// OCR-verify facts/status (§11).
async fn booked_units_of_receipt(db: &SurrealDb, receipt_id: &str) -> Vec<BookedUnit> {
    let units: Vec<Value> = db
        .query(
            "SELECT record::id(id) AS unit_id, serial, barcode, ai_facts, ocr_verify \
             FROM receipt_unit WHERE receipt_id = $r ORDER BY created_at ASC",
        )
        .bind(("r", json!(receipt_id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let mut out = Vec::with_capacity(units.len());
    for u in &units {
        let Some(unit_id) = s(u, "unit_id") else { continue };
        let serial = s(u, "serial").or_else(|| s(u, "barcode"));
        let has_device_photo = has_image_attachment(db, "receipt_unit", &unit_id).await;
        let facts = u.get("ai_facts").map(facts_from_ai_facts).unwrap_or_default();
        let verify = u.get("ocr_verify").and_then(|v| verify_from_ocr_verify(v));
        let verify_reason = u.get("ocr_verify").and_then(|v| verify_reason_from_ocr_verify(v));
        out.push(BookedUnit { unit_id, serial, has_device_photo, facts, verify, verify_reason });
    }
    out
}

/// Load the receiving plan for a query. No source matched → the empty plan
/// (HTTP 200, never 404). For a `?serial` with no ticket, the serial itself is
/// the one expected item (a serial is self-matching — see §8.1 "pre-receipt").
pub async fn load_receiving_plan(db: &SurrealDb, query: &PlanQuery) -> Plan {
    match query {
        PlanQuery::Receipt(id) => {
            let receipt: Option<Value> = db
                .query("SELECT record::id(id) AS receipt_id, * FROM type::record($rid) LIMIT 1")
                .bind(("rid", rid("receipt", id)))
                .await
                .and_then(|mut r| r.take(0))
                .ok()
                .flatten();
            let Some(receipt) = receipt else {
                return Plan::empty("receipt", id);
            };

            let intake = crate::services::receiving::normalize_intake(
                receipt.get("intake").and_then(|v| v.as_str()),
            )
            .to_string();
            let postal_done = s(&receipt, "postal_barcode").is_some();
            let has_shipment_link = s(&receipt, "zoho_ticket").is_some()
                || s(&receipt, "ticket_id").is_some()
                || s(&receipt, "shipment_tracking").is_some();
            let box_photo_done = has_image_attachment(db, "receipt", id).await;

            let meta = ticket_facts(
                db,
                s(&receipt, "ticket_id").as_deref(),
                s(&receipt, "zoho_ticket").as_deref(),
            )
            .await;
            let expected = meta.as_ref().map(expected_from_ticket).unwrap_or_default();
            let units = booked_units_of_receipt(db, id).await;

            let title = s(&receipt, "client_name").unwrap_or_else(|| id.clone());
            build_receiving_plan(&ReceivingPlanInput {
                subject: Subject { kind: "receipt".into(), id: id.clone(), title },
                intake,
                postal_done,
                has_shipment_link,
                box_photo_done,
                expected,
                units,
            })
        }
        PlanQuery::Ticket(key) => {
            // The `document` leaf (ticket_id) or the human ticket number.
            let meta = ticket_facts(db, Some(key), Some(key)).await;
            let Some(meta) = meta else {
                return Plan::empty("ticket", key);
            };
            let title = s(&meta, "ticket_number")
                .map(|n| format!("#{n}"))
                .or_else(|| s(&meta, "company"))
                .or_else(|| s(&meta, "customer"))
                .unwrap_or_else(|| key.clone());
            build_receiving_plan(&ReceivingPlanInput {
                subject: Subject { kind: "ticket".into(), id: key.clone(), title },
                intake: "parcel".into(),
                postal_done: false,
                has_shipment_link: true,
                box_photo_done: false,
                expected: expected_from_ticket(&meta),
                units: Vec::new(),
            })
        }
        PlanQuery::Shipment(tracking) => {
            let row: Option<Value> = db
                .query(
                    "SELECT tracking_number FROM stock_picking_delivery \
                     WHERE string::lowercase(tracking_number) = string::lowercase($t) LIMIT 1",
                )
                .bind(("t", json!(tracking)))
                .await
                .and_then(|mut r| r.take(0))
                .ok()
                .flatten();
            let Some(row) = row else {
                return Plan::empty("shipment", tracking);
            };
            let title = s(&row, "tracking_number").unwrap_or_else(|| tracking.clone());
            // A shipment carries only a tracking number — no expected serials.
            build_receiving_plan(&ReceivingPlanInput {
                subject: Subject { kind: "shipment".into(), id: tracking.clone(), title },
                intake: "parcel".into(),
                postal_done: false,
                has_shipment_link: true,
                box_photo_done: false,
                expected: Vec::new(),
                units: Vec::new(),
            })
        }
        PlanQuery::Serial(serial) => {
            // A serial is self-matching: produce a plan with the serial as the one
            // expected item, enriched with the device model if a ticket carries it.
            let row: Option<Value> = db
                .query(
                    "SELECT meta FROM document \
                     WHERE type = 'support_ticket' AND meta.serial_number = $sn LIMIT 1",
                )
                .bind(("sn", json!(serial)))
                .await
                .and_then(|mut r| r.take(0))
                .ok()
                .flatten();
            let meta = row
                .and_then(|r| r.get("meta").cloned())
                .filter(Value::is_object);
            let label = meta
                .as_ref()
                .and_then(|m| s(m, "device_model"))
                .map(|model| format!("{model} · {serial}"))
                .unwrap_or_else(|| serial.clone());
            build_receiving_plan(&ReceivingPlanInput {
                subject: Subject { kind: "serial".into(), id: serial.clone(), title: serial.clone() },
                intake: "parcel".into(),
                postal_done: false,
                has_shipment_link: meta.is_some(),
                box_photo_done: false,
                expected: vec![ExpectedSerial { serial: serial.clone(), label }],
                units: Vec::new(),
            })
        }
    }
}

// ─── Order plan (§8.1, REPAIR_LIFECYCLE §1) ───────────────────────────────────

/// A related file pointer on an order item (`ref:{kind:"file",id}`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileRef {
    pub kind: String,
    pub id: String,
}

/// One checklist item of the order plan.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderItem {
    pub key: String,
    pub label: String,
    /// `done | missing | n/a`.
    pub state: String,
    pub evidence_type: String,
    #[serde(rename = "ref")]
    pub ref_: Option<FileRef>,
}

/// The order plan (`{stage, items}`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderPlan {
    pub stage: String,
    pub items: Vec<OrderItem>,
}

/// Map `order.status` onto a coarse repair stage. PROVISIONAL (documented in
/// §8.1): the 8-stage lifecycle (REPAIR_LIFECYCLE §1) is spec; today the server
/// only has `order.status`. Unknown statuses fall to `intake` (the safe floor).
pub fn stage_for_status(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "pending" | "received" => "intake",
        "processing" => "repair",
        "completed" => "shipping",
        "cancelled" => "cancelled",
        _ => "intake",
    }
}

/// The DEFAULT per-stage evidence checklist (REPAIR_LIFECYCLE §1), keyed by
/// stage; each item's `key` doubles as its `evidence_type`. `outgoing_test` has
/// no `outgoing` stage in the coarse status map, so it rides the `repair` stage
/// (documented). Overridable per tenant via `system_config:plan_templates`.
const DEFAULT_ORDER_CHECKLIST: &[(&str, &str)] = &[
    ("intake", "serial_photo"),
    ("intake", "accessories_photo"),
    ("intake", "packaging_photo"),
    ("intake", "db_backup"),
    ("repair", "parts_scanned"),
    ("repair", "master_report"),
    ("repair", "outgoing_test"),
    ("shipping", "shipping_label"),
];

/// A readable English label for a checklist key (labels stay English; i18n is
/// layered later — `feedback_english_labels_i18n`).
fn checklist_label(key: &str) -> String {
    match key {
        "serial_photo" => "Serial-number photo",
        "accessories_photo" => "Accessories photo",
        "packaging_photo" => "Packaging photo",
        "db_backup" => "DB backup",
        "parts_scanned" => "Parts scanned",
        "master_report" => "Master report",
        "outgoing_test" => "Outgoing test",
        "shipping_label" => "Shipping label",
        other => other,
    }
    .to_string()
}

/// Load the order checklist: a tenant override from `system_config:plan_templates`
/// (`{ "order": { "<stage>": ["<key>", …] } }`) when it parses cleanly, else the
/// const default. Receiving step templates are NOT overridable yet (v1 TODO —
/// see TECH_DEBT); only the order checklist reads config here.
async fn load_order_checklist(db: &SurrealDb) -> Vec<(String, String)> {
    let row: Option<Value> = db
        .query("SELECT * FROM type::record('system_config', 'plan_templates') LIMIT 1")
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    if let Some(order) = row
        .as_ref()
        .and_then(|r| r.get("order"))
        .and_then(|v| v.as_object())
    {
        let mut out = Vec::new();
        for (stage, arr) in order {
            if let Some(keys) = arr.as_array() {
                for k in keys {
                    if let Some(key) = k.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        out.push((stage.clone(), key.to_string()));
                    }
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    DEFAULT_ORDER_CHECKLIST
        .iter()
        .map(|(s, k)| (s.to_string(), k.to_string()))
        .collect()
}

/// Build the order plan. Pure → unit-tested. Every checklist item is emitted:
/// an item of the CURRENT stage is `done` (evidence edge with that
/// `evidence_type` exists) or `missing`; an item of another stage is `n/a`.
/// STRICT: a legacy photo with NO `evidence_type` on its edge does NOT count.
pub fn build_order_plan(
    stage: &str,
    checklist: &[(String, String)],
    evidence: &HashMap<String, String>,
) -> OrderPlan {
    let mut items = Vec::with_capacity(checklist.len());
    for (item_stage, key) in checklist {
        let (state, ref_) = if item_stage == stage {
            match evidence.get(key) {
                Some(file_id) => (
                    "done",
                    Some(FileRef { kind: "file".into(), id: file_id.clone() }),
                ),
                None => ("missing", None),
            }
        } else {
            ("n/a", None)
        };
        items.push(OrderItem {
            key: key.clone(),
            label: checklist_label(key),
            state: state.to_string(),
            evidence_type: key.clone(),
            ref_,
        });
    }
    OrderPlan {
        stage: stage.to_string(),
        items,
    }
}

/// Evidence edges (`has_attachment` carrying an `evidence_type`) on one record,
/// newest first, folded into `evidence_type -> file_id` (newest wins). Legacy
/// edges with no `evidence_type` are excluded by the query (STRICT rule).
async fn evidence_edges(db: &SurrealDb, table: &str, id: &str, out: &mut HashMap<String, String>) {
    let rows: Vec<Value> = db
        .query(
            "SELECT record::id(out.id) AS file_id, evidence_type, created_at FROM has_attachment \
             WHERE in = type::record($tb, $id) AND evidence_type != NONE \
             ORDER BY created_at DESC",
        )
        .bind(("tb", json!(table)))
        .bind(("id", json!(id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    for r in rows {
        if let (Some(et), Some(fid)) = (s(&r, "evidence_type"), s(&r, "file_id")) {
            // Newest first → the first one we see per evidence_type wins.
            out.entry(et).or_insert(fid);
        }
    }
}

/// Load the order plan for an order record-id leaf, or `None` when the order
/// does not exist. Evidence is gathered from the order, its linked unit
/// (`order.unit_id`) and its receipt (`metadata.receipt_uuid`).
pub async fn load_order_plan(db: &SurrealDb, order_id: &str) -> Option<OrderPlan> {
    let order: Option<Value> = db
        .query(
            "SELECT status, unit_id, metadata.receipt_uuid AS receipt_uuid \
             FROM type::record($rid) LIMIT 1",
        )
        .bind(("rid", rid("order", order_id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    let order = order?;
    let stage = stage_for_status(order.get("status").and_then(|v| v.as_str()).unwrap_or(""));

    let mut evidence: HashMap<String, String> = HashMap::new();
    evidence_edges(db, "order", order_id, &mut evidence).await;
    if let Some(uid) = s(&order, "unit_id") {
        evidence_edges(db, "receipt_unit", &uid, &mut evidence).await;
    }
    if let Some(ruid) = s(&order, "receipt_uuid") {
        evidence_edges(db, "receipt", &ruid, &mut evidence).await;
    }

    let checklist = load_order_checklist(db).await;
    Some(build_order_plan(stage, &checklist, &evidence))
}

/// The order plan as a JSON value (for the `plan` field of the EntityView and
/// the capture response). `null` when the order is unknown.
pub async fn order_plan_value(db: &SurrealDb, order_id: &str) -> Value {
    match load_order_plan(db, order_id).await {
        Some(p) => serde_json::to_value(p).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject() -> Subject {
        Subject { kind: "receipt".into(), id: "r1".into(), title: "ACME".into() }
    }

    fn base_input() -> ReceivingPlanInput {
        ReceivingPlanInput {
            subject: subject(),
            intake: "parcel".into(),
            postal_done: false,
            has_shipment_link: false,
            box_photo_done: false,
            expected: Vec::new(),
            units: Vec::new(),
        }
    }

    fn step<'a>(p: &'a Plan, key: &str) -> &'a Step {
        p.steps.iter().find(|s| s.key == key).expect("step present")
    }

    #[test]
    fn exactly_one_active_step_and_it_is_the_first_unsatisfied() {
        let p = build_receiving_plan(&base_input());
        let actives: Vec<&str> = p
            .steps
            .iter()
            .filter(|s| s.state == "active")
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(actives, vec!["postal"], "fresh parcel: postal is active");
        assert_eq!(p.steps.iter().filter(|s| s.state == "active").count(), 1);
    }

    #[test]
    fn walk_in_skips_postal_and_shipment_becomes_active() {
        let mut input = base_input();
        input.intake = "walk_in".into();
        let p = build_receiving_plan(&input);
        assert_eq!(step(&p, "postal").state, "skipped");
        assert_eq!(step(&p, "shipment").state, "active");
    }

    #[test]
    fn done_active_todo_transition_advances_with_progress() {
        let mut input = base_input();
        input.postal_done = true; // postal done → shipment active
        let p = build_receiving_plan(&input);
        assert_eq!(step(&p, "postal").state, "done");
        assert_eq!(step(&p, "shipment").state, "active");
        assert_eq!(step(&p, "box_photo").state, "todo");

        input.has_shipment_link = true; // → box_photo active
        let p = build_receiving_plan(&input);
        assert_eq!(step(&p, "shipment").state, "done");
        assert_eq!(step(&p, "box_photo").state, "active");
    }

    #[test]
    fn expected_vs_booked_with_an_extra_unit() {
        let mut input = base_input();
        input.postal_done = true;
        input.has_shipment_link = true;
        input.box_photo_done = true;
        input.expected = vec![ExpectedSerial {
            serial: "I81800562".into(),
            label: "Acme 770 · I81800562".into(),
        }];
        input.units = vec![
            // Matches the expected serial (whitespace/case ignored).
            BookedUnit { unit_id: "u-match".into(), serial: Some("i81800562 ".into()), has_device_photo: false, ..Default::default() },
            // Not expected → extra.
            BookedUnit { unit_id: "u-extra".into(), serial: Some("PSU-99".into()), has_device_photo: false, ..Default::default() },
        ];
        let p = build_receiving_plan(&input);
        let units = step(&p, "units");
        assert_eq!(units.expected, 1);
        assert_eq!(units.done, 1);
        assert_eq!(units.state, "done");
        let matched = units.items.iter().find(|i| i.serial.as_deref() == Some("I81800562")).unwrap();
        assert_eq!(matched.state, "done");
        assert_eq!(matched.entity.as_ref().unwrap().id, "u-match");
        let extra = units.items.iter().find(|i| i.key == "u-extra").unwrap();
        assert_eq!(extra.state, "extra");
        // device_photo is then the active step (neither unit has a photo).
        assert_eq!(step(&p, "device_photo").state, "active");
        // exact match → no `match` field on the item.
        assert!(matched.match_kind.is_none());
    }

    #[test]
    fn serial_match_exact_ignores_case_and_whitespace() {
        assert_eq!(serial_match("I81800562", " i81800562 "), Some(SerialMatch::Exact));
    }

    #[test]
    fn serial_match_fuzzy_folds_ocr_confusables() {
        // O/0, I/1, L/1, S/5, B/8, Z/2 misreads on either side still match.
        assert_eq!(serial_match("PS0123456789", "PS0I23456789"), Some(SerialMatch::Fuzzy));
        assert_eq!(serial_match("I81800562", "181800562"), Some(SerialMatch::Fuzzy));
    }

    #[test]
    fn serial_match_never_crosses_an_edit() {
        // A dropped digit is NOT a confusable fold — must stay unmatched.
        assert_eq!(serial_match("PS123456789", "PS12345678"), None);
        assert_eq!(serial_match("ABCDEF", "XYZ123"), None);
    }

    #[test]
    fn units_step_fuzzy_match_is_still_done_but_flagged() {
        let mut input = base_input();
        input.postal_done = true;
        input.has_shipment_link = true;
        input.box_photo_done = true;
        input.expected = vec![ExpectedSerial { serial: "I81800562".into(), label: "Acme 770 · I81800562".into() }];
        // OCR misread O→0: an exact compare would miss this.
        input.units = vec![BookedUnit { unit_id: "u-ocr".into(), serial: Some("181800562".into()), has_device_photo: false, ..Default::default() }];
        let p = build_receiving_plan(&input);
        let units = step(&p, "units");
        assert_eq!(units.done, 1);
        assert_eq!(units.state, "done");
        let matched = units.items.iter().find(|i| i.serial.as_deref() == Some("I81800562")).unwrap();
        assert_eq!(matched.state, "done");
        assert_eq!(matched.match_kind.as_deref(), Some("fuzzy"));
    }

    #[test]
    fn no_ticket_booked_unit_counts_as_done() {
        let mut input = base_input();
        input.postal_done = true;
        input.has_shipment_link = true;
        input.box_photo_done = true;
        input.units = vec![BookedUnit {
            unit_id: "u1".into(),
            serial: Some("SN1".into()),
            has_device_photo: true,
            ..Default::default()
        }];
        let p = build_receiving_plan(&input);
        let units = step(&p, "units");
        assert_eq!(units.expected, 1);
        assert_eq!(units.done, 1);
        assert_eq!(units.state, "done");
        // Every step satisfied → no active step, and active hint is the receipt.
        assert_eq!(p.steps.iter().filter(|s| s.state == "active").count(), 0);
        assert_eq!(p.active.as_ref().unwrap().kind, "receipt");
    }

    #[test]
    fn units_step_items_carry_ocr_verify_facts_and_status() {
        let mut input = base_input();
        input.postal_done = true;
        input.has_shipment_link = true;
        input.box_photo_done = true;
        input.units = vec![BookedUnit {
            unit_id: "u1".into(),
            serial: Some("SN1".into()),
            has_device_photo: true,
            facts: vec![PlanFact { kind: "model".into(), value: "Acme 770".into(), confidence: 0.9 }],
            verify: Some("suggested".into()),
            verify_reason: Some("serial OCR differs from ticket".into()),
        }];
        let p = build_receiving_plan(&input);
        let units = step(&p, "units");
        let item = units.items.iter().find(|i| i.key == "u1").unwrap();
        assert_eq!(item.verify.as_deref(), Some("suggested"));
        // §12 Task B: verify_reason surfaces because verify == "suggested".
        assert_eq!(item.verify_reason.as_deref(), Some("serial OCR differs from ticket"));
        assert_eq!(item.facts.len(), 1);
        assert_eq!(item.facts[0].kind, "model");
        // A device_photo item of the SAME unit carries no facts/verify (§11).
        let dp = step(&p, "device_photo");
        let dpi = dp.items.iter().find(|i| i.key == "u1").unwrap();
        assert!(dpi.facts.is_empty());
        assert!(dpi.verify.is_none());
        assert!(dpi.verify_reason.is_none());
    }

    #[test]
    fn verify_reason_only_when_status_suggested() {
        // A reason present but status != "suggested" MUST NOT surface (§12 Task B).
        let mut input = base_input();
        input.postal_done = true;
        input.has_shipment_link = true;
        input.box_photo_done = true;
        input.units = vec![BookedUnit {
            unit_id: "u1".into(),
            serial: Some("SN1".into()),
            has_device_photo: true,
            verify: Some("pending".into()),
            verify_reason: Some("ignored while pending".into()),
            ..Default::default()
        }];
        let p = build_receiving_plan(&input);
        let item = step(&p, "units").items.iter().find(|i| i.key == "u1").unwrap().clone();
        assert_eq!(item.verify.as_deref(), Some("pending"));
        assert!(item.verify_reason.is_none());

        // Parser pulls suggestion.reason out of an ocr_verify object.
        let ov = json!({ "status": "suggested", "suggestion": { "reason": "low confidence" } });
        assert_eq!(verify_reason_from_ocr_verify(&ov).as_deref(), Some("low confidence"));
        assert!(verify_reason_from_ocr_verify(&json!({ "status": "ok" })).is_none());
    }

    #[test]
    fn ai_facts_and_ocr_verify_parsers() {
        let ai = json!({
            "model": { "value": "Acme 770", "confidence": 0.9, "source": "ocr_verify" },
            "manufacturer": { "value": "Acme", "confidence": 0.8 }
        });
        let facts = facts_from_ai_facts(&ai);
        assert_eq!(facts.len(), 2);
        // sorted by kind
        assert_eq!(facts[0].kind, "manufacturer");
        assert_eq!(facts[1].kind, "model");
        assert_eq!(facts[1].value, "Acme 770");
        assert!(facts_from_ai_facts(&Value::Null).is_empty());

        assert_eq!(
            verify_from_ocr_verify(&json!({ "status": "pending" })).as_deref(),
            Some("pending")
        );
        assert!(verify_from_ocr_verify(&Value::Null).is_none());
    }

    #[test]
    fn active_hint_points_at_first_unit_without_a_photo() {
        let mut input = base_input();
        input.units = vec![
            BookedUnit { unit_id: "a".into(), serial: None, has_device_photo: true, ..Default::default() },
            BookedUnit { unit_id: "b".into(), serial: None, has_device_photo: false, ..Default::default() },
        ];
        let p = build_receiving_plan(&input);
        let hint = p.active.as_ref().unwrap();
        assert_eq!((hint.kind.as_str(), hint.id.as_str()), ("unit", "b"));
    }

    // ── order plan ──

    fn checklist() -> Vec<(String, String)> {
        DEFAULT_ORDER_CHECKLIST
            .iter()
            .map(|(s, k)| (s.to_string(), k.to_string()))
            .collect()
    }

    #[test]
    fn order_stage_mapping() {
        assert_eq!(stage_for_status("pending"), "intake");
        assert_eq!(stage_for_status("received"), "intake");
        assert_eq!(stage_for_status("processing"), "repair");
        assert_eq!(stage_for_status("completed"), "shipping");
        assert_eq!(stage_for_status("cancelled"), "cancelled");
        assert_eq!(stage_for_status("weird"), "intake");
    }

    #[test]
    fn order_plan_strict_evidence_and_na() {
        let mut ev = HashMap::new();
        ev.insert("serial_photo".to_string(), "f-serial".to_string());
        // A current-stage (intake) item with evidence is done + carries ref;
        // one without is missing; an item of another stage is n/a.
        let plan = build_order_plan("intake", &checklist(), &ev);
        let serial = plan.items.iter().find(|i| i.key == "serial_photo").unwrap();
        assert_eq!(serial.state, "done");
        assert_eq!(serial.ref_.as_ref().unwrap().id, "f-serial");
        let backup = plan.items.iter().find(|i| i.key == "db_backup").unwrap();
        assert_eq!(backup.state, "missing");
        assert!(backup.ref_.is_none());
        let label = plan.items.iter().find(|i| i.key == "shipping_label").unwrap();
        assert_eq!(label.state, "n/a");
    }

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        db.query(
            "DEFINE TABLE order SCHEMALESS; DEFINE TABLE receipt SCHEMALESS; \
             DEFINE TABLE receipt_unit SCHEMALESS; DEFINE TABLE document SCHEMALESS; \
             DEFINE TABLE file_resource SCHEMALESS; DEFINE TABLE system_config SCHEMALESS; \
             DEFINE TABLE stock_picking_delivery SCHEMALESS; \
             DEFINE TABLE has_attachment TYPE RELATION SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    /// Evidence found through the order's LINKED UNIT (`order.unit_id`) counts —
    /// the strict evidence rule looks at the order, its unit and its receipt.
    #[tokio::test]
    async fn order_plan_finds_evidence_through_the_linked_unit() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('order','o1') SET status='pending', unit_id='u1'; \
             CREATE type::record('receipt_unit','u1') SET serial='SN1'; \
             CREATE type::record('file_resource','f1') SET cas_uuid='f1', mime_type='image/jpeg'; \
             RELATE (type::record('receipt_unit','u1'))->has_attachment->(type::record('file_resource','f1')) \
                SET created_at=time::now(), evidence_type='serial_photo'; \
             CREATE type::record('file_resource','f2') SET cas_uuid='f2', mime_type='image/jpeg'; \
             RELATE (type::record('order','o1'))->has_attachment->(type::record('file_resource','f2')) \
                SET created_at=time::now();",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let plan = load_order_plan(&db, "o1").await.unwrap();
        assert_eq!(plan.stage, "intake");
        let serial = plan.items.iter().find(|i| i.key == "serial_photo").unwrap();
        // Found via the linked unit's evidence edge.
        assert_eq!(serial.state, "done");
        assert_eq!(serial.ref_.as_ref().unwrap().id, "f1");
        // The order's own photo has NO evidence_type → the strict rule ignores it,
        // so another intake item stays missing.
        let pkg = plan.items.iter().find(|i| i.key == "packaging_photo").unwrap();
        assert_eq!(pkg.state, "missing");
    }

    /// No source matched → HTTP-200-shaped empty plan (no steps, empty title).
    #[tokio::test]
    async fn no_match_returns_empty_plan() {
        let db = test_db().await;
        let p = load_receiving_plan(&db, &PlanQuery::Receipt("nope".into())).await;
        assert!(p.steps.is_empty());
        assert_eq!(p.subject.title, "");
        assert_eq!(p.subject.kind, "receipt");

        let p = load_receiving_plan(&db, &PlanQuery::Ticket("nope".into())).await;
        assert!(p.steps.is_empty());
    }

    /// A booked receipt with a ticket serial yields the full step list with the
    /// expected serial as a `units` item.
    #[tokio::test]
    async fn receipt_plan_pulls_expected_serial_from_ticket() {
        let db = test_db().await;
        db.query(
            "CREATE type::record('receipt','r1') SET client_name='ACME', intake='parcel', \
                postal_barcode='00340', ticket_id='t1'; \
             CREATE type::record('document','t1') SET type='support_ticket', \
                meta={ ticket_number: '32834', serial_number: 'I81800562', device_model: 'Acme 770' };",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        let p = load_receiving_plan(&db, &PlanQuery::Receipt("r1".into())).await;
        assert_eq!(p.subject.title, "ACME");
        assert_eq!(step(&p, "postal").state, "done");
        assert_eq!(step(&p, "shipment").state, "done");
        let units = step(&p, "units");
        assert_eq!(units.expected, 1);
        assert_eq!(units.items[0].serial.as_deref(), Some("I81800562"));
        assert_eq!(units.items[0].state, "expected");
    }
}
