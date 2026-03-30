use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use std::io::Cursor;
use tracing::debug;

/// Compute a 128-bit MurmurHash3 of the given bytes, returned as 32-char hex string.
pub fn murmur3_hex(data: &[u8]) -> String {
    let hash = murmur3::murmur3_x64_128(&mut Cursor::new(data), 0)
        .expect("murmur3 should not fail on in-memory data");
    format!("{:032x}", hash)
}

/// Result of an import operation.
pub struct ImportResult {
    pub changed: bool,
    pub id: String,
}

/// Import or update a Zoho Desk ticket into the `document` table.
/// Computes MurmurHash3 for change detection. On change, sets `summary_status = "pending"`.
pub async fn import_ticket(
    db: &SurrealDb,
    ticket_id: &str,
    ticket: &Value,
) -> Result<ImportResult, surrealdb::Error> {
    let content = serde_json::to_string(ticket).unwrap_or_default();
    let new_hash = murmur3_hex(content.as_bytes());

    let id_owned = ticket_id.to_string();

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

    // Upsert with new hash + mark for summarization
    let _: Option<Value> = db
        .upsert(("document", id_owned.as_str()))
        .content(json!({
            "type": "support_ticket",
            "status": ticket.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "payload": ticket,
            "source_hash": new_hash,
            "summary_status": "pending",
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }))
        .await?;

    debug!("Ticket {} updated (hash={})", ticket_id, &new_hash[..8]);
    Ok(ImportResult { changed: true, id: id_owned })
}

/// Import or update a Zoho Desk thread into the `document` table.
/// On change, marks the parent ticket for re-summarization.
pub async fn import_thread(
    db: &SurrealDb,
    thread_id: &str,
    ticket_id: &str,
    thread: &Value,
) -> Result<ImportResult, surrealdb::Error> {
    let content = serde_json::to_string(thread).unwrap_or_default();
    let new_hash = murmur3_hex(content.as_bytes());
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

    // Upsert thread document
    let _: Option<Value> = db
        .upsert(("document", tid_owned.as_str()))
        .content(json!({
            "type": "support_thread",
            "payload": thread,
            "source_hash": new_hash,
            "ticket_id": ticket_id,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }))
        .await?;

    // Mark parent ticket for re-summarization
    let _: Option<Value> = db
        .query("UPDATE document SET summary_status = 'pending' WHERE record::id(id) = $tid")
        .bind(("tid", parent_owned))
        .await?
        .take(0)?;

    debug!("Thread {} updated, ticket {} marked for re-summarization", thread_id, ticket_id);
    Ok(ImportResult { changed: true, id: tid_owned })
}
