//! `POST /api/entity/:kind/:id/capture` — the single PDA capture path
//! (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §8.3, joint contract with movFast).
//!
//! A THIN dispatcher over the existing handlers — it never re-implements their
//! logic. A photo is RELATEd through the SAME helper as `/api/files/attach`
//! (`files::relate_attachment`, with the capture's `evidence_type`); a barcode
//! on a receipt's active `units` step adds a unit via `receiving::add_unit`; an
//! OCR/note on a receipt goes through `receiving::append_note`. Every capture is
//! logged as an append-only `event` referencing its client-minted `capture_id`,
//! which makes a retry idempotent (no second attachment / unit / note) and the
//! response carries the REFRESHED plan so the panel repaints in one round trip.
//!
//! Mounted in the same device-JWT group as `/api/scan` and `/receiving/*`; each
//! call additionally enforces `require_active_device` exactly like those do.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use eck_core::auth::Claims;
use eck_core::db::SurrealDb;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::services::{events, plan, receiving};
use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn bad(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

/// The capture body. Every field is lenient (`#[serde(default)]`) so a missing
/// `capture_id` is a clean 400 from our own check rather than a 422 from the
/// JSON extractor.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CaptureRequest {
    /// `photo | barcode | ocr | note`.
    #[serde(rename = "type", default)]
    pub ctype: String,
    /// Client-minted UUID — MANDATORY, makes the retry idempotent.
    #[serde(default)]
    pub capture_id: String,
    #[serde(default)]
    pub evidence_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub barcode: Option<String>,
    /// How `barcode` was obtained: `barcode` (scanned, default) | `ocr` (the
    /// OCR review tap-out picked these words off a camera still) | `manual`.
    /// An `ocr` capture is treated as a serial even when no expected serial
    /// matched it (OCR has no barcode fallback — there was never a barcode).
    #[serde(default)]
    pub serial_source: Option<String>,
    /// CAS uuid of a photo already uploaded via `/api/upload/image`.
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// Storage table for a capture `kind` (`unit` → `receipt_unit`).
fn table_for(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "receipt" => "receipt",
        "unit" => "receipt_unit",
        "order" => "order",
        _ => return None,
    })
}

/// `POST /api/entity/:kind/:id/capture`
pub async fn capture(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path((kind, id)): Path<(String, String)>,
    Json(req): Json<CaptureRequest>,
) -> ApiResult<Json<Value>> {
    let device_id = req.device_id.clone().unwrap_or_else(|| claims.sub.clone());
    crate::handlers::pda::require_active_device(&state, &device_id).await?;

    let out = do_capture(&state.db, &state.instance_id, &kind, &id, &device_id, &req).await?;
    Ok(Json(out))
}

/// The capture core, free of `AppState`/HTTP auth so it is testable against a
/// `mem://` DB. Returns the response JSON.
pub(crate) async fn do_capture(
    db: &SurrealDb,
    instance_id: &str,
    kind: &str,
    id: &str,
    actor: &str,
    req: &CaptureRequest,
) -> ApiResult<Value> {
    let Some(table) = table_for(kind) else {
        return Err(bad(format!("unknown capture kind '{kind}'")));
    };
    let capture_id = req.capture_id.trim();
    if capture_id.is_empty() {
        return Err(bad("capture_id is required"));
    }
    let ctype = req.ctype.trim().to_ascii_lowercase();
    if !matches!(ctype.as_str(), "photo" | "barcode" | "ocr" | "note") {
        return Err(bad(format!("unknown capture type '{}'", req.ctype)));
    }

    // The entity must exist (a receipt can grow a unit, but the receipt itself
    // has to be there). Missing record → 404.
    if !record_exists(db, table, id).await {
        return Err((StatusCode::NOT_FOUND, format!("{kind} '{id}' not found")));
    }

    let actor = if actor.trim().is_empty() { None } else { Some(actor.trim()) };

    // Idempotency: a retry of the same capture_id for the same entity does NOT
    // repeat the mutation — it just re-renders the current plan.
    let replayed = events::capture_seen(db, kind, id, capture_id).await;
    if !replayed {
        let summary = perform(db, instance_id, kind, table, id, &ctype, actor, req).await?;
        // Log the capture itself (append-only, references the capture_id) — this
        // is also what `capture_seen` keys on for the next retry.
        events::record(
            db,
            instance_id,
            events::Event::new(&ctype, kind, id, summary)
                .actor(actor.map(|s| s.to_string()))
                .reference("capture", capture_id),
        )
        .await;
    }

    let plan = plan_for_entity(db, kind, id).await;
    let (title, status) = entity_summary(db, kind, id).await;
    Ok(json!({
        "ok": true,
        "replayed": replayed,
        "capture_id": capture_id,
        "plan": plan,
        "entity": { "kind": kind, "id": id, "title": title, "status": status },
    }))
}

/// Dispatch the mutation and return a one-line summary for the event log.
async fn perform(
    db: &SurrealDb,
    instance_id: &str,
    kind: &str,
    table: &str,
    id: &str,
    ctype: &str,
    actor: Option<&str>,
    req: &CaptureRequest,
) -> ApiResult<String> {
    match ctype {
        "photo" => {
            let file_id = req
                .file_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("file_id is required for a photo capture"))?;
            let label = req
                .evidence_type
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("capture");
            // Shared RELATE helper (same as /api/files/attach). An unknown file
            // is a client error here (400), not a 404.
            crate::handlers::files::relate_attachment(
                db,
                instance_id,
                table,
                id,
                file_id,
                label,
                req.evidence_type.as_deref(),
                actor,
            )
            .await
            .map_err(|(code, msg)| {
                if code == StatusCode::NOT_FOUND {
                    bad(msg)
                } else {
                    (code, msg)
                }
            })?;
            Ok(format!("Photo captured ({label})"))
        }
        "barcode" => {
            let barcode = req
                .barcode
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("barcode is required for a barcode capture"))?;
            capture_barcode(db, instance_id, kind, id, barcode, req).await
        }
        "ocr" | "note" => {
            let text = req
                .text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("text is required for an ocr/note capture"))?;
            match kind {
                "receipt" => {
                    let source = if ctype == "ocr" { "ocr" } else { "note" };
                    receiving::append_note(db, instance_id, id, text, source)
                        .await
                        .map_err(bad)?;
                    Ok(format!("{} on receipt", if ctype == "ocr" { "OCR" } else { "Note" }))
                }
                // unit / order: v1 just logs the text as an event (done by the
                // capture-event write in `do_capture`). The summary carries it.
                _ => Ok(format!("{}: {}", if ctype == "ocr" { "OCR" } else { "Note" }, text)),
            }
        }
        other => Err(bad(format!("unknown capture type '{other}'"))),
    }
}

/// Barcode dispatch by entity kind (§8.3).
async fn capture_barcode(
    db: &SurrealDb,
    instance_id: &str,
    kind: &str,
    id: &str,
    barcode: &str,
    req: &CaptureRequest,
) -> ApiResult<String> {
    match kind {
        "receipt" => {
            // Only when the receipt's `units` step is the ACTIVE step do we add a
            // unit; otherwise the barcode is just logged (via the capture event).
            let p = plan::load_receiving_plan(db, &plan::PlanQuery::Receipt(id.to_string())).await;
            let units = p.steps.iter().find(|s| s.key == "units");
            let units_active = units.map(|s| s.state == "active").unwrap_or(false);
            if !units_active {
                return Ok(format!("Barcode {barcode} (units step not active — logged only)"));
            }
            // A scan equal to an expected serial is stored as the serial; any
            // other code is stored as the barcode — UNLESS it came off the OCR
            // review (§10), which has no barcode fallback: there was never a
            // barcode, only the OCR'd text, so it is always a serial.
            let matches_expected = units
                .map(|s| {
                    s.items.iter().any(|it| {
                        it.state == "expected"
                            && it
                                .serial
                                .as_deref()
                                .map(norm_serial)
                                .as_deref()
                                == Some(norm_serial(barcode).as_str())
                    })
                })
                .unwrap_or(false);
            let serial_source = receiving::normalize_serial_source(req.serial_source.as_deref());
            let as_serial = matches_expected || serial_source == "ocr";
            // The unit id is the capture_id → `add_unit` is itself idempotent.
            let unit = receiving::UnitInput {
                unit_id: req.capture_id.trim().to_string(),
                kind: Some("item".into()),
                barcode: if as_serial { None } else { Some(barcode.to_string()) },
                code: None,
                serial: if as_serial { Some(barcode.to_string()) } else { None },
                serial_source: if as_serial { Some(serial_source.to_string()) } else { None },
                note: None,
            };
            receiving::add_unit(db, instance_id, id, &unit)
                .await
                .map_err(bad)?;
            Ok(format!("Unit added from barcode {barcode}"))
        }
        "unit" => {
            // Set the serial only if the unit has none yet; otherwise log only.
            let row = load_record(db, "receipt_unit", id).await;
            let has_serial = row
                .as_ref()
                .and_then(|r| r.get("serial"))
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if has_serial {
                return Ok(format!("Barcode {barcode} (unit already has a serial — logged only)"));
            }
            let vclock = next_unit_vclock(db, id, instance_id).await;
            let serial_source = receiving::normalize_serial_source(req.serial_source.as_deref());
            let res = db
                .query(
                    "UPDATE type::record($rid) SET serial = $serial, serial_source = $serial_source, \
                        _vclock = $vclock, updated_at = time::now() RETURN NONE",
                )
                .bind(("rid", rid("receipt_unit", id)))
                .bind(("serial", json!(barcode)))
                .bind(("serial_source", json!(serial_source)))
                .bind(("vclock", vclock))
                .await
                .and_then(|r| r.check());
            if let Err(e) = res {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("set serial: {e}")));
            }
            // An OCR-sourced serial set here arms the async OCR-verify review
            // (§11), same as the receipt-SAVE / add_unit path.
            if serial_source == "ocr" {
                receiving::arm_ocr_verify_pending(db, instance_id, id).await;
            }
            Ok(format!("Serial set to {barcode}"))
        }
        // order: v1 logs the barcode only (the capture event carries it).
        _ => Ok(format!("Barcode {barcode} on order (logged only)")),
    }
}

// ─── Plan + summary for the response ─────────────────────────────────────────

/// The refreshed plan of the entity's subject (§8.3): the order plan for an
/// `order` or for a unit already in repair; otherwise the receiving plan of the
/// receipt the entity belongs to.
async fn plan_for_entity(db: &SurrealDb, kind: &str, id: &str) -> Value {
    match kind {
        "receipt" => {
            let p = plan::load_receiving_plan(db, &plan::PlanQuery::Receipt(id.to_string())).await;
            serde_json::to_value(p).unwrap_or(Value::Null)
        }
        "order" => plan::order_plan_value(db, id).await,
        "unit" => {
            let row = load_record(db, "receipt_unit", id).await;
            // A unit in repair (live `order_uuid`) shows its order plan; a unit
            // still awaiting triage shows the receiving plan of its receipt.
            if let Some(ouid) = row
                .as_ref()
                .and_then(|r| r.get("order_uuid"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let v = plan::order_plan_value(db, ouid).await;
                if !v.is_null() {
                    return v;
                }
            }
            if let Some(ruid) = row
                .as_ref()
                .and_then(|r| r.get("receipt_id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let p = plan::load_receiving_plan(db, &plan::PlanQuery::Receipt(ruid.to_string())).await;
                return serde_json::to_value(p).unwrap_or(Value::Null);
            }
            Value::Null
        }
        _ => Value::Null,
    }
}

/// `(title, status)` for the response's `entity` block, via the EntityView.
async fn entity_summary(db: &SurrealDb, kind: &str, id: &str) -> (String, Value) {
    match crate::services::entity::load(db, kind, id).await {
        Ok(Some(v)) => (v.title, v.status.map(Value::String).unwrap_or(Value::Null)),
        _ => (String::new(), Value::Null),
    }
}

// ─── Small DB helpers ─────────────────────────────────────────────────────────

fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

fn norm_serial(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase()
}

async fn record_exists(db: &SurrealDb, table: &str, id: &str) -> bool {
    let rows: Vec<Value> = db
        .query("SELECT id FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid(table, id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    !rows.is_empty()
}

async fn load_record(db: &SurrealDb, table: &str, id: &str) -> Option<Value> {
    db.query("SELECT record::id(id) AS id, * FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid(table, id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten()
}

async fn next_unit_vclock(db: &SurrealDb, key: &str, instance_id: &str) -> Value {
    let existing: Option<Value> = db
        .query("SELECT _vclock FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid("receipt_unit", key)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    eck_core::sync::conflict::next_local_vclock(
        existing.as_ref().and_then(|v| v.get("_vclock")),
        instance_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        db.query(
            "DEFINE TABLE order SCHEMALESS; DEFINE TABLE receipt SCHEMALESS; \
             DEFINE TABLE receipt_unit SCHEMALESS; DEFINE TABLE document SCHEMALESS; \
             DEFINE TABLE file_resource SCHEMALESS; DEFINE TABLE system_config SCHEMALESS; \
             DEFINE TABLE location SCHEMALESS; DEFINE TABLE event SCHEMALESS; \
             DEFINE TABLE scan_log SCHEMALESS; DEFINE TABLE repair_event SCHEMALESS; \
             DEFINE TABLE stock_picking_delivery SCHEMALESS; \
             DEFINE TABLE has_attachment TYPE RELATION SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    async fn mk_receipt(db: &SurrealDb, id: &str) {
        db.query("CREATE type::record('receipt', $id) SET client_name='ACME', intake='parcel', created_at=time::now()")
            .bind(("id", id.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap();
    }

    fn req(ctype: &str, capture_id: &str) -> CaptureRequest {
        CaptureRequest {
            ctype: ctype.into(),
            capture_id: capture_id.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn missing_capture_id_is_400() {
        let db = test_db().await;
        mk_receipt(&db, "r1").await;
        let err = do_capture(&db, "node", "receipt", "r1", "dimi", &req("note", "  "))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_entity_is_404() {
        let db = test_db().await;
        let err = do_capture(&db, "node", "receipt", "ghost", "dimi", &req("note", "c1"))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn photo_attaches_with_evidence_type_and_replay_is_idempotent() {
        let db = test_db().await;
        mk_receipt(&db, "r1").await;
        db.query("CREATE type::record('file_resource','f1') SET cas_uuid='f1', mime_type='image/jpeg'")
            .await
            .unwrap()
            .check()
            .unwrap();
        let mut r = req("photo", "cap-1");
        r.file_id = Some("f1".into());
        r.evidence_type = Some("box_photo".into());

        let out = do_capture(&db, "node", "receipt", "r1", "dimi", &r).await.unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["replayed"], json!(false));

        // The edge carries the evidence_type.
        let edges: Vec<Value> = db
            .query("SELECT evidence_type FROM has_attachment WHERE in = type::record('receipt','r1')")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["evidence_type"], json!("box_photo"));

        // Replay: same capture_id → replayed=true, NO second edge.
        let out = do_capture(&db, "node", "receipt", "r1", "dimi", &r).await.unwrap();
        assert_eq!(out["replayed"], json!(true));
        let edges: Vec<Value> = db
            .query("SELECT id FROM has_attachment WHERE in = type::record('receipt','r1')")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(edges.len(), 1, "replay must not create a second attachment");
    }

    #[tokio::test]
    async fn barcode_on_receipt_adds_exactly_one_unit() {
        let db = test_db().await;
        mk_receipt(&db, "r1").await;
        // Fresh parcel: postal/shipment/box not done, so the receiving plan's
        // first unsatisfied step is `postal`, NOT `units` — a barcode then only
        // logs. Drive the plan so `units` is the active step by marking the
        // earlier steps done (postal barcode + ticket link + a box photo).
        db.query(
            "UPDATE type::record('receipt','r1') SET postal_barcode='00340', ticket_id='t1'; \
             CREATE type::record('document','t1') SET type='support_ticket', meta={ ticket_number:'1' }; \
             CREATE type::record('file_resource','fbox') SET cas_uuid='fbox', mime_type='image/jpeg'; \
             RELATE (type::record('receipt','r1'))->has_attachment->(type::record('file_resource','fbox')) \
                SET created_at=time::now(), label='box';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let mut r = req("barcode", "cap-unit-1");
        r.barcode = Some("SN-NEW-1".into());
        let out = do_capture(&db, "node", "receipt", "r1", "dimi", &r).await.unwrap();
        assert_eq!(out["ok"], json!(true));

        let units: Vec<Value> = db
            .query("SELECT record::id(id) AS unit_id, barcode FROM receipt_unit WHERE receipt_id='r1'")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(units.len(), 1, "exactly one unit added");
        assert_eq!(units[0]["unit_id"], json!("cap-unit-1"));
        assert_eq!(units[0]["barcode"], json!("SN-NEW-1"));

        // Replay adds nothing.
        let out = do_capture(&db, "node", "receipt", "r1", "dimi", &r).await.unwrap();
        assert_eq!(out["replayed"], json!(true));
        let units: Vec<Value> = db
            .query("SELECT id FROM receipt_unit WHERE receipt_id='r1'")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(units.len(), 1, "replay must not add a second unit");
    }
}
