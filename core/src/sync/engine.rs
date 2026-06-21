use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use surrealdb::types::SurrealValue;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};


use crate::db::SurrealDb;
use crate::sync::{
    merkle::{self, extract_entity_leaf_id, MerkleRequest, MerkleService},
    mesh_client::{self, MeshClient},
    relay_client::RelayClient,
};

/// Entity types to include in Merkle sync sweeps.
/// Add new SurrealDB tables here as they become syncable.
const SYNC_ENTITY_TYPES: &[&str] = &[
    "item",
    "order",
    "product",
    "partner",
    // `user` is intentionally absent: it's a Zone 1 (PII) table and lives
    // in `users_db`, not `db`. Each node holds its own local accounts; we
    // do NOT replicate credentials across the mesh.
    "file_resource",
    "location",
    "quant",
    "picking",
    "move_line",
    "rack",
    "action_proof",
    "delivery_carrier",
    "delivery_tracking",
    "device_intake",
    "inventory_discrepancy",
    "product_alias",
    "category",
    "menu_item",
    "ai_sop",
    // Carries the Xelixir C2 control plane (xelixir_command/status/token fields).
    // The edge node's `LIVE SELECT` watcher reacts to remote `xelixir_command` writes.
    "registered_device",
    // Processed support-ticket metadata + AI summary + PPRL-anonymized embedding.
    // The raw Zoho payload lives in `document_raw` (intentionally NOT synced — stays
    // on the scraper node). When a peer needs the full raw body, it requests it via
    // the `mesh_task` reverse-fetch queue (see `engine.rs::process_tasks`).
    "document",
    // Fahrtenbuch — replicate across a customer's own PAID mesh (HA/backup).
    // The blind relay can't read the payload (encrypted), and raw track points
    // are pruned at TRIP_RAW_RETENTION_DAYS; what survives is the sealed
    // aggregate. cell_tower is a PII-free mast cache (shared = fewer lookups).
    "trip",
    "visit_task",
    "cell_tower",
    // Fahrtenbuch vehicle registry (plate / Kennzeichen + plate-photo CAS ref).
    // PII-free reference data — replicated across the customer's own mesh.
    "vehicle",
];

/// Coordinates mesh synchronization for this node.
///
/// Architecture: the central relay at 9eck.com is **strictly a tracker** (service
/// discovery via heartbeat + mesh status). It never routes actual data payloads.
/// All entity sync happens directly P2P between nodes using Merkle tree diffing.
///
/// Flow:
/// 1. Relay heartbeat (every 5 min) — register our base_url so peers can find us
/// 2. Relay discovery — ask relay for list of online peers
/// 3. For each peer: compare Merkle roots → drill into differing buckets → exchange entities
/// Per-peer adaptive backoff state. Cross-NAT meshes have peers that are
/// permanently unreachable from this side (the other peer dials in instead),
/// so a peer that fails repeatedly should not be retried every minute.
///
/// Stored as wall-clock (`chrono::DateTime<Utc>`) rather than monotonic
/// `Instant` so the state can be persisted to SurrealDB and restored across
/// WMS restarts — otherwise a chronically-unreachable peer ramps through
/// 0→2→3→…→7+ from scratch on every launch.
#[derive(Debug, Default, Clone)]
struct PeerHealth {
    consecutive_failures: u32,
    skip_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl PeerHealth {
    /// Backoff schedule based on consecutive failures.
    /// 0–2 fails: no skip (transient hiccups). From the 3rd fail on it doubles:
    /// 30s, 60s, 120s, 240s, 480s, 960s, then capped at 30min. A peer that
    /// recovers resets the counter, so a brief outage doesn't poison the
    /// long-term cadence.
    fn next_skip(&self) -> Option<chrono::Duration> {
        if self.consecutive_failures <= 2 {
            return None;
        }
        let shift = (self.consecutive_failures - 3).min(7);
        let secs = (30u32 << shift).min(1800);
        Some(chrono::Duration::seconds(secs as i64))
    }
}

pub struct SyncEngine {
    instance_id: String,
    mesh_id: String,
    relay: RelayClient,
    db: SurrealDb,
    sync_secret: Option<String>,
    /// `"full"` (default) or `"cache"`. Cache nodes skip the periodic merkle
    /// sync — they only pull entities on demand from full peers.
    node_role: String,
    /// Tracks per-peer reachability. Keyed by base_url so it survives peer
    /// rediscovery from the relay (instance_id might change after re-registration
    /// but the URL is stable). Cleared if a peer recovers.
    peer_health: Arc<Mutex<HashMap<String, PeerHealth>>>,
}

impl SyncEngine {
    pub fn new(
        instance_id: String,
        mesh_id: String,
        relay: RelayClient,
        db: SurrealDb,
        sync_secret: Option<String>,
        node_role: String,
    ) -> Self {
        Self {
            instance_id,
            mesh_id,
            relay,
            db,
            sync_secret,
            node_role,
            peer_health: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn is_cache_node(&self) -> bool {
        self.node_role == "cache"
    }

    // ─── Peer health persistence ─────────────────────────────────────────────

    /// Restore peer_health from the `peer_health_state` table. Called once
    /// at startup so a chronically-unreachable peer (cross-NAT) doesn't ramp
    /// through 0→2→3→…→7+ again on every WMS restart.
    pub async fn load_peer_health(&self) -> anyhow::Result<usize> {
        let rows: Vec<Value> = self
            .db
            .query(
                "SELECT base_url, consecutive_failures, \
                        type::string(skip_until) AS skip_until \
                 FROM peer_health_state",
            )
            .await?
            .take(0)
            .map_err(|e| anyhow::anyhow!(e))?;

        let mut health = self.peer_health.lock().await;
        let mut loaded = 0usize;
        for row in &rows {
            let Some(url) = row.get("base_url").and_then(|v| v.as_str()) else {
                continue;
            };
            let cf = row
                .get("consecutive_failures")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let skip_until = row
                .get("skip_until")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
            health.insert(
                url.to_string(),
                PeerHealth {
                    consecutive_failures: cf,
                    skip_until,
                },
            );
            loaded += 1;
        }
        if loaded > 0 {
            info!("Restored peer_health for {} peer(s) from previous run", loaded);
        }
        Ok(loaded)
    }

    /// Persist a single peer's health to the DB. Called from sync_cycle when
    /// the entry transitions (failure count increment or recovery). UPSERT
    /// keyed by base_url so it's idempotent across re-discoveries.
    async fn persist_peer_health(&self, url: &str, health: &PeerHealth) {
        let skip = health.skip_until.map(|d| d.to_rfc3339()).unwrap_or_default();
        let cf = health.consecutive_failures as i64;
        let _ = self
            .db
            .query(
                "UPSERT peer_health_state SET \
                    base_url = $url, \
                    consecutive_failures = $cf, \
                    skip_until = $skip, \
                    updated_at = time::now() \
                 WHERE base_url = $url",
            )
            .bind(("url", url.to_string()))
            .bind(("cf", cf))
            .bind(("skip", skip))
            .await;
    }

    // ─── Cache mode — pull-on-demand ─────────────────────────────────────────

    /// Pull a single entity from any reachable full peer and store it locally
    /// marked as `is_cache=true`. Intended for cache nodes — when an API hit
    /// on a synced table doesn't find a row, the handler calls this to lazily
    /// hydrate the entity from a full peer instead of returning 404.
    ///
    /// Returns `Some(entity_json)` on first successful pull, `None` if no peer
    /// has it. Idempotent: re-pulling an already-cached entity just refreshes
    /// `last_accessed_at`.
    pub async fn pull_entity_on_demand(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Option<Value> {
        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        // Skip cache peers — they only have what they themselves cached,
        // and asking the cache for what we don't have just bounces.
        for peer in peers.into_iter().filter(|p| !p.is_cache()) {
            // Direct HTTP first (fast on a reachable peer). On failure fall back
            // to the relay reverse-fetch queue: dispatch a `pull_request` that
            // the peer's mesh_relay_poller fulfills and acks. This is the same
            // cross-NAT path the periodic sync uses — without it a cache node
            // could never hydrate from a NAT'd full peer (e.g. the kiosk master
            // registered with a LAN-only base_url, unreachable from the public
            // cache node).
            let entities: Vec<Value> = match peer
                .pull_entities(entity_type, &[entity_id.to_string()])
                .await
            {
                Ok(e) if !e.is_empty() => e,
                Ok(_) => continue, // peer reachable but lacks this id — next peer
                Err(err) => {
                    debug!(
                        "pull_entity_on_demand: direct {} failed ({}) — relay reverse-fetch from {}",
                        peer.peer_url(),
                        err,
                        peer.target_instance_id()
                    );
                    match self
                        .relay
                        .pull_entities_via_relay(
                            peer.target_instance_id(),
                            entity_type,
                            &[entity_id.to_string()],
                            15,
                        )
                        .await
                    {
                        Ok(e) if !e.is_empty() => e,
                        Ok(_) => continue,
                        Err(e) => {
                            debug!(
                                "pull_entity_on_demand: relay reverse-fetch from {} failed: {}",
                                peer.target_instance_id(),
                                e
                            );
                            continue;
                        }
                    }
                }
            };

            let entity = entities.into_iter().next().unwrap();

            // Upsert into the local table — same path conflict::resolve_and_upsert
            // would take on a sync push from this peer.
            if let Err(e) = crate::sync::conflict::resolve_and_upsert(
                &self.db,
                entity_type,
                entity_id,
                entity.clone(),
                &self.instance_id,
            )
            .await
            {
                warn!(
                    "pull_entity_on_demand: upsert {}:{} failed: {}",
                    entity_type, entity_id, e
                );
                continue;
            }

            // Record the checksum, flagged as cache + freshly accessed.
            let _ = self
                .db
                .query(
                    "UPSERT entity_checksum SET \
                        entity_type = $et, \
                        entity_id = $eid, \
                        content_hash = $ch, \
                        full_hash = $ch, \
                        source_instance = $src, \
                        is_cache = true, \
                        last_accessed_at = time::now(), \
                        last_updated = time::now(), \
                        updated_at = time::now() \
                     WHERE entity_type = $et AND entity_id = $eid",
                )
                .bind(("et", entity_type.to_string()))
                .bind(("eid", entity_id.to_string()))
                .bind((
                    "ch",
                    merkle::compute_content_hash(&entity).unwrap_or_default(),
                ))
                .bind(("src", peer.peer_url().to_string()))
                .await;

            info!(
                "Cache pull: {}:{} from {} ({} bytes)",
                entity_type,
                entity_id,
                peer.peer_url(),
                entity.to_string().len()
            );
            return Some(entity);
        }
        None
    }

    /// Bump `last_accessed_at` on an existing cached row. Called by API
    /// handlers when a cached entity is read so the LRU evictor knows it's
    /// still hot. No-op if no checksum row exists (e.g. row was never
    /// cache-tagged because it's locally owned).
    pub async fn touch_cache(&self, entity_type: &str, entity_id: &str) {
        let _ = self
            .db
            .query(
                "UPDATE entity_checksum SET last_accessed_at = time::now() \
                 WHERE entity_type = $et AND entity_id = $eid AND is_cache = true",
            )
            .bind(("et", entity_type.to_string()))
            .bind(("eid", entity_id.to_string()))
            .await;
    }

    /// Evict cached rows beyond the configured budget, LRU-style. Called
    /// periodically on cache nodes. `budget_rows` is the total row count
    /// across all is_cache=true entries; over-budget removes oldest by
    /// `last_accessed_at` until under budget.
    pub async fn evict_cache_lru(&self, budget_rows: u64) -> anyhow::Result<usize> {
        if !self.is_cache_node() {
            return Ok(0);
        }

        // Count.
        let count: Option<i64> = self
            .db
            .query(
                "SELECT count() AS n FROM entity_checksum WHERE is_cache = true GROUP ALL",
            )
            .await?
            .take::<Vec<Value>>(0)
            .map_err(|e| anyhow::anyhow!(e))?
            .into_iter()
            .next()
            .and_then(|v| v.get("n")?.as_i64());

        let count = count.unwrap_or(0) as u64;
        if count <= budget_rows {
            return Ok(0);
        }
        let to_evict = (count - budget_rows) as i64;

        let victims: Vec<Value> = self
            .db
            .query(
                "SELECT entity_type, entity_id FROM entity_checksum \
                 WHERE is_cache = true \
                 ORDER BY last_accessed_at ASC \
                 LIMIT $n",
            )
            .bind(("n", to_evict))
            .await?
            .take(0)
            .map_err(|e| anyhow::anyhow!(e))?;

        let mut evicted = 0usize;
        for v in &victims {
            let Some(et) = v.get("entity_type").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(eid) = v.get("entity_id").and_then(|x| x.as_str()) else {
                continue;
            };

            // Drop source row.
            let q1 = format!("DELETE {}:{}", et, eid);
            if let Err(e) = self.db.query(&q1).await {
                warn!("evict_cache: DELETE {}:{} failed: {}", et, eid, e);
                continue;
            }
            // Drop checksum.
            if let Err(e) = self
                .db
                .query(
                    "DELETE entity_checksum WHERE entity_type = $et AND entity_id = $eid",
                )
                .bind(("et", et.to_string()))
                .bind(("eid", eid.to_string()))
                .await
            {
                warn!("evict_cache: drop checksum {}:{} failed: {}", et, eid, e);
            }
            evicted += 1;
        }

        if evicted > 0 {
            info!(
                "Cache LRU eviction: removed {} entries (now ≤{})",
                evicted, budget_rows
            );
        }
        Ok(evicted)
    }

    // ─── Bootstrap ───────────────────────────────────────────────────────────

    /// Backfill `entity_checksum` for every existing row across all
    /// SYNC_ENTITY_TYPES tables. Call once at startup. Without this the merkle
    /// tree shows an empty root and `sync_entity_with_peer` short-circuits with
    /// "roots match, nothing to sync" — even though both peers have legitimate
    /// records they have never exchanged. Idempotent: re-records the same hash
    /// on every boot, which costs only a few UPSERTs per table.
    pub async fn bootstrap_checksums(&self) -> anyhow::Result<usize> {
        // Cache nodes don't advertise content via merkle — they pull on demand
        // and store with is_cache=true. A full scan + UPSERT of every checksum
        // row defeats the lightweight design.
        if self.is_cache_node() {
            info!("bootstrap_checksums: skipped (node_role=cache)");
            return Ok(0);
        }
        self.bootstrap_checksums_inner(true).await
    }

    /// Idempotent variant — no pre-wipe. Used from the periodic sync loop so
    /// records inserted after startup (e.g. agent_manager's self-registration
    /// that runs *after* the initial bootstrap) get their checksum advertised
    /// on the next cycle without needing a process restart.
    pub async fn refresh_checksums(&self) -> anyhow::Result<usize> {
        if self.is_cache_node() {
            return Ok(0);
        }
        self.bootstrap_checksums_inner(false).await
    }

    async fn bootstrap_checksums_inner(&self, wipe: bool) -> anyhow::Result<usize> {
        let merkle_svc = merkle::MerkleService::new(self.db.clone(), self.instance_id.clone());
        let mut total = 0usize;
        // Profiling: total wall time across all tables — surface this at INFO so
        // a single grep in the journal exposes runaway bootstrap cost without
        // needing to bump the whole sync namespace to debug.
        let bootstrap_started = std::time::Instant::now();

        if wipe {
            // Wipe and rebuild — earlier versions of this code keyed checksums by
            // SurrealDB's Thing repr (e.g. "registered_device:de1911de-…") instead
            // of the bare leaf, which placed every record into the wrong merkle
            // bucket. Clearing on each boot lets the re-record overwrite that
            // legacy state cleanly. Cheap: O(rows) per node, runs once per launch.
            if let Err(e) = self
                .db
                .query("DELETE entity_checksum")
                .await
                .and_then(|mut r| r.take::<Vec<serde_json::Value>>(0))
            {
                warn!("bootstrap_checksums: pre-wipe of entity_checksum failed: {}", e);
            }
        }

        for entity_type in SYNC_ENTITY_TYPES {
            let table_started = std::time::Instant::now();
            let mut hash_us_total: u64 = 0;
            let mut upsert_ms_total: u64 = 0;
            // Phase 1: snapshot existing checksums for this entity_type so we
            // can compare and skip UPSERT calls when the row is unchanged.
            // Without this, on the very first refresh after a cold start with
            // a 5000-row partner table we'd issue 5000 fsync'd UPSERTs every
            // 60 s — sustained 100% disk write, with sync_cycle effectively
            // never finishing.
            let existing: std::collections::HashMap<String, String> = if !wipe {
                let q = "SELECT entity_id, content_hash FROM entity_checksum WHERE entity_type = $et";
                let rows: Vec<serde_json::Value> = self
                    .db
                    .query(q)
                    .bind(("et", entity_type.to_string()))
                    .await
                    .and_then(|mut r| r.take(0))
                    .unwrap_or_default();
                rows.into_iter()
                    .filter_map(|v| {
                        let eid = v.get("entity_id")?.as_str()?.to_string();
                        let ch = v.get("content_hash")?.as_str()?.to_string();
                        Some((eid, ch))
                    })
                    .collect()
            } else {
                Default::default()
            };

            let query = format!("SELECT * FROM {}", entity_type);
            let rows: Vec<serde_json::Value> = match self
                .db
                .query(&query)
                .await
                .and_then(|mut r| r.take(0))
            {
                Ok(rows) => rows,
                Err(e) => {
                    // Tables in SYNC_ENTITY_TYPES that don't exist yet are
                    // normal on a freshly bootstrapped node — don't logspam
                    // them on every cycle. Demote to debug.
                    debug!(
                        "bootstrap_checksums: SELECT * FROM {} failed: {}",
                        entity_type, e
                    );
                    continue;
                }
            };

            let mut count = 0usize;
            let mut skipped = 0usize;

            // Pre-compute hashes and collect only the (eid, new_hash) rows that
            // actually need writing. The batch path below pays one fsync per
            // BATCH_SIZE rows instead of one fsync per row — on SurrealKV that's
            // the difference between minutes and seconds on big tables.
            const BATCH_SIZE: usize = 100;
            let mut dirty: Vec<(String, String)> = Vec::with_capacity(BATCH_SIZE);

            for entity in &rows {
                // Prefer the canonical foo_id column (a bare UUID) when the table
                // has one — that's what the API and the conflict resolver use.
                // Fall back to extracting the leaf from SurrealDB's implicit `id`
                // Thing for tables without a dedicated id column. Skip rows we
                // can't key at all.
                let id_field = match *entity_type {
                    "registered_device" => Some("device_id"),
                    "order" => Some("order_id"),
                    _ => None,
                };
                let eid_opt = id_field
                    .and_then(|f| entity.get(f).and_then(|v| v.as_str()).map(String::from))
                    .or_else(|| entity.get("id").and_then(extract_entity_leaf_id));

                let Some(eid) = eid_opt else {
                    continue;
                };

                // Compute the would-be content hash once. If we already have
                // that exact hash recorded, skip the UPSERT (and its fsync).
                let hash_started = std::time::Instant::now();
                let new_hash = match merkle::compute_content_hash(entity) {
                    Some(h) => h,
                    None => continue,
                };
                hash_us_total += hash_started.elapsed().as_micros() as u64;

                if let Some(existing_hash) = existing.get(&eid) {
                    if *existing_hash == new_hash {
                        skipped += 1;
                        continue;
                    }
                }

                dirty.push((eid, new_hash));
                if dirty.len() >= BATCH_SIZE {
                    let upsert_started = std::time::Instant::now();
                    let n = dirty.len();
                    match merkle_svc.upsert_checksums_batch(entity_type, &dirty).await {
                        Ok(()) => { count += n; }
                        Err(e) => warn!(
                            "bootstrap_checksums: batch UPSERT ({}, n={}) failed: {}",
                            entity_type, n, e
                        ),
                    }
                    upsert_ms_total += upsert_started.elapsed().as_millis() as u64;
                    dirty.clear();
                }
            }
            // Flush the remainder.
            if !dirty.is_empty() {
                let upsert_started = std::time::Instant::now();
                let n = dirty.len();
                match merkle_svc.upsert_checksums_batch(entity_type, &dirty).await {
                    Ok(()) => { count += n; }
                    Err(e) => warn!(
                        "bootstrap_checksums: batch UPSERT ({}, n={}) failed: {}",
                        entity_type, n, e
                    ),
                }
                upsert_ms_total += upsert_started.elapsed().as_millis() as u64;
                dirty.clear();
            }

            let table_ms = table_started.elapsed().as_millis() as u64;
            let nrows = rows.len();
            if count > 0 {
                // Per-table profile. `hash_us` and `upsert_ms` are sums across
                // every row we actually processed (skipped rows excluded).
                // Divide `upsert_ms / count` to see avg per-row write latency,
                // which is what fsync-per-commit dominates.
                info!(
                    "bootstrap_checksums: {} -> {} updated ({} unchanged, {} rows scanned in {} ms; hash_us_sum={} upsert_ms_sum={})",
                    entity_type, count, skipped, nrows, table_ms, hash_us_total, upsert_ms_total
                );
            } else if skipped > 0 {
                debug!(
                    "bootstrap_checksums: {} -> 0 changes ({} unchanged, {} rows in {} ms)",
                    entity_type, skipped, nrows, table_ms
                );
            }
            total += count;
        }

        info!(
            "bootstrap_checksums: total {} entity checksums updated in {} ms",
            total,
            bootstrap_started.elapsed().as_millis()
        );
        Ok(total)
    }

    // ─── P2P Merkle Sync ─────────────────────────────────────────────────────

    /// Run a full sync cycle: discover peers via relay, then Merkle-diff with each.
    /// Returns total entities exchanged (pulled + pushed) across all peers.
    pub async fn sync_cycle(&self) -> anyhow::Result<usize> {
        // Cache nodes don't advertise data; they pull on demand. Skip the
        // periodic merkle dance entirely — heartbeat in main.rs keeps the
        // node visible to discovery.
        if self.is_cache_node() {
            debug!("sync_cycle skipped (node_role=cache)");
            return Ok(0);
        }

        // Re-record checksums first so rows inserted since the previous cycle
        // (most notably AgentController's `ensure_self_device_record`, which
        // runs after the initial bootstrap) are picked up before we compare
        // roots with peers. Cheap UPSERT path, no wipe.
        if let Err(e) = self.refresh_checksums().await {
            warn!("refresh_checksums before sync_cycle failed: {}", e);
        }

        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        if peers.is_empty() {
            debug!("No online peers found, skipping sync cycle");
            return Ok(0);
        }

        // Apply per-peer backoff for unreachable peers. Cache peers are NOT
        // skipped at this stage anymore — they advertise their own
        // authoritative subset (the cache-aware merkle view filters out
        // is_cache=true rows), so the pull direction is still useful. The
        // push half is gated later inside sync_entity_with_peer.
        let now = chrono::Utc::now();
        let mut active_peers = Vec::with_capacity(peers.len());
        {
            let health = self.peer_health.lock().await;
            for peer in &peers {
                let url = peer.peer_url().to_string();
                if let Some(state) = health.get(&url) {
                    if let Some(skip_until) = state.skip_until {
                        if now < skip_until {
                            let remaining = (skip_until - now).num_seconds();
                            debug!(
                                "Backoff: skipping {} for {}s more ({} consecutive failures)",
                                url, remaining, state.consecutive_failures
                            );
                            continue;
                        }
                    }
                }
                active_peers.push(peer.clone());
            }
        }

        if active_peers.is_empty() {
            debug!(
                "All {} discovered peer(s) are in backoff, skipping cycle body",
                peers.len()
            );
            return Ok(0);
        }

        info!(
            "Sync cycle: {} active peer(s) out of {} discovered",
            active_peers.len(),
            peers.len()
        );
        let mut total = 0usize;

        for peer in &active_peers {
            let url = peer.peer_url().to_string();
            let mut peer_failures = 0u32;
            let mut peer_attempts = 0u32;

            // Fan out per-entity sync across all worker threads. Each
            // sync_entity_with_peer is an independent merkle walk + HTTP
            // exchange, so they share no state except the SurrealKV handle
            // (which serialises writes internally anyway). join_all lets
            // Tokio schedule them across cores; on a quiet machine the cycle
            // finishes ~N× faster, on a busy one OS nice/priority still
            // throttles us down.
            let results = futures_util::future::join_all(
                SYNC_ENTITY_TYPES
                    .iter()
                    .map(|et| async move { (*et, self.sync_entity_with_peer(peer, et).await) }),
            )
            .await;

            for (entity_type, res) in results {
                peer_attempts += 1;
                match res {
                    Ok(n) => total += n,
                    Err(e) => {
                        peer_failures += 1;
                        if peer_failures == 1 {
                            warn!(
                                "Sync first entity ({}) with {} failed: {} (further failures suppressed this cycle)",
                                entity_type,
                                url,
                                e
                            );
                        } else {
                            debug!(
                                "Sync {} with {} failed: {}",
                                entity_type,
                                url,
                                e
                            );
                        }
                    }
                }
            }

            // Update peer health for next cycle.
            let mut persist_pair: Option<(String, PeerHealth)> = None;
            {
                let mut health = self.peer_health.lock().await;
                let entry = health.entry(url.clone()).or_default();
                let mut changed = false;
                if peer_failures == 0 {
                    if entry.consecutive_failures > 0 {
                        info!(
                            "Peer {} recovered after {} failures",
                            url, entry.consecutive_failures
                        );
                        changed = true;
                    }
                    entry.consecutive_failures = 0;
                    entry.skip_until = None;
                } else if peer_failures == peer_attempts {
                    // Every entity type failed → almost certainly the peer itself is
                    // unreachable, not a per-table issue. Apply backoff.
                    entry.consecutive_failures += 1;
                    if let Some(skip) = entry.next_skip() {
                        entry.skip_until = Some(chrono::Utc::now() + skip);
                        if entry.consecutive_failures == 3 {
                            info!(
                                "Peer {} unreachable, backing off for {}s",
                                url,
                                skip.num_seconds()
                            );
                        }
                    }
                    changed = true;
                }
                if changed {
                    persist_pair = Some((url.clone(), entry.clone()));
                }
            }
            // UPSERT outside the lock to keep the contention window short.
            if let Some((u, h)) = persist_pair {
                self.persist_peer_health(&u, &h).await;
            }
        }

        // Process reverse-fetch task queue (NAT traversal)
        match self.process_tasks().await {
            Ok(n) => total += n,
            Err(e) => warn!("Task queue processing failed: {}", e),
        }

        if total > 0 {
            info!("Sync cycle complete: {} entities exchanged", total);
        }
        Ok(total)
    }

    /// Merkle-diff a single entity type with a single peer.
    ///
    /// 1. Compare roots — if identical, nothing to do.
    /// 2. Find differing buckets.
    /// 3. Drill into each differing bucket to find specific entity IDs.
    /// 4. Pull missing entities from peer, push our missing entities to peer.
    async fn sync_entity_with_peer(
        &self,
        peer: &MeshClient,
        entity_type: &str,
    ) -> Result<usize, String> {
        let merkle_svc = MerkleService::new(self.db.clone(), self.instance_id.clone());

        // Step 1: Compare roots
        let local_root = merkle_svc
            .get_state(&MerkleRequest {
                entity_type: entity_type.to_string(),
                level: 0,
                bucket: None,
            })
            .await?;

        let remote_root = peer
            .get_merkle_state(&MerkleRequest {
                entity_type: entity_type.to_string(),
                level: 0,
                bucket: None,
            })
            .await?;

        if local_root.hash == remote_root.hash {
            debug!("{}: roots match, nothing to sync", entity_type);
            return Ok(0);
        }

        debug!(
            "{}: roots differ (local={}, remote={}), drilling down",
            entity_type,
            &local_root.hash[..8],
            &remote_root.hash[..8]
        );

        // Step 2: Find differing buckets.
        // Asymmetric rule: a cache peer holds only what API hits asked it to
        // hold, so "we have rows the cache doesn't" is the steady state and
        // we MUST NOT interpret that as "push canonical data into the
        // cache". Drop push_ids entirely when peer.is_cache(). pull_ids is
        // kept — the cache may own authoritative records (its own
        // registered_device, system_config) that full peers should mirror.
        let (buckets_to_pull, mut buckets_to_push) =
            merkle::compare_trees(&local_root.children, &remote_root.children);
        if peer.is_cache() {
            buckets_to_push.clear();
        }

        let mut pull_ids: Vec<String> = Vec::new();
        let mut push_ids: Vec<String> = Vec::new();

        // Step 3: Drill into each differing bucket
        let all_buckets: Vec<String> = buckets_to_pull
            .iter()
            .chain(buckets_to_push.iter())
            .cloned()
            .collect::<std::collections::HashSet<String>>()
            .into_iter()
            .collect();

        for bucket in &all_buckets {
            let local_bucket = merkle_svc
                .get_state(&MerkleRequest {
                    entity_type: entity_type.to_string(),
                    level: 1,
                    bucket: Some(bucket.clone()),
                })
                .await?;

            let remote_bucket = peer
                .get_merkle_state(&MerkleRequest {
                    entity_type: entity_type.to_string(),
                    level: 1,
                    bucket: Some(bucket.clone()),
                })
                .await?;

            let (need_pull, need_push) =
                merkle::compare_trees(&local_bucket.children, &remote_bucket.children);

            pull_ids.extend(need_pull);
            // Same asymmetric rule at entity level: don't push into a cache.
            if !peer.is_cache() {
                push_ids.extend(need_push);
            }
        }

        let mut exchanged = 0usize;

        // Step 4: Pull missing entities from peer
        if !pull_ids.is_empty() {
            debug!(
                "{}: pulling {} entities from {}",
                entity_type,
                pull_ids.len(),
                peer.peer_url()
            );

            let pull_started = std::time::Instant::now();
            let entities = peer.pull_entities(entity_type, &pull_ids).await?;
            let pull_ms = pull_started.elapsed().as_millis() as u64;
            let pulled_n = entities.len();
            let resolve_started = std::time::Instant::now();
            let mut writes_n = 0usize;
            for entity in &entities {
                // Match the bootstrap policy: prefer canonical foo_id, fall back
                // to the implicit Thing id. Both sides MUST use the same key or
                // upsert routes the record to the wrong record-id.
                let id_field = match entity_type {
                    "registered_device" => Some("device_id"),
                    "order" => Some("order_id"),
                    _ => None,
                };
                let eid_opt = id_field
                    .and_then(|f| entity.get(f).and_then(|v| v.as_str()).map(String::from))
                    .or_else(|| entity.get("id").and_then(extract_entity_leaf_id));
                if let Some(eid_owned) = eid_opt {

                    match crate::sync::conflict::resolve_and_upsert(
                        &self.db,
                        entity_type,
                        &eid_owned,
                        entity.clone(),
                        &self.instance_id,
                    )
                    .await
                    {
                        Ok(written) => {
                            if written {
                                exchanged += 1;
                                writes_n += 1;
                                // Update local Merkle checksum
                                if let Err(e) = merkle_svc
                                    .record_checksum(entity_type, &eid_owned, entity)
                                    .await
                                {
                                    warn!("Checksum update failed for {}:{}: {}", entity_type, eid_owned, e);
                                }
                            }
                        }
                        Err(e) => warn!("Conflict resolve {}:{} failed: {}", entity_type, eid_owned, e),
                    }
                }
            }
            let resolve_ms = resolve_started.elapsed().as_millis() as u64;
            // Loud signal when a pull returns rows but none get written. That's
            // the cache-stale-checksum / version-skew "merkle never converges"
            // pattern — we keep pulling N entities every cycle, resolve says
            // "local wins/equal" on all of them, mark zero writes, next cycle
            // sees the same divergent root, drills, pulls the same N, repeats.
            // Cost: O(N) conflict resolves per cycle, indefinitely.
            if pulled_n > 0 && writes_n == 0 {
                warn!(
                    "{}: pulled {} entities from {} but wrote 0 — possible merkle non-convergence (cache stale checksums, peer version skew, hash determinism bug). pull_ms={} resolve_ms={}",
                    entity_type, pulled_n, peer.peer_url(), pull_ms, resolve_ms
                );
            } else {
                debug!(
                    "{}: pulled {} from {}, wrote {} (pull_ms={} resolve_ms={})",
                    entity_type, pulled_n, peer.peer_url(), writes_n, pull_ms, resolve_ms
                );
            }
        }

        // Step 5: Push our entities to peer
        if !push_ids.is_empty() {
            debug!(
                "{}: pushing {} entities to {}",
                entity_type,
                push_ids.len(),
                peer.peer_url()
            );

            // Fetch local entities by IDs
            let query = format!(
                "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) IN $ids",
                entity_type
            );
            let local_entities: Vec<Value> = self
                .db
                .query(&query)
                .bind(("ids", push_ids))
                .await
                .map_err(|e| e.to_string())?
                .take(0)
                .map_err(|e| e.to_string())?;

            if !local_entities.is_empty() {
                match peer
                    .push_entities(entity_type, &local_entities, &self.instance_id)
                    .await
                {
                    Ok(n) => exchanged += n,
                    Err(e) => warn!("Push {} to peer failed: {}", entity_type, e),
                }
            }
        }

        if exchanged > 0 {
            info!(
                "{}: exchanged {} entities with {}",
                entity_type,
                exchanged,
                peer.peer_url()
            );
        }

        Ok(exchanged)
    }

    // ─── Task Queue (reverse-fetch for NAT traversal) ─────────────────────────

    /// Poll peers for tasks assigned to us, execute them, and push results back.
    /// Returns total tasks processed across all peers.
    pub async fn process_tasks(&self) -> anyhow::Result<usize> {
        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        if peers.is_empty() {
            return Ok(0);
        }

        // Honour the same per-peer backoff that gates merkle sync — otherwise
        // fetch_tasks keeps hitting an unreachable peer every cycle and
        // floods the log even though the entity-sync half already gave up.
        let now = chrono::Utc::now();
        let active_peers: Vec<_> = {
            let health = self.peer_health.lock().await;
            peers
                .iter()
                .filter(|p| {
                    let url = p.peer_url().to_string();
                    match health.get(&url).and_then(|s| s.skip_until) {
                        Some(skip_until) if now < skip_until => {
                            debug!(
                                "Tasks: skipping {} ({}s remaining in backoff)",
                                url,
                                (skip_until - now).num_seconds()
                            );
                            false
                        }
                        _ => true,
                    }
                })
                .cloned()
                .collect()
        };

        if active_peers.is_empty() {
            return Ok(0);
        }

        let mut processed = 0usize;

        for peer in &active_peers {
            let tasks = match peer.fetch_tasks(&self.instance_id).await {
                Ok(t) => t,
                Err(e) => {
                    debug!("Failed to fetch tasks from {}: {}", peer.peer_url(), e);
                    continue;
                }
            };

            for task in &tasks {
                let action = task.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let task_id = task.get("id").and_then(|v| v.as_str()).unwrap_or("");

                if action == "request_raw_docs" {
                    let ticket_id = task.get("ticket_id").and_then(|v| v.as_str()).unwrap_or("");
                    if ticket_id.is_empty() || task_id.is_empty() {
                        continue;
                    }

                    // Query local document_raw for this ticket (parent + threads)
                    let docs: Vec<Value> = match self.db
                        .query("SELECT record::id(id) AS id, type, ticket_id, payload, updated_at FROM document_raw WHERE record::id(id) = $tid OR ticket_id = $tid ORDER BY updated_at ASC")
                        .bind(("tid", ticket_id.to_string()))
                        .await
                        .and_then(|mut r| r.take(0))
                    {
                        Ok(d) => d,
                        Err(e) => {
                            warn!("Task {}: failed to query local docs for ticket {}: {}", task_id, ticket_id, e);
                            continue;
                        }
                    };

                    if docs.is_empty() {
                        debug!("Task {}: no local docs for ticket {}, skipping", task_id, ticket_id);
                        continue;
                    }

                    // Push documents to the requesting peer
                    match peer.push_entities("document_raw", &docs, &self.instance_id).await {
                        Ok(n) => {
                            info!("Task {}: pushed {} document_raw records for ticket {} to {}", task_id, n, ticket_id, peer.peer_url());
                        }
                        Err(e) => {
                            warn!("Task {}: push to {} failed: {}", task_id, peer.peer_url(), e);
                            continue;
                        }
                    }

                    // Mark task as completed
                    if let Err(e) = peer.complete_task(task_id).await {
                        warn!("Task {}: failed to delete from peer: {}", task_id, e);
                    }

                    processed += 1;
                } else {
                    debug!("Unknown task action '{}', skipping task {}", action, task_id);
                }
            }
        }

        if processed > 0 {
            info!("Task queue: processed {} task(s)", processed);
        }
        Ok(processed)
    }

    // ─── Outbox (background retry for failed pushes) ─────────────────────────

    /// Process pending outbox records: for each, find a peer and push directly.
    ///
    /// On success the record is deleted. On failure `error_count` is incremented
    /// and `next_attempt_at` is pushed back with exponential backoff.
    pub async fn process_outbox(&self) -> anyhow::Result<usize> {
        // NOTE: OutboxRecord uses String for `id`, `next_attempt_at`, and
        // `created_at`, but SurrealDB stores `id` as a Thing and the two
        // timestamps as `datetime`. Without explicit projection / coercion
        // the deserialize fails ("Expected string, got record/datetime")
        // and the outbox stalls.
        let pending: Vec<OutboxRecord> = self
            .db
            .query(
                "SELECT *, \
                        record::id(id) AS id, \
                        type::string(next_attempt_at) AS next_attempt_at, \
                        type::string(created_at) AS created_at \
                 FROM sync_outbox \
                 WHERE next_attempt_at <= time::now() \
                 ORDER BY created_at ASC",
            )
            .await
            .map_err(|e| anyhow::anyhow!("Outbox query failed: {}", e))?
            .take(0)
            .map_err(|e| anyhow::anyhow!("Outbox deserialize failed: {}", e))?;

        if pending.is_empty() {
            return Ok(0);
        }

        // Discover peers once for the whole outbox batch
        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        if peers.is_empty() {
            debug!("Outbox: no peers online, deferring {} record(s)", pending.len());
            return Ok(0);
        }

        let total = pending.len();
        let mut sent = 0usize;

        for record in pending {
            // Try first available peer
            let mut pushed = false;
            for peer in &peers {
                match peer
                    .push_entities(
                        &record.entity_type,
                        &[record.payload.clone()],
                        &self.instance_id,
                    )
                    .await
                {
                    Ok(_) => {
                        // Delete the successfully sent record
                        let _: Option<Value> = self
                            .db
                            .delete(("sync_outbox", record.id.as_str()))
                            .await
                            .unwrap_or(None);
                        sent += 1;
                        pushed = true;
                        break;
                    }
                    Err(e) => {
                        debug!(
                            "Outbox push to {} failed: {}",
                            peer.peer_url(),
                            e
                        );
                        // Cross-NAT fallback: queue the push on the relay so the
                        // peer's mesh_relay_poller can apply it once it polls.
                        // We dispatch only — don't block waiting for the ack —
                        // because outbox sends are fire-and-forget and the next
                        // sync_cycle's merkle pass will reconcile if anything
                        // was missed. Without target_instance_id we can't address
                        // the relay queue, so skip silently.
                        if !peer.target_instance_id().is_empty() {
                            match self
                                .relay
                                .mesh_dispatch(
                                    peer.target_instance_id(),
                                    "push",
                                    serde_json::json!({
                                        "entity_type": record.entity_type,
                                        "entities": [record.payload.clone()],
                                        "source_instance": self.instance_id,
                                    }),
                                )
                                .await
                            {
                                Ok(_) => {
                                    let _: Option<Value> = self
                                        .db
                                        .delete(("sync_outbox", record.id.as_str()))
                                        .await
                                        .unwrap_or(None);
                                    sent += 1;
                                    pushed = true;
                                    debug!(
                                        "Outbox push via relay queued for {}",
                                        peer.target_instance_id()
                                    );
                                    break;
                                }
                                Err(e2) => {
                                    debug!(
                                        "Relay-routed push to {} also failed: {}",
                                        peer.target_instance_id(),
                                        e2
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if !pushed {
                let new_count = record.error_count + 1;
                let backoff_secs = 10i64 * 2i64.pow(new_count.min(6) as u32);

                if let Err(e2) = self
                    .db
                    .query(
                        "UPDATE sync_outbox SET \
                            error_count = $count, \
                            next_attempt_at = time::now() + $backoff \
                         WHERE id = $id",
                    )
                    .bind(("count", new_count))
                    .bind(("backoff", format!("{}s", backoff_secs)))
                    .bind(("id", record.id.clone()))
                    .await
                {
                    warn!("Failed to update outbox record backoff: {}", e2);
                }
            }
        }

        if sent > 0 {
            info!("Outbox: pushed {}/{} record(s) to peers", sent, total);
        }
        Ok(sent)
    }

    // ─── Real-Time Live Query Watcher ─────────────────────────────────────────

    /// Watch every `SYNC_ENTITY_TYPES` table via LIVE SELECT and keep
    /// `entity_checksum` in lockstep with their content. Spawns one task per
    /// table so a stalled stream on one table doesn't block the others.
    ///
    /// Cache nodes skip this — they don't maintain a full merkle tree.
    ///
    /// On CREATE/UPDATE: recompute content hash, UPSERT the matching
    /// `entity_checksum` row (skip-unchanged path same as `refresh_checksums`).
    /// On DELETE: drop the corresponding `entity_checksum` row.
    ///
    /// This replaces the scheduled `refresh_checksums()` call inside
    /// `sync_cycle()` as the primary up-to-date mechanism — scheduled refresh
    /// stays as a safety net in case a live stream silently drops events.
    pub fn spawn_live_watchers(self: &std::sync::Arc<Self>) {
        if self.is_cache_node() {
            debug!("spawn_live_watchers: skipped (node_role=cache)");
            return;
        }
        for entity_type in SYNC_ENTITY_TYPES {
            let engine = std::sync::Arc::clone(self);
            let et = entity_type.to_string();
            tokio::spawn(async move {
                if let Err(e) = engine.watch_one_entity(&et).await {
                    warn!("[live-watch] {} terminated: {}", et, e);
                }
            });
        }
        info!(
            "spawn_live_watchers: started {} live streams",
            SYNC_ENTITY_TYPES.len()
        );
    }

    async fn watch_one_entity(&self, entity_type: &str) -> anyhow::Result<()> {
        let merkle_svc = merkle::MerkleService::new(self.db.clone(), self.instance_id.clone());

        let query = format!("LIVE SELECT * FROM {}", entity_type);
        let mut response = match self.db.query(&query).await {
            Ok(r) => r,
            Err(e) => {
                // Table may not exist yet on a fresh node — treat as expected.
                debug!("[live-watch] {}: LIVE SELECT setup failed: {}", entity_type, e);
                return Ok(());
            }
        };
        let mut stream = response.stream::<surrealdb::Notification<Value>>(0)?;

        info!("[live-watch] {} → entity_checksum bridge active", entity_type);

        let id_field: Option<&'static str> = match entity_type {
            "registered_device" => Some("device_id"),
            "order" => Some("order_id"),
            _ => None,
        };

        while let Some(result) = stream.next().await {
            match result {
                Ok(notification) => {
                    let action = notification.action.to_string();
                    let data = &notification.data;

                    // Resolve canonical leaf id.
                    let eid_opt = id_field
                        .and_then(|f| data.get(f).and_then(|v| v.as_str()).map(String::from))
                        .or_else(|| data.get("id").and_then(extract_entity_leaf_id));
                    let Some(eid) = eid_opt else {
                        continue;
                    };

                    if action == "Delete" {
                        let _ = self
                            .db
                            .query("DELETE entity_checksum WHERE entity_type = $et AND entity_id = $eid")
                            .bind(("et", entity_type.to_string()))
                            .bind(("eid", eid.clone()))
                            .await;
                        debug!("[live-watch] {} delete → drop checksum {}", entity_type, eid);
                        continue;
                    }

                    // CREATE / UPDATE → recompute + upsert
                    if let Some(hash) = merkle::compute_content_hash(data) {
                        if let Err(e) = merkle_svc
                            .upsert_checksum(entity_type, &eid, &hash)
                            .await
                        {
                            warn!(
                                "[live-watch] {}: upsert checksum {}: {}",
                                entity_type, eid, e
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!("[live-watch] {} stream error: {}", entity_type, e);
                }
            }
        }

        warn!("[live-watch] {} stream ended unexpectedly", entity_type);
        Ok(())
    }

    /// Watch `sync_outbox` via LIVE SELECT. Triggers an immediate `process_outbox()`
    /// whenever a record is created or updated, giving zero-latency P2P push.
    /// Falls back to the 60s polling loop if this stream ends or errors.
    pub async fn watch_outbox(&self) -> anyhow::Result<()> {
        info!("[Sync] Starting real-time LIVE SELECT watcher for sync_outbox");

        let mut response = self.db.query("LIVE SELECT * FROM sync_outbox").await?;
        let mut stream = response.stream::<surrealdb::Notification<Value>>(0)?;

        while let Some(result) = stream.next().await {
            match result {
                Ok(notification) => {
                    let action = notification.action.to_string();
                    if action == "Create" || action == "Update" {
                        debug!("[Sync] Live event ({}) in sync_outbox, triggering immediate push", action);
                        if let Err(e) = self.process_outbox().await {
                            warn!("[Sync] Real-time outbox push failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("[Sync] Live stream error: {}", e);
                }
            }
        }

        warn!("[Sync] Live query stream for sync_outbox ended unexpectedly");
        Ok(())
    }

    // ─── Accessors ───────────────────────────────────────────────────────────

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn mesh_id(&self) -> &str {
        &self.mesh_id
    }

    pub fn db(&self) -> &SurrealDb {
        &self.db
    }

    pub fn relay(&self) -> &RelayClient {
        &self.relay
    }
}

/// Lightweight struct for deserializing outbox rows from SurrealDB.
#[derive(Debug, Clone, serde::Deserialize, surrealdb::types::SurrealValue)]
struct OutboxRecord {
    id: String,
    entity_type: String,
    #[allow(dead_code)]
    entity_id: String,
    payload: Value,
    error_count: u32,
    #[allow(dead_code)]
    next_attempt_at: String,
    #[allow(dead_code)]
    created_at: String,
}
