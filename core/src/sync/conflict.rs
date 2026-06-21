use serde_json::Value;
use tracing::{debug, info};

use crate::db::SurrealDb;
use crate::sync::vector_clock::{ClockRelation, VectorClock};

/// Resolve conflicts between a remote entity and the local copy using VectorClock causality.
///
/// Returns `true` if the DB was written to (insert or update), `false` if local wins or equal.
pub async fn resolve_and_upsert(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    mut remote_entity: Value,
    local_instance_id: &str,
) -> anyhow::Result<bool> {
    // 1. Fetch local entity
    let query = format!(
        "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) = $eid LIMIT 1",
        entity_type
    );
    let local_rows: Vec<Value> = db
        .query(&query)
        .bind(("eid", entity_id.to_string()))
        .await?
        .take(0)?;

    let local_entity = local_rows.into_iter().next();

    // 2. No local copy — just insert with an initialized vclock
    if local_entity.is_none() {
        ensure_vclock(&mut remote_entity);
        let mut clean = remote_entity.clone();
        if let Some(obj) = clean.as_object_mut() {
            obj.remove("id");
        }
        let _: Option<Value> = db
            .upsert((entity_type, entity_id))
            .content(clean)
            .await?;
        debug!("conflict::resolve: inserted new {}:{}", entity_type, entity_id);
        return Ok(true);
    }

    let local_entity = local_entity.unwrap();

    // 3. Parse vector clocks from both sides
    let local_vc = parse_vclock(&local_entity);
    let remote_vc = parse_vclock(&remote_entity);

    match local_vc.compare(&remote_vc) {
        ClockRelation::Before => {
            // Remote is strictly newer — accept it
            let mut merged_vc = remote_vc.clone();
            merged_vc.increment(local_instance_id);
            attach_vclock(&mut remote_entity, &merged_vc);

            let mut clean = remote_entity.clone();
            if let Some(obj) = clean.as_object_mut() {
                obj.remove("id");
            }
            let _: Option<Value> = db
                .upsert((entity_type, entity_id))
                .content(clean)
                .await?;
            debug!(
                "conflict::resolve: remote wins (After) {}:{}",
                entity_type, entity_id
            );
            Ok(true)
        }
        ClockRelation::After | ClockRelation::Equal => {
            // Local is newer or identical — keep ours
            debug!(
                "conflict::resolve: local wins/equal {}:{}",
                entity_type, entity_id
            );
            Ok(false)
        }
        ClockRelation::Concurrent => {
            // True conflict — fall back to timestamp comparison (LWW)
            let local_ts = extract_timestamp(&local_entity);
            let remote_ts = extract_timestamp(&remote_entity);

            info!(
                "conflict::resolve: CONCURRENT {}:{} (local_ts={:?}, remote_ts={:?})",
                entity_type, entity_id, local_ts, remote_ts
            );

            // Merge clocks regardless of who wins
            let mut merged_vc = local_vc.clone();
            merged_vc.merge(&remote_vc);
            merged_vc.increment(local_instance_id);

            if remote_ts > local_ts {
                // Remote wins by timestamp
                attach_vclock(&mut remote_entity, &merged_vc);
                let mut clean = remote_entity.clone();
                if let Some(obj) = clean.as_object_mut() {
                    obj.remove("id");
                }
                let _: Option<Value> = db
                    .upsert((entity_type, entity_id))
                    .content(clean)
                    .await?;
                info!(
                    "conflict::resolve: remote wins (LWW) {}:{}",
                    entity_type, entity_id
                );
                Ok(true)
            } else {
                // Local wins by timestamp (or tie) — update only the vclock
                attach_vclock_to_db(db, entity_type, entity_id, &merged_vc).await?;
                info!(
                    "conflict::resolve: local wins (LWW) {}:{}",
                    entity_type, entity_id
                );
                Ok(false)
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse `_vclock` from a JSON entity, defaulting to empty if missing/malformed.
fn parse_vclock(entity: &Value) -> VectorClock {
    entity
        .get("_vclock")
        .and_then(|v| serde_json::from_value::<VectorClock>(v.clone()).ok())
        .unwrap_or_default()
}

/// Ensure the entity has a `_vclock` field (initialize empty if missing).
fn ensure_vclock(entity: &mut Value) {
    if entity.get("_vclock").is_none() {
        if let Some(obj) = entity.as_object_mut() {
            obj.insert(
                "_vclock".to_string(),
                serde_json::to_value(VectorClock::new()).unwrap(),
            );
        }
    }
}

/// Attach a VectorClock to a JSON entity.
fn attach_vclock(entity: &mut Value, vc: &VectorClock) {
    if let Some(obj) = entity.as_object_mut() {
        obj.insert(
            "_vclock".to_string(),
            serde_json::to_value(vc).unwrap(),
        );
    }
}

/// Update only the `_vclock` field on an existing DB record (local wins, just merge clocks).
async fn attach_vclock_to_db(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    vc: &VectorClock,
) -> anyhow::Result<()> {
    let vc_val = serde_json::to_value(vc)?;
    let q = format!(
        "UPDATE {}:{} MERGE {{ _vclock: $vc }}",
        entity_type, entity_id
    );
    db.query(&q).bind(("vc", vc_val)).await?;
    Ok(())
}

/// Extract a comparable timestamp from the entity. Tries `updated_at` then `updatedAt`.
/// Returns `None` if neither exists or can't be parsed, which sorts before any real timestamp.
fn extract_timestamp(entity: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    for field in &["updated_at", "updatedAt"] {
        if let Some(ts_str) = entity.get(*field).and_then(|v| v.as_str()) {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                return Some(dt);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vclock_missing() {
        let entity = serde_json::json!({"name": "test"});
        let vc = parse_vclock(&entity);
        assert_eq!(vc.0.len(), 0);
    }

    #[test]
    fn test_parse_vclock_present() {
        let entity = serde_json::json!({
            "name": "test",
            "_vclock": {"node_a": 3, "node_b": 1}
        });
        let vc = parse_vclock(&entity);
        assert_eq!(vc.get("node_a"), 3);
        assert_eq!(vc.get("node_b"), 1);
    }

    #[test]
    fn test_ensure_vclock() {
        let mut entity = serde_json::json!({"name": "test"});
        ensure_vclock(&mut entity);
        assert!(entity.get("_vclock").is_some());
    }

    #[test]
    fn test_extract_timestamp() {
        let entity = serde_json::json!({
            "updated_at": "2026-03-31T10:00:00+00:00"
        });
        assert!(extract_timestamp(&entity).is_some());

        let entity2 = serde_json::json!({"name": "no ts"});
        assert!(extract_timestamp(&entity2).is_none());
    }

    #[test]
    fn test_attach_vclock() {
        let mut entity = serde_json::json!({"name": "test"});
        let mut vc = VectorClock::new();
        vc.increment("node_a");
        attach_vclock(&mut entity, &vc);
        let parsed = parse_vclock(&entity);
        assert_eq!(parsed.get("node_a"), 1);
    }
}
