use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use crate::services::support as svc;
use eck_core::utils::anonymizer::obfuscate_pii;

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
    let result = svc::import_ticket(&state.db, &payload.ticket_id, &payload.ticket, &state.instance_id)
        .await
        .map_err(db_err)?;
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
        if let Err(e) = svc::import_ticket(&state.db, ticket_id, ticket, &state.instance_id).await {
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

        match svc::import_ticket(&state.db, &ticket_id_owned, ticket, &state.instance_id).await {
            Ok(r) => {
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

        let res = state.db
            .query("UPDATE type::record($rid) SET meta.assignee_id = $aid, meta.assignee_name = $aname")
            .bind(("rid", format!("document:`{}`", id)))
            .bind(("aid", a_id))
            .bind(("aname", a_name))
            .await;

        if res.is_ok() { updated += 1; } else { skipped += 1; }
    }

    Ok(Json(json!({ "updated": updated, "skipped": skipped, "total": raws.len() })))
}

/// POST /api/support/backfill-meta — re-run metadata extraction against the
/// payloads already stored in `document_raw`. Use this after changing the
/// extraction logic in `services::support::extract_ticket_metadata` (e.g.
/// tightening custom-field matching or adding an address parser) so existing
/// tickets pick up the new rules without round-tripping to Zoho.
///
/// No scraper needed. Fast. Safe to re-run.
pub async fn backfill_meta(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Value>> {
    let raws: Vec<Value> = state.db
        .query("SELECT record::id(id) AS id, payload FROM document_raw WHERE type = 'support_ticket'")
        .await
        .and_then(|mut r| r.take(0))
        .map_err(db_err)?;

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

        let meta = crate::services::support::extract_ticket_metadata(payload);

        let res = state.db
            .query("UPDATE type::record($rid) SET meta = $meta")
            .bind(("rid", format!("document:`{}`", ticket_id)))
            .bind(("meta", meta))
            .await;

        if res.is_ok() { updated += 1; } else { skipped += 1; }
    }

    Ok(Json(json!({ "total": total, "updated": updated, "skipped": skipped })))
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
        let res = state.db
            .query(
                "UPDATE document SET meta.last_outbound_at = $t \
                 WHERE record::id(id) = $tid \
                 AND type = 'support_ticket' \
                 AND (meta.last_outbound_at IS NONE OR meta.last_outbound_at = '' OR meta.last_outbound_at < $t);"
            )
            .bind(("tid", ticket_id.clone()))
            .bind(("t", t.clone()))
            .await;
        if res.is_ok() { updated += 1; }
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

        match svc::import_ticket(&state.db, &ticket_id, enriched_ticket, &state.instance_id).await {
            Ok(_) => { enriched += 1; }
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
pub async fn list_tickets(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<Value>>> {
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

        json!({
            "ticket_id": id,
            "ticket_number": m["ticket_number"].as_str().unwrap_or(""),
            "subject": m["subject"].as_str().unwrap_or("(no subject)"),
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
}

pub async fn get_ticket_threads(
    State(state): State<Arc<AppState>>,
    Path(ticket_id): Path<String>,
    Query(q): Query<GetThreadsQuery>,
) -> ApiResult<Json<Value>> {
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
            .bind(("iid", source_instance_id))
            .bind(("tid", ticket_id.clone()))
            .await;
        true
    } else {
        false
    };

    let ticket_payload = raw_ticket
        .and_then(|d| d.get("payload").cloned())
        .unwrap_or_else(|| {
            // Fallback: reconstruct minimal payload from meta
            ticket_doc.as_ref()
                .and_then(|d| d.get("meta").cloned())
                .unwrap_or(json!({}))
        });

    // Unmask PPRL tokens in the AI summary
    let ai_summary = if ai_summary_raw.is_empty() {
        json!(null)
    } else {
        let mut unmasked = ai_summary_raw.to_string();
        let contact = ticket_payload.get("contact").cloned().unwrap_or(json!({}));
        let first = contact.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
        let last = contact.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
        let full_name = format!("{first} {last}").trim().to_string();

        let pii_pairs: Vec<(&str, String)> = vec![
            ("Name", full_name.clone()),
            ("Name", first.to_string()),
            ("Name", last.to_string()),
            ("Email", contact.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string()),
            ("Phone", contact.get("phone").and_then(|v| v.as_str()).unwrap_or("").to_string()),
            ("Company", contact.get("account").and_then(|a| a.get("accountName")).and_then(|v| v.as_str()).unwrap_or("").to_string()),
        ];

        for (pii_type, value) in &pii_pairs {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed.len() < 2 { continue; }
            let token = obfuscate_pii(trimmed, pii_type);
            unmasked = unmasked.replace(&token, trimmed);
        }

        json!(unmasked)
    };

    // In full mode, enrich each thread's payload with parent ticket metadata
    // (matches legacy eckwmsr format the frontend's contact-info fallbacks
    // still rely on). In meta_only mode there is no payload to enrich — the
    // header list is rendered straight from the projected scalar fields.
    let enriched: Vec<Value> = if q.meta_only {
        raw_threads
    } else {
        raw_threads.into_iter().map(|mut row| {
            if let Some(payload) = row.get_mut("payload").and_then(|p| p.as_object_mut()) {
                payload.insert("ticket".to_string(), ticket_payload.clone());
                payload.insert("ticketId".to_string(), json!(ticket_id));
            }
            row
        }).collect()
    };

    Ok(Json(json!({
        "ticket": ticket_payload,
        "threads": enriched,
        "ai_summary": ai_summary,
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
                .bind(("iid", iid))
                .bind(("tid", ticket_id))
                .await;
            true
        }
        _ => false,
    };

    Ok(Json(json!({
        "payload": null,
        "is_fetching_from_peer": is_fetching,
    })))
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
