use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Constant content-hash for a TOMBSTONE (deleted-entity) leaf. All tombstones for
/// a given id share this leaf hash, so once a peer applies the delete + records the
/// same tombstone checksum, the merkle leaf matches and the pair converges on
/// "deleted" (instead of the deleter dropping the leaf → peer re-advertises the id →
/// resurrection). Deliberately not a hash of any content — a tombstone carries no
/// content, only the fact of deletion + its causality (`vclock` on the checksum row).
pub const TOMBSTONE_HASH: &str = "__tombstone__";

/// Extract the LEAF portion of a SurrealDB Thing serialized as JSON.
///
/// SurrealDB v3 serializes the implicit record id in one of these shapes:
///   * `"registered_device:de1911de-..."` (string Thing repr)
///   * `"registered_device:\`de1911de-...\`"` (string Thing repr with backticks)
///   * `{ "tb": "registered_device", "id": "de1911de-..." }`
///   * `{ "tb": "registered_device", "id": { "String": "de1911de-..." } }`
///
/// We always return the bare leaf (no table prefix, no backticks) so merkle
/// bucket keys, pull/push IDs, and canonical foo_id columns all agree on
/// the same identifier across nodes.
pub fn extract_entity_leaf_id(v: &Value) -> Option<String> {
    fn strip_quotes(s: &str) -> &str {
        s.trim_matches('`').trim_matches('\u{2018}').trim_matches('\u{2019}')
    }
    if let Some(s) = v.as_str() {
        let leaf = s.rsplit_once(':').map(|(_, leaf)| leaf).unwrap_or(s);
        let leaf = strip_quotes(leaf);
        if leaf.is_empty() {
            return None;
        }
        return Some(leaf.to_string());
    }
    let obj = v.as_object()?;
    let id = obj.get("id")?;
    if let Some(s) = id.as_str() {
        let leaf = s.rsplit_once(':').map(|(_, leaf)| leaf).unwrap_or(s);
        return Some(strip_quotes(leaf).to_string());
    }
    if let Some(s) = id
        .as_object()
        .and_then(|m| m.get("String"))
        .and_then(|x| x.as_str())
    {
        return Some(strip_quotes(s).to_string());
    }
    None
}

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
    /// Digest of the peer's effective content-hash schema (see
    /// [`hash_schema_version`]). ADDITIVE transport field: only the
    /// `merkle/state` HTTP handler fills it; local tree nodes leave it `None`,
    /// and a peer on an older binary omits it entirely (deserializes to `None`,
    /// grandfathered as compatible). Never participates in any hash — carried
    /// so the sync engine can skip tree-repair between mismatched schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_schema: Option<String>,
}

impl MerkleNode {
    pub fn new(level: u8, key: String) -> Self {
        Self {
            level,
            key,
            hash: String::new(),
            children: BTreeMap::new(),
            hash_schema: None,
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
    // Record identity is the merkle LEAF KEY (entity_id), not content: two
    // distinct rows are already distinct leaves (see `compute_bucket_hash`,
    // which hashes entity_id → content_hash pairs) regardless of their content
    // hash. Hashing `id` was actively harmful because different query paths
    // present it in different forms — the checksum sweep and live-watch hash the
    // Thing form ("document:`53451…`") while pull/conflict paths hash the bare
    // leaf via `record::id(id) AS id` — so the hourly sweep re-flagged exactly
    // the pulled rows as DRIFTED, a perpetual sweep-vs-pull standoff.
    "id",
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
    "vector_clock",
    "_vclock",
    // Per-node operational noise on synced `user` rows: login bookkeeping
    // differs per node and must not drive cross-node re-syncs.
    "lastLogin",
    "failed_login_attempts",
    // Per-node embedding-worker BOOKKEEPING. Status/retries/error are local
    // scheduling state (a worker-off node legitimately keeps 'pending' forever
    // while the vector arrives via mesh), so hashing them would block
    // convergence between a node that embedded and one that received.
    //
    // NOTE (2026-07-12): `embedding` (the vector) and `embedding_model` are
    // deliberately NOT ignored anymore — same reasoning as `ai_summary` below.
    // The old non-convergence (every node unconditionally embedding its own
    // copy → N different vectors per row → perpetual drill) is fixed at the
    // WRITE side instead: a worker only embeds rows whose vector is absent
    // (`embedding IS NONE`) and bumps `_vclock` on the success write, so
    // exactly one author wins and peers adopt its vector. A concurrent race
    // between two nodes resolves once through the normal conflict path and
    // then stays converged (both copies HAVE a vector — nobody re-embeds).
    // This is what lets a single AI-dedicated node (`ECK_EMBED_WORKER=0`
    // everywhere else) serve vectors to the whole mesh, and re-embedding is
    // signalled fleet-wide by nulling the vector (see wms summarization).
    "embedding_status",
    "embedding_retries",
    "embedding_error",
    "summary_status",
    "summary_retries",
    "summary_error",
    // Change-detection bookkeeping of the scraper-holding node (which subset
    // of ticket meta the last summary was armed for). Pure local scheduling
    // state like summary_status; hashing it would force a one-time fleet-wide
    // re-sync wave of every document row for zero content value.
    "summary_source_hash",
    // Pseudonym-token index of a document's masked fields (ai_summary +
    // meta.subject). Pure DERIVED data: a regex scan over fields that DO
    // sync, so every node re-derives it locally (wms self-heal) and always
    // agrees. Hashing it would only add a mixed-fleet divergence window
    // while old binaries lack the field — for zero content value.
    "pii_fingerprints",
    // NOTE (2026-07-09): `ai_summary` is deliberately NOT ignored anymore.
    // Only the raw-holding node can author a summary (peers skip on empty
    // text), so it is CONTENT, not a per-node derivative — ignoring it froze
    // every summary written AFTER a row's first sync (195 stale docs on the
    // kiosk). The author's write bumps `_vclock` (see wms summarization
    // worker), so its version strictly dominates a peer's summary-less copy
    // regardless of LWW timestamps. `summary_status`/`retries` stay ignored
    // (peers legitimately mark raw-less copies 'skipped'); `pii_fingerprints`
    // stays ignored too — it is written per-node by the EMBEDDING worker and
    // self-converges because the tokens are deterministic over the summary.
    "pii_fingerprints",
    "last_accessed_at",
    // Per-node repair-distiller job state: set by the node that distilled a
    // resolved ticket into the xelixir KB. A node that hasn't run the distiller
    // (or can't — e.g. no AI) legitimately lacks these, so hashing them would
    // make a distilled node and a non-distilled node never converge.
    "repair_distilled",
    "repair_distilled_at",
    "repair_distill_attempts",
    // Per-node operational/agent/volatile state on other synced tables — each
    // node writes these locally (device heartbeat, image optimizer, xelixir
    // agent session, trip point-pruning), so they legitimately differ across
    // nodes and must not drive cross-node convergence:
    //   registered_device: last_seen_at + the xelixir_* agent-session fields
    //   file_resource:     optimization_level (node-local image optimization)
    //   trip:              points_pruned_at (node-local raw-point pruning ts)
    "last_seen_at",
    "optimization_level",
    "xelixir_command",
    "xelixir_status",
    "xelixir_token",
    "xelixir_session_url",
    "xelixir_updated_at",
    "points_pruned_at",
    // More per-node operational state surfaced by the (c) audit (trip cell-resolve
    // timestamp, file-resource node-local optimization timestamp + storage path):
    "resolved_at",
    "optimized_at",
    "storage_path",
];

/// Compute a deterministic SHA-256 content hash from any serde_json::Value.
/// Strips timestamp/sync-only fields so that two nodes with identical business
/// data produce the same hash regardless of when they were last synced.
pub fn compute_content_hash(value: &Value) -> Option<String> {
    crate::metrics::tick(crate::metrics::M::MerkleHash);
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
            // Everything else (numbers, bools, arrays, nested objects) goes
            // through the general recursive canonical serializer, which rounds
            // every f64 through f32 at ANY depth. ULP-level f64 residue appears
            // whenever floats cross node boundaries — first seen on `embedding`
            // vectors, then on the nested `doc_class.confidence` written by the
            // layer-2 classifier (0.9200000166893005 vs …04). Content identity
            // is DEFINED at f32 precision, so that residue must never drive it
            // (LWW tie → no write → re-pull forever).
            let mut canon = String::new();
            write_canonical_value(v, &mut canon);
            canonical.push_str(&format!("{}:{};", k, canon));
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Some(hex::encode(hasher.finalize()))
}

/// Recursively serialize a JSON value into a deterministic canonical string for
/// the content hash, rounding every `f64` number through `f32` at any depth.
///
/// Vectors and model scores are canonically f32 fleet-wide (HNSW indexes are
/// TYPE F32; the embed worker emits `Vec<f32>`), but floats pick up ULP-level
/// residue when they cross node boundaries — the `0.1` vs `0.10000000149011612`
/// divergence on `embedding`, and the 1-ULP nested `doc_class.confidence`
/// (0.9200000166893005 vs …04) written by the layer-2 classifier. Both round to
/// identical f32 bits whose shortest `Display` is deterministic, so hashing that
/// f32 form collapses the residue. INTEGERS (i64/u64) are kept EXACT — they
/// carry identity/counters and must never be squashed by an f32 cast (only 24
/// mantissa bits). Strings/bools/null are serialized as-is (no float transform).
fn write_canonical_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::String(s) => {
            // JSON-quoted + escaped: prevents a string value/key that contains
            // structural characters from colliding with real structure.
            out.push_str(&serde_json::to_string(s).expect("string serialization is infallible"));
        }
        Value::Number(n) => {
            if n.is_f64() {
                // Float → round through f32; the shortest f32 Display is
                // deterministic and collapses cross-node ULP residue.
                let f = n.as_f64().expect("is_f64 checked above") as f32;
                out.push_str(&format!("{}", f));
            } else {
                // i64/u64 integer → exact, never through f32.
                out.push_str(&n.to_string());
            }
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, e) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical_value(e, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // serde_json's default Map is already sorted; sort explicitly so the
            // canonical form is order-independent regardless of build features.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string serialization is infallible"));
                out.push(':');
                write_canonical_value(&map[*k], out);
            }
            out.push('}');
        }
    }
}

// ─── Hash-schema version (mixed-build guard, layer 2) ────────────────────────

/// Manually-bumped algorithm revision of the content-hash canonicalization.
///
/// BUMP THIS whenever `compute_content_hash`'s canonicalization or the hashing
/// ALGORITHM changes (canonical-string construction, sort order, timestamp
/// normalization, digest function) — anything that alters the bytes hashed for
/// otherwise-identical business data. Changes to the IGNORED_FIELDS *set* need
/// NO bump: they are folded into the schema digest automatically below.
const HASH_ALGO_REV: u32 = 3;

/// Short, stable digest of this build's effective content-hash schema — the
/// algorithm revision plus the (sorted, de-duplicated) `IGNORED_FIELDS` set.
///
/// Two nodes whose digests differ can NEVER agree on a merkle root even on
/// byte-identical data (they hash different field-sets), so the sync engine
/// skips tree-repair between them instead of re-pulling the same rows forever
/// (see `engine::sync_entity_with_peer`). Carried additively in the merkle
/// exchange; a peer that omits it is grandfathered as compatible.
///
/// Adding or removing an `IGNORED_FIELDS` entry changes this digest with ZERO
/// human action — that is the point: a field-set change is a fleet-visible
/// protocol change and must announce itself.
pub fn hash_schema_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| compute_hash_schema_digest(HASH_ALGO_REV, IGNORED_FIELDS))
}

/// Inner, pure digest fn (injectable for tests): SHA-256 over the algorithm
/// revision + the sorted/deduped field list, truncated to 16 hex chars.
fn compute_hash_schema_digest(rev: u32, ignored: &[&str]) -> String {
    let mut fields: Vec<&str> = ignored.to_vec();
    fields.sort_unstable();
    fields.dedup();
    let mut hasher = Sha256::new();
    hasher.update(b"rev:");
    hasher.update(rev.to_le_bytes());
    hasher.update(b";fields:");
    for f in fields {
        hasher.update(f.as_bytes());
        hasher.update(b",");
    }
    hex::encode(hasher.finalize())[..16].to_string()
}

// ─── Frozen LWW tie-break comparator ─────────────────────────────────────────

/// Canonical-bytes hash of the FULL record for the equal-timestamp LWW
/// tie-break — the cross-version referee both builds must agree on.
///
/// **FROZEN FOREVER — never add exclusions, never normalize, never change the
/// canonicalization.** This function MUST produce identical output in every past
/// and future build; it is what stops two peers from each concluding "I win" and
/// ping-ponging a record's vector clock. It excludes ONLY the two top-level
/// vector-clock fields (`_vclock`, `vector_clock`) — causality metadata that
/// legitimately differs between the two copies being tie-broken — and hashes
/// every other field exactly as stored, with NO timestamp normalization.
///
/// It is deliberately INDEPENDENT of `IGNORED_FIELDS` (which evolves per build):
/// a field that is merkle-ignored still PARTICIPATES here. Do NOT refactor to
/// share code with `compute_content_hash`; their divergence is intentional and
/// permanent.
pub fn tiebreak_hash(entity: &Value) -> String {
    fn write_canonical(v: &Value, out: &mut String) {
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => {
                // serde_json string serialization = quoted + JSON-escaped. The
                // escaping is REQUIRED for the frozen contract: an unescaped
                // `","` inside business text could make two structurally
                // different records share one canonical byte string.
                out.push_str(&serde_json::to_string(s).expect("string serialization is infallible"));
            }
            Value::Array(arr) => {
                out.push('[');
                for (i, e) in arr.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_canonical(e, out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_unstable();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string serialization is infallible"));
                    out.push(':');
                    write_canonical(&map[*k], out);
                }
                out.push('}');
            }
        }
    }

    let mut work = entity.clone();
    if let Some(obj) = work.as_object_mut() {
        obj.remove("_vclock");
        obj.remove("vector_clock");
    }
    let mut canon = String::new();
    write_canonical(&work, &mut canon);
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    hex::encode(hasher.finalize())
}

// ─── Root cache ──────────────────────────────────────────────────────────────
//
// The sync loop asks for the level-0 root of every entity type once per peer
// per cycle, and every peer's sync loop asks US the same via the merkle/state
// handler — with ad-hoc MerkleService instances each time. Without a cache
// that's a full `entity_checksum` SELECT + bucket/root hashing per peer×table
// every 60 s even when nothing changed. The cache holds the parsed bucket maps
// per (view, entity_type) and is invalidated by every write path of
// `entity_checksum` (the live-watch bridge funnels all live mutations through
// `upsert_checksum`/`record_tombstone`, so real-time invalidation is free).

struct RootCacheEntry {
    root: MerkleNode,
    /// bucket key → (entity_id → full_hash); serves level-1 requests without
    /// another SELECT.
    buckets: std::collections::HashMap<String, BTreeMap<String, String>>,
}

#[derive(Default)]
struct RootCacheInner {
    /// Per-entity-type generation, bumped on every invalidation. An in-flight
    /// build snapshots the generation before its SELECT and only stores the
    /// result if the generation still stands — otherwise a write that landed
    /// mid-build would be shadowed by a stale cache entry until the next write.
    gen: std::collections::HashMap<String, u64>,
    /// Keyed by (advertise_cache_only, entity_type) — cache nodes serve a
    /// filtered view whose root differs from the unfiltered one.
    entries: std::collections::HashMap<(bool, String), std::sync::Arc<RootCacheEntry>>,
}

static ROOT_CACHE: std::sync::LazyLock<std::sync::Mutex<RootCacheInner>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(RootCacheInner::default()));

/// Drop the cached merkle view of one entity type. Call after ANY write to its
/// `entity_checksum` rows — including direct DB writes that bypass
/// MerkleService (cache pulls, LRU eviction, live-watch checksum deletes).
pub fn invalidate_root_cache(entity_type: &str) {
    let mut c = ROOT_CACHE.lock().unwrap();
    *c.gen.entry(entity_type.to_string()).or_insert(0) += 1;
    c.entries.remove(&(false, entity_type.to_string()));
    c.entries.remove(&(true, entity_type.to_string()));
}

/// Drop every cached merkle view. For bulk operations on `entity_checksum`
/// (the boot-time pre-wipe).
pub fn invalidate_root_cache_all() {
    let mut c = ROOT_CACHE.lock().unwrap();
    for g in c.gen.values_mut() {
        *g += 1;
    }
    c.entries.clear();
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
    /// When true, `get_root`/`get_bucket` filter out rows where
    /// `is_cache = true`. Set on cache nodes so peers see only the cache's
    /// authoritative records (its own device row, system_config, ...), not
    /// the lazily-pulled cached subset whose membership churns under LRU.
    advertise_cache_only: bool,
}

impl MerkleService {
    pub fn new(db: SurrealDb, instance_id: String) -> Self {
        Self {
            db,
            instance_id,
            advertise_cache_only: false,
        }
    }

    /// Variant for cache nodes: skip `is_cache=true` rows when computing
    /// the merkle tree response, so the advertised root reflects only
    /// authoritative data.
    pub fn new_cache_filtered(db: SurrealDb, instance_id: String) -> Self {
        Self {
            db,
            instance_id,
            advertise_cache_only: true,
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn select_query(&self) -> &'static str {
        if self.advertise_cache_only {
            "SELECT entity_id, full_hash FROM entity_checksum \
             WHERE entity_type = $et AND (is_cache IS NONE OR is_cache = false)"
        } else {
            "SELECT entity_id, full_hash FROM entity_checksum WHERE entity_type = $et"
        }
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

    /// Fetch (or build) the cached merkle view for one entity type. Both the
    /// level-0 root and level-1 bucket requests are served from the same entry,
    /// so an idle node answers every peer from memory; the entry lives until a
    /// checksum write invalidates it.
    async fn load_view(
        &self,
        entity_type: &str,
    ) -> Result<std::sync::Arc<RootCacheEntry>, String> {
        let key = (self.advertise_cache_only, entity_type.to_string());
        let gen_snapshot = {
            let mut c = ROOT_CACHE.lock().unwrap();
            if let Some(e) = c.entries.get(&key) {
                return Ok(std::sync::Arc::clone(e));
            }
            *c.gen.entry(entity_type.to_string()).or_insert(0)
        };

        crate::metrics::tick(crate::metrics::M::MerkleRootBuild);
        let et = entity_type.to_string();
        let rows: Vec<ChecksumRow> = self
            .db
            .query(self.select_query())
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

        let entry = std::sync::Arc::new(RootCacheEntry {
            root: MerkleNode {
                level: 0,
                key: "root".to_string(),
                hash: root_hash,
                children: buckets,
                hash_schema: None,
            },
            buckets: bucket_items,
        });

        let mut c = ROOT_CACHE.lock().unwrap();
        if *c.gen.entry(entity_type.to_string()).or_insert(0) == gen_snapshot {
            c.entries.insert(key, std::sync::Arc::clone(&entry));
        }
        Ok(entry)
    }

    /// Level 0: root node whose children are bucket_key → bucket_hash.
    async fn get_root(&self, entity_type: &str) -> Result<MerkleNode, String> {
        Ok(self.load_view(entity_type).await?.root.clone())
    }

    /// Level 1: single bucket with entity_id → content_hash children.
    async fn get_bucket(&self, entity_type: &str, bucket: &str) -> Result<MerkleNode, String> {
        let view = self.load_view(entity_type).await?;
        let items = view.buckets.get(bucket).cloned().unwrap_or_default();
        let hash = compute_bucket_hash(&items);

        Ok(MerkleNode {
            level: 1,
            key: bucket.to_string(),
            hash,
            children: items,
            hash_schema: None,
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
        let started = std::time::Instant::now();
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
                    deleted = false, \
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
        invalidate_root_cache(entity_type);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // Profiling hook: emit at trace by default (zero cost when filter higher).
        // Bump to debug temporarily by setting `RUST_LOG=...,eck_core::sync::merkle=debug`
        // to see per-row UPSERT latency — useful when bootstrap is mysteriously slow.
        tracing::trace!(
            target: "eck_core::sync::merkle",
            entity_type = entity_type,
            entity_id = entity_id,
            elapsed_ms = elapsed_ms,
            "merkle::upsert_checksum"
        );
        Ok(())
    }

    /// Batched UPSERT of multiple `(entity_id, content_hash)` rows for a
    /// single entity_type, all under one SurrealDB transaction. SurrealKV's
    /// default durability is one fsync per committed transaction, so this
    /// pays the disk-write cost ONCE per N rows instead of once per row.
    /// On the kiosk's `bootstrap_checksums` cold path (5000-row tables, 1
    /// ms per per-row UPSERT) this is the difference between 5 seconds and
    /// 50 minutes. The single-row `upsert_checksum` stays for live mutations
    /// where you only have one row at a time.
    pub async fn upsert_checksums_batch(
        &self,
        entity_type: &str,
        rows: &[(String, String)],
    ) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let nrows = rows.len();
        let payload: Vec<serde_json::Value> = rows
            .iter()
            .map(|(eid, hash)| serde_json::json!({ "eid": eid, "ch": hash }))
            .collect();
        let q = "BEGIN TRANSACTION; \
                 FOR $row IN $rows { \
                     UPSERT entity_checksum SET \
                         entity_type = $et, \
                         entity_id = $row.eid, \
                         content_hash = $row.ch, \
                         full_hash = $row.ch, \
                         source_instance = $src, \
                         last_updated = time::now(), \
                         updated_at = time::now() \
                     WHERE entity_type = $et AND entity_id = $row.eid; \
                 }; \
                 COMMIT TRANSACTION;";
        self.db
            .query(q)
            .bind(("et", entity_type.to_string()))
            .bind(("src", self.instance_id.clone()))
            .bind(("rows", payload))
            .await
            .map_err(|e| format!("batch checksum upsert failed: {}", e))?;
        invalidate_root_cache(entity_type);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        tracing::trace!(
            target: "eck_core::sync::merkle",
            entity_type = entity_type,
            nrows = nrows,
            elapsed_ms = elapsed_ms,
            "merkle::upsert_checksums_batch"
        );
        Ok(())
    }

    /// Record a TOMBSTONE checksum for a deleted entity: the leaf hash becomes the
    /// constant [`TOMBSTONE_HASH`], `deleted = true`, and the deleting node's
    /// advanced `vclock` is stored so `sync_pull` can serve it and peers can compare
    /// causality (a later re-create with a dominating clock resurrects; otherwise the
    /// delete wins). Idempotent — re-recording the same tombstone is a no-op-shaped
    /// upsert. The entity ROW itself is deleted separately (reads never see it).
    pub async fn record_tombstone(
        &self,
        entity_type: &str,
        entity_id: &str,
        vclock: &Value,
    ) -> Result<(), String> {
        self.db
            .query(
                "UPSERT entity_checksum SET \
                    entity_type = $et, \
                    entity_id = $eid, \
                    content_hash = $ch, \
                    full_hash = $ch, \
                    deleted = true, \
                    vclock = $vc, \
                    source_instance = $src, \
                    last_updated = time::now(), \
                    updated_at = time::now() \
                 WHERE entity_type = $et AND entity_id = $eid",
            )
            .bind(("et", entity_type.to_string()))
            .bind(("eid", entity_id.to_string()))
            .bind(("ch", TOMBSTONE_HASH.to_string()))
            .bind(("vc", vclock.clone()))
            .bind(("src", self.instance_id.clone()))
            .await
            .map_err(|e| format!("tombstone upsert failed: {}", e))?;
        invalidate_root_cache(entity_type);
        Ok(())
    }

    /// Read the tombstone state of a leaf: `Some(vclock)` if `entity_checksum` marks
    /// it deleted, else `None`. Used by conflict resolution when the entity ROW is
    /// absent — to tell "never had it" from "we deleted it" (and at what clock).
    pub async fn tombstone_vclock(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Option<Value> {
        let rows: Vec<Value> = self
            .db
            .query(
                "SELECT VALUE vclock FROM entity_checksum \
                 WHERE entity_type = $et AND entity_id = $eid AND deleted = true LIMIT 1",
            )
            .bind(("et", entity_type.to_string()))
            .bind(("eid", entity_id.to_string()))
            .await
            .ok()?
            .take(0)
            .ok()?;
        rows.into_iter().next().filter(|v| !v.is_null())
    }

    /// Cache-node variant of `record_checksum`: marks the checksum row
    /// `is_cache = true` so the advertised (cache-filtered) merkle view skips
    /// it. A keyless blind cache WITHHOLDS plaintext rows in `sync_pull`, so
    /// advertising a leaf it will refuse to serve locks every full peer into
    /// a "pulling 1 → pulled 0" loop each cycle (2026-07-11: the kiosk device
    /// row re-pushed to eck1/2/3 re-created exactly that within 16 min of a
    /// manual cleanup). Same shape as the `pull_entity_on_demand` checksum.
    pub async fn record_checksum_cached(
        &self,
        entity_type: &str,
        entity_id: &str,
        value: &Value,
    ) -> Result<(), String> {
        let hash = compute_content_hash(value).ok_or("Entity must be a JSON object")?;
        self.db
            .query(
                "UPSERT entity_checksum SET \
                    entity_type = $et, \
                    entity_id = $eid, \
                    content_hash = $ch, \
                    full_hash = $ch, \
                    deleted = false, \
                    is_cache = true, \
                    source_instance = $src, \
                    last_accessed_at = time::now(), \
                    last_updated = time::now(), \
                    updated_at = time::now() \
                 WHERE entity_type = $et AND entity_id = $eid",
            )
            .bind(("et", entity_type.to_string()))
            .bind(("eid", entity_id.to_string()))
            .bind(("ch", hash))
            .bind(("src", self.instance_id.clone()))
            .await
            .map_err(|e| format!("cached checksum upsert failed: {}", e))?;
        invalidate_root_cache(entity_type);
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
        let hash_start = std::time::Instant::now();
        let hash = compute_content_hash(value).ok_or("Entity must be a JSON object")?;
        let hash_us = hash_start.elapsed().as_micros() as u64;
        tracing::trace!(
            target: "eck_core::sync::merkle",
            entity_type = entity_type,
            entity_id = entity_id,
            hash_us = hash_us,
            "merkle::compute_content_hash"
        );
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

    #[test]
    fn test_content_hash_ignores_id_form() {
        // Record identity is the merkle leaf key, not content. `id` is now in
        // IGNORED_FIELDS, so a row hashes identically whether a query path
        // presents its id as the Thing form ("document:`53451…`"), the bare
        // leaf, or omits it entirely — the exact drift that made the hourly
        // sweep re-flag pulled rows as DRIFTED.
        assert!(IGNORED_FIELDS.contains(&"id"));
        let thing = serde_json::json!({"id": "document:`53451abc`", "name": "d", "n": 7});
        let leaf = serde_json::json!({"id": "53451abc", "name": "d", "n": 7});
        let absent = serde_json::json!({"name": "d", "n": 7});
        assert_eq!(compute_content_hash(&thing), compute_content_hash(&leaf));
        assert_eq!(compute_content_hash(&thing), compute_content_hash(&absent));
        // A genuine content difference is still detected with id ignored.
        let diff = serde_json::json!({"id": "document:`53451abc`", "name": "d", "n": 8});
        assert_ne!(compute_content_hash(&thing), compute_content_hash(&diff));
    }

    #[test]
    fn test_content_hash_canonicalizes_embedding_f32_vs_f64() {
        // One node stores the vector as exact f32 (shortest-f32 wire text like
        // "0.1"); another stores the SAME vector widened to f64 (a 1-ULP residue
        // that serializes to longer text like "0.10000000149011612"). Both round
        // to identical f32 bits, so they MUST hash equal — otherwise the rows
        // ping-pong forever under an LWW tie. Build the two Values the way they
        // arrive over the wire: serialize the typed vec with the real string
        // serializer, then parse it back.
        let v32: Vec<f32> = vec![0.1, 0.2, 0.3, -0.25, 1.0 / 3.0, 42.0];
        let v64: Vec<f64> = v32.iter().map(|&x| x as f64).collect();
        let s32 = serde_json::to_string(&v32).unwrap();
        let s64 = serde_json::to_string(&v64).unwrap();
        // Sanity: the two wire forms genuinely differ, so the test proves the fix.
        assert_ne!(
            s32, s64,
            "f32 and f64-widened wire text must differ, else the test proves nothing"
        );
        let emb32: Value = serde_json::from_str(&s32).unwrap();
        let emb64: Value = serde_json::from_str(&s64).unwrap();
        let doc32 = serde_json::json!({"id": "document:x", "meta": "hi", "embedding": emb32});
        let doc64 = serde_json::json!({"id": "document:x", "meta": "hi", "embedding": emb64});
        assert_eq!(
            compute_content_hash(&doc32),
            compute_content_hash(&doc64),
            "f32-exact and f64-widened copies of one vector must hash equal"
        );
        // A genuinely different vector still hashes differently.
        let v_diff: Vec<f32> = vec![0.1, 0.2, 0.3, -0.25, 1.0 / 3.0, 42.5];
        let emb_diff: Value =
            serde_json::from_str(&serde_json::to_string(&v_diff).unwrap()).unwrap();
        let doc_diff = serde_json::json!({"id": "document:x", "meta": "hi", "embedding": emb_diff});
        assert_ne!(compute_content_hash(&doc32), compute_content_hash(&doc_diff));
    }

    #[test]
    fn test_content_hash_canonicalizes_nested_float() {
        // Live standoff (2026-07-29): file_resource:zvl72cv2lye4kgk80bc3 was
        // byte-identical on both full nodes EXCEPT a nested `doc_class.confidence`
        // that differed by 1 ULP of f64 (0.9200000166893005 vs …04), written by
        // the layer-2 classifier. The general recursive serializer must round
        // that nested float through f32, collapsing the residue.
        let a = serde_json::json!({
            "id": "file_resource:zvl72cv2lye4kgk80bc3",
            "name": "scan.pdf",
            "doc_class": {"label": "invoice", "confidence": 0.9200000166893005_f64},
        });
        let b = serde_json::json!({
            "id": "file_resource:zvl72cv2lye4kgk80bc3",
            "name": "scan.pdf",
            "doc_class": {"label": "invoice", "confidence": 0.9200000166893004_f64},
        });
        assert_eq!(
            compute_content_hash(&a),
            compute_content_hash(&b),
            "1-ULP f64 residue in a NESTED field must not change the content hash"
        );
        // A genuinely different nested confidence still changes the hash.
        let c = serde_json::json!({
            "id": "file_resource:zvl72cv2lye4kgk80bc3",
            "name": "scan.pdf",
            "doc_class": {"label": "invoice", "confidence": 0.93_f64},
        });
        assert_ne!(compute_content_hash(&a), compute_content_hash(&c));
    }

    #[test]
    fn test_content_hash_keeps_integers_exact() {
        // Integers carry identity/counters and must NOT be squashed by the f32
        // round-trip (f32 has only 24 mantissa bits). Two records differing only
        // by a huge i64 beyond both f32 and f64 exact-integer range must still
        // hash differently — the general serializer keeps i64/u64 exact.
        let a = serde_json::json!({"name": "x", "n": 9007199254740993_i64});
        let b = serde_json::json!({"name": "x", "n": 9007199254740994_i64});
        assert_ne!(
            compute_content_hash(&a),
            compute_content_hash(&b),
            "i64 fields must keep exact identity beyond f32 precision"
        );
    }

    // ── Hash-schema digest (layer 2) ─────────────────────────────────────────

    #[test]
    fn hash_schema_digest_is_stable_for_same_inputs() {
        let fields = ["created_at", "updated_at", "_vclock"];
        let a = compute_hash_schema_digest(1, &fields);
        let b = compute_hash_schema_digest(1, &fields);
        assert_eq!(a, b, "same rev + same field list ⇒ identical digest");
        assert_eq!(a.len(), 16, "short hex, 16 chars");
    }

    #[test]
    fn hash_schema_digest_is_order_independent() {
        // The field-set is a SET: declaration order (and duplicates) must not
        // move the digest — only membership does.
        let a = compute_hash_schema_digest(1, &["created_at", "updated_at"]);
        let b = compute_hash_schema_digest(1, &["updated_at", "created_at", "updated_at"]);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_schema_digest_changes_when_field_added_or_removed() {
        let base = compute_hash_schema_digest(1, &["created_at", "updated_at"]);
        let added = compute_hash_schema_digest(1, &["created_at", "updated_at", "pii_fingerprints"]);
        assert_ne!(base, added, "adding an ignored field MUST change the digest");
        let removed = compute_hash_schema_digest(1, &["created_at"]);
        assert_ne!(base, removed, "removing an ignored field MUST change the digest");
    }

    #[test]
    fn hash_schema_digest_changes_when_algo_rev_bumped() {
        let fields = ["created_at", "updated_at"];
        assert_ne!(
            compute_hash_schema_digest(1, &fields),
            compute_hash_schema_digest(2, &fields),
            "an algorithm-revision bump MUST change the digest"
        );
    }

    #[test]
    fn hash_schema_version_is_nonempty_and_deterministic() {
        assert_eq!(hash_schema_version(), hash_schema_version());
        assert_eq!(hash_schema_version().len(), 16);
    }

    // ── Frozen LWW tie-break comparator (hygiene [H]) ─────────────────────────

    #[test]
    fn tiebreak_hash_ignores_vclock_fields() {
        let a = serde_json::json!({
            "id": 1, "name": "x", "_vclock": {"node_a": 3},
        });
        let b = serde_json::json!({
            "id": 1, "name": "x", "_vclock": {"node_b": 99}, "vector_clock": {"n": 1},
        });
        assert_eq!(
            tiebreak_hash(&a),
            tiebreak_hash(&b),
            "verdict must be independent of _vclock / vector_clock differences"
        );
    }

    #[test]
    fn tiebreak_hash_is_deterministic_and_symmetric() {
        let a = serde_json::json!({"id": 1, "name": "alpha", "n": 2});
        let b = serde_json::json!({"id": 1, "name": "beta", "n": 2});
        // Deterministic.
        assert_eq!(tiebreak_hash(&a), tiebreak_hash(&a));
        // A total order: exactly one side wins, and both peers compute the same
        // winner regardless of which side they hold as "local".
        let ha = tiebreak_hash(&a);
        let hb = tiebreak_hash(&b);
        assert_ne!(ha, hb);
        let remote_wins_on_node1 = hb > ha; // node1: local=a, remote=b
        let remote_wins_on_node2 = ha > hb; // node2: local=b, remote=a
        assert_ne!(
            remote_wins_on_node1, remote_wins_on_node2,
            "both nodes must agree on the single winner (antisymmetric)"
        );
    }

    #[test]
    fn tiebreak_hash_counts_merkle_ignored_fields() {
        // `pii_fingerprints` and `embedding_status` are in IGNORED_FIELDS (merkle
        // strips them from compute_content_hash) — but the tie-break referee MUST
        // still see them, so two records differing only there resolve to distinct
        // hashes. This is the version-independence guarantee: the tie-break
        // field-set is frozen and never tracks IGNORED_FIELDS.
        assert!(IGNORED_FIELDS.contains(&"pii_fingerprints"));
        assert!(IGNORED_FIELDS.contains(&"embedding_status"));
        let base = serde_json::json!({"id": 1, "name": "t"});
        let with_pii = serde_json::json!({"id": 1, "name": "t", "pii_fingerprints": ["Name_ab"]});
        let with_status = serde_json::json!({"id": 1, "name": "t", "embedding_status": "pending"});
        // Sanity: the merkle content hash DOES ignore these (proves they are
        // genuinely ignored there, so tiebreak's inclusion is the divergence).
        assert_eq!(compute_content_hash(&base), compute_content_hash(&with_pii));
        assert_eq!(compute_content_hash(&base), compute_content_hash(&with_status));
        // The frozen tie-break comparator counts them.
        assert_ne!(tiebreak_hash(&base), tiebreak_hash(&with_pii));
        assert_ne!(tiebreak_hash(&base), tiebreak_hash(&with_status));
    }

    #[test]
    fn tiebreak_hash_escapes_strings_no_structural_collision() {
        // A string VALUE containing JSON structure characters must not collide
        // with a record where that structure is real. Without escaping, both
        // would canonicalize to the same bytes — and this comparator is frozen,
        // so the escaping guarantee is part of the permanent contract.
        let smuggled = serde_json::json!({"a": "x\",\"b\":\"y"});
        let real = serde_json::json!({"a": "x", "b": "y"});
        assert_ne!(tiebreak_hash(&smuggled), tiebreak_hash(&real));
        // Same for a KEY with structure characters.
        let key_smuggled = serde_json::json!({"a\":1,\"z": 2});
        let key_real = serde_json::json!({"a": 1, "z": 2});
        assert_ne!(tiebreak_hash(&key_smuggled), tiebreak_hash(&key_real));
    }
}
