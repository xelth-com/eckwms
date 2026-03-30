use serde_json::Value;
use surrealdb::types::SurrealValue;
use tracing::{debug, info, warn};

use crate::db::SurrealDb;
use crate::sync::{
    merkle::{self, MerkleRequest, MerkleService},
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
    "user",
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
pub struct SyncEngine {
    instance_id: String,
    mesh_id: String,
    relay: RelayClient,
    db: SurrealDb,
    sync_secret: Option<String>,
}

impl SyncEngine {
    pub fn new(
        instance_id: String,
        mesh_id: String,
        relay: RelayClient,
        db: SurrealDb,
        sync_secret: Option<String>,
    ) -> Self {
        Self {
            instance_id,
            mesh_id,
            relay,
            db,
            sync_secret,
        }
    }

    // ─── P2P Merkle Sync ─────────────────────────────────────────────────────

    /// Run a full sync cycle: discover peers via relay, then Merkle-diff with each.
    /// Returns total entities exchanged (pulled + pushed) across all peers.
    pub async fn sync_cycle(&self) -> anyhow::Result<usize> {
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

        info!("Sync cycle: found {} online peer(s)", peers.len());
        let mut total = 0usize;

        for peer in &peers {
            for entity_type in SYNC_ENTITY_TYPES {
                match self.sync_entity_with_peer(peer, entity_type).await {
                    Ok(n) => total += n,
                    Err(e) => warn!(
                        "Sync {} with {} failed: {}",
                        entity_type,
                        peer.peer_url(),
                        e
                    ),
                }
            }
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

        // Step 2: Find differing buckets
        let (buckets_to_pull, buckets_to_push) =
            merkle::compare_trees(&local_root.children, &remote_root.children);

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
            push_ids.extend(need_push);
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

            let entities = peer.pull_entities(entity_type, &pull_ids).await?;
            for entity in &entities {
                if let Some(eid) = entity.get("id").and_then(|v| v.as_str()) {
                    let eid_owned = eid.to_string();
                    let mut clean = entity.clone();
                    if let Some(obj) = clean.as_object_mut() {
                        obj.remove("id");
                    }

                    let result: Result<Option<Value>, _> = self
                        .db
                        .upsert((entity_type, eid_owned.as_str()))
                        .content(clean)
                        .await;

                    match result {
                        Ok(_) => {
                            exchanged += 1;
                            // Update local Merkle checksum
                            if let Err(e) = merkle_svc
                                .record_checksum(entity_type, &eid_owned, entity)
                                .await
                            {
                                warn!("Checksum update failed for {}:{}: {}", entity_type, eid_owned, e);
                            }
                        }
                        Err(e) => warn!("UPSERT {}:{} failed: {}", entity_type, eid_owned, e),
                    }
                }
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

    // ─── Outbox (background retry for failed pushes) ─────────────────────────

    /// Process pending outbox records: for each, find a peer and push directly.
    ///
    /// On success the record is deleted. On failure `error_count` is incremented
    /// and `next_attempt_at` is pushed back with exponential backoff.
    pub async fn process_outbox(&self) -> anyhow::Result<usize> {
        let pending: Vec<OutboxRecord> = self
            .db
            .query(
                "SELECT * FROM sync_outbox \
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

    // ─── Accessors ───────────────────────────────────────────────────────────

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn mesh_id(&self) -> &str {
        &self.mesh_id
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
