use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use crate::services::support as svc;
use eck_core::auth::Claims;
use eck_core::utils::anonymizer::{obfuscate_pii, parse_named_addresses, pprl_reveal_map, scrub_pii_regex};
use tracing::{info, warn};

/// Roles permitted to see un-masked PII in ticket summaries. These are the
/// human staff who actually action tickets (look up the customer, ship the
/// repaired device). Everyone else — `observer`/kiosk, and any mesh/service
/// token — receives the summary exactly as stored: PPRL-tokenised. The reveal
/// is purely a presentation-layer decision; the stored data is always masked.
fn may_reveal_pii(claims: Option<&Claims>) -> bool {
    matches!(claims.map(|c| c.role.as_str()), Some("admin") | Some("operator"))
}

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct ImportTicketRequest {
    /// Zoho ticket ID (used as SurrealDB record key)
    pub ticket_id: String,
    /// Full Zoho ticket payload
    pub ticket: Value,
}

/// POST /api/support/import-ticket — import or update a Zoho Desk ticket.
pub async fn import_ticket(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ImportTicketRequest>,
) -> ApiResult<Json<Value>> {
    let result = svc::import_ticket(
        &state.db,
        &payload.ticket_id,
        &payload.ticket,
        &state.instance_id,
        state.plugins.as_ref(),
    )
    .await
    .map_err(db_err)?;
    // Single-shot import — no enrichment follows in this call, release the
    // doc to the summarization worker right away.
    let _ = svc::finalize_ticket_ingest(&state.db, &payload.ticket_id).await;
    Ok(Json(json!({ "changed": result.changed, "ticket_id": result.id })))
}

#[derive(Deserialize)]
pub struct ImportThreadRequest {
    /// Parent Zoho ticket ID (accepts both ticketId and ticket_id)
    #[serde(alias = "ticketId")]
    pub ticket_id: String,
    /// Array of Zoho thread objects (each must have an "id" field)
    pub threads: Vec<Value>,
    /// Optional: full ticket payload (for re-importing the ticket itself)
    #[serde(default)]
    pub ticket: Option<Value>,
}

/// POST /api/support/import-thread — bulk import Zoho Desk threads for a ticket.
/// Accepts `{ ticketId, threads: [...], ticket? }` from the frontend scraper UI.
/// On change, marks the PARENT TICKET for re-summarization.
pub async fn import_thread(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ImportThreadRequest>,
) -> ApiResult<Json<Value>> {
    let ticket_id = &payload.ticket_id;
    let mut imported = 0i64;
    let mut skipped = 0i64;
    let mut errors: Vec<String> = Vec::new();

    // Optionally re-import the parent ticket if provided
    if let Some(ref ticket) = payload.ticket {
        if let Err(e) =
            svc::import_ticket(&state.db, ticket_id, ticket, &state.instance_id, state.plugins.as_ref()).await
        {
            errors.push(format!("ticket {}: {}", ticket_id, e));
        }
    }

    for thread in &payload.threads {
        let thread_id = match thread.get("id") {
            Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
            Some(v) if v.is_number() => v.to_string(),
            _ => { skipped += 1; continue; }
        };

        match svc::import_thread(&state.db, &thread_id, ticket_id, thread, &state.instance_id).await {
            Ok(r) if r.changed => { imported += 1; }
            Ok(_) => { skipped += 1; }
            Err(e) => { errors.push(format!("thread {}: {}", thread_id, e)); }
        }
    }

    // The batch is this caller's whole delivery for the ticket — release it
    // to the summarization worker (the settle window still coalesces rapid
    // successive batches into one summary).
    let _ = svc::finalize_ticket_ingest(&state.db, ticket_id).await;

    Ok(Json(json!({ "imported": imported, "skipped": skipped, "errors": errors })))
}

/// POST /api/support/import-tickets — bulk import Zoho tickets.
/// If payload is empty, returns pending summary statuses from the DB.
pub async fn import_tickets(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let tickets = body.get("tickets").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // If no tickets provided, return pending summarization statuses
    if tickets.is_empty() {
        let pending: Vec<Value> = state.db
            .query("SELECT record::id(id) AS id, meta.subject AS subject, summary_status FROM document WHERE type = 'support_ticket' AND summary_status = 'pending' ORDER BY updated_at DESC LIMIT 200")
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();

        let mut statuses = serde_json::Map::new();
        for row in &pending {
            let id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            if !id.is_empty() {
                statuses.insert(id.to_string(), json!({
                    "subject": row.get("subject").unwrap_or(&json!(null)),
                    "summary_status": "pending",
                }));
            }
        }

        return Ok(Json(json!({ "imported": 0, "skipped": 0, "statuses": statuses })));
    }

    let mut imported = 0i64;
    let mut skipped = 0i64;
    let mut statuses = serde_json::Map::new();

    for ticket in &tickets {
        let ticket_id_owned = match ticket.get("id") {
            Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
            Some(v) if v.is_number() => v.to_string(),
            _ => { skipped += 1; continue; }
        };

        match svc::import_ticket(
            &state.db,
            &ticket_id_owned,
            ticket,
            &state.instance_id,
            state.plugins.as_ref(),
        )
        .await
        {
            Ok(r) => {
                // Bulk list import is single-shot per ticket — release to the
                // summarization worker immediately.
                let _ = svc::finalize_ticket_ingest(&state.db, &ticket_id_owned).await;
                statuses.insert(ticket_id_owned, json!({ "changed": r.changed, "id": r.id }));
                if r.changed { imported += 1; } else { skipped += 1; }
            }
            Err(e) => {
                statuses.insert(ticket_id_owned, json!({ "error": e.to_string() }));
                skipped += 1;
            }
        }
    }

    Ok(Json(json!({ "imported": imported, "skipped": skipped, "statuses": statuses })))
}

/// GET /api/support/debug/:ticket_id — diagnostic: dumps the raw payload keys
/// and candidate address fields. Used to decide which fields to promote into
/// meta. Safe to remove once promotion is stable.
pub async fn debug_ticket(
    State(state): State<Arc<AppState>>,
    Path(ticket_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let row: Option<Value> = state.db
        .query("SELECT payload FROM document_raw WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", ticket_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let payload = row.as_ref().and_then(|v| v.get("payload")).cloned().unwrap_or(json!({}));
    let top_keys: Vec<String> = payload.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let contact = payload.get("contact").cloned().unwrap_or(json!({}));
    let contact_keys: Vec<String> = contact.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let custom_fields = payload.get("customFields").cloned().unwrap_or(json!({}));
    let cf_keys: Vec<String> = custom_fields.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let cf_alt = payload.get("cf").cloned().unwrap_or(json!({}));
    let cf_alt_keys: Vec<String> = cf_alt.as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    Ok(Json(json!({
        "ticket_id": ticket_id,
        "top_keys": top_keys,
        "contact": contact,
        "contact_keys": contact_keys,
        "customFields_keys": cf_keys,
        "customFields_sample": custom_fields,
        "cf_keys": cf_alt_keys,
        "cf_sample": cf_alt,
    })))
}

/// POST /api/support/backfill-assignees — one-shot backfill of meta.assignee_*
/// from the full Zoho payload stored in document_raw.payload.assignee.
/// Used to populate existing tickets that were imported before assignee
/// extraction was added.
pub async fn backfill_assignees(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let raws: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, payload.assignee AS assignee FROM document_raw WHERE type = 'support_ticket'")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let mut updated = 0i64;
    let mut skipped = 0i64;

    for row in &raws {
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => { skipped += 1; continue; }
        };
        let assignee = row.get("assignee").cloned().unwrap_or(json!(null));

        let (a_id, a_name) = if assignee.is_object() {
            let first = assignee.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
            let last = assignee.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
            let mut name = format!("{first} {last}").trim().to_string();
            if name.is_empty() {
                name = assignee.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            }
            let aid = assignee.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            (aid, name)
        } else {
            (String::new(), String::new())
        };

        let rid = format!("document:`{}`", id);
        let res = state.db
            .query("UPDATE type::record($rid) SET meta.assignee_id = $aid, meta.assignee_name = $aname")
            .bind(("rid", rid.clone()))
            .bind(("aid", a_id))
            .bind(("aname", a_name))
            .await;

        if res.is_ok() {
            // meta is a synced/hashed field → advance this node's vclock so the
            // change propagates to peers instead of being dropped as local-wins.
            let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &rid, &state.instance_id).await;
            updated += 1;
        } else { skipped += 1; }
    }

    Ok(Json(json!({ "updated": updated, "skipped": skipped, "total": raws.len() })))
}

/// POST /api/support/restamp-thread-hashes — one-shot: recompute every stored
/// support_thread's `source_hash` from its `document_raw` payload using the
/// CURRENT hash algorithm. Run after any change to the thread hash seed
/// (2026-07-25: `stable_seed_hash` started stripping Zoho's re-signed
/// `et`/`ha` image-URL params) — otherwise the algorithm switch itself makes
/// every thread look edited on its next fetch, re-arming the parent's summary
/// once per ticket as Zoho traffic trickles in (a slow-motion Gemini wave).
///
/// Status-neutral: touches ONLY source_hash on the thread shell. No vclock
/// bump — the hash is change-detection state for THIS node's importer;
/// content did not change, so peers must not re-adopt anything.
/// No scraper needed. Idempotent.
/// Detached like force-sync?full: ~9k rows take way past any sane HTTP
/// timeout, and axum cancels the handler the moment the client hangs up — two
/// runs died exactly that way on 2026-07-25 (600 s and 3600 s curls, both
/// cancelled mid-walk). Returns immediately; progress + the final counts go to
/// the log. Addressing is `type::record('document', $tid)` (point write), NOT
/// `WHERE record::id(id) = $tid` — the function-over-id predicate scans the
/// whole 19k-row table per UPDATE, which is what made the inline version take
/// >1 h in the first place.
pub async fn restamp_thread_hashes(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let db = state.db.clone();
    tokio::spawn(async move {
        let raws: Vec<Value> = match db
            .query("SELECT record::id(id) AS id, payload FROM document_raw WHERE type = 'support_thread'")
            .await
            .and_then(|mut r| r.take(0))
        {
            Ok(r) => r,
            Err(e) => {
                warn!("[restamp-threads] payload scan failed: {e}");
                return;
            }
        };
        let total = raws.len();
        let (mut done, mut failed) = (0usize, 0usize);
        for row in &raws {
            let (Some(tid), Some(payload)) = (
                row.get("id").and_then(|v| v.as_str()),
                row.get("payload"),
            ) else {
                failed += 1;
                continue;
            };
            let new_hash = svc::thread_source_hash(payload);
            let res = db
                .query(
                    "UPDATE type::record('document', $tid) SET source_hash = $hash \
                     WHERE type = 'support_thread' AND source_hash != $hash",
                )
                .bind(("hash", new_hash))
                .bind(("tid", tid.to_string()))
                .await;
            match res {
                Ok(_) => done += 1,
                Err(_) => failed += 1,
            }
            if done % 1000 == 0 && done > 0 {
                info!("[restamp-threads] {done}/{total} restamped");
            }
        }
        info!("[restamp-threads] DONE: {done}/{total} restamped, {failed} failed");
    });
    Ok(Json(json!({
        "ok": true,
        "mode": "background",
        "note": "restamp running detached — watch wms.log for [restamp-threads] DONE",
    })))
}

/// POST /api/support/backfill-meta — re-run metadata extraction against the
/// payloads already stored in `document_raw`. Use this after changing the
/// extraction logic in `services::support::extract_ticket_metadata` (e.g.
/// tightening custom-field matching or adding an address parser) so existing
/// tickets pick up the new rules without round-tripping to Zoho.
///
/// No scraper needed. Detached like restamp-thread-hashes (NER per ticket ×
/// ~1.8k rows outlives any HTTP timeout, and a client hang-up cancels an
/// inline handler mid-walk); returns immediately, counts land in the log.
pub async fn backfill_meta(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let db = state.db.clone();
    let instance_id = state.instance_id.clone();
    tokio::spawn(async move {
        let raws: Vec<Value> = match db
            .query("SELECT record::id(id) AS id, payload FROM document_raw WHERE type = 'support_ticket'")
            .await
            .and_then(|mut r| r.take(0))
        {
            Ok(r) => r,
            Err(e) => {
                warn!("[backfill-meta] payload scan failed: {e}");
                return;
            }
        };
        let total = raws.len();
        let mut updated = 0i64;
        let mut skipped = 0i64;

        for row in &raws {
            let ticket_id = match row.get("id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => { skipped += 1; continue; }
            };
            let payload = match row.get("payload") {
                Some(p) => p,
                None => { skipped += 1; continue; }
            };

            let ner = crate::services::support::subject_ner(payload).await;
            let mut meta = crate::services::support::extract_ticket_metadata_with_ner(payload, ner.as_ref());

            let rid = format!("document:`{}`", ticket_id);

            // Extraction owns exactly the keys it emits; every OTHER key on the
            // stored meta is an ENRICHMENT written after import (meta.geo* from
            // the geocoder/manual grounding, meta.last_outbound_at from the
            // outbound-time pass, …) and must survive the rewrite. The
            // 2026-07-31 run replaced meta wholesale and wiped geo on all 1814
            // tickets — the sweep then re-bought hours of Nominatim lookups and
            // manual geo_override pins were lost for good.
            let old_meta: Option<Value> = db
                .query("SELECT VALUE meta FROM ONLY type::record($rid)")
                .bind(("rid", rid.clone()))
                .await
                .ok()
                .and_then(|mut r| r.take(0).ok())
                .flatten();
            if let (Some(new_obj), Some(old_obj)) = (
                meta.as_object_mut(),
                old_meta.as_ref().and_then(|v| v.as_object()),
            ) {
                for (k, v) in old_obj {
                    if !new_obj.contains_key(k) {
                        new_obj.insert(k.clone(), v.clone());
                    }
                }
            }

            // Restamp the summary-seed hash alongside the meta: extraction-rule
            // changes (e.g. subject PII masking) shift the seed for EVERY ticket,
            // and without the restamp the next incremental import would re-arm a
            // Gemini summary per ticket for zero content change (the 2026-07-17
            // burn pattern). summary_source_hash is merkle-IGNORED local state, so
            // writing it here does not perturb sync.
            let shash = crate::services::support::summary_seed_hash(&meta);

            // pii_fingerprints is DERIVED from meta.subject (among others) — a
            // rewritten meta invalidates it. NONE re-enters the row into the
            // fingerprint self-heal sweep, which re-derives from the new values.
            let res = db
                .query("UPDATE type::record($rid) SET meta = $meta, summary_source_hash = $shash, pii_fingerprints = NONE")
                .bind(("rid", rid.clone()))
                .bind(("meta", meta))
                .bind(("shash", shash))
                .await;

            if res.is_ok() {
                // meta is a synced/hashed field → bump this node's vclock so the
                // re-extracted meta propagates to peers (kiosk). This also makes a
                // one-shot re-run the reconvergence tool for already-diverged docs.
                let _ = eck_core::sync::conflict::bump_local_vclock(&db, &rid, &instance_id).await;
                updated += 1;
            } else { skipped += 1; }
            if updated % 500 == 0 && updated > 0 {
                info!("[backfill-meta] {updated}/{total} rewritten");
            }
        }
        info!("[backfill-meta] DONE: {updated}/{total} rewritten, {skipped} skipped");
    });
    Ok(Json(json!({
        "ok": true,
        "mode": "background",
        "note": "backfill running detached — watch wms.log for [backfill-meta] DONE",
    })))
}

/// POST /api/support/restamp-vclocks — one-shot: stamp a `_vclock` on synced
/// support documents that have NONE. Such docs (imported before vclock
/// propagation existed, or skipped by `backfill-meta` for lacking a
/// `document_raw` to re-extract from) have a null/empty clock, so
/// conflict::resolve treats them as equal to a peer's copy and never propagates
/// the local (authoritative) version — they stay divergent forever. Stamping
/// `{ this_node: 1 }` makes them causally After an empty-clock peer so the next
/// sync adopts them. Targets ONLY null/empty clocks so already-stamped docs are
/// left alone (re-bumping them would needlessly re-sync the whole set).
pub async fn restamp_vclocks(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let cond = "(_vclock IS NONE OR _vclock = {}) \
                AND type IN ['support_ticket', 'support_thread']";

    let pre: Vec<Value> = state.db
        .query(format!("SELECT count() AS c FROM document WHERE {} GROUP ALL", cond))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let n = pre
        .first()
        .and_then(|v| v.get("c"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if n > 0 {
        let mut vc = serde_json::Map::new();
        vc.insert(state.instance_id.clone(), json!(1));
        state.db
            .query(format!("UPDATE document SET _vclock = $vc WHERE {} RETURN NONE", cond))
            .bind(("vc", Value::Object(vc)))
            .await
            .map_err(db_err)?;
    }

    Ok(Json(json!({ "restamped": n })))
}

/// POST /api/support/backfill-summary-sync — one-shot after the 2026-07-09
/// merkle change that made `ai_summary` part of the content hash: advance the
/// `_vclock` of every locally-summarized document so the summaries written
/// while `ai_summary` was hash-IGNORED (and therefore never propagated)
/// causally dominate the peers' summary-less copies. Without the bump the
/// clocks compare Equal/Concurrent and LWW would pick the peer's row — whose
/// 'skipped' write often carries a NEWER updated_at — silently DELETING the
/// summary on the author node. Run ONCE on the raw-holding/author node (the
/// scraper node); do NOT run on peers.
pub async fn backfill_summary_sync(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let ids: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id FROM document \
                WHERE ai_summary != NONE AND ai_summary != ''")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let mut bumped = 0usize;
    for row in &ids {
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        match eck_core::sync::conflict::bump_local_vclock_by_leaf(
            &state.db, "document", id, &state.instance_id,
        ).await {
            Ok(()) => bumped += 1,
            Err(e) => tracing::warn!("backfill-summary-sync: bump failed for {}: {}", id, e),
        }
    }

    Ok(Json(json!({ "documents_with_summary": ids.len(), "vclocks_bumped": bumped })))
}

/// Tables carrying a mesh-synced `embedding` vector (mirror of the embed
/// worker's table list in `ai::embeddings`).
const EMBED_TABLES: &[&str] = &["document", "order", "partner", "product", "picking"];

/// POST /api/support/backfill-embedding-sync — one-shot after the 2026-07-12
/// merkle change that made `embedding` part of the content hash: advance the
/// `_vclock` of every locally-embedded row so vectors written while
/// `embedding` was hash-IGNORED (and therefore never propagated) causally
/// dominate the peers' vector-less copies. Run ONCE, on the node holding the
/// richest vector set (the long-running scraper/studio node); do NOT run on
/// peers — concurrent bumps on both sides recreate the very divergence this
/// is fixing.
pub async fn backfill_embedding_sync(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let mut report = serde_json::Map::new();
    for table in EMBED_TABLES {
        let ids: Vec<Value> = state.db
            .query(&format!(
                "SELECT record::id(id) AS id FROM {table} WHERE embedding IS NOT NONE"
            ))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(db_err)?;

        let mut bumped = 0usize;
        for row in &ids {
            let id = match row.get("id").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            match eck_core::sync::conflict::bump_local_vclock_by_leaf(
                &state.db, table, id, &state.instance_id,
            ).await {
                Ok(()) => bumped += 1,
                Err(e) => tracing::warn!("backfill-embedding-sync: bump failed for {table}:{id}: {e}"),
            }
        }
        report.insert(
            table.to_string(),
            json!({ "rows_with_embedding": ids.len(), "vclocks_bumped": bumped }),
        );
    }
    Ok(Json(Value::Object(report)))
}

/// POST /api/support/requeue-embeddings — revive rows parked in a terminal
/// embedding state so the (now working) embed backend picks them up again.
/// Default statuses: 'unavailable' (parked when the authority had no
/// embedding backend) and 'failed' (Observer-mitigated). Optional body:
/// `{ "statuses": ["unavailable", ...] }`. Retry counters reset so the
/// exponential backoff starts fresh. Rows that already hold a vector are
/// excluded — vector present means done (the worker's `embedding IS NONE`
/// filter would skip them anyway).
pub async fn requeue_embeddings(
    State(state): State<Arc<AppState>>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let statuses: Vec<String> = body
        .as_ref()
        .and_then(|Json(v)| v.get("statuses"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec!["unavailable".into(), "failed".into()]);

    let mut report = serde_json::Map::new();
    for table in EMBED_TABLES {
        let requeued: Vec<Value> = state.db
            .query(&format!(
                "UPDATE {table} SET \
                    embedding_status = 'pending', \
                    embedding_retries = 0, \
                    embedding_error = NONE \
                 WHERE embedding_status IN $statuses AND embedding IS NONE \
                 RETURN record::id(id) AS id"
            ))
            .bind(("statuses", statuses.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(db_err)?;
        report.insert(table.to_string(), json!({ "requeued": requeued.len() }));
    }
    Ok(Json(Value::Object(report)))
}

/// POST /api/support/claim-home — make THIS node the home/authority for the
/// given synced entity types and stamp its version so peers adopt it. Ownership
/// rule (c): creator is authority (`home_instance_id`), transferable — our case
/// is the KIOSK is home for its devices/trips. Body:
/// `{ "entity_types": ["registered_device","trip"] }`. Sets home_instance_id =
/// self, refreshes _vclock + updated_at so this node's row wins conflict
/// resolution (concurrent clocks → LWW-newest). For `registered_device` it SKIPS
/// node-self rows (a node homes itself, so `home_instance_id == device_id`) and
/// re-homes only operated devices — format-independent (works for legacy 16-hex
/// ANDROID_IDs and future UUIDs alike).
pub async fn claim_home(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    const ALLOWED: &[&str] = &["registered_device", "trip", "file_resource"];
    let types: Vec<String> = body
        .get("entity_types")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let _now = chrono::Utc::now().to_rfc3339();
    let mut claimed = serde_json::Map::new();
    for et in &types {
        if !ALLOWED.contains(&et.as_str()) {
            continue;
        }
        // Operated devices only — never re-home a node-self row (a node homes
        // itself, so home_instance_id == device_id). Format-independent.
        let filter = if et == "registered_device" {
            "WHERE home_instance_id != device_id"
        } else {
            ""
        };
        let ids: Vec<Value> = state.db
            .query(format!("SELECT VALUE record::id(id) FROM {} {}", et, filter))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(db_err)?;

        let mut n = 0i64;
        for idv in &ids {
            let id = match idv.as_str() {
                Some(s) => s,
                None => continue,
            };
            let rid = format!("{}:`{}`", et, id);
            // Read-increment THIS node's vclock component (preserving peer
            // components) so the claim dominates / is LWW-newest vs a peer that
            // already merged an earlier version — a flat {self:1} would lose to
            // a peer clock that carries extra components.
            let cur: Vec<Value> = state.db
                .query("SELECT VALUE _vclock FROM type::record($rid) LIMIT 1")
                .bind(("rid", rid.clone()))
                .await
                .and_then(|mut r| r.take(0))
                .map_err(db_err)?;
            let next = eck_core::sync::conflict::next_local_vclock(
                cur.into_iter().next().as_ref(),
                &state.instance_id,
            );
            state.db
                .query("UPDATE type::record($rid) SET home_instance_id = $home_id, _vclock = $vc, updated_at = time::now()")
                .bind(("rid", rid))
                .bind(("home_id", state.instance_id.clone()))
                .bind(("vc", next))
                .await
                .map_err(db_err)?;
            n += 1;
        }
        claimed.insert(et.clone(), json!(n));
    }

    Ok(Json(json!({ "home": state.instance_id, "claimed": claimed })))
}

/// POST /api/support/backfill-outbound-times — one-shot backfill for
/// `meta.last_outbound_at` on tickets. Reads every local `document_raw` thread,
/// finds the latest `createdTime` where `direction == 'out'` per ticket, and
/// writes the result back into the synced `document.meta.last_outbound_at`.
/// Idempotent: only forward-monotonic writes.
pub async fn backfill_outbound_times(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    // Projection-only query — pull two scalar fields instead of the full
    // payload (which can carry base64 attachments). Filter direction in the
    // engine so we hand back only outbound rows.
    let threads: Vec<Value> = state.db
        .query("SELECT ticket_id, payload.createdTime AS createdTime FROM document_raw \
                WHERE type = 'support_thread' AND payload.direction = 'out'")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let mut latest: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let scanned = threads.len() as i64;
    let mut outbound = 0i64;

    for row in &threads {
        let ticket_id = match row.get("ticket_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let created = match row.get("createdTime").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        outbound += 1;
        latest
            .entry(ticket_id)
            .and_modify(|cur| { if created > *cur { *cur = created.clone(); } })
            .or_insert(created);
    }

    let mut updated = 0i64;
    for (ticket_id, t) in &latest {
        let doc_rid = format!("document:`{}`", ticket_id);
        // Pre-read so no-op tickets are skipped entirely: re-runs must not
        // touch the record or its vclock.
        let cur: Vec<Value> = state.db
            .query(
                "SELECT VALUE meta.last_outbound_at FROM type::record($doc_rid) \
                 WHERE type = 'support_ticket'",
            )
            .bind(("doc_rid", doc_rid.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();
        match cur.first() {
            None => continue, // thread without a ticket document
            Some(v) => {
                if let Some(existing) = v.as_str() {
                    if !existing.is_empty() && existing >= t.as_str() { continue; }
                }
            }
        }
        // `WHERE record::id(id) = $tid` cannot use the primary key — it scans
        // the whole `document` table once per ticket (hours at ~1.5k tickets).
        // Target the record directly; the monotonic guard lives inside SET
        // because v3 evaluates SET eagerly, before WHERE filtering.
        let res = state.db
            .query(
                "UPDATE type::record($doc_rid) SET meta.last_outbound_at = \
                 (IF meta.last_outbound_at IS NONE OR meta.last_outbound_at = '' \
                  OR meta.last_outbound_at < $t THEN $t ELSE meta.last_outbound_at END) \
                 WHERE type = 'support_ticket';"
            )
            .bind(("doc_rid", doc_rid.clone()))
            .bind(("t", t.clone()))
            .await;
        if res.is_ok() {
            updated += 1;
            // A direct UPDATE bypasses resolve_and_upsert, leaving the new
            // content at an EQUAL clock — every peer sweep then re-fights it
            // as an LWW tie (2026-08-02: nightly conflict churn, and ties
            // that went remote wiped the backfill on ~200 tickets).
            let _ = eck_core::sync::conflict::bump_local_vclock(
                &state.db,
                &doc_rid,
                &state.instance_id,
            )
            .await;
        }
    }

    Ok(Json(json!({
        "threads_scanned": scanned,
        "outbound_threads": outbound,
        "tickets_with_outbound": latest.len(),
        "tickets_updated": updated,
    })))
}

/// POST /api/support/backfill-customfields — one-shot backfill for tickets
/// whose `document_raw.payload.customFields` is empty/missing.
///
/// The Zoho list endpoint (`/tickets?include=contacts,assignee,departments`)
/// does NOT return `customFields`; only the detail endpoint (`/tickets/{id}`)
/// does. Legacy tickets imported before the scheduler's detail-enrichment logic
/// was in place have `customFields: {}` and therefore no promoted address/
/// city/zip in `meta`. This handler iterates those tickets, fetches each one's
/// detail via the scraper (`POST /api/zoho/ticket-threads`), and re-imports
/// with the enriched payload so `meta.address/city/zip` get populated.
///
/// Optional body: `{ "limit": N }` — cap batch size (defaults to all).
pub async fn backfill_customfields(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let scraper_base = crate::handlers::scraper_proxy::scraper_base();

    let limit = body
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let limit_clause = if limit > 0 { format!("LIMIT {}", limit) } else { String::new() };
    let sql = format!(
        "SELECT record::id(id) AS id FROM document_raw \
         WHERE type = 'support_ticket' \
         AND (payload.customFields IS NONE OR payload.customFields = {{}}) \
         {}",
        limit_clause
    );

    let raws: Vec<Value> = state.db
        .query(sql)
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let total = raws.len();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(db_err)?;

    let mut enriched = 0i64;
    let mut skipped = 0i64;
    let mut errors: Vec<String> = Vec::new();

    for row in &raws {
        let ticket_id = match row.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => { skipped += 1; continue; }
        };

        let res = client
            .post(format!("{}/api/zoho/ticket-threads", scraper_base))
            .json(&json!({ "ticketId": ticket_id, "_from_env": true }))
            .send()
            .await;

        let resp = match res {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                errors.push(format!("ticket {}: scraper status {}", ticket_id, r.status()));
                skipped += 1;
                continue;
            }
            Err(e) => {
                errors.push(format!("ticket {}: scraper unreachable: {}", ticket_id, e));
                skipped += 1;
                continue;
            }
        };

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("ticket {}: bad JSON: {}", ticket_id, e));
                skipped += 1;
                continue;
            }
        };

        let enriched_ticket = match body.get("ticket") {
            Some(t) if t.get("cf").is_some() || t.get("customFields").is_some() => t,
            _ => {
                skipped += 1;
                continue;
            }
        };

        match svc::import_ticket(
            &state.db,
            &ticket_id,
            enriched_ticket,
            &state.instance_id,
            state.plugins.as_ref(),
        )
        .await
        {
            Ok(_) => {
                let _ = svc::finalize_ticket_ingest(&state.db, &ticket_id).await;
                enriched += 1;
            }
            Err(e) => {
                errors.push(format!("ticket {}: import failed: {}", ticket_id, e));
                skipped += 1;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    Ok(Json(json!({
        "total_candidates": total,
        "enriched": enriched,
        "skipped": skipped,
        "errors_count": errors.len(),
        "errors_sample": errors.iter().take(10).collect::<Vec<_>>(),
    })))
}

/// GET /api/support/tickets — list all imported support tickets.
/// Reads promoted metadata from `document.meta` (no payload needed).
///
/// `meta.subject` is stored MASKED (the distiller tokenises the contact's
/// name/email/phone — see `services::support::mask_subject`), so for staff
/// (admin/operator) the subject is un-masked at display time from the clear
/// identity fields that live right next to it in `meta`. Free-text scrub
/// tokens (a stray third-party email in a subject line) stay masked in the
/// list — the detail view's full reveal restores those from the raw payload.
pub async fn list_tickets(
    State(state): State<Arc<AppState>>,
    claims: Option<Extension<Claims>>,
) -> ApiResult<Json<Vec<Value>>> {
    let reveal = may_reveal_pii(claims.as_ref().map(|Extension(c)| c));
    let rows: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, status, meta, summary_status, updated_at FROM document WHERE type = 'support_ticket' ORDER BY updated_at DESC")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    // Count threads per ticket
    let thread_counts: Vec<Value> = state.db
        .query("SELECT ticket_id, count() AS cnt FROM document WHERE type = 'support_thread' GROUP BY ticket_id")
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();

    let thread_map: std::collections::HashMap<String, i64> = thread_counts
        .iter()
        .filter_map(|v| {
            let tid = v.get("ticket_id")?.as_str()?;
            let cnt = v.get("cnt")?.as_i64()?;
            Some((tid.to_string(), cnt))
        })
        .collect();

    let summaries: Vec<Value> = rows.iter().map(|row| {
        let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let m = row.get("meta").cloned().unwrap_or(json!({}));

        let mut subject = m["subject"].as_str().unwrap_or("(no subject)").to_string();
        if reveal && subject.contains('_') {
            // Same value set the distiller masked with: full customer name,
            // its parts, email, phone. Plain replace, no reveal sentinels —
            // the list UI renders subjects verbatim.
            let customer = m["customer"].as_str().unwrap_or("");
            let mut pairs: Vec<(&str, String)> = vec![("Name", customer.to_string())];
            for part in customer.split_whitespace() {
                if part.len() > 2 {
                    pairs.push(("Name", part.to_string()));
                }
            }
            pairs.push(("Email", m["email"].as_str().unwrap_or("").to_string()));
            pairs.push(("Phone", m["phone"].as_str().unwrap_or("").to_string()));
            for (label, value) in &pairs {
                let v = value.trim();
                if v.len() < 2 {
                    continue;
                }
                let token = obfuscate_pii(v, label);
                if subject.contains(&token) {
                    subject = subject.replace(&token, v);
                }
            }
        }

        json!({
            "ticket_id": id,
            "ticket_number": m["ticket_number"].as_str().unwrap_or(""),
            "subject": subject,
            "status": m["status"].as_str().or_else(|| row.get("status").and_then(|v| v.as_str())).unwrap_or("unknown"),
            "customer": m["customer"].as_str().unwrap_or(""),
            "email": m["email"].as_str().unwrap_or(""),
            "phone": m["phone"].as_str().unwrap_or(""),
            "company": m["company"].as_str().unwrap_or(""),
            "address": m["address"].as_str().unwrap_or(""),
            "city": m["city"].as_str().unwrap_or(""),
            "zip": m["zip"].as_str().unwrap_or(""),
            "geo": m.get("geo").cloned().unwrap_or(serde_json::Value::Null),
            "device_model": m["device_model"].as_str().unwrap_or(""),
            "serial_number": m["serial_number"].as_str().unwrap_or(""),
            "manufacturing_date": m["manufacturing_date"].as_str().unwrap_or(""),
            "thread_count": thread_map.get(id).copied().unwrap_or(0),
            "latest_update": m["created_time"].as_str().or_else(|| row.get("updated_at").and_then(|v| v.as_str())).unwrap_or(""),
            "last_outbound_at": m["last_outbound_at"].as_str().unwrap_or(""),
            "assignee_id": m["assignee_id"].as_str().unwrap_or(""),
            "assignee_name": m["assignee_name"].as_str().unwrap_or(""),
        })
    }).collect();

    Ok(Json(summaries))
}

/// GET /api/support/tickets/:ticket_id/threads — get threads for a ticket.
/// Reads heavy payloads from local `document_raw`.
/// If `document_raw` is missing (thin node), attempts P2P fetch from source instance.
#[derive(Deserialize)]
pub struct GetThreadsQuery {
    /// When true, skip heavy thread `content` / `plainText` / attachments fields
    /// AND skip the mesh-fetch task that pulls full payloads from the source
    /// node. Lets the dashboard open a ticket instantly with summary + header
    /// list, deferring the body fetch until the operator actually expands a
    /// thread (via `/threads/:thread_id/payload`).
    #[serde(default)]
    pub meta_only: bool,
    /// Optional target language (ISO 639-1, e.g. `ko`). When present, the AI
    /// summary is run through the translation resolver: a fresh translation is
    /// served if one exists, otherwise the original masked summary is returned
    /// with `summary_pending: true` and a background job is enqueued. Empty or
    /// the source language (`en`) is a no-op.
    #[serde(default)]
    pub lang: Option<String>,
}

/// Sentinel chars (Unicode Private Use Area) wrapping each value the reveal
/// substituted back into a summary. The frontend turns them into a yellow
/// highlight so staff can see at a glance which PII was restored locally (and,
/// by elimination, which `Name_…`/`Email_…` tokens could NOT be restored). They
/// are stripped from the clean `ai_summary` so copy/RMA/Repair prefill stay
/// plaintext-only.
const PII_OPEN: char = '\u{E000}';
const PII_CLOSE: char = '\u{E001}';

/// Replace every occurrence of a deterministic PPRL `token` with its plaintext
/// `value`, wrapping the value in reveal sentinels. No-op if the token isn't
/// present (so it never double-wraps an already-revealed value).
fn reveal_replace(text: &mut String, token: &str, value: &str) {
    if token.is_empty() || value.is_empty() || !text.contains(token) {
        return;
    }
    let wrapped = format!("{}{}{}", PII_OPEN, value, PII_CLOSE);
    *text = text.replace(token, &wrapped);
}

/// Drop reveal sentinels, yielding the clean plaintext summary.
fn strip_pii_markers(s: &str) -> String {
    s.chars().filter(|&c| c != PII_OPEN && c != PII_CLOSE).collect()
}

/// Re-derive the original PII for a stored (tokenised) summary and swap the
/// deterministic SimHash tokens back, wrapping each restored value in reveal
/// sentinels. Symmetric with the masker in `ai::summarization::build_ticket_text`
/// — same fields, same `obfuscate_pii`, no plaintext vault. `ticket_payload`
/// must be the FULL raw ticket payload and `threads` the FULL thread payloads
/// (callers fetch these explicitly so the reveal works even when the response is
/// served in `meta_only` mode, which strips thread payloads). `meta` carries the
/// flat `document.meta` fields that survive even when no raw payload is present.
fn reveal_summary(masked: &str, ticket_payload: &Value, threads: &[Value], meta: &Value) -> String {
    let mut out = masked.to_string();
    for (token, value) in derive_reveal_pairs(ticket_payload, threads, meta) {
        reveal_replace(&mut out, &token, &value);
    }
    out
}

/// Re-derive every `token → clear value` pair this ticket's data can produce:
/// structured contact/company/cf fields, flat `meta` fields, thread participants,
/// and the regex-backstop matches over the raw bodies. This is the ONLY reveal
/// dictionary — both the staff display path (`reveal_summary`) and the master
/// `reveal_tokens` DB fallback (`mcp::reveal`) consume it, so a token either
/// resolves identically everywhere or nowhere. No plaintext vault: everything is
/// re-hashed from the raw source on demand.
pub(crate) fn derive_reveal_pairs(
    ticket_payload: &Value,
    threads: &[Value],
    meta: &Value,
) -> Vec<(String, String)> {
    let contact = ticket_payload.get("contact").cloned().unwrap_or(json!({}));
    let first = contact.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
    let last = contact.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
    let full_name = format!("{first} {last}").trim().to_string();
    let cf = ticket_payload.get("cf").cloned().unwrap_or(json!({}));

    // (a) structured contact + company + cf address/company, and (a2) the flat
    // `meta` fields which exist even on a thin node with no raw payload — the
    // masker tokenises all of these, so re-hash and swap them back.
    let mut pairs: Vec<(&str, String)> = vec![
        ("Name", full_name),
        ("Name", first.to_string()),
        ("Name", last.to_string()),
        ("Company", contact.get("account").and_then(|a| a.get("accountName")).and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ("Company", cf.get("cf_company").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ("Address", cf.get("cf_street").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ("Name", meta.get("customer").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ("Email", meta.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ("Phone", meta.get("phone").and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ("Company", meta.get("company").and_then(|v| v.as_str()).unwrap_or("").to_string()),
    ];

    // (c) Participant names/emails from every thread (sender / recipients /
    // author) — the signature names the masker tokenises across the whole thread.
    for t in threads {
        let p = match t.get("payload") { Some(p) => p, None => continue };
        for field in ["fromEmailAddress", "to", "cc"] {
            if let Some(s) = p.get(field).and_then(|v| v.as_str()) {
                for (n, e) in parse_named_addresses(s) {
                    if n.contains(' ') && n.len() >= 4 { pairs.push(("Name", n)); }
                    if !e.is_empty() { pairs.push(("Email", e)); }
                }
            }
        }
        if let Some(a) = p.get("author") {
            if let Some(v) = a.get("name").and_then(|v| v.as_str()) {
                if v.contains(' ') && v.len() >= 4 { pairs.push(("Name", v.to_string())); }
            }
            if let Some(em) = a.get("email").and_then(|v| v.as_str()) {
                pairs.push(("Email", em.to_string()));
            }
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    for (pii_type, value) in &pairs {
        let trimmed = value.trim();
        if trimmed.len() < 2 { continue; }
        out.push((obfuscate_pii(trimmed, pii_type), trimmed.to_string()));
        // Zoho stores HTML entities in structured fields ("A &amp; B") while the
        // summary text carries the decoded form — hash the decoded variant too so
        // its token resolves to the same stored value.
        if trimmed.contains("&amp;") {
            let decoded = trimmed.replace("&amp;", "&");
            out.push((obfuscate_pii(&decoded, pii_type), decoded));
        }
    }

    // (b) Emails/phones/IBANs/etc. — including free-text ones the regex backstop
    // masks — re-derived by re-running the SAME patterns over the raw source
    // (ticket fields + every thread body) and rebuilding the token→value map.
    let mut reveal_src = String::new();
    for k in ["subject", "description"] {
        if let Some(v) = ticket_payload.get(k).and_then(|v| v.as_str()) {
            reveal_src.push_str(v);
            reveal_src.push('\n');
        }
    }
    for k in ["email", "phone"] {
        if let Some(v) = contact.get(k).and_then(|v| v.as_str()) {
            reveal_src.push_str(v);
            reveal_src.push('\n');
        }
    }
    for t in threads {
        if let Some(p) = t.get("payload") {
            for k in ["content", "plainText", "fromEmailAddress", "from"] {
                if let Some(v) = p.get(k).and_then(|v| v.as_str()) {
                    reveal_src.push_str(v);
                    reveal_src.push('\n');
                }
            }
        }
    }
    for (token, original) in pprl_reveal_map(&reveal_src) {
        out.push((token, original));
    }

    // (d) Heuristic person-name tokens baked into `meta.subject` — third-party
    // names (end-customer on a dealer's ticket) that no dictionary above knows.
    // Re-running the same heuristic over the RAW subject reproduces the exact
    // `(token, span)` pairs the masker emitted at distillation.
    if let Some(subj) = ticket_payload.get("subject").and_then(|v| v.as_str()) {
        out.extend(eck_core::utils::anonymizer::mask_person_names_heuristic(subj).1);
    }

    out
}

pub async fn get_ticket_threads(
    State(state): State<Arc<AppState>>,
    Path(ticket_id): Path<String>,
    Query(q): Query<GetThreadsQuery>,
    claims: Option<Extension<Claims>>,
) -> ApiResult<Json<Value>> {
    // Display-time PII policy: staff (admin/operator) see real values; kiosk
    // observers and mesh/service callers see the tokenised summary as stored.
    let reveal = may_reveal_pii(claims.as_ref().map(|Extension(c)| c));
    // Fetch the parent ticket metadata from document (synced)
    let ticket_doc: Option<Value> = state.db
        .query("SELECT record::id(id) AS id, meta, status, summary_status, ai_summary, source_instance_id FROM document WHERE type = 'support_ticket' AND record::id(id) = $tid LIMIT 1")
        .bind(("tid", ticket_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let ai_summary_raw = ticket_doc
        .as_ref()
        .and_then(|d| d.get("ai_summary").and_then(|v| v.as_str()))
        .unwrap_or("");

    let source_instance_id = ticket_doc
        .as_ref()
        .and_then(|d| d.get("source_instance_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    // On-demand translation of the (already PPRL-masked) AI summary into the
    // requesting user's language. Thick-backend: serve the best text NOW — a
    // fresh translation if one exists, else the original masked summary plus a
    // `summary_pending` flag — and let the background queue produce the rest,
    // announced over the dashboard WebSocket (`translation_ready`). Translating a
    // masked summary leaks nothing new (PII is already tokenised); the PPRL reveal
    // below still runs on the translated text because the translation prompt
    // preserves the `Word_HEX` tokens verbatim.
    let req_lang = q.lang.as_deref().unwrap_or("").trim().to_string();
    let (summary_source, summary_lang, summary_pending): (String, String, bool) =
        if req_lang.is_empty() || ai_summary_raw.is_empty() {
            (ai_summary_raw.to_string(), eck_core::models::translation::DEFAULT_SOURCE_LANG.to_string(), false)
        } else {
            let src_thing = format!("document:`{}`", ticket_id);
            let r = crate::services::translation::resolve_translation(
                &state.db, &src_thing, "summary", ai_summary_raw, &req_lang,
            ).await;
            (r.text, r.lang, r.pending)
        };
    // Everything below (PPRL reveal, response) reads the RESOLVED summary text.
    let ai_summary_raw: &str = &summary_source;

    // Try local document_raw first. In meta_only mode we skip the raw_ticket
    // fetch entirely — meta from `document` is enough for the header list,
    // and the page does not yet need a full ticket payload.
    let raw_ticket: Option<Value> = if q.meta_only {
        None
    } else {
        state.db
            .query("SELECT payload FROM document_raw WHERE record::id(id) = $tid LIMIT 1")
            .bind(("tid", ticket_id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(db_err)?
    };

    // meta_only path projects out the heavy fields. Full path keeps the legacy
    // payload-everything shape so we don't have to teach every caller about the
    // new flag at once.
    let threads_query = if q.meta_only {
        "SELECT record::id(id) AS id, type, ticket_id, updated_at, \
         payload.direction AS direction, payload.fromEmailAddress AS fromEmailAddress, \
         payload.from AS from, payload.createdTime AS createdTime, payload.summary AS summary \
         FROM document_raw WHERE type = 'support_thread' AND ticket_id = $tid ORDER BY updated_at ASC"
    } else {
        "SELECT record::id(id) AS id, type, payload, ticket_id, updated_at \
         FROM document_raw WHERE type = 'support_thread' AND ticket_id = $tid ORDER BY updated_at ASC"
    };

    let raw_threads: Vec<Value> = state.db
        .query(threads_query)
        .bind(("tid", ticket_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    // If raw data is missing locally and this ticket came from another instance,
    // queue a task for the fat node to push the data to us (reverse-fetch via NAT).
    // meta_only skips this entirely — the header list does not need raw payload,
    // and queuing the fetch every time someone glances at a ticket undoes the
    // point of lazy-loading. The per-thread payload endpoint will queue when
    // the operator actually opens an individual thread.
    let is_fetching_from_peer = if !q.meta_only
        && raw_ticket.is_none()
        && !source_instance_id.is_empty()
        && source_instance_id != state.instance_id
    {
        let _ = state.db
            .query("INSERT INTO mesh_task { target_instance_id: $iid, action: 'request_raw_docs', ticket_id: $tid, created_at: time::now() }")
            .bind(("iid", source_instance_id.clone()))
            .bind(("tid", ticket_id.clone()))
            .await;
        // Poke the source node so a reachable peer serves this in seconds
        // instead of its next poll cycle (the human is watching a spinner).
        crate::handlers::mesh::spawn_task_nudge(&state, source_instance_id);
        true
    } else {
        false
    };

    let mut ticket_payload = raw_ticket
        .and_then(|d| d.get("payload").cloned())
        .unwrap_or_else(|| {
            // Fallback: reconstruct minimal payload from meta
            ticket_doc.as_ref()
                .and_then(|d| d.get("meta").cloned())
                .unwrap_or(json!({}))
        });

    // PPRL reveal — role-gated. The summary is stored fully tokenised (PII never
    // hits the DB in clear). For authorised staff we re-derive the original
    // values from the raw source and swap the deterministic SimHash tokens back
    // (wrapping each in reveal sentinels so the UI can highlight them); for
    // everyone else the tokenised summary is returned untouched.
    //
    // The reveal MUST read the FULL raw payloads, not the `meta_only` projection:
    // the detail page and the dashboard card both request `meta_only=true`, which
    // strips thread payloads — so the participant reveal would silently no-op and
    // every thread-sourced name/email (a CC'd colleague, third-party support, the
    // assignee's signature) would stay masked. So when revealing we fetch a
    // dedicated full source: in full mode we already have it (`ticket_payload` /
    // `raw_threads`); in meta_only we query it explicitly here.
    let (ai_summary, ai_summary_marked) = if ai_summary_raw.is_empty() {
        (json!(null), json!(null))
    } else if !reveal {
        // Observer/kiosk/mesh: leave masked (and marked == clean, no sentinels).
        (json!(ai_summary_raw), json!(ai_summary_raw))
    } else {
        let meta = ticket_doc.as_ref().and_then(|d| d.get("meta")).cloned().unwrap_or(json!({}));
        let fetched_ticket: Value;
        let fetched_threads: Vec<Value>;
        let (rt_payload, rt_threads): (&Value, &[Value]) = if q.meta_only {
            let row: Option<Value> = state.db
                .query("SELECT payload FROM document_raw WHERE record::id(id) = $tid LIMIT 1")
                .bind(("tid", ticket_id.clone()))
                .await
                .and_then(|mut r| r.take(0))
                .map_err(db_err)?;
            fetched_ticket = row
                .and_then(|d| d.get("payload").cloned())
                .unwrap_or_else(|| meta.clone());
            fetched_threads = state.db
                .query("SELECT payload FROM document_raw WHERE type = 'support_thread' AND ticket_id = $tid ORDER BY updated_at ASC")
                .bind(("tid", ticket_id.clone()))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            (&fetched_ticket, fetched_threads.as_slice())
        } else {
            (&ticket_payload, raw_threads.as_slice())
        };

        let marked = reveal_summary(ai_summary_raw, rt_payload, rt_threads, &meta);
        let clean = strip_pii_markers(&marked);
        (json!(clean), json!(marked))
    };

    // In full mode, enrich each thread's payload with parent ticket metadata
    // (matches the legacy system's format the frontend's contact-info fallbacks
    // still rely on). In meta_only mode there is no payload to enrich — the
    // header list is rendered straight from the projected scalar fields.
    let enriched: Vec<Value> = if q.meta_only {
        if raw_threads.is_empty() {
            // Peer node (e.g. kiosk): the bodies (document_raw) didn't sync, but
            // the thread SHELLS (document, type=support_thread) did. Build the
            // header list from those so the operator still sees every message and
            // can click to lazily pull its body via the per-thread endpoint
            // (which queues the reverse-fetch). Without this the list is empty and
            // the summary looks orphaned. `created_time` is aliased to the camelCase
            // the frontend header rows use; `body_pending` flags a lazy row.
            let shells: Vec<Value> = state.db
                .query("SELECT record::id(id) AS id, type, ticket_id, direction, \
                        created_time AS createdTime FROM document \
                        WHERE type = 'support_thread' AND ticket_id = $tid \
                        ORDER BY created_time ASC, updated_at ASC")
                .bind(("tid", ticket_id.clone()))
                .await
                .and_then(|mut r| r.take(0))
                .unwrap_or_default();
            shells.into_iter().map(|mut s| {
                if let Some(o) = s.as_object_mut() {
                    o.insert("body_pending".to_string(), json!(true));
                }
                s
            }).collect()
        } else {
            raw_threads
        }
    } else {
        raw_threads.into_iter().map(|mut row| {
            if let Some(payload) = row.get_mut("payload").and_then(|p| p.as_object_mut()) {
                payload.insert("ticket".to_string(), ticket_payload.clone());
                payload.insert("ticketId".to_string(), json!(ticket_id));
            }
            row
        }).collect()
    };

    // meta_only serves the META shell as `ticket`, and `meta.subject` is
    // stored MASKED since the distiller tokenises it. Staff get the clear
    // subject restored from the local raw payload (a thin node without raw
    // keeps the masked one — same degradation as the summary reveal). Full
    // mode already serves the raw payload, whose subject was never masked.
    if reveal && q.meta_only {
        let raw_subject: Option<Value> = state.db
            .query("SELECT payload.subject AS subject FROM document_raw WHERE record::id(id) = $tid LIMIT 1")
            .bind(("tid", ticket_id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or(None);
        if let Some(s) = raw_subject
            .and_then(|r| r.get("subject").and_then(|v| v.as_str()).map(String::from))
        {
            if !s.is_empty() && ticket_payload.is_object() {
                ticket_payload["subject"] = json!(s);
            }
        }
    }

    Ok(Json(json!({
        "ticket": ticket_payload,
        "threads": enriched,
        "ai_summary": ai_summary,
        // Same summary with restored PII wrapped in reveal sentinels (only differs
        // from `ai_summary` for authorised viewers). The frontend renders the
        // sentinels as a yellow highlight; `ai_summary` stays clean for copy/RMA.
        "ai_summary_marked": ai_summary_marked,
        // Translation of the AI summary (only meaningful when ?lang= was passed).
        // `summary_lang` is the language `ai_summary`/`ai_summary_marked` are
        // actually in (the requested lang once translated, else source `en`);
        // `summary_pending` = a translation is still being produced in the
        // background — re-fetch on the `translation_ready` WebSocket event.
        "summary_lang": summary_lang,
        "summary_pending": summary_pending,
        "is_fetching_from_peer": is_fetching_from_peer,
    })))
}

/// GET /api/support/tickets/:ticket_id/threads/:thread_id/payload
///
/// Lazy fetch of a single thread's full payload. Returns the payload only
/// (frontend already has the meta row from the header list). If the payload
/// is missing locally and the ticket has a known source instance, queues the
/// usual `request_raw_docs` mesh task so the source node pushes the bundle
/// to us; the response includes `is_fetching_from_peer: true` and the client
/// can poll the same endpoint until the payload lands.
pub async fn get_thread_payload(
    State(state): State<Arc<AppState>>,
    Path((ticket_id, thread_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let row: Option<Value> = state.db
        .query("SELECT payload FROM document_raw WHERE record::id(id) = $tid LIMIT 1")
        .bind(("tid", thread_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    if let Some(r) = row {
        if let Some(payload) = r.get("payload").cloned() {
            return Ok(Json(json!({
                "payload": payload,
                "is_fetching_from_peer": false,
            })));
        }
    }

    // Missing locally — queue a pull from the source instance (same shape as
    // the legacy bulk path) so a subsequent poll will find the row.
    let row: Option<Value> = state.db
        .query("SELECT source_instance_id FROM document WHERE type = 'support_ticket' AND record::id(id) = $tid LIMIT 1")
        .bind(("tid", ticket_id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;
    let source_instance_id: Option<String> = row
        .and_then(|v| v.get("source_instance_id").and_then(|s| s.as_str()).map(String::from));

    let is_fetching = match source_instance_id {
        Some(iid) if !iid.is_empty() && iid != state.instance_id => {
            let _ = state.db
                .query("INSERT INTO mesh_task { target_instance_id: $iid, action: 'request_raw_docs', ticket_id: $tid, created_at: time::now() }")
                .bind(("iid", iid.clone()))
                .bind(("tid", ticket_id))
                .await;
            // Poke the source node — the operator is watching a spinner.
            crate::handlers::mesh::spawn_task_nudge(&state, iid);
            true
        }
        _ => false,
    };

    Ok(Json(json!({
        "payload": null,
        "is_fetching_from_peer": is_fetching,
    })))
}

/// POST /api/support/backfill-thread-headers — one-shot: stamp `direction` +
/// `created_time` onto every synced thread shell from the local raw payload, so
/// peers (kiosk) that only receive the shells (not the bodies) can render a
/// thread header list. Threads imported before these fields existed skip the
/// hash-gated re-import, so they need this explicit pass. Advances each shell's
/// `_vclock` so the enriched shell actually propagates across the mesh instead
/// of being dropped as "local wins/equal". Run on the scraper node (raw lives there).
pub async fn backfill_thread_headers(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let raws: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, payload.direction AS direction, \
                payload.createdTime AS createdTime FROM document_raw WHERE type = 'support_thread'")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    let _now = chrono::Utc::now().to_rfc3339();
    let mut updated = 0i64;
    for row in &raws {
        let id = match row.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let direction = row.get("direction").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let created = row.get("createdTime").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let existing: Option<Value> = state.db
            .query("SELECT _vclock FROM document WHERE type = 'support_thread' AND record::id(id) = $id LIMIT 1")
            .bind(("id", id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(db_err)?;
        // Only touch shells that actually exist and still lack the fields is not
        // strictly necessary — MERGE is idempotent — but skip if no shell.
        if existing.is_none() { continue; }
        let next_vclock = eck_core::sync::conflict::next_local_vclock(
            existing.as_ref().and_then(|v| v.get("_vclock")),
            &state.instance_id,
        );

        let _: Option<Value> = state.db
            .query("UPDATE type::record($rid) MERGE { direction: $d, created_time: $c, _vclock: $vc, updated_at: time::now() } \
                    WHERE type = 'support_thread'")
            .bind(("rid", format!("document:`{}`", id)))
            .bind(("d", direction))
            .bind(("c", created))
            .bind(("vc", next_vclock))
            .await
            .and_then(|mut r| r.take(0))
            .map_err(db_err)?;
        updated += 1;
    }

    Ok(Json(json!({ "ok": true, "updated": updated })))
}

/// Re-mask one stored summary with everything derivable deterministically:
/// (1) the regex backstop (emails/phones/IBANs/cards/streets), (2) the ticket's
/// own identity dictionary from flat `meta` (customer/email/phone/company, plus
/// the HTML-entity-decoded variant Zoho text carries), (3) participant signature
/// names from the raw threads. Tokens are hashed from the CANONICAL stored value
/// so `derive_reveal_pairs` resolves them back. Full multi-word names only —
/// splitting into parts would corrupt German prose ("Kraft", "Alle" are words).
/// Returns `Some(masked)` when anything changed, `None` when already clean.
fn scrub_summary_text(summary: &str, meta: &Value, participant_names: &[String]) -> Option<String> {
    let (mut out, fps) = scrub_pii_regex(summary);
    let mut dirty = !fps.is_empty() && out != summary;

    let get = |k: &str| meta.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let mut entries: Vec<(&str, String)> = Vec::new();
    let customer = get("customer");
    if customer.contains(' ') && customer.len() >= 4 {
        entries.push(("Name", customer));
    }
    let company = get("company");
    if company.len() >= 4 {
        entries.push(("Company", company));
    }
    // meta email/phone are usually caught by (1) already; the dictionary makes
    // them robust against formats the conservative regexes skip.
    let email = get("email");
    if email.len() >= 5 {
        entries.push(("Email", email));
    }
    let phone = get("phone");
    if phone.len() >= 5 {
        entries.push(("Phone", phone));
    }
    for n in participant_names {
        if n.contains(' ') && n.len() >= 4 {
            entries.push(("Name", n.clone()));
        }
    }

    // Longest value first so "Erika Musterfrau Fitness" wins over "Erika Musterfrau".
    entries.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    entries.dedup();
    for (label, value) in &entries {
        let token = obfuscate_pii(value, label);
        // Multi-part names/companies get separator/case-flexible matching: the
        // LLM normalizes spellings ("Yeon Woo Jeong" → "Yeon-woo Jeong",
        // "Liberty GmbH" → "LIBERTY GmbH"), which an exact replace can never
        // catch. Matching runs over the HTML-entity-decoded form, but the token
        // stays the hash of the CANONICAL stored spelling, so reveal resolves it.
        if matches!(*label, "Name" | "Company") {
            let match_value = value.replace("&amp;", "&");
            let parts: Vec<&str> = match_value.split(|c: char| c.is_whitespace() || c == '-').filter(|p| !p.is_empty()).collect();
            if parts.len() >= 2 {
                let pat = format!(
                    r"(?i)\b{}\b",
                    parts.iter().map(|p| regex::escape(p)).collect::<Vec<_>>().join(r"[\s\-]+")
                );
                if let Ok(re) = regex::Regex::new(&pat) {
                    if re.is_match(&out) {
                        out = re.replace_all(&out, token.as_str()).into_owned();
                        dirty = true;
                    }
                }
                continue;
            }
        }
        let mut variants = vec![value.clone()];
        if value.contains("&amp;") {
            variants.push(value.replace("&amp;", "&"));
        }
        for var in variants {
            if out.contains(&var) {
                out = out.replace(&var, &token);
                dirty = true;
            }
        }
    }

    if dirty { Some(out) } else { None }
}

/// POST /api/support/admin/scrub-summaries — re-runnable GDPR backfill.
///
/// Summaries produced before the current masker/backstop have clear PII baked
/// in (it reached Gemini and the DB in plaintext). Re-mask every stored
/// `ai_summary` via [`scrub_summary_text`]. Idempotent: an already-clean
/// summary is left untouched. Reveal still works afterwards because authorised
/// viewers re-derive the values from the raw source.
///
/// Every write ADVANCES the row's `_vclock` (author bump). `ai_summary` is
/// merkle-hashed since d4c8559, so a bump-less UPDATE either never leaves this
/// node or gets reverted at the next convergence sweep — which is exactly how
/// the 2026-06-28 run's scrubs quietly came back leaky (observed on ticket
/// 6219, 2026-07-18). `pii_fingerprints` is nulled so the self-heal sweep
/// re-derives it from the new token set. Admin-only.
pub async fn scrub_summaries(
    State(state): State<Arc<AppState>>,
    claims: Option<Extension<Claims>>,
) -> ApiResult<Json<Value>> {
    if !matches!(claims.as_ref().map(|Extension(c)| c.role.as_str()), Some("admin")) {
        return Err((StatusCode::FORBIDDEN, "Admin required".into()));
    }

    let docs: Vec<Value> = state.db
        .query(
            "SELECT record::id(id) AS id, type, ai_summary, meta, _vclock FROM document \
             WHERE type IN ['support_ticket', 'invoice'] AND ai_summary != NONE AND ai_summary != ''",
        )
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

    // Participant signature names per ticket in ONE projection scan — the
    // `ticket_id` filter has no index, so a per-ticket query is a full
    // document_raw scan ×1.7k and runs past any sane HTTP timeout (a client
    // disconnect then cancels the handler mid-run).
    let thread_rows: Vec<Value> = state.db
        .query(
            "SELECT ticket_id, payload.fromEmailAddress AS from_h, payload.to AS to_h, \
             payload.cc AS cc_h, payload.author.name AS author_name \
             FROM document_raw WHERE type = 'support_thread'",
        )
        .await
        .and_then(|mut r| r.take(0))
        .unwrap_or_default();
    let mut names_by_ticket: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for row in &thread_rows {
        let Some(tid) = row.get("ticket_id").and_then(|v| v.as_str()) else { continue };
        let entry = names_by_ticket.entry(tid.to_string()).or_default();
        for f in ["from_h", "to_h", "cc_h"] {
            if let Some(s) = row.get(f).and_then(|v| v.as_str()) {
                for (n, _e) in parse_named_addresses(s) {
                    entry.push(n);
                }
            }
        }
        if let Some(n) = row.get("author_name").and_then(|v| v.as_str()) {
            entry.push(n.to_string());
        }
    }

    let mut scanned = 0u32;
    let mut changed = 0u32;
    for d in &docs {
        scanned += 1;
        let id = match d.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let summary = d.get("ai_summary").and_then(|v| v.as_str()).unwrap_or("");
        let is_ticket = d.get("type").and_then(|v| v.as_str()) == Some("support_ticket");
        let meta = d.get("meta").cloned().unwrap_or(json!({}));

        let names: Vec<String> = if is_ticket {
            names_by_ticket.get(&id).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };

        let Some(out) = scrub_summary_text(summary, &meta, &names) else { continue };

        let next_vclock = eck_core::sync::conflict::next_local_vclock(
            d.get("_vclock"),
            &state.instance_id,
        );
        let _ = state.db
            .query("UPDATE type::record($rid) SET ai_summary = $s, pii_fingerprints = NONE, \
                    _vclock = $vc, updated_at = time::now()")
            .bind(("rid", format!("document:`{}`", id)))
            .bind(("s", out))
            .bind(("vc", next_vclock))
            .await
            .map_err(db_err)?;
        changed += 1;
    }

    Ok(Json(json!({ "scanned": scanned, "changed": changed })))
}

/// POST /api/support/tickets/:ticket_id/summary — stub (AI summarization)
pub async fn summarize_ticket(
    State(_state): State<Arc<AppState>>,
    Path(ticket_id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "summary": format!("Summary not yet implemented for ticket {}", ticket_id) })))
}

/// GET /api/support/tickets/:ticket_id/similar — stub (vector search)
pub async fn find_similar(
    State(_state): State<Arc<AppState>>,
    Path(_ticket_id): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(vec![]))
}

#[cfg(test)]
mod reveal_tests {
    use super::*;

    fn wrap(v: &str) -> String {
        format!("{}{}{}", PII_OPEN, v, PII_CLOSE)
    }

    /// The regression this guards: a second contact who only ever appears in a
    /// thread `cc:` (not in the structured customer fields) must be revealed, not
    /// left as a `Name_…`/`Email_…` token — and every restored value must be
    /// wrapped in reveal sentinels for the UI highlight.
    #[test]
    fn reveals_primary_and_thread_participant() {
        // ONE shared pepper value across every in-process test: the env var is
        // process-global and tests run in parallel, so a module setting a
        // DIFFERENT value flips the pepper between this test's mask and reveal
        // calls and the tokens stop matching (observed flake 2026-07-18).
        std::env::set_var("SYNC_SECRET", "test_secret");

        // Build a masked summary out of the SAME deterministic tokens the masker
        // would have produced, then prove reveal swaps them all back.
        let primary_name = "Andreas Roth";
        let primary_email = "a.roth@example.de";
        let second_name = "Bernd Falk";
        let second_email = "b.falk@example-med.de";

        let masked = format!(
            "Kontakt: {} ({}), {} ({})",
            obfuscate_pii(primary_name, "Name"),
            obfuscate_pii(primary_email, "Email"),
            obfuscate_pii(second_name, "Name"),
            obfuscate_pii(second_email, "Email"),
        );

        let ticket_payload = json!({
            "contact": {
                "firstName": "Andreas",
                "lastName": "Roth",
                "email": primary_email,
            }
        });
        let threads = vec![json!({
            "payload": {
                "cc": format!("\"{}\"<{}>", second_name, second_email),
            }
        })];
        let meta = json!({ "customer": primary_name, "email": primary_email });

        let out = reveal_summary(&masked, &ticket_payload, &threads, &meta);

        // Everything restored and highlighted, nothing left tokenised.
        assert!(out.contains(&wrap(primary_name)), "primary name not revealed: {out}");
        assert!(out.contains(&wrap(primary_email)), "primary email not revealed: {out}");
        assert!(out.contains(&wrap(second_name)), "thread cc name not revealed: {out}");
        assert!(out.contains(&wrap(second_email)), "thread cc email not revealed: {out}");
        assert!(!out.contains("Name_"), "a Name_ token leaked: {out}");
        assert!(!out.contains("Email_"), "an Email_ token leaked: {out}");

        // The clean variant drops the sentinels but keeps the plaintext.
        let clean = strip_pii_markers(&out);
        assert!(clean.contains(second_name) && clean.contains(second_email));
        assert!(!clean.contains(&wrap(second_name)));
    }

    /// The backfill scrub masks the identity dictionary (meta customer/company —
    /// including the HTML-entity form Zoho stores vs the decoded form summaries
    /// carry), participant names, and regex-caught free text; and everything it
    /// bakes in resolves back through `derive_reveal_pairs` (round-trip, no
    /// vault). Guards the 6219-class leak: staff name + email + mobile in clear
    /// in an old summary.
    #[test]
    fn scrub_summary_text_masks_and_stays_revealable() {
        std::env::set_var("SYNC_SECRET", "test_secret");

        let meta = json!({
            "customer": "Thomas Berger",
            "company": "Aktiv Fitness &amp; Wellness Lounge",
            "email": "info@example-fit.de",
        });
        let participants = vec!["Andreas Keller".to_string(), "Anna Lena Vogt".to_string()];
        // "Anna-lena Vogt" = the LLM-normalized spelling of participant
        // "Anna Lena Vogt" — must still be caught (flexible-separator match).
        let summary = "Kontakt: Thomas Berger (Aktiv Fitness & Wellness Lounge, info@example-fit.de). \
                       Bearbeitet von Andreas Keller und Anna-lena Vogt, erreichbar unter +49 (0) 172 / 637 88 21.";

        let out = scrub_summary_text(summary, &meta, &participants)
            .expect("a leaky summary must be flagged dirty");
        for leaked in [
            "Thomas Berger",
            "Aktiv Fitness & Wellness Lounge",
            "info@example-fit.de",
            "Andreas Keller",
            "Anna-lena Vogt",
            "637 88 21",
        ] {
            assert!(!out.contains(leaked), "still leaks {leaked}: {out}");
        }
        // The decoded company text must carry the CANONICAL (stored-form) token
        // so the reveal dictionary — which hashes meta values — resolves it.
        assert!(out.contains(&obfuscate_pii("Aktiv Fitness &amp; Wellness Lounge", "Company")), "company token mismatch: {out}");

        // Round-trip: every baked token resolves via the shared reveal pairs.
        let pairs = derive_reveal_pairs(
            &json!({}),
            &[json!({"payload": {"cc": "\"Andreas Keller\"<a.keller@example-med.de>,\"Anna Lena Vogt\"<a.vogt@example-med.de>", "content": "Tel: +49 (0) 172 / 637 88 21"}})],
            &meta,
        );
        let mut revealed = out.clone();
        for (token, value) in &pairs {
            revealed = revealed.replace(token.as_str(), value.as_str());
        }
        for (label, _) in [("Name_", ""), ("Email_", ""), ("Phone_", ""), ("Company_", "")] {
            assert!(!revealed.contains(label), "{label} token left unresolved: {revealed}");
        }
        assert!(revealed.contains("Aktiv Fitness &amp; Wellness Lounge") || revealed.contains("Aktiv Fitness & Wellness Lounge"));

        // Idempotent: a clean summary is not flagged.
        assert!(scrub_summary_text(&out, &meta, &participants).is_none());
    }
}
