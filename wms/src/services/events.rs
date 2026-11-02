//! Minimal domain-event log (DOMAIN_MODEL Layer 5, `.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §3)
//! plus the timeline assembly that unions it with the legacy audit tables.
//!
//! `event` is a SCHEMALESS, mesh-synced table (in `SYNC_ENTITY_TYPES`): every
//! row carries `home_instance_id` + `_vclock` the house way so a PDA-side act
//! converges to the office master. Ids are random UUIDs and rows are never
//! updated (append-only), so — like `has_attachment` edges — there is no id
//! collision to resolve and the merkle bootstrap keys each row by its leaf id.
//!
//! Every writer is FIRE-AND-FORGET-SAFE: a failed event write only `warn!`s and
//! never fails the caller — the business act (the move, the status change) has
//! already committed and must not be undone because its journal entry didn't.

use eck_core::db::SurrealDb;
use serde::Serialize;
use serde_json::{json, Value};

/// One domain event to record. `kind` is a short verb
/// (`move|status|created|photo|scan|repair_event`); `entity_kind`/`entity_id`
/// name the record it belongs to; `ref_*` optionally points at a related record
/// (a move's destination place, a photo's file).
#[derive(Debug, Clone, Default)]
pub struct Event {
    pub kind: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub actor: Option<String>,
    pub text: String,
    pub data: Option<Value>,
    pub ref_kind: Option<String>,
    pub ref_id: Option<String>,
}

impl Event {
    /// Convenience constructor for the common `kind/entity/text` triple.
    pub fn new(kind: &str, entity_kind: &str, entity_id: &str, text: impl Into<String>) -> Self {
        Event {
            kind: kind.to_string(),
            entity_kind: entity_kind.to_string(),
            entity_id: entity_id.to_string(),
            text: text.into(),
            ..Default::default()
        }
    }
    pub fn actor(mut self, a: Option<String>) -> Self {
        self.actor = a.filter(|s| !s.trim().is_empty());
        self
    }
    pub fn reference(mut self, kind: &str, id: &str) -> Self {
        if !id.trim().is_empty() {
            self.ref_kind = Some(kind.to_string());
            self.ref_id = Some(id.to_string());
        }
        self
    }
}

/// Record id for `type::record($rid)` — backtick-quoted so all-digit keys stay
/// STRING ids (`type::thing` does not exist on SurrealDB 3.2). Same idiom as
/// `services::receiving` / `services::repair`.
fn rid(table: &str, key: &str) -> String {
    format!("{}:`{}`", table, key.replace('`', ""))
}

/// Append one event. Fire-and-forget-safe: logs and swallows every error.
pub async fn record(db: &SurrealDb, instance_id: &str, ev: Event) {
    let id = uuid::Uuid::new_v4().to_string();
    let vclock = eck_core::sync::conflict::next_local_vclock(None, instance_id);
    let res = db
        .query(
            "UPSERT type::record($rid) MERGE { \
                at: time::now(), kind: $kind, entity_kind: $ek, entity_id: $eid, \
                actor: $actor, text: $text, data: $data, \
                ref_kind: $rk, ref_id: $ri, \
                home_instance_id: $home, _vclock: $vclock \
             } RETURN NONE",
        )
        .bind(("rid", rid("event", &id)))
        .bind(("kind", json!(ev.kind)))
        .bind(("ek", json!(ev.entity_kind)))
        .bind(("eid", json!(ev.entity_id)))
        .bind(("actor", json!(ev.actor)))
        .bind(("text", json!(ev.text)))
        .bind(("data", ev.data.unwrap_or(Value::Null)))
        .bind(("rk", json!(ev.ref_kind)))
        .bind(("ri", json!(ev.ref_id)))
        .bind(("home", json!(instance_id)))
        .bind(("vclock", vclock))
        .await;
    match res {
        Ok(r) => {
            if let Err(e) = r.check() {
                tracing::warn!("[event] write failed (non-fatal): {}", e);
            }
        }
        Err(e) => tracing::warn!("[event] write failed (non-fatal): {}", e),
    }
}

/// Has a capture with this `capture_id` already been recorded for this entity?
/// The capture handler (`handlers::capture`) records every capture as an `event`
/// row referencing `{kind:"capture", id:<capture_id>}`, so a client retry can be
/// detected and made idempotent (no second attachment / unit / note). Fail-soft:
/// a query error reads as "not seen" (the capture then proceeds, which is safe —
/// the underlying writes are themselves idempotent by client-minted id).
pub async fn capture_seen(
    db: &SurrealDb,
    entity_kind: &str,
    entity_id: &str,
    capture_id: &str,
) -> bool {
    let rows: Vec<Value> = db
        .query(
            "SELECT id FROM event \
             WHERE entity_kind = $ek AND entity_id = $eid \
               AND ref_kind = 'capture' AND ref_id = $cid LIMIT 1",
        )
        .bind(("ek", json!(entity_kind)))
        .bind(("eid", json!(entity_id)))
        .bind(("cid", json!(capture_id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    !rows.is_empty()
}

// ─── Timeline (read path) ────────────────────────────────────────────────────

/// A related record pointer in a timeline entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EventRef {
    pub kind: String,
    pub id: String,
}

/// One row of the assembled timeline, in the exact shape of
/// `EntityView.events[]` (§2).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EventOut {
    pub at: String,
    pub kind: String,
    pub text: String,
    pub actor: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<EventRef>,
}

/// Max timeline length (§2).
const TIMELINE_CAP: usize = 200;

/// Sort key for an `at` string: SurrealDB serialises a `datetime` column to an
/// RFC3339 STRING over the SDK (same assumption as `services::repair`), and
/// legacy rows may carry a plain RFC3339 string — parse both, epoch on failure.
fn ts_key(at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// Sort newest-first and cap at [`TIMELINE_CAP`]. Pure → unit-tested.
pub fn merge_and_cap(mut rows: Vec<EventOut>) -> Vec<EventOut> {
    rows.sort_by(|a, b| ts_key(&b.at).cmp(&ts_key(&a.at)));
    rows.truncate(TIMELINE_CAP);
    rows
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Assemble the union timeline for one entity (§2): `event` rows + `scan_log`
/// (matched on the entity's codes/barcodes/serial) + `repair_event` (matched on
/// serial) + photo attachment edges + the record's own `created_at`. Each query
/// is fail-soft (a missing table on a fresh node yields no rows, never an error).
pub async fn timeline(
    db: &SurrealDb,
    entity_kind: &str,
    entity_id: &str,
    serial: Option<&str>,
    codes: &[String],
) -> Vec<EventOut> {
    let table = match crate::services::entity::table_for(entity_kind) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut out: Vec<EventOut> = Vec::new();

    // 1. Native `event` rows for this entity.
    let evs: Vec<Value> = db
        .query(
            "SELECT at, kind, text, actor, ref_kind, ref_id FROM event \
             WHERE entity_kind = $ek AND entity_id = $eid \
             ORDER BY at DESC LIMIT 200",
        )
        .bind(("ek", json!(entity_kind)))
        .bind(("eid", json!(entity_id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    for e in evs {
        let ref_ = match (s(&e, "ref_kind"), s(&e, "ref_id")) {
            (Some(k), Some(id)) => Some(EventRef { kind: k, id }),
            _ => None,
        };
        out.push(EventOut {
            at: s(&e, "at").unwrap_or_default(),
            kind: s(&e, "kind").unwrap_or_else(|| "event".into()),
            text: s(&e, "text").unwrap_or_default(),
            actor: s(&e, "actor"),
            ref_,
        });
    }

    // 2. Legacy scan_log rows matching any of the entity's codes/barcodes/serial.
    if !codes.is_empty() {
        let logs: Vec<Value> = db
            .query(
                "SELECT barcode, device_id, created_at FROM scan_log \
                 WHERE barcode IN $codes ORDER BY created_at DESC LIMIT 200",
            )
            .bind(("codes", json!(codes)))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        for l in logs {
            out.push(EventOut {
                at: s(&l, "created_at").unwrap_or_default(),
                kind: "scan".into(),
                text: format!("Scanned {}", s(&l, "barcode").unwrap_or_default()),
                actor: s(&l, "device_id"),
                ref_: None,
            });
        }
    }

    // 2b. CRM edit log for a `partner` (the offline PDA queue, `crm_update_log`),
    // keyed on the partner id (the `event` table carries native scans/notes; this
    // folds in the status/notes edits that never went through `event`).
    if entity_kind == "partner" {
        let logs: Vec<Value> = db
            .query(
                "SELECT entity_type, changes, device_id, created_at FROM crm_update_log \
                 WHERE entity_id = $eid ORDER BY created_at DESC LIMIT 200",
            )
            .bind(("eid", json!(entity_id)))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        for l in logs {
            let changes = l.get("changes");
            let status = changes.and_then(|c| c.get("status")).and_then(|v| v.as_str());
            let note = changes.and_then(|c| c.get("notes")).and_then(|v| v.as_str());
            let text = match (status, note) {
                (Some(st), Some(n)) if !st.is_empty() && !n.is_empty() => {
                    format!("Status → {st}; note: {n}")
                }
                (Some(st), _) if !st.is_empty() => format!("Status → {st}"),
                (_, Some(n)) if !n.is_empty() => format!("Note: {n}"),
                _ => "CRM update".into(),
            };
            out.push(EventOut {
                at: s(&l, "created_at").unwrap_or_default(),
                kind: "crm_update".into(),
                text,
                actor: s(&l, "device_id"),
                ref_: None,
            });
        }
    }

    // 3. Legacy repair_event rows keyed on the serial.
    if let Some(sn) = serial.map(str::trim).filter(|s| !s.is_empty()) {
        let res: Vec<Value> = db
            .query(
                "SELECT event_type, data, source_device_id, created_at FROM repair_event \
                 WHERE target_device_id = $sn ORDER BY created_at DESC LIMIT 200",
            )
            .bind(("sn", json!(sn)))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        for r in res {
            let et = s(&r, "event_type").unwrap_or_else(|| "repair_event".into());
            let data = s(&r, "data").unwrap_or_default();
            let text = if data.is_empty() {
                et.clone()
            } else {
                format!("{et}: {data}")
            };
            out.push(EventOut {
                at: s(&r, "created_at").unwrap_or_default(),
                kind: "repair_event".into(),
                text,
                actor: s(&r, "source_device_id"),
                ref_: None,
            });
        }
    }

    // 4. Photo attachment edges on the entity's own record.
    let photos: Vec<Value> = db
        .query(
            "SELECT record::id(out.id) AS file_id, label, created_at AS attached_at \
             FROM has_attachment WHERE in = type::record($tb, $id) \
             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(("tb", json!(table)))
        .bind(("id", json!(entity_id)))
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    for p in photos {
        if let Some(fid) = s(&p, "file_id") {
            let label = s(&p, "label").unwrap_or_else(|| "photo".into());
            out.push(EventOut {
                at: s(&p, "attached_at").unwrap_or_default(),
                kind: "photo".into(),
                text: format!("Photo attached ({label})"),
                actor: None,
                ref_: Some(EventRef {
                    kind: "file".into(),
                    id: fid,
                }),
            });
        }
    }

    // 5. The record's own creation.
    let own: Option<Value> = db
        .query("SELECT created_at FROM type::record($rid) LIMIT 1")
        .bind(("rid", rid(table, entity_id)))
        .await
        .and_then(|mut r| r.take(0))
        .ok()
        .flatten();
    if let Some(at) = own.as_ref().and_then(|v| s(v, "created_at")) {
        out.push(EventOut {
            at,
            kind: "created".into(),
            text: "Record created".into(),
            actor: None,
            ref_: None,
        });
    }

    merge_and_cap(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(at: &str, kind: &str) -> EventOut {
        EventOut {
            at: at.into(),
            kind: kind.into(),
            text: String::new(),
            actor: None,
            ref_: None,
        }
    }

    #[test]
    fn merge_sorts_newest_first() {
        let rows = vec![
            ev("2026-10-30T10:00:00Z", "a"),
            ev("2026-10-31T09:00:00Z", "b"),
            ev("2026-10-29T23:59:59Z", "c"),
        ];
        let sorted = merge_and_cap(rows);
        let kinds: Vec<&str> = sorted.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["b", "a", "c"]);
    }

    #[test]
    fn merge_caps_at_200() {
        let rows: Vec<EventOut> = (0..250)
            .map(|i| ev(&format!("2026-10-{:02}T00:00:00Z", (i % 28) + 1), "x"))
            .collect();
        assert_eq!(merge_and_cap(rows).len(), 200);
    }

    #[test]
    fn unparseable_timestamps_sink_to_epoch() {
        let rows = vec![ev("not-a-date", "bad"), ev("2026-01-01T00:00:00Z", "good")];
        let sorted = merge_and_cap(rows);
        assert_eq!(sorted[0].kind, "good");
        assert_eq!(sorted[1].kind, "bad");
    }

    async fn test_db() -> SurrealDb {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        db.query(
            "DEFINE TABLE event SCHEMALESS; DEFINE TABLE receipt_unit SCHEMALESS; \
             DEFINE TABLE order SCHEMALESS; DEFINE TABLE location SCHEMALESS; \
             DEFINE TABLE file_resource SCHEMALESS; DEFINE TABLE receipt SCHEMALESS; \
             DEFINE TABLE item SCHEMALESS; DEFINE TABLE product SCHEMALESS; \
             DEFINE TABLE has_attachment TYPE RELATION SCHEMALESS; \
             DEFINE TABLE scan_log SCHEMALESS; DEFINE TABLE repair_event SCHEMALESS;",
        )
        .await
        .unwrap()
        .check()
        .unwrap();
        db
    }

    /// `record` writes a row the `UPSERT type::record` way; `timeline` reads it
    /// back with its `ref` and sorts it into the union. Validates the §3 schema
    /// and the §2 timeline SurrealQL round-trip.
    #[tokio::test]
    async fn record_then_timeline_round_trip() {
        let db = test_db().await;
        record(
            &db,
            "node-a",
            Event::new("move", "unit", "u1", "Moved to Ausgang")
                .actor(Some("dimi".into()))
                .reference("place", "p1"),
        )
        .await;
        record(
            &db,
            "node-a",
            Event::new("status", "unit", "u1", "Repair add → in_repair"),
        )
        .await;
        // An event for a DIFFERENT unit must not leak into u1's timeline.
        record(&db, "node-a", Event::new("move", "unit", "u2", "elsewhere")).await;

        let t = timeline(&db, "unit", "u1", None, &[]).await;
        assert_eq!(t.len(), 2, "only u1's two events");
        let mv = t.iter().find(|e| e.kind == "move").expect("move present");
        assert_eq!(mv.actor.as_deref(), Some("dimi"));
        assert_eq!(
            mv.ref_.as_ref().map(|r| (r.kind.as_str(), r.id.as_str())),
            Some(("place", "p1"))
        );
    }
}
