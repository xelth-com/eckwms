use serde_json::Value;
use tracing::{debug, info};

use crate::db::SurrealDb;
use crate::sync::vector_clock::{ClockRelation, VectorClock};

/// Outcome of a conflict resolution — richer than a bool so the caller can keep
/// the Merkle tree consistent on the NO-WRITE paths too. The "pulled N wrote 0
/// never-converges" churn came from only recording the checksum when the DB was
/// written: on `Equal` (already identical) and `LocalNewer` (we kept ours) the
/// local `entity_checksum` was never (re)written, so `compare_trees` kept seeing
/// a divergent leaf and re-pulling the same rows forever.
pub enum ResolveOutcome {
    /// DB was written (insert or remote-wins). Record the REMOTE entity's hash;
    /// counts as an exchanged entity.
    Wrote,
    /// Causally identical — nothing written. Record the LOCAL hash (that is what
    /// is stored; on true equality it equals the remote's, and if it somehow
    /// doesn't we must NOT mask that by recording the remote's). Carries the local
    /// entity. Does NOT count as exchanged.
    AlreadyEqual(Value),
    /// Local version stands (After, or Concurrent→local-wins). Record the LOCAL
    /// entity's hash (that is what's actually stored) so the leaf stops being
    /// re-pulled. Carries the local entity for the caller to checksum.
    LocalNewer(Value),
    /// A DELETE was applied (or a held tombstone stood): resolve already wrote the
    /// tombstone `entity_checksum` itself, so the caller records NOTHING (recording
    /// a content checksum would clobber the tombstone leaf). Not an exchanged write.
    Tombstoned,
}

/// Resolve conflicts between a remote entity and the local copy using VectorClock causality.
///
/// Returns a [`ResolveOutcome`] telling the caller which entity's checksum to
/// record (the actually-stored content) — recording the wrong one would MASK real
/// divergence in a GoBD store, so each branch names the stored content explicitly.
pub async fn resolve_and_upsert(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    mut remote_entity: Value,
    local_instance_id: &str,
) -> anyhow::Result<ResolveOutcome> {
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
    let remote_vc = parse_vclock(&remote_entity);
    let remote_deleted = remote_entity
        .get("_deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // If the local ROW is absent we may still hold a TOMBSTONE (we deleted it). Its
    // clock — not "empty" — is our causal position, so a stale re-create can't
    // silently resurrect a delete.
    let local_tomb_vc: Option<VectorClock> = if local_entity.is_none() {
        let tomb: Vec<Value> = db
            .query(
                "SELECT VALUE vclock FROM entity_checksum \
                 WHERE entity_type = $et AND entity_id = $eid AND deleted = true LIMIT 1",
            )
            .bind(("et", entity_type.to_string()))
            .bind(("eid", entity_id.to_string()))
            .await?
            .take(0)?;
        tomb.into_iter()
            .next()
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value::<VectorClock>(v).unwrap_or_default())
    } else {
        None
    };
    let local_vc = match &local_entity {
        Some(e) => parse_vclock(e),
        None => local_tomb_vc.clone().unwrap_or_default(),
    };

    // ── Incoming TOMBSTONE: a peer deleted this entity ──────────────────────────
    if remote_deleted {
        // A delete is intent: it wins on Before/Equal/Concurrent. Only a strictly
        // NEWER local version (After) survives it (an intervening re-create).
        if local_vc.compare(&remote_vc) == ClockRelation::After {
            debug!("conflict::resolve: local newer than tombstone, keep {}:{}", entity_type, entity_id);
            return Ok(match local_entity {
                Some(e) => ResolveOutcome::LocalNewer(e),
                None => ResolveOutcome::Tombstoned,
            });
        }
        if local_entity.is_some() {
            let _: Option<Value> = db.delete((entity_type, entity_id)).await?;
        }
        // Record the tombstone with the PEER's clock so both nodes land on the same
        // TOMBSTONE leaf (done explicitly, not left to the live-watch, so it holds
        // even where the LIVE stream isn't running).
        crate::sync::merkle::MerkleService::new(db.clone(), local_instance_id.to_string())
            .record_tombstone(
                entity_type,
                entity_id,
                remote_entity.get("_vclock").unwrap_or(&Value::Null),
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        debug!("conflict::resolve: applied tombstone {}:{}", entity_type, entity_id);
        return Ok(ResolveOutcome::Tombstoned);
    }

    // ── Incoming LIVE entity, local ROW absent ──────────────────────────────────
    if local_entity.is_none() {
        // A held tombstone blocks resurrection unless the incoming create STRICTLY
        // dominates it (tomb Before remote). Equal/Concurrent/After → stay deleted.
        if let Some(tomb_vc) = &local_tomb_vc {
            if tomb_vc.compare(&remote_vc) != ClockRelation::Before {
                debug!("conflict::resolve: tombstone wins over incoming {}:{}", entity_type, entity_id);
                return Ok(ResolveOutcome::Tombstoned);
            }
        }
        ensure_vclock(&mut remote_entity);
        write_adopted(db, entity_type, entity_id, remote_entity).await?;
        debug!("conflict::resolve: inserted new {}:{}", entity_type, entity_id);
        return Ok(ResolveOutcome::Wrote);
    }

    // ── Incoming LIVE entity, local LIVE row — vclock causality ─────────────────
    let local_entity = local_entity.unwrap();

    match local_vc.compare(&remote_vc) {
        ClockRelation::Before => {
            // Remote is strictly newer — ADOPT its clock as-is. (Previously this
            // incremented the local component, which made a node that merely
            // *applied* a peer version look causally After it → the peer's next
            // real edit then read as Concurrent and everything degraded to LWW
            // with unbounded clock growth. Standard behaviour: adopt on Before.)
            attach_vclock(&mut remote_entity, &remote_vc);
            write_adopted(db, entity_type, entity_id, remote_entity).await?;
            debug!(
                "conflict::resolve: remote wins (After) {}:{}",
                entity_type, entity_id
            );
            Ok(ResolveOutcome::Wrote)
        }
        ClockRelation::Equal => {
            // Equal clock SHOULD mean identical content. If the hashes match →
            // truly converged. If they DIFFER, this is a REAL divergence that
            // never advanced the clock — legacy null/empty-`_vclock` rows (e.g.
            // trips uploaded before the writer stamped `_vclock`): 189 holds a
            // stale OPEN trip, the kiosk an ENDED one, both `_vclock={}` → Equal →
            // the old "local wins" kept 189's stale copy FOREVER (pulled N wrote 0,
            // converged, never adopting). Resolve it by LWW (updated_at) instead,
            // so the newer side wins and the pair actually converges.
            let lh = crate::sync::merkle::compute_content_hash(&local_entity).unwrap_or_default();
            let rh = crate::sync::merkle::compute_content_hash(&remote_entity).unwrap_or_default();
            if lh == rh {
                debug!("conflict::resolve: equal (converged) {}:{}", entity_type, entity_id);
                Ok(ResolveOutcome::AlreadyEqual(local_entity))
            } else {
                debug!("conflict::resolve: equal-clock but content differs → LWW {}:{}", entity_type, entity_id);
                resolve_lww_conflict(db, entity_type, entity_id, local_entity, remote_entity, &local_vc, &remote_vc, local_instance_id).await
            }
        }
        ClockRelation::After => {
            // Local is strictly newer — keep ours; caller records the LOCAL hash.
            debug!("conflict::resolve: local wins (After) {}:{}", entity_type, entity_id);
            Ok(ResolveOutcome::LocalNewer(local_entity))
        }
        ClockRelation::Concurrent => {
            resolve_lww_conflict(db, entity_type, entity_id, local_entity, remote_entity, &local_vc, &remote_vc, local_instance_id).await
        }
    }
}

/// Last-write-wins resolution (by `updated_at`, FROZEN `tiebreak_hash` on a tie),
/// shared by the `Concurrent` branch and the `Equal`-clock-but-content-differs
/// case. Merges the clocks componentwise (max) — WITHOUT a local increment: for
/// a Concurrent pair the merge alone strictly dominates the remote clock, so the
/// kept side becomes causally After while the clock converges to a fixpoint
/// instead of growing on every keep-local (the 1371-bump inflation).
async fn resolve_lww_conflict(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    local_entity: Value,
    mut remote_entity: Value,
    local_vc: &VectorClock,
    remote_vc: &VectorClock,
    local_instance_id: &str,
) -> anyhow::Result<ResolveOutcome> {
    let local_ts = extract_timestamp(&local_entity);
    let remote_ts = extract_timestamp(&remote_entity);
    info!(
        "conflict::resolve: LWW {}:{} (local_ts={:?}, remote_ts={:?})",
        entity_type, entity_id, local_ts, remote_ts
    );

    // Merge the two clocks componentwise (max). Do NOT increment the local
    // component (2026-07-18, mixed-build churn fix): when the relation was
    // Concurrent, the merge already strictly dominates the remote clock, which
    // makes the keep/adopt decision a causal event on its own. Incrementing on
    // every keep-local is what let two peers with disagreeing verdicts
    // ping-pong a document's vclock to 1371 bumps — each bump re-made the pair
    // Concurrent, forcing another LWW and another bump. Merge-only reaches a
    // fixpoint at the componentwise max instead of growing unboundedly.
    let mut merged_vc = local_vc.clone();
    merged_vc.merge(remote_vc);

    // Ownership rule (2026-07-09, project_sync_ownership): entities that carry
    // `home_instance_id` are authored by their HOME node (creator = authority,
    // transferable via claim-home). When both copies agree on the home, the
    // version that has seen MORE of the home node's history — the higher home
    // component in its vector clock — wins outright; LWW only breaks a tie.
    // This stops a peer's incidental touch (a UI edit with a "newer"
    // timestamp) from overriding the authority's version of its own record.
    // If the copies DISAGREE on the home (an in-flight transfer), fall
    // through to LWW — the new home's subsequent writes dominate naturally.
    let home_verdict = ownership_verdict(&local_entity, &remote_entity, local_vc, remote_vc);

    // Timestamp TIE (incl. both-missing) needs a tiebreak BOTH sides agree on, or
    // each keeps its own copy and the pair re-exchanges the same rows forever.
    // Content hash is symmetric: the side whose hash sorts lower adopts the other's.
    let remote_wins = match home_verdict {
        Some(v) => {
            info!("conflict::resolve: ownership rule decides {}:{} (remote_wins={})", entity_type, entity_id, v);
            v
        }
        None if remote_ts == local_ts => {
            // Tie-break with the FROZEN, version-independent comparator — NOT
            // compute_content_hash, whose field-set evolves per build. A
            // version-dependent tiebreak lets two builds reach OPPOSITE verdicts
            // on the same pair → both keep their own copy AND bump → the write
            // ping-pong that inflated one document to 1371 vclock bumps. Same
            // polarity: the side whose hash sorts lower adopts the other's.
            let local_hash = crate::sync::merkle::tiebreak_hash(&local_entity);
            let remote_hash = crate::sync::merkle::tiebreak_hash(&remote_entity);
            remote_hash > local_hash
        }
        None => remote_ts > local_ts,
    };

    if remote_wins {
        attach_vclock(&mut remote_entity, &merged_vc);
        write_adopted(db, entity_type, entity_id, remote_entity).await?;
        info!("conflict::resolve: remote wins (LWW) {}:{}", entity_type, entity_id);
        Ok(ResolveOutcome::Wrote)
    } else {
        // Local wins by timestamp (or tie). The stored content is still LOCAL,
        // so the caller records the local hash. Persist the merged clock ONLY if
        // it actually advanced past what's already stored: for a Concurrent pair
        // the merge adds the remote's component (strictly After → write once),
        // but if local already dominates remote (merge is a no-op — the Equal
        // relation below, or a defensive re-resolution of an already-merged
        // pair) we skip the DB write entirely. Re-writing an identical _vclock is
        // pure churn — exactly the per-cycle bump that inflated one doc to 1371.
        if merged_vc.compare(local_vc) != ClockRelation::Equal {
            attach_vclock_to_db(db, entity_type, entity_id, &merged_vc).await?;
        } else {
            debug!(
                "conflict::resolve: local wins (LWW), clock already dominates — no vclock write {}:{}",
                entity_type, entity_id
            );
        }
        info!("conflict::resolve: local wins (LWW) {}:{}", entity_type, entity_id);
        Ok(ResolveOutcome::LocalNewer(local_entity))
    }
}

/// Ownership-rule verdict for a concurrent/divergent pair: `Some(true)` =
/// remote wins, `Some(false)` = local wins, `None` = rule doesn't apply
/// (no/conflicting `home_instance_id`, or both sides saw the same amount of
/// the home node's history) → caller falls back to LWW.
fn ownership_verdict(
    local_entity: &Value,
    remote_entity: &Value,
    local_vc: &VectorClock,
    remote_vc: &VectorClock,
) -> Option<bool> {
    let lh = local_entity.get("home_instance_id").and_then(|v| v.as_str());
    let rh = remote_entity.get("home_instance_id").and_then(|v| v.as_str());
    let home = match (lh, rh) {
        (Some(a), Some(b)) if a == b => a,
        (Some(a), None) => a,
        (None, Some(b)) => b,
        _ => return None, // absent on both, or an in-flight transfer disagrees
    };
    let l = local_vc.get(home);
    let r = remote_vc.get(home);
    if l == r { None } else { Some(r > l) }
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

/// Write an ADOPTED remote entity (insert-new / remote-wins), shared by all three
/// adopt paths. The mesh payload is JSON, so every SurrealDB datetime crossed the
/// wire as an RFC3339 STRING (`…Z` + nanos) and `.content()` stores it back as
/// that string. For `updated_at` the string later poisons `updated_at + duration`
/// backoff arithmetic (the a0c275d/133279d "stuck documents" class — the
/// unexplained Z+nanos rows of 2026-07-21 were exactly these adoptions), so
/// re-coerce it to a real datetime right after the write, KEEPING the peer's
/// value (it is the LWW timestamp; only the type changes). `updated_at` is in
/// `merkle::IGNORED_FIELDS`, so the coercion moves neither the content hash nor
/// the vclock. Addressed by `record::id(id) = $eid`, NOT a `type::record`
/// literal — all-digit ids would misparse (see [`attach_vclock_to_db`]).
async fn write_adopted(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    mut adopted: Value,
) -> anyhow::Result<()> {
    if let Some(obj) = adopted.as_object_mut() {
        obj.remove("id");
    }
    // Graph edges can't take the generic CONTENT path — see below.
    if crate::sync::engine::is_relation_entity_type(entity_type) {
        return write_adopted_relation(db, entity_type, entity_id, adopted).await;
    }
    let _: Option<Value> = db.upsert((entity_type, entity_id)).content(adopted).await?;
    // TOTAL expression (IF/ELSE), not a bare cast: SurrealDB v3 evaluates SET
    // eagerly on scanned rows before WHERE, and a row MISSING `updated_at` would
    // fail the `<datetime> NONE` cast — erroring the whole adoption. `.check()`
    // so a statement-level error surfaces instead of being swallowed.
    let coerce = format!(
        "UPDATE {} SET updated_at = \
             IF type::is_string(updated_at) THEN <datetime> updated_at ELSE updated_at END \
         WHERE record::id(id) = $eid AND type::is_string(updated_at)",
        entity_type
    );
    db.query(&coerce)
        .bind(("eid", entity_id.to_string()))
        .await?
        .check()?;
    Ok(())
}

/// Adopt a GRAPH-EDGE row (`DEFINE TABLE … TYPE RELATION`). SurrealDB v3 refuses
/// to materialize a relation through `UPSERT … CONTENT` — verified 2026-07-25,
/// both with string and with `type::record()` endpoints it answers *"Found
/// record: `has_attachment:x` which is not a relation, but expected a RELATION"*.
/// Only `RELATE` / `INSERT RELATION` create one, and `in`/`out` must be real
/// records: the same statement with `in` as a STRING fails with *"Cannot execute
/// INSERT statement where property 'in' is: …"*. Over the mesh both endpoints
/// necessarily arrive as Thing strings (``document:`53451000033373254` ``), so
/// this path coerces them back with `type::record()`.
///
/// DELETE-then-INSERT (rather than `ON DUPLICATE KEY UPDATE`) so an adopted edge
/// whose endpoints CHANGED lands on the peer's exact shape — `in`/`out` are not
/// updatable in place, and re-pointing an edge is indistinguishable from a new
/// one. Idempotent, and the edge id is preserved, so the merkle leaf converges.
///
/// `created_at` is coerced from the wire's RFC3339 string back to a real
/// datetime for the same reason `updated_at` is on the generic path: it is what
/// `list_attachments` sorts on, and mixing strings with datetimes in `ORDER BY`
/// puts adopted edges in a different order than natively-created ones. Both are
/// in `merkle::IGNORED_FIELDS`, so neither coercion moves the content hash.
async fn write_adopted_relation(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    mut adopted: Value,
) -> anyhow::Result<()> {
    let (in_rid, out_rid) = {
        let obj = adopted
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("relation adopt {}:{}: not an object", entity_type, entity_id))?;
        let end = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(str::to_string);
        match (end("in"), end("out")) {
            (Some(i), Some(o)) => (i, o),
            _ => {
                // A relation row without endpoints is unrepresentable; writing it
                // as a plain record would poison the table for every later RELATE.
                return Err(anyhow::anyhow!(
                    "relation adopt {}:{}: missing/non-string in|out",
                    entity_type,
                    entity_id
                ));
            }
        }
    };
    if let Some(obj) = adopted.as_object_mut() {
        obj.remove("in");
        obj.remove("out");
    }

    // Table name is interpolated (SurrealQL can't bind a table in INSERT INTO);
    // safe because it comes from RELATION_ENTITY_TYPES, never from the peer.
    let q = format!(
        "DELETE type::record($tb, $eid); \
         INSERT RELATION INTO {tb} {{ id: $eid, in: type::record($in), out: type::record($out) }}; \
         UPDATE type::record($tb, $eid) MERGE $rest; \
         UPDATE type::record($tb, $eid) SET \
            created_at = IF type::is_string(created_at) THEN <datetime> created_at ELSE created_at END, \
            updated_at = IF type::is_string(updated_at) THEN <datetime> updated_at ELSE updated_at END;",
        tb = entity_type
    );
    db.query(&q)
        .bind(("tb", entity_type.to_string()))
        .bind(("eid", entity_id.to_string()))
        .bind(("in", in_rid))
        .bind(("out", out_rid))
        .bind(("rest", adopted))
        .await?
        .check()?;
    Ok(())
}

/// Update only the `_vclock` field on an existing DB record (local wins, just merge clocks).
///
/// Addressed by `record::id(id) = $eid` (same as the local fetch above), NOT by a
/// `table:id` literal: all-digit string ids are stored backtick-quoted
/// (``partner:`00193``` — an unquoted literal parses as an integer id and hits a
/// different record) and UUID leaves contain dashes that don't parse unquoted.
async fn attach_vclock_to_db(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    vc: &VectorClock,
) -> anyhow::Result<()> {
    let vc_val = serde_json::to_value(vc)?;
    let q = format!(
        "UPDATE {} MERGE {{ _vclock: $vc }} WHERE record::id(id) = $eid",
        entity_type
    );
    db.query(&q)
        .bind(("vc", vc_val))
        .bind(("eid", entity_id.to_string()))
        .await?;
    Ok(())
}

/// Compute the next `_vclock` value for a LOCAL mutation: take the entity's
/// current clock (or empty) and advance THIS node's component by one. Returns a
/// JSON value ready to `MERGE { _vclock: <this> }` into the record, so a local
/// change to a synced/hashed field becomes causally `After` peers and propagates
/// via the normal conflict-resolution path instead of being silently dropped
/// (`local wins/equal, wrote 0`) — the bug behind perpetual merkle non-convergence.
///
/// `_vclock` is itself in `merkle::IGNORED_FIELDS`, so bumping it does NOT change
/// the content hash — it only advances causality. Callers MUST gate this on an
/// actual content change, or every no-op write storms the mesh with re-syncs.
pub fn next_local_vclock(current: Option<&Value>, local_instance_id: &str) -> Value {
    let mut vc = current
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<VectorClock>(v.clone()).ok())
        .unwrap_or_default();
    vc.increment(local_instance_id);
    serde_json::to_value(vc).unwrap_or_else(|_| serde_json::json!({}))
}

/// Like [`bump_local_vclock`] but addressed by `(entity_type, record-id leaf)`
/// via `record::id(id) = $eid` — safe for leaf ids containing dots/dashes
/// (usernames, UUIDs) that a `type::record` literal would misparse. Use after
/// a direct `UPDATE … WHERE record::id(id) = $eid` on a synced table.
pub async fn bump_local_vclock_by_leaf(
    db: &SurrealDb,
    entity_type: &str,
    entity_id: &str,
    local_instance_id: &str,
) -> anyhow::Result<()> {
    let q = format!(
        "SELECT VALUE _vclock FROM {} WHERE record::id(id) = $eid LIMIT 1",
        entity_type
    );
    let rows: Vec<Value> = db
        .query(&q)
        .bind(("eid", entity_id.to_string()))
        .await?
        .take(0)?;
    let next = next_local_vclock(rows.into_iter().next().as_ref(), local_instance_id);
    let update = format!(
        "UPDATE {} MERGE {{ _vclock: $vc }} WHERE record::id(id) = $eid",
        entity_type
    );
    db.query(&update)
        .bind(("vc", next))
        .bind(("eid", entity_id.to_string()))
        .await?;
    Ok(())
}

/// Read-increment-write the `_vclock` of a single record for a LOCAL mutation.
/// `record_thing` is a Thing literal string (e.g. ``"document:`123`"``) resolved
/// via `type::record`. Use after a direct `UPDATE … SET <synced field>` that
/// bypasses [`resolve_and_upsert`]; gate on an actual change to avoid storms.
pub async fn bump_local_vclock(
    db: &SurrealDb,
    record_thing: &str,
    local_instance_id: &str,
) -> anyhow::Result<()> {
    let rows: Vec<Value> = db
        .query("SELECT VALUE _vclock FROM type::record($rid) LIMIT 1")
        .bind(("rid", record_thing.to_string()))
        .await?
        .take(0)?;
    let next = next_local_vclock(rows.into_iter().next().as_ref(), local_instance_id);
    db.query("UPDATE type::record($rid) MERGE { _vclock: $vc }")
        .bind(("rid", record_thing.to_string()))
        .bind(("vc", next))
        .await?;
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
    fn test_next_local_vclock_from_missing() {
        // No prior clock → this node's component starts at 1.
        let v = next_local_vclock(None, "node_a");
        let vc = parse_vclock(&serde_json::json!({ "_vclock": v }));
        assert_eq!(vc.get("node_a"), 1);
    }

    #[test]
    fn test_next_local_vclock_advances_and_preserves_peers() {
        let current = serde_json::json!({ "node_a": 2, "node_b": 5 });
        let v = next_local_vclock(Some(&current), "node_a");
        let vc = parse_vclock(&serde_json::json!({ "_vclock": v }));
        assert_eq!(vc.get("node_a"), 3, "local component advances");
        assert_eq!(vc.get("node_b"), 5, "peer components preserved");
    }

    #[test]
    fn test_next_local_vclock_null_is_empty() {
        let v = next_local_vclock(Some(&Value::Null), "node_a");
        let vc = parse_vclock(&serde_json::json!({ "_vclock": v }));
        assert_eq!(vc.get("node_a"), 1);
    }

    #[test]
    fn ownership_home_history_beats_newer_timestamp_side() {
        // KIOSK is home for its trip; the dev node touched the row later (newer
        // updated_at) but saw LESS of the kiosk's history → kiosk copy must win.
        let local = serde_json::json!({ "home_instance_id": "kiosk", "state": "stale-open" });
        let remote = serde_json::json!({ "home_instance_id": "kiosk", "state": "ended" });
        let lvc: VectorClock = serde_json::from_value(serde_json::json!({ "kiosk": 2, "dev": 5 })).unwrap();
        let rvc: VectorClock = serde_json::from_value(serde_json::json!({ "kiosk": 4 })).unwrap();
        assert_eq!(ownership_verdict(&local, &remote, &lvc, &rvc), Some(true), "remote (home-richer) wins");
        assert_eq!(ownership_verdict(&remote, &local, &rvc, &lvc), Some(false), "mirrored: local wins");
    }

    #[test]
    fn ownership_falls_back_on_tie_missing_or_transfer() {
        let lvc: VectorClock = serde_json::from_value(serde_json::json!({ "kiosk": 3 })).unwrap();
        let rvc: VectorClock = serde_json::from_value(serde_json::json!({ "kiosk": 3 })).unwrap();
        // Same home component → tie → LWW fallback.
        let a = serde_json::json!({ "home_instance_id": "kiosk" });
        assert_eq!(ownership_verdict(&a, &a, &lvc, &rvc), None);
        // No home field at all → rule doesn't apply.
        let plain = serde_json::json!({ "state": "x" });
        assert_eq!(ownership_verdict(&plain, &plain, &lvc, &rvc), None);
        // In-flight transfer: homes disagree → LWW fallback.
        let b = serde_json::json!({ "home_instance_id": "dev" });
        assert_eq!(ownership_verdict(&a, &b, &lvc, &rvc), None);
        // One side lacks the field → the present side's home applies.
        let lvc2: VectorClock = serde_json::from_value(serde_json::json!({ "kiosk": 1 })).unwrap();
        assert_eq!(ownership_verdict(&plain, &a, &lvc2, &rvc), Some(true));
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

    // ── Keep-local without vclock inflation (hygiene [M], 2026-07-18) ─────────
    //
    // These assert the SEMANTICS of the merge-without-increment change in
    // `resolve_lww_conflict`: the LWW branch now does `merged = local.merge(remote)`
    // with no `increment(local_instance_id)`. The DB round-trip is exercised by
    // the integration suite; here we prove the clock algebra converges.

    fn vc(pairs: &[(&str, i64)]) -> VectorClock {
        let mut c = VectorClock::new();
        for (k, v) in pairs {
            c.0.insert((*k).to_string(), *v);
        }
        c
    }

    #[test]
    fn keep_local_merge_dominates_remote_without_increment() {
        // Concurrent pair: local knows node_a, remote knows node_b.
        let local = vc(&[("node_a", 1)]);
        let remote = vc(&[("node_b", 1)]);
        assert_eq!(local.compare(&remote), ClockRelation::Concurrent);

        let mut merged = local.clone();
        merged.merge(&remote);
        // Merge alone (NO increment) already strictly dominates the remote clock,
        // so the keep-local decision is causal.
        assert_eq!(merged.compare(&remote), ClockRelation::After);
        // And it did not invent a new component or over-count either side.
        assert_eq!(merged.get("node_a"), 1);
        assert_eq!(merged.get("node_b"), 1);
    }

    #[test]
    fn keep_local_is_idempotent_no_clock_growth() {
        // Re-resolving the SAME unchanged pair after the first merge must not
        // grow the clock — this is the defensive `merged == local ⇒ skip write`.
        let local = vc(&[("node_a", 1)]);
        let remote = vc(&[("node_b", 1)]);

        let mut clock = local.clone();
        clock.merge(&remote); // first resolution
        let after_first = clock.clone();

        // The DB now holds `after_first`. Every subsequent resolution reads that
        // back as "local" and re-merges the same remote → fixpoint, no growth.
        for _ in 0..5 {
            let mut next = after_first.clone();
            next.merge(&remote);
            assert_eq!(next.compare(&after_first), ClockRelation::Equal, "clock stable across N resolutions");
        }
        // The would-be DB write is skipped because merged == local (Equal).
        let mut re = after_first.clone();
        re.merge(&remote);
        assert_eq!(re.compare(&after_first), ClockRelation::Equal);
        assert_eq!(after_first.get("node_a"), 1);
        assert_eq!(after_first.get("node_b"), 1);
    }

    #[test]
    fn ping_pong_converges_to_componentwise_max_not_growth() {
        // Two nodes with disagreeing LWW verdicts each keep-local and merge the
        // other's clock, alternating. With increment this grew without bound
        // (the 1371-bump incident); merge-only reaches a fixpoint.
        let mut a = vc(&[("node_a", 1)]);
        let mut b = vc(&[("node_b", 1)]);

        for _ in 0..10 {
            let mut na = a.clone();
            na.merge(&b); // node A keeps local, merges B — no increment
            a = na;
            let mut nb = b.clone();
            nb.merge(&a); // node B keeps local, merges A — no increment
            b = nb;
        }

        // Both settle at the componentwise max {node_a:1, node_b:1} — no growth.
        assert_eq!(a.compare(&b), ClockRelation::Equal);
        assert_eq!(a.get("node_a"), 1);
        assert_eq!(a.get("node_b"), 1);
        assert_eq!(b.get("node_a"), 1);
        assert_eq!(b.get("node_b"), 1);
    }
}
