use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use eck_core::db::SurrealDb;
use eck_core::utils::filestore::FileStore;
use serde_json::{json, Value};
use std::io::Cursor;
use tracing::debug;

/// Compute a 128-bit MurmurHash3 of the given bytes, returned as 32-char hex string.
pub fn murmur3_hex(data: &[u8]) -> String {
    let hash = murmur3::murmur3_x64_128(&mut Cursor::new(data), 0)
        .expect("murmur3 should not fail on in-memory data");
    format!("{:032x}", hash)
}

/// Split a German-style address "Street 42, 12345 City" into (zip, city).
/// Returns (None, None) when the 5-digit zip boundary cannot be located.
///
/// Shared between the ticket-metadata extractor, the geocoder (so it queries
/// Nominatim with just zip+city — never the street), and the embedding-text
/// builder (so the street gets PII-masked while city/zip stay in the clear).
pub fn parse_zip_city(addr: &str) -> (Option<String>, Option<String>) {
    let bytes = addr.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 5 <= n {
        let is_digit = |b: u8| b.is_ascii_digit();
        let boundary_before = i == 0 || !is_digit(bytes[i - 1]);
        let window_digits = (0..5).all(|k| is_digit(bytes[i + k]));
        let boundary_after = i + 5 == n || !is_digit(bytes[i + 5]);
        if boundary_before && window_digits && boundary_after {
            let zip = addr[i..i + 5].to_string();
            let tail = addr[i + 5..].trim();
            let city = tail
                .trim_start_matches(|c: char| c.is_whitespace() || c == ',')
                .trim_end_matches(|c: char| c.is_whitespace() || c == ',')
                .to_string();
            return (Some(zip), if city.is_empty() { None } else { Some(city) });
        }
        i += 1;
    }
    (None, None)
}

/// Result of an import operation.
pub struct ImportResult {
    pub changed: bool,
    pub id: String,
}

/// Decide whether a ticket's metadata is rich enough to justify seeding an
/// `ai_task`. Returns false for replies/forwards, empty descriptions, and
/// tickets whose "customer" is an internal support agent — those cases
/// waste Gemini budget on ask_human dead-ends.
fn should_seed_ai_task(meta: &Value) -> bool {
    let description = meta.get("description").and_then(|v| v.as_str()).unwrap_or("").trim();
    if description.is_empty() {
        return false;
    }

    let subject = meta.get("subject").and_then(|v| v.as_str()).unwrap_or("").trim_start();
    let lower = subject.to_ascii_lowercase();
    if lower.starts_with("re:") || lower.starts_with("fwd:") || lower.starts_with("fw:") || lower.starts_with("aw:") {
        return false;
    }

    let email = meta.get("email").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
    if email.ends_with("@inbodysupport.eu") || email.ends_with("@inbody.com") {
        return false;
    }
    let customer = meta.get("customer").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
    if customer.starts_with("support_") || customer == "support" {
        return false;
    }

    true
}

/// Extract lightweight metadata from a Zoho ticket payload for the synced `document` table.
pub fn extract_ticket_metadata(ticket: &Value) -> Value {
    let contact = ticket.get("contact").cloned().unwrap_or(json!({}));
    let first = contact.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
    let last = contact.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
    let mut customer = format!("{first} {last}").trim().to_string();
    if customer.is_empty() {
        customer = contact.get("fullName").and_then(|v| v.as_str()).unwrap_or("").to_string();
    }

    // Exact-match extractor for custom fields.
    //
    // Matches a field if its normalized key (lowercased, spaces/hyphens/
    // leading "cf_" stripped) equals any of the supplied keywords. Earlier
    // versions used substring matching which disastrously let short tokens
    // like "ort" bleed into `imp[ort]ed`, `sp[ort]`, `opp[ort]unity` — the
    // city field ended up holding literal "false" from `PMI Opportunity` /
    // `Imported`. Exact match + boolean-string guard prevents that.
    let normalize_key = |k: &str| -> String {
        k.trim()
            .trim_start_matches("cf_")
            .to_lowercase()
            .replace([' ', '-'], "_")
    };
    let looks_like_value = |s: &str| -> bool {
        let t = s.trim();
        !t.is_empty() && t != "null" && t != "false" && t != "true"
    };
    let find_cf = |keys: &[&str]| -> String {
        let normalized_keys: Vec<String> = keys.iter().map(|k| normalize_key(k)).collect();
        for field in ["customFields", "cf"] {
            if let Some(cfs) = ticket.get(field).and_then(|v| v.as_object()) {
                for (k, v) in cfs {
                    let kn = normalize_key(k);
                    if normalized_keys.iter().any(|nk| nk == &kn) {
                        if let Some(s) = v.as_str() {
                            if looks_like_value(s) { return s.to_string(); }
                        }
                    }
                }
            }
        }
        String::new()
    };

    // German address parser — shared helper at module level. Zoho's
    // InBody-EU department mostly leaves standalone City / PLZ custom
    // fields empty; city info is embedded in the Address custom field
    // in the standard "Street 123, 12345 City" format.
    let extract_zip_city = |addr: &str| -> (String, String) {
        let (z, c) = parse_zip_city(addr);
        (z.unwrap_or_default(), c.unwrap_or_default())
    };

    // Truncate the free-text description at 2000 chars so it fits inside the
    // orchestrator's LLM context without blowing up the prompt. char-based
    // `take` is UTF-8-safe; byte-based truncate would panic mid-code-point
    // on German umlauts or multibyte symbols.
    let description_raw = ticket
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let description: String = description_raw.chars().take(2000).collect();

    let assignee = ticket.get("assignee").cloned().unwrap_or(json!({}));
    let a_first = assignee.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
    let a_last = assignee.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
    let mut assignee_name = format!("{a_first} {a_last}").trim().to_string();
    if assignee_name.is_empty() {
        assignee_name = assignee
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    let assignee_id = assignee
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            ticket
                .get("assigneeId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let address = find_cf(&["address", "adresse", "street"]);
    let mut city = find_cf(&["city", "ort", "stadt"]);
    let mut zip = find_cf(&["zip", "plz", "postcode"]);
    if zip.is_empty() || city.is_empty() {
        let (z, c) = extract_zip_city(&address);
        if zip.is_empty() { zip = z; }
        if city.is_empty() { city = c; }
    }

    json!({
        "subject": ticket.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
        "ticket_number": ticket.get("ticketNumber").and_then(|v| v.as_str()).unwrap_or(""),
        "status": ticket.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "customer": customer,
        "email": contact.get("email").and_then(|v| v.as_str()).unwrap_or(""),
        "phone": contact.get("phone").and_then(|v| v.as_str()).unwrap_or(""),
        "company": find_cf(&["company", "einrichtung"]),
        "address": address,
        "city": city,
        "zip": zip,
        "device_model": find_cf(&["inbody model", "inbodymodel", "in_body_model"]),
        "serial_number": find_cf(&["serial", "seriennummer", "serial_number"]),
        "manufacturing_date": find_cf(&["herstellungsdatum", "manufacturing date", "manufacturing"]),
        "created_time": ticket.get("createdTime").and_then(|v| v.as_str()).unwrap_or(""),
        "description": description,
        "assignee_id": assignee_id,
        "assignee_name": assignee_name,
    })
}

/// Import or update a Zoho Desk ticket.
/// - `document` table: lightweight metadata + AI summary (synced across mesh)
/// - `document_raw` table: full Zoho payload (local only, not synced)
pub async fn import_ticket(
    db: &SurrealDb,
    ticket_id: &str,
    ticket: &Value,
    instance_id: &str,
) -> Result<ImportResult, surrealdb::Error> {
    let id_owned = ticket_id.to_string();

    // Compute hash from the distilled metadata, NOT the raw Zoho payload.
    // Zoho bumps `modifiedTime` on every fetch, which would flip the hash
    // every sync and force a full AI-state wipe even when nothing changed.
    let meta = extract_ticket_metadata(ticket);
    let meta_content = serde_json::to_string(&meta).unwrap_or_default();
    let new_hash = murmur3_hex(meta_content.as_bytes());

    // Check existing source_hash
    let existing: Option<Value> = db
        .query("SELECT source_hash FROM document WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", id_owned.clone()))
        .await?
        .take(0)?;

    let old_hash = existing
        .as_ref()
        .and_then(|v| v.get("source_hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if old_hash == new_hash {
        debug!("Ticket {} unchanged (hash={})", ticket_id, &new_hash[..8]);
        return Ok(ImportResult { changed: false, id: id_owned });
    }

    let now = chrono::Utc::now().to_rfc3339();
    let status_str = meta["status"].as_str().unwrap_or("unknown").to_string();

    // UPSERT document (synced) — MERGE preserves unlisted fields.
    // Reaching here means meta content truly changed, so stale AI state is
    // explicitly cleared and retry counters reset. Using CONTENT here would
    // wipe *everything*, including retry counters on records the Observer
    // already marked 'failed' — breaking the circuit breaker.
    let _: Option<Value> = db
        .query(
            "UPSERT type::record($doc_rid) MERGE { \
                 type: 'support_ticket', \
                 status: $status, \
                 meta: $meta, \
                 source_hash: $hash, \
                 source_instance_id: $iid, \
                 summary_status: 'pending', \
                 ai_summary: NONE, \
                 embedding_status: 'pending', \
                 embedding: NONE, \
                 summary_retries: 0, \
                 embedding_retries: 0, \
                 summary_error: NONE, \
                 embedding_error: NONE, \
                 updated_at: $now \
             }",
        )
        .bind(("doc_rid", format!("document:`{}`", id_owned)))
        .bind(("status", status_str))
        .bind(("meta", meta.clone()))
        .bind(("hash", new_hash.clone()))
        .bind(("iid", instance_id.to_string()))
        .bind(("now", now.clone()))
        .await?
        .take(0)?;

    // UPSERT document_raw (local only) — full payload
    let _: Option<Value> = db
        .query("UPSERT type::record($raw_rid) CONTENT { type: 'support_ticket', payload: $payload, updated_at: $now }")
        .bind(("raw_rid", format!("document_raw:`{}`", id_owned)))
        .bind(("payload", ticket.clone()))
        .bind(("now", now))
        .await?
        .take(0)?;

    // Seed an `ai_task` for this ticket if one doesn't exist yet. The
    // orchestrator's LIVE SELECT on `ai_task` picks it up and dispatches
    // to the ReAct executor without any further prompting. Failures here
    // are logged but do NOT abort the import — ticket data must land even
    // if AI triage is momentarily unavailable.
    //
    // Skip noise variants that cannot produce a useful triage:
    //   - empty description (nothing for the model to reason about),
    //   - Re:/Fwd: (reply threads — real content lives in the parent,
    //     already triaged separately),
    //   - internal-sender tickets (support_* / @inbodysupport.eu) —
    //     these are agent-side responses, not customer problems.
    // Without this, the orchestrator spends Gemini budget on tickets
    // whose only sane outcome is ask_human("give me a QC report"), which
    // deadlocks the queue and confuses operators.
    if !should_seed_ai_task(&meta) {
        debug!("Ticket {} skipped ai_task seeding (noise)", ticket_id);
    } else if let Err(e) = db
        .query(
            "LET $exists = (SELECT id FROM ai_task WHERE context.ticket_id = $tid LIMIT 1); \
             IF array::len($exists) == 0 { \
                 INSERT INTO ai_task { \
                     state: 'ready', \
                     owner_instance_id: $iid, \
                     context: { ticket_id: $tid, source: 'zoho_import', meta: $meta }, \
                     created_at: time::now(), \
                     updated_at: time::now() \
                 }; \
             };",
        )
        .bind(("tid", id_owned.clone()))
        .bind(("iid", instance_id.to_string()))
        .bind(("meta", meta))
        .await
    {
        debug!("Ticket {} ai_task seeding failed (non-fatal): {}", ticket_id, e);
    }

    debug!("Ticket {} updated (hash={})", ticket_id, &new_hash[..8]);
    Ok(ImportResult { changed: true, id: id_owned })
}

/// Import or update a Zoho Desk thread.
/// On change, marks the parent ticket for re-summarization.
pub async fn import_thread(
    db: &SurrealDb,
    thread_id: &str,
    ticket_id: &str,
    thread: &Value,
    instance_id: &str,
) -> Result<ImportResult, surrealdb::Error> {
    // Hash only the stable content fields. Zoho may include volatile
    // delivery metadata in thread payloads; hashing the whole object
    // would make every re-fetch look like a change.
    let hash_seed = json!({
        "content": thread.get("content").and_then(|v| v.as_str()).unwrap_or(""),
        "plainText": thread.get("plainText").and_then(|v| v.as_str()).unwrap_or(""),
        "fromEmailAddress": thread.get("fromEmailAddress").and_then(|v| v.as_str()).unwrap_or(""),
        "to": thread.get("to").and_then(|v| v.as_str()).unwrap_or(""),
        "summary": thread.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
    });
    let hash_content = serde_json::to_string(&hash_seed).unwrap_or_default();
    let new_hash = murmur3_hex(hash_content.as_bytes());
    let tid_owned = thread_id.to_string();
    let parent_owned = ticket_id.to_string();

    let existing: Option<Value> = db
        .query("SELECT source_hash FROM document WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", tid_owned.clone()))
        .await?
        .take(0)?;

    let old_hash = existing
        .as_ref()
        .and_then(|v| v.get("source_hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if old_hash == new_hash {
        debug!("Thread {} unchanged (hash={})", thread_id, &new_hash[..8]);
        return Ok(ImportResult { changed: false, id: tid_owned });
    }

    let now = chrono::Utc::now().to_rfc3339();

    // UPSERT thread document (MERGE preserves any unlisted fields) and
    // conditionally mark parent ticket for re-summarization. The WHERE on
    // the parent update guards against stomping an in-flight retry cycle:
    // only terminal states (completed/failed/skipped) are reset. Without
    // this guard, every thread arrival would restart the parent's retry
    // counter and feed the infinite loop the Observer auto-mitigated.
    let _: Option<Value> = db
        .query(
             "UPSERT type::record($doc_rid) MERGE { \
                 type: 'support_thread', \
                 source_hash: $hash, \
                 ticket_id: $tid, \
                 source_instance_id: $iid, \
                 updated_at: $now \
             }; \
             UPDATE document SET \
                 summary_status = 'pending', \
                 ai_summary = NONE, \
                 summary_retries = 0, \
                 summary_error = NONE, \
                 embedding_status = 'pending', \
                 embedding = NONE, \
                 embedding_retries = 0, \
                 embedding_error = NONE, \
                 updated_at = $now \
             WHERE record::id(id) = $tid \
             AND summary_status IN ['completed', 'failed', 'skipped'] \
             AND embedding_status NOTIN ['pending'];",
        )
        .bind(("doc_rid", format!("document:`{}`", tid_owned)))
        .bind(("hash", new_hash.clone()))
        .bind(("tid", parent_owned))
        .bind(("iid", instance_id.to_string()))
        .bind(("now", now.clone()))
        .await?
        .take(0)?;

    // UPSERT document_raw (local only) — full thread payload
    let _: Option<Value> = db
        .query("UPSERT type::record($raw_rid) CONTENT { type: 'support_thread', ticket_id: $tid, payload: $payload, updated_at: $now }")
        .bind(("raw_rid", format!("document_raw:`{}`", tid_owned)))
        .bind(("tid", ticket_id.to_string()))
        .bind(("payload", thread.clone()))
        .bind(("now", now))
        .await?
        .take(0)?;

    // If this thread is outbound (agent → customer), bump the parent ticket's
    // last_outbound_at so the dashboard urgency scale measures "silence from
    // our side" instead of raw age. Only forward-monotonic updates.
    let direction = thread.get("direction").and_then(|v| v.as_str()).unwrap_or("");
    let created_time = thread.get("createdTime").and_then(|v| v.as_str()).unwrap_or("");
    if direction == "out" && !created_time.is_empty() {
        let _: Option<Value> = db
            .query(
                "UPDATE document SET meta.last_outbound_at = $t \
                 WHERE record::id(id) = $tid \
                 AND type = 'support_ticket' \
                 AND (meta.last_outbound_at IS NONE OR meta.last_outbound_at = '' OR meta.last_outbound_at < $t);"
            )
            .bind(("tid", ticket_id.to_string()))
            .bind(("t", created_time.to_string()))
            .await?
            .take(0)?;
    }

    debug!("Thread {} updated, ticket {} marked for re-summarization", thread_id, ticket_id);
    Ok(ImportResult { changed: true, id: tid_owned })
}

/// Persist a Zoho thread attachment: decode base64, write to CAS filestore,
/// INSERT into file_resource (dedup by cas_uuid), RELATE document:$ticket_id
/// -> has_attachment -> file_resource.
///
/// Skips silently when `content_base64` is absent — the scraper only bundles
/// binaries when asked via `includeAttachmentContent` on full sync, so the
/// hourly incremental path naturally no-ops here.
pub async fn import_attachment(
    db: &SurrealDb,
    ticket_id: &str,
    attachment: &Value,
) -> Result<(), anyhow::Error> {
    let b64 = match attachment.get("content_base64").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };
    let content = B64.decode(b64).map_err(|e| anyhow::anyhow!("base64 decode: {e}"))?;
    let name = attachment
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("attachment.bin")
        .to_string();
    let mime = attachment
        .get("content_type")
        .or_else(|| attachment.get("contentType"))
        .and_then(|v| v.as_str())
        .unwrap_or("application/octet-stream")
        .to_string();

    let filestore = FileStore::new(".");
    let saved = filestore
        .save(&content, &name, None, None)
        .await
        .map_err(|e| anyhow::anyhow!("filestore.save: {e}"))?;
    let cas_id = saved.cas_uuid.to_string();

    // INSERT file_resource only if a row with the same cas_uuid doesn't
    // already exist. Without this guard, duplicate uploads (same QC report
    // on multiple tickets) would create parallel file_resource rows with
    // different SurrealDB IDs and break the RELATE dedup below.
    let avatar_b64: Option<String> = saved.avatar_data.as_ref().map(|a| B64.encode(a));
    let _: Option<Value> = db
        .query(
            "LET $exists = (SELECT id FROM file_resource WHERE cas_uuid = $cas LIMIT 1); \
             IF array::len($exists) == 0 { \
                 INSERT INTO file_resource { \
                     cas_uuid: $cas, \
                     hash: $hash, \
                     original_name: $name, \
                     mime_type: $mime, \
                     size_bytes: $size, \
                     avatar_b64: $avatar, \
                     storage_path: $path, \
                     context: 'zoho_attachment', \
                     created_at: time::now(), \
                     updated_at: time::now() \
                 }; \
             };",
        )
        .bind(("cas", cas_id.clone()))
        .bind(("hash", saved.sha256.clone()))
        .bind(("name", name.clone()))
        .bind(("mime", mime.clone()))
        .bind(("size", saved.size_bytes))
        .bind(("avatar", avatar_b64))
        .bind(("path", saved.storage_path.clone()))
        .await?
        .take(0)?;

    // RELATE document:$ticket_id -> has_attachment -> file_resource:$cas_uuid.
    // Idempotent: if the edge already exists (same ticket, same CAS), the
    // duplicate RELATE is dropped by the pre-check on has_attachment.
    let ticket_rid = format!("document:`{}`", ticket_id);
    let _: Option<Value> = db
        .query(
            "LET $fid = (SELECT id FROM file_resource WHERE cas_uuid = $cas LIMIT 1)[0].id; \
             LET $tid = type::record($trid); \
             LET $edge_exists = (SELECT id FROM has_attachment WHERE in = $tid AND out = $fid LIMIT 1); \
             IF $fid IS NOT NONE AND array::len($edge_exists) == 0 { \
                 RELATE $tid -> has_attachment -> $fid \
                     SET created_at = time::now(), label = 'zoho_attachment'; \
             };",
        )
        .bind(("cas", cas_id.clone()))
        .bind(("trid", ticket_rid))
        .await?
        .take(0)?;

    debug!(
        "Attachment saved: ticket={} cas={} size={} mime={}",
        ticket_id,
        &cas_id,
        content.len(),
        mime
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_zip_city_handles_standard_german() {
        let (z, c) = parse_zip_city("Musterstraße 42, 12345 Berlin");
        assert_eq!(z.as_deref(), Some("12345"));
        assert_eq!(c.as_deref(), Some("Berlin"));
    }

    #[test]
    fn parse_zip_city_handles_trailing_comma() {
        let (z, c) = parse_zip_city("Am Markt 1, 80331 München,");
        assert_eq!(z.as_deref(), Some("80331"));
        assert_eq!(c.as_deref(), Some("München"));
    }

    #[test]
    fn parse_zip_city_rejects_short_and_long_numbers() {
        // 4-digit and 6-digit numbers fail the boundary checks.
        let (z4, _) = parse_zip_city("Kaufpreis 1234 EUR");
        assert!(z4.is_none());
        let (z6, _) = parse_zip_city("Ref 123456 offer");
        assert!(z6.is_none());
    }

    #[test]
    fn parse_zip_city_picks_first_valid_zip() {
        let (z, c) = parse_zip_city("Street 10, 10115 Berlin ref 99999");
        assert_eq!(z.as_deref(), Some("10115"));
        assert_eq!(c.as_deref(), Some("Berlin ref 99999"));
    }

    #[test]
    fn parse_zip_city_no_match_on_plain_text() {
        let (z, c) = parse_zip_city("No address here at all");
        assert!(z.is_none());
        assert!(c.is_none());
    }
}
