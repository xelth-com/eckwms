use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// A node in the two-level Merkle tree.
///
/// Level 0 = root (children are bucket hashes).
/// Level 1 = bucket (children are entity_id → content_hash pairs).
///
/// The tree enables O(log n) sync: compare roots, drill into differing
/// buckets, then exchange only the entities whose hashes diverge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleNode {
    pub level: u8,
    pub key: String,
    pub hash: String,
    pub children: BTreeMap<String, String>,
}

impl MerkleNode {
    pub fn new(level: u8, key: String) -> Self {
        Self {
            level,
            key,
            hash: String::new(),
            children: BTreeMap::new(),
        }
    }
}

/// Request payload for Merkle tree comparison between peers.
#[derive(Debug, Serialize, Deserialize)]
pub struct MerkleRequest {
    pub entity_type: String,
    pub level: u8,
    pub bucket: Option<String>,
}

// ─── Hashing ─────────────────────────────────────────────────────────────────

/// Compute deterministic SHA-256 hash for a bucket of (entity_id → hash) pairs.
/// BTreeMap guarantees sorted iteration — same entries always produce same hash.
pub fn compute_bucket_hash(items: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (id, hash) in items {
        hasher.update(id.as_bytes());
        hasher.update(b":");
        hasher.update(hash.as_bytes());
        hasher.update(b";");
    }
    hex::encode(hasher.finalize())
}

/// Compute root hash from bucket hashes (sorted by bucket key).
pub fn compute_root_hash(buckets: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (bucket_key, hash) in buckets {
        hasher.update(bucket_key.as_bytes());
        hasher.update(b":");
        hasher.update(hash.as_bytes());
        hasher.update(b";");
    }
    hex::encode(hasher.finalize())
}

/// Simple bucketing: first character of entity_id, lowercased.
pub fn get_bucket_index(entity_id: &str) -> String {
    entity_id
        .chars()
        .next()
        .unwrap_or('_')
        .to_lowercase()
        .to_string()
}

// ─── Tree Diffing ────────────────────────────────────────────────────────────

/// Compare local vs remote hash maps (works at both bucket and entity level).
/// Returns `(need_from_remote, need_to_push)` — keys that each side is missing
/// or has a different hash for.
pub fn compare_trees(
    local: &BTreeMap<String, String>,
    remote: &BTreeMap<String, String>,
) -> (Vec<String>, Vec<String>) {
    let mut need_from_remote = Vec::new();
    let mut need_to_push = Vec::new();

    for (r_key, r_hash) in remote {
        match local.get(r_key) {
            Some(l_hash) if l_hash == r_hash => {} // identical
            Some(_) => {
                // Both have it but hashes differ — exchange both ways
                need_from_remote.push(r_key.clone());
                need_to_push.push(r_key.clone());
            }
            None => need_from_remote.push(r_key.clone()),
        }
    }

    for l_key in local.keys() {
        if !remote.contains_key(l_key) {
            need_to_push.push(l_key.clone());
        }
    }

    (need_from_remote, need_to_push)
}

// ─── Checksum Calculator ─────────────────────────────────────────────────────

/// Fields to strip before hashing — timestamps that change on sync but don't
/// represent content changes. Covers snake_case, camelCase, and PascalCase.
const IGNORED_FIELDS: &[&str] = &[
    "created_at",
    "updated_at",
    "last_synced_at",
    "synced_by",
    "CreatedAt",
    "UpdatedAt",
    "LastSyncedAt",
    "createdAt",
    "updatedAt",
    "lastSyncedAt",
];

/// Compute a deterministic SHA-256 content hash from any serde_json::Value.
/// Strips timestamp/sync-only fields so that two nodes with identical business
/// data produce the same hash regardless of when they were last synced.
pub fn compute_content_hash(value: &Value) -> Option<String> {
    let map = match value {
        Value::Object(m) => m,
        _ => return None,
    };

    let mut sorted: BTreeMap<&str, &Value> = BTreeMap::new();
    for (k, v) in map {
        if !IGNORED_FIELDS.contains(&k.as_str()) {
            sorted.insert(k.as_str(), v);
        }
    }

    let mut canonical = String::new();
    for (k, v) in &sorted {
        if v.is_null() {
            canonical.push_str(&format!("{}:null;", k));
        } else if let Some(s) = v.as_str() {
            // Normalize RFC3339 timestamps to UTC for cross-timezone determinism
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
                canonical.push_str(&format!(
                    "{}:{};",
                    k,
                    t.with_timezone(&chrono::Utc).to_rfc3339()
                ));
                continue;
            }
            canonical.push_str(&format!("{}:{};", k, s));
        } else {
            canonical.push_str(&format!("{}:{};", k, v));
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Some(hex::encode(hasher.finalize()))
}

// ─── SurrealDB-backed Merkle Service ─────────────────────────────────────────

use crate::db::SurrealDb;
use surrealdb::types::SurrealValue;

/// Lightweight row from the `entity_checksum` table.
#[derive(Debug, Clone, Deserialize, SurrealValue)]
struct ChecksumRow {
    entity_id: String,
    full_hash: String,
}

/// Builds Merkle trees on demand from the `entity_checksum` table in SurrealDB.
pub struct MerkleService {
    db: SurrealDb,
    instance_id: String,
}

impl MerkleService {
    pub fn new(db: SurrealDb, instance_id: String) -> Self {
        Self { db, instance_id }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Get tree state at the requested level.
    pub async fn get_state(&self, req: &MerkleRequest) -> Result<MerkleNode, String> {
        match req.level {
            0 => self.get_root(&req.entity_type).await,
            1 if req.bucket.is_some() => {
                self.get_bucket(&req.entity_type, req.bucket.as_ref().unwrap())
                    .await
            }
            _ => Err("Invalid merkle request: level must be 0, or 1 with bucket".into()),
        }
    }

    /// Level 0: root node whose children are bucket_key → bucket_hash.
    async fn get_root(&self, entity_type: &str) -> Result<MerkleNode, String> {
        let et = entity_type.to_string();
        let rows: Vec<ChecksumRow> = self
            .db
            .query("SELECT entity_id, full_hash FROM entity_checksum WHERE entity_type = $et")
            .bind(("et", et))
            .await
            .map_err(|e| e.to_string())?
            .take(0)
            .map_err(|e| e.to_string())?;

        let mut bucket_items: std::collections::HashMap<String, BTreeMap<String, String>> =
            std::collections::HashMap::new();

        for row in rows {
            let b_key = get_bucket_index(&row.entity_id);
            bucket_items
                .entry(b_key)
                .or_default()
                .insert(row.entity_id, row.full_hash);
        }

        let mut buckets: BTreeMap<String, String> = BTreeMap::new();
        for (b_key, items) in &bucket_items {
            buckets.insert(b_key.clone(), compute_bucket_hash(items));
        }

        let root_hash = compute_root_hash(&buckets);

        Ok(MerkleNode {
            level: 0,
            key: "root".to_string(),
            hash: root_hash,
            children: buckets,
        })
    }

    /// Level 1: single bucket with entity_id → content_hash children.
    async fn get_bucket(&self, entity_type: &str, bucket: &str) -> Result<MerkleNode, String> {
        let et = entity_type.to_string();
        let rows: Vec<ChecksumRow> = self
            .db
            .query("SELECT entity_id, full_hash FROM entity_checksum WHERE entity_type = $et")
            .bind(("et", et))
            .await
            .map_err(|e| e.to_string())?
            .take(0)
            .map_err(|e| e.to_string())?;

        let mut items: BTreeMap<String, String> = BTreeMap::new();
        for row in rows {
            if get_bucket_index(&row.entity_id) == bucket {
                items.insert(row.entity_id, row.full_hash);
            }
        }

        let hash = compute_bucket_hash(&items);

        Ok(MerkleNode {
            level: 1,
            key: bucket.to_string(),
            hash,
            children: items,
        })
    }

    /// Upsert an entity's content hash into the `entity_checksum` table.
    /// Called after local mutations or incoming syncs to keep the tree fresh.
    pub async fn upsert_checksum(
        &self,
        entity_type: &str,
        entity_id: &str,
        content_hash: &str,
    ) -> Result<(), String> {
        let et = entity_type.to_string();
        let eid = entity_id.to_string();
        let ch = content_hash.to_string();
        let src = self.instance_id.clone();
        self.db
            .query(
                "UPSERT entity_checksum SET \
                    entity_type = $et, \
                    entity_id = $eid, \
                    content_hash = $ch, \
                    full_hash = $ch, \
                    source_instance = $src, \
                    last_updated = time::now(), \
                    updated_at = time::now() \
                 WHERE entity_type = $et AND entity_id = $eid",
            )
            .bind(("et", et))
            .bind(("eid", eid))
            .bind(("ch", ch))
            .bind(("src", src))
            .await
            .map_err(|e| format!("checksum upsert failed: {}", e))?;
        Ok(())
    }

    /// Compute and upsert checksum for a raw serde_json::Value entity.
    /// Convenience wrapper combining `compute_content_hash` + `upsert_checksum`.
    pub async fn record_checksum(
        &self,
        entity_type: &str,
        entity_id: &str,
        value: &Value,
    ) -> Result<(), String> {
        let hash = compute_content_hash(value).ok_or("Entity must be a JSON object")?;
        self.upsert_checksum(entity_type, entity_id, &hash).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_hash_deterministic() {
        let mut items = BTreeMap::new();
        items.insert("a".into(), "hash_a".into());
        items.insert("b".into(), "hash_b".into());
        let h1 = compute_bucket_hash(&items);
        let h2 = compute_bucket_hash(&items);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_bucket_hash_differs() {
        let mut a = BTreeMap::new();
        a.insert("x".into(), "1".into());
        let mut b = BTreeMap::new();
        b.insert("x".into(), "2".into());
        assert_ne!(compute_bucket_hash(&a), compute_bucket_hash(&b));
    }

    #[test]
    fn test_compare_trees() {
        let mut local = BTreeMap::new();
        local.insert("a".into(), "hash1".into());
        local.insert("b".into(), "hash2".into());
        local.insert("c".into(), "hash3".into());

        let mut remote = BTreeMap::new();
        remote.insert("a".into(), "hash1".into());
        remote.insert("b".into(), "changed".into());
        remote.insert("d".into(), "hash4".into());

        let (need_from_remote, need_to_push) = compare_trees(&local, &remote);
        assert!(need_from_remote.contains(&"b".to_string()));
        assert!(need_from_remote.contains(&"d".to_string()));
        assert!(need_to_push.contains(&"c".to_string()));
        assert!(need_to_push.contains(&"b".to_string()));
        assert!(!need_to_push.contains(&"a".to_string()));
    }

    #[test]
    fn test_get_bucket_index() {
        assert_eq!(get_bucket_index("Product-123"), "p");
        assert_eq!(get_bucket_index("abc"), "a");
        assert_eq!(get_bucket_index(""), "_");
    }

    #[test]
    fn test_content_hash_ignores_timestamps() {
        let e1 = serde_json::json!({
            "id": 1,
            "name": "test",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
        });
        let e2 = serde_json::json!({
            "id": 1,
            "name": "test",
            "created_at": "2026-06-15T12:00:00Z",
            "updated_at": "2026-06-15T12:00:00Z",
        });

        assert_eq!(
            compute_content_hash(&e1),
            compute_content_hash(&e2),
            "Timestamps should be stripped before hashing"
        );
    }

    #[test]
    fn test_content_hash_detects_change() {
        let e1 = serde_json::json!({"id": 1, "name": "alpha"});
        let e2 = serde_json::json!({"id": 1, "name": "beta"});
        assert_ne!(compute_content_hash(&e1), compute_content_hash(&e2));
    }
}
