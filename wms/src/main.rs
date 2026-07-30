mod ai;
mod handlers;
mod mcp;
mod middleware;
mod services;
mod utils;
mod web;

use chrono::Timelike;
use axum::{extract::DefaultBodyLimit, middleware as axum_mw, routing::{any, delete, get, post, put}, Json, Router};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};

use eck_core::db::SurrealDb;
use eck_core::sync::engine::SyncEngine;
use eck_core::sync::hedera::HederaClient;
use eck_core::sync::relay_client::RelayClient;
use eck_core::utils::identity::ServerIdentity;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    server: String,
    version: String,
}

pub struct AppState {
    /// Zone 2 — business / operational data. xelixir reads this freely
    /// via /X/ops/surrealql_read; ops handlers query it for items,
    /// orders, devices, configs, etc.
    pub db: SurrealDb,
    /// Zone 1 — end-user PII (accounts, credentials, anything that
    /// identifies a real human). Physically a separate SurrealKv file
    /// (`data/wms_users.db`) so OS file permissions can enforce that
    /// only the WMS process opens it. xelixir does NOT receive this
    /// handle — all /X/ops/* code paths use `db` only.
    pub users_db: SurrealDb,
    pub sync_engine: Arc<SyncEngine>,
    pub hedera: Option<HederaClient>,
    pub jwt_secret: String,
    pub sync_secret: Option<String>,
    pub server_identity: ServerIdentity,
    pub instance_id: String,
    pub mesh_id: String,
    pub port: u16,
    pub setup_password: RwLock<Option<String>>,
    pub ws_tx: tokio::sync::broadcast::Sender<String>,
    pub agent_controller: Arc<services::agent_manager::AgentController>,
    /// `"full"` (default) or `"cache"`. Cache nodes keep heartbeating but skip
    /// the periodic merkle sync — they pull entities on demand instead.
    pub node_role: String,
    /// In-flight status of customer-added-language translation jobs (process-
    /// local; see `handlers::i18n::add_language`). Not mesh-synced — the
    /// `i18n_label` rows are the durable result.
    pub i18n_lang_jobs: crate::handlers::i18n::LangJobMap,
    /// Ephemeral `pseudonym-token → clear-value` reverse map for the MCP
    /// surface. Populated whenever a `/mcp` tool tokenizes a PII field; read
    /// back by the master-only `reveal_tokens` method. RAM only, bounded FIFO —
    /// deliberately NO persistent plaintext vault (see `mcp::reveal`).
    pub pii_reveal: Arc<crate::mcp::reveal::PiiRevealStore>,
}

impl AppState {
    /// GET-by-id helper that integrates cache-mode pull-through.
    ///
    /// On a local hit: returns the row, bumps `last_accessed_at` on the
    /// matching `entity_checksum` row so the LRU evictor knows it's still
    /// hot (no-op on full peers since `is_cache=true` filter won't match).
    ///
    /// On a local miss + node_role=cache: pulls the row from any reachable
    /// full peer via [`crate::sync::engine::SyncEngine::pull_entity_on_demand`],
    /// upserts it locally flagged `is_cache=true`, and returns it.
    ///
    /// On a local miss + node_role=full: returns `None` immediately. The
    /// caller is expected to surface a 404.
    pub async fn get_synced_entity(
        &self,
        entity_type: &str,
        id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let row: Option<serde_json::Value> = self
            .db
            .select((entity_type, id))
            .await
            .map_err(|e| e.to_string())?;

        if let Some(v) = row {
            if self.node_role == "cache" {
                self.sync_engine.touch_cache(entity_type, id).await;
            }
            return Ok(Some(v));
        }

        if self.node_role == "cache" {
            return Ok(self.sync_engine.pull_entity_on_demand(entity_type, id).await);
        }
        Ok(None)
    }

    /// Write helper for SYNCED tables: MERGE `fields` into `entity_type:id` AND
    /// advance THIS node's `_vclock` so the change causally DOMINATES peers.
    ///
    /// A synced-table write that leaves `_vclock` untouched resolves on peers as
    /// "local wins/equal" and never adopts (and, before the resolve-layer fix,
    /// churned the mesh). So every create/update of a synced entity should route
    /// through here instead of a raw `UPSERT/UPDATE` — one place stamps the clock
    /// so a new handler can't forget (the per-writer patching kept regressing).
    ///
    /// Merges in-memory over the current row and rewrites via `.content()` (MERGE
    /// semantics, no field loss). `updated_at` is always stamped; `_vclock` only
    /// advances on a REAL hashed-content change (ignored fields excluded), so a
    /// no-op Save doesn't bump the clock and storm the mesh. Returns the stored row.
    pub async fn put_synced_entity(
        &self,
        entity_type: &str,
        id: &str,
        fields: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let current: Option<serde_json::Value> = self
            .db
            .select((entity_type, id))
            .await
            .map_err(|e| e.to_string())?;

        // Build the full merged record (current ← fields).
        let mut merged = current.clone().unwrap_or_else(|| serde_json::json!({}));
        if let (Some(m), Some(f)) = (merged.as_object_mut(), fields.as_object()) {
            for (k, v) in f.iter() {
                m.insert(k.clone(), v.clone());
            }
        }

        // Real content change? (both sides still carry `id`, so it cancels; ignored
        // fields like updated_at/_vclock are excluded by compute_content_hash.)
        let changed = match &current {
            None => true,
            Some(cur) => {
                eck_core::sync::merkle::compute_content_hash(&merged)
                    != eck_core::sync::merkle::compute_content_hash(cur)
            }
        };

        if let Some(obj) = merged.as_object_mut() {
            obj.remove("id"); // addressed by type::record($tb,$id); not part of content
            if changed {
                let cur_vc = current.as_ref().and_then(|c| c.get("_vclock").cloned());
                obj.insert(
                    "_vclock".to_string(),
                    eck_core::sync::conflict::next_local_vclock(cur_vc.as_ref(), &self.instance_id),
                );
            }
        }

        // CONTENT first, then stamp `updated_at = time::now()` server-side: through
        // a serde_json bind the stamp could only be an RFC3339 STRING, and string
        // timestamps poison `updated_at + duration` backoff arithmetic downstream
        // (the a0c275d/133279d class — this helper used to leave one on every
        // write). Bound `type::record($tb,$id)` keeps all-digit string ids intact
        // (a literal would parse as an integer id — see conflict::attach_vclock_to_db).
        let mut resp = self
            .db
            .query(
                "UPSERT type::record($tb, $id) CONTENT $content; \
                 UPDATE type::record($tb, $id) SET updated_at = time::now() RETURN AFTER;",
            )
            .bind(("tb", entity_type.to_string()))
            .bind(("id", id.to_string()))
            .bind(("content", merged))
            .await
            .map_err(|e| e.to_string())?;
        let written: Vec<serde_json::Value> = resp.take(1).map_err(|e| e.to_string())?;
        written
            .into_iter()
            .next()
            .ok_or_else(|| "put_synced_entity: upsert returned nothing".to_string())
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024) // 16 MB — SurrealDB debug builds + large Zoho payloads need extra stack
        .build()
        .expect("Failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    let _ = dotenvy::dotenv();

    // Dual sink: pretty ANSI in the terminal (for the human), plain rolling
    // file in `data/logs/wms.log.YYYY-MM-DD` (for the AI / postmortems).
    // `WMS_LOG_DIR` overrides the directory; default lives next to the DB.
    // Non-blocking writer keeps the runtime off the disk fsync path; the
    // returned guard must stay alive until the process exits, otherwise
    // buffered log lines are dropped on shutdown.
    let log_dir = std::env::var("WMS_LOG_DIR").unwrap_or_else(|_| "data/logs".into());
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "wms.log");
    let (file_writer, _file_guard) = tracing_appender::non_blocking(file_appender);

    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .init();

    info!("Starting eckWMS (9eck.com monorepo edition)");
    info!("File logs: {}/wms.log.<date>", log_dir);

    // SurrealDB — Zone 2 (business / operational data).
    let db_path = std::env::var("SURREAL_DB_PATH")
        .unwrap_or_else(|_| "data/wms.db".into());
    let db = eck_core::db::connect(&db_path)
        .await
        .expect("Failed to connect to SurrealDB");

    // SurrealDB — Zone 1 (PII / credentials). Physically separate file so
    // file-level OS permissions can enforce that only the WMS process
    // opens it. xelixir's process must not be granted read on this file.
    let users_db_path = std::env::var("SURREAL_USERS_DB_PATH")
        .unwrap_or_else(|_| "data/wms_users.db".into());
    let users_db = eck_core::db::connect_with_db(&users_db_path, "users")
        .await
        .expect("Failed to connect to users SurrealDB");

    // Zone 2 schemaless bootstrap. `user` moved BACK into the mesh DB on
    // 2026-07-09 (owner decision): real staff accounts replicate across the
    // customer's own mesh so one password works on every node. The record id
    // is the USERNAME (deterministic → independently-seeded nodes converge
    // per-account instead of duplicating). The install-time setup-admin stays
    // in the node-local `users_db` and never meshes. Do NOT reintroduce the
    // old `REMOVE TABLE IF EXISTS user` guard here — it would wipe the synced
    // table on every boot.
    if let Err(e) = db
        .query(
            "DEFINE TABLE IF NOT EXISTS user SCHEMALESS;
             DEFINE INDEX IF NOT EXISTS user_username ON user FIELDS username UNIQUE;
             DEFINE TABLE IF NOT EXISTS item SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS product SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS partner SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS order SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS picking SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS location SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS rack SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS quant SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS document SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS document_raw SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS registered_device SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS file_resource SCHEMALESS;
             -- OVERWRITE (not IF NOT EXISTS): the original definition said
             -- IN record, which SurrealDB reads as the table literally named
             -- record — so EVERY RELATE (user, order, …) failed coercion and
             -- no attachment edge was ever written. A bare TYPE RELATION
             -- accepts any record on both ends; OVERWRITE keeps existing rows.
             DEFINE TABLE OVERWRITE has_attachment TYPE RELATION SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS contains TYPE RELATION IN location OUT rack SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ai_telemetry SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS entity_checksum SCHEMALESS;
             -- entity_checksum is keyed by (entity_type, entity_id): every merkle
             -- upsert_checksum / delete / per-type select filters on these. Without
             -- this index each upsert is a FULL SCAN of the growing table, so
             -- bootstrap_checksums / refresh_checksums become O(n²) and peg ~2
             -- cores on a full node with real data (root cause of the 2026-06
             -- full-node idle spin — perf: Expr::compute + KMergeIterator scan).
             DEFINE INDEX IF NOT EXISTS entity_checksum_key ON entity_checksum FIELDS entity_type, entity_id;
             DEFINE TABLE IF NOT EXISTS sync_outbox SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS system_alert SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ai_task SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ai_thought SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ai_sop SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ai_inbox SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS mesh_task SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS system_config SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ops_audit_log SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS xelixir_nonce SCHEMALESS;
             -- The replay guard's ONLY arbiter: the nonce column is unique, so two
             -- concurrent `/mcp/signed` (or ops-envelope) requests bearing the same
             -- nonce can't both be admitted — exactly one INSERT commits, the rest
             -- bounce off this index. Without it the check-then-insert guard raced
             -- under the direct HTTP ingress. Consumers treat the violation as seen.
             DEFINE INDEX IF NOT EXISTS xelixir_nonce_unique ON xelixir_nonce FIELDS nonce UNIQUE;
             -- Server-issued single-use challenge nonces for replay-proof device
             -- auth (GET /api/auth/device-challenge → signed into register-device).
             -- Consumed (deleted) on use; a pruner drops expired rows.
             DEFINE TABLE IF NOT EXISTS device_auth_challenge SCHEMALESS;
             DEFINE INDEX IF NOT EXISTS device_auth_challenge_nonce ON device_auth_challenge FIELDS nonce UNIQUE;
             DEFINE TABLE IF NOT EXISTS peer_health_state SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS scan_log SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS repair_event SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS crm_update_log SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS opportunity SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS trip SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS cell_tower SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS visit_task SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS vehicle SCHEMALESS;
             -- Every SYNC_ENTITY_TYPES table must exist before the live-watch
             -- LIVE SELECT bridges start, or the watcher for that type dies at
             -- boot ('table does not exist') and local writes stop reaching the
             -- merkle tree until restart-after-first-insert.
             DEFINE TABLE IF NOT EXISTS move_line SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS action_proof SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS delivery_carrier SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS delivery_tracking SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS stock_picking_delivery SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS device_intake SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS inventory_discrepancy SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS product_alias SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS category SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS menu_item SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS odoo_warehouse SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS odoo_location SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS odoo_product SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS odoo_quant SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS stock_position SCHEMALESS;",
        )
        .await
    {
        tracing::warn!("Failed to ensure Zone 2 tables: {}", e);
    }

    // Zone 1 schemaless bootstrap — since 2026-07-09 this DB holds ONLY the
    // install-time setup-admin bootstrap account (node-local, never meshes).
    // Real accounts live in the mesh DB above. Sidecar PII tables (order_pii,
    // partner_pii, picking_pii) are created lazily by the handlers that own
    // them when those handlers land.
    if let Err(e) = users_db
        .query("DEFINE TABLE IF NOT EXISTS user SCHEMALESS;")
        .await
    {
        tracing::warn!("Failed to ensure Zone 1 tables: {}", e);
    }


    // Ensure search indexes exist (idempotent) — in the BACKGROUND. On an
    // up-to-date node every statement is an instant no-op, but a node meeting
    // a NEW index migration builds it over its full dataset (FULLTEXT over
    // ~10k documents), which on weak hardware takes many minutes. Running
    // that synchronously kept the HTTP listener down past the kiosk OTA
    // health window → the self-updater rollback-looped for a whole day on a
    // perfectly good binary (2026-07-19). Boot must reach the listener fast;
    // queries that need a still-building index degrade transiently instead
    // (BM25/@@ and KNN error, CONTAINS falls back to a scan) — self-healing
    // the moment the build lands.
    {
        let db_idx = db.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let res = db_idx
                .query(
                    "DEFINE ANALYZER IF NOT EXISTS custom_analyzer TOKENIZERS blank,class,camel,punct FILTERS lowercase,ascii;
             DEFINE INDEX IF NOT EXISTS issue_bm25 ON order FIELDS issue_description FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS order_number_bm25 ON order FIELDS order_number FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS customer_name_bm25 ON order FIELDS customer_name FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS embedding_hnsw ON order FIELDS embedding HNSW DIMENSION 768 DIST COSINE;
             DEFINE INDEX IF NOT EXISTS task_state_idx ON ai_task FIELDS state;
             DEFINE INDEX IF NOT EXISTS sop_trigger_bm25 ON ai_sop FIELDS trigger_context FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS sop_embedding_hnsw ON ai_sop FIELDS embedding HNSW DIMENSION 768 DIST COSINE;
             -- Polling-status indexes. The embedding worker + observer poll
             -- WHERE embedding_status/summary_status = 'pending' every few seconds.
             -- Without these each poll is a FULL TABLE SCAN (idle-CPU root cause).
             DEFINE INDEX IF NOT EXISTS document_embstatus_idx ON document FIELDS embedding_status;
             DEFINE INDEX IF NOT EXISTS document_sumstatus_idx ON document FIELDS summary_status;
             -- Document full-text: search the MASKED fields (ai_summary +
             -- distilled subject), never raw payload. The legacy
             -- doc_content_bm25 (payload.content) indexed a field
             -- support_ticket rows don't even have — document BM25 was dead
             -- weight — and pointing search at raw text would let callers
             -- probe PII the output layer tokenizes. 2026-07-18.
             REMOVE INDEX IF EXISTS doc_content_bm25 ON document;
             DEFINE INDEX IF NOT EXISTS doc_summary_bm25 ON document FIELDS ai_summary FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS doc_subject_bm25 ON document FIELDS meta.subject FULLTEXT ANALYZER custom_analyzer BM25;
             -- Exact pseudonym-token lookup (pii_fingerprints array).
             DEFINE INDEX IF NOT EXISTS doc_pii_fps_idx ON document FIELDS pii_fingerprints;
             DEFINE INDEX IF NOT EXISTS order_embstatus_idx ON order FIELDS embedding_status;
             DEFINE INDEX IF NOT EXISTS partner_embstatus_idx ON partner FIELDS embedding_status;
             DEFINE INDEX IF NOT EXISTS product_embstatus_idx ON product FIELDS embedding_status;
             DEFINE INDEX IF NOT EXISTS picking_embstatus_idx ON picking FIELDS embedding_status;
             -- Content translations (viewer-language summaries) + graph edge
             DEFINE TABLE IF NOT EXISTS translation SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS has_translation TYPE RELATION IN record OUT translation SCHEMALESS;",
                )
                .await;
            match res {
                Ok(_) => tracing::info!(
                    "search indexes ensured in {} ms",
                    started.elapsed().as_millis()
                ),
                Err(e) => tracing::warn!("Failed to ensure search indexes: {}", e),
            }
        });
    }

    // Self-heal for the datetime-as-string bug class (2026-07-05 a0c275d,
    // 2026-07-21 133279d, 2026-07-22 mesh-adopt round-trip): writers/adopters
    // that stored `updated_at` as an RFC3339 STRING poison the AI backoff
    // filter's `time::now() > updated_at + duration` ("Cannot perform addition
    // with '<string>' and '2m'") — the whole cycle errored and docs never left
    // 'pending'. The writers are fixed (time::now() stamps; conflict.rs
    // post-adopt coercion), but rows written by OLDER builds — and rows adopted
    // from peers still running them — persist, so sweep EVERY synced table on
    // each start (was: `document` only; partner/product had silently
    // accumulated ~9k unhealed string stamps). Idempotent and cheap: the string
    // type IS the bug's fingerprint, a healed table matches nothing.
    for et in eck_core::sync::engine::SYNC_ENTITY_TYPES {
        // The SET expression must be TOTAL: SurrealDB v3 evaluates it eagerly on
        // scanned rows BEFORE the WHERE filter applies, so a bare `<datetime>
        // updated_at` dies on any row MISSING the field ("Could not cast into
        // `datetime` using input `NONE`") and the whole table silently stays
        // unhealed (item/user/location had exactly such rows). The WHERE still
        // limits which rows are WRITTEN (no LIVE/outbox storm on healed tables).
        let q = format!(
            "UPDATE {et} SET updated_at = \
                 IF type::is_string(updated_at) THEN <datetime> updated_at ELSE updated_at END \
             WHERE type::is_string(updated_at)"
        );
        // `.check()` matters: a statement-level error (bad cast, parse) lives
        // INSIDE the Response — a bare `.await?` only catches transport errors
        // and would silently skip the table.
        if let Err(e) = db.query(&q).await.and_then(|r| r.check()) {
            tracing::warn!("{}.updated_at string→datetime self-heal failed: {}", et, e);
        }
    }
    // ONE-TIME re-queue (system_config flag) of every doc the 2026-07-06 bug
    // stranded — both those the Observer already retired ('failed') and those
    // left in 'pending' with a burnt retry counter — so they embed/summarize
    // afresh now that the cycle runs. Reset counters so the Observer stops
    // retiring them on the next tick.
    if let Err(e) = db
        .query(
            "LET $healed = (SELECT VALUE done FROM system_config:doc_heal_20260706_v2)[0]; \
             IF $healed != true { \
                 UPDATE document SET embedding_status = 'pending', embedding_retries = 0, embedding_error = NONE \
                     WHERE embedding_status = 'failed' OR (embedding_status = 'pending' AND embedding_retries >= 3); \
                 UPDATE document SET summary_status = 'pending', summary_retries = 0, summary_error = NONE \
                     WHERE summary_status = 'failed' OR (summary_status = 'pending' AND summary_retries >= 3); \
                 UPSERT system_config:doc_heal_20260706_v2 SET done = true, at = time::now(); \
             };",
        )
        .await
    {
        tracing::warn!("stranded-document one-time re-queue failed: {}", e);
    }

    // Seed UI labels (i18n_label) — INSERT-if-missing from the baked-in seed;
    // existing DB rows (manual edits) always win. DB is the runtime truth.
    if let Err(e) = handlers::i18n::seed_i18n_labels(&db).await {
        warn!("i18n label seeding failed: {}", e);
    }

    // pii_fingerprints self-heal: the pseudonym-token index of a document's
    // masked fields is DERIVED (token regex over ai_summary + meta.subject,
    // plus re-hashing the meta identity fields — all meshed inputs) and
    // merkle-IGNORED, so every node maintains its own copy — consumer nodes
    // receive summaries over mesh without fingerprints and must derive
    // locally. Startup + every 30 min; batches of 500 so a first run over the
    // whole table doesn't hold one query open for minutes. Rows the GDPR
    // erasure parked (`pii_fingerprints = []`) are NOT `IS NONE` and stay
    // untouched.
    {
        let db_fps = db.clone();
        tokio::spawn(async move {
            // obfuscate_pii needs the SYNC_SECRET pepper (fails closed
            // without one); the regex extraction over already-masked text
            // works regardless.
            let has_pepper = std::env::var("SYNC_SECRET").is_ok_and(|s| !s.is_empty());
            loop {
                let rows: Vec<serde_json::Value> = match db_fps
                    .query(
                        "SELECT record::id(id) AS id, ai_summary, meta FROM document \
                         WHERE ai_summary != NONE AND ai_summary != '' \
                           AND pii_fingerprints IS NONE \
                         LIMIT 500",
                    )
                    .await
                    .and_then(|mut r| r.take(0))
                {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("pii_fingerprints self-heal query failed: {}", e);
                        Vec::new()
                    }
                };
                let n = rows.len();
                for row in &rows {
                    let Some(id) = row.get("id").and_then(|v| v.as_str()) else { continue };
                    let summary = row.get("ai_summary").and_then(|v| v.as_str()).unwrap_or("");
                    let meta = row.get("meta").cloned().unwrap_or_default();
                    let subject = meta.get("subject").and_then(|v| v.as_str()).unwrap_or("");
                    let mut fps = eck_core::utils::anonymizer::extract_pii_tokens(
                        &format!("{subject}\n{summary}"),
                    );
                    if has_pepper {
                        for (field, label) in [
                            ("customer", "Name"),
                            ("email", "Email"),
                            ("phone", "Phone"),
                            ("address", "Address"),
                        ] {
                            if let Some(v) = meta.get(field).and_then(|v| v.as_str()) {
                                if !v.trim().is_empty() {
                                    fps.push(eck_core::utils::anonymizer::obfuscate_pii(
                                        v.trim(),
                                        label,
                                    ));
                                }
                            }
                        }
                    }
                    fps.sort();
                    fps.dedup();
                    // An empty result still writes [] so the row leaves the
                    // IS NONE candidate set (self-limiting sweep).
                    if let Err(e) = db_fps
                        .query("UPDATE type::record($rid) SET pii_fingerprints = $fps RETURN NONE")
                        .bind(("rid", format!("document:`{}`", id)))
                        .bind(("fps", fps))
                        .await
                    {
                        warn!("pii_fingerprints self-heal write failed for {}: {}", id, e);
                    }
                }
                if n > 0 {
                    info!("pii_fingerprints self-heal: derived for {} document(s)", n);
                }
                if n == 500 {
                    continue; // more batches pending — keep draining
                }
                tokio::time::sleep(std::time::Duration::from_secs(1800)).await;
            }
        });
    }

    // Mesh Sync
    use eck_core::utils::identity::{ensure_uuid_instance_id, compute_mesh_id};
    let raw_id = std::env::var("INSTANCE_ID").unwrap_or_default();
    let instance_id = ensure_uuid_instance_id(&raw_id);
    // Let the geocoder's free-function writers stamp the local node's vector
    // clock so resolved coordinates win mesh convergence (no HQ-revert).
    services::geocoder::set_instance_id(instance_id.clone());

    // One-time (idempotent) migration of pre-existing local accounts from the
    // old node-local users_db into the mesh `user` table (setup-admin excluded).
    // Runs every boot; no-ops once the mesh table is at least as new per username.
    match handlers::auth::migrate_users_to_mesh(&db, &users_db, &instance_id).await {
        Ok(0) => {}
        Ok(n) => info!("Migrated {} user account(s) from users_db into the mesh user table", n),
        Err(e) => tracing::warn!("users_db → mesh user migration failed: {}", e),
    }
    let sync_secret = std::env::var("SYNC_SECRET").ok().filter(|s| !s.is_empty());
    let mesh_id = compute_mesh_id(sync_secret.as_deref().unwrap_or("0"));
    let relay = RelayClient::new_multi(
        &RelayClient::relay_urls_from_env(),
        &instance_id,
        &mesh_id,
    );
    let sync_engine_role = std::env::var("MESH_NODE_ROLE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "full".to_string());
    let sync_engine = Arc::new(SyncEngine::new(
        instance_id.clone(),
        mesh_id.clone(),
        relay,
        db.clone(),
        sync_secret.clone(),
        sync_engine_role,
    ));

    // Backfill entity_checksum for every existing row so the merkle tree
    // reflects local data on first boot. Historically only sync-engine code
    // paths called record_checksum, so direct .create/.update/.upsert on a
    // synced table left the merkle root empty — and "roots match" short-
    // circuited replication. Run once at startup, idempotent thereafter.
    //
    // Spawned async — blocking startup here would hold the HTTP listener for
    // tens of minutes when fresh tables enter SYNC_ENTITY_TYPES (e.g. adding
    // `document` against 6k rows × per-row fsync). The live watchers below
    // already serve new writes correctly; bootstrap only catches up the
    // historical backfill, which can race with serving traffic — sync_cycle
    // tolerates a partially-built merkle root and converges on the next tick.
    {
        let engine_boot = Arc::clone(&sync_engine);
        tokio::spawn(async move {
            if let Err(e) = engine_boot.bootstrap_checksums().await {
                warn!("entity_checksum bootstrap failed: {}", e);
            }
        });
    }

    // Restore peer backoff state from disk so chronically-unreachable peers
    // (cross-NAT scenarios where the other side dials in instead) don't
    // spend the first 5 min of each restart pretending they might be
    // reachable. State written by `SyncEngine::persist_peer_health` on
    // every transition.
    if let Err(e) = sync_engine.load_peer_health().await {
        warn!("peer_health restore failed: {}", e);
    }

    // Real-time entity_checksum maintenance: one LIVE SELECT per synced
    // table. This is the PRIMARY up-to-date path — anything that
    // .create/.update/.upsert/.delete's a row in a SYNC_ENTITY_TYPES table
    // updates the merkle tree within ~milliseconds. Watchers self-respawn on
    // stream loss; the full re-hash in sync_cycle is only an hourly
    // integrity sweep (ECK_CHECKSUM_SWEEP_SECS), not a per-minute chore.
    sync_engine.spawn_live_watchers();

    // Spawn real-time outbox watcher (LIVE SELECT — zero-latency push)
    {
        let engine_live = Arc::clone(&sync_engine);
        tokio::spawn(async move {
            if let Err(e) = engine_live.watch_outbox().await {
                warn!("Live outbox watcher failed: {} — falling back to polling only", e);
            }
        });
    }

    // Spawn periodic sync worker (Merkle reconciliation every 60s + outbox fallback)
    {
        let engine = Arc::clone(&sync_engine);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Err(e) = engine.process_outbox().await {
                    warn!("Sync outbox error: {}", e);
                }
                if let Err(e) = engine.sync_cycle().await {
                    warn!("Sync cycle error: {}", e);
                }
            }
        });
    }

    // Spawn nightly backup worker (3:00 AM)
    {
        let bg_db = db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let now = chrono::Local::now();
                if now.hour() == 3 {
                    match services::backup::create_backup(&bg_db).await {
                        Ok(_) => info!("Nightly backup completed successfully"),
                        Err(e) => error!("Nightly backup failed: {}", e),
                    }
                    // Sleep 23 hours to prevent multiple triggers in the same hour
                    tokio::time::sleep(std::time::Duration::from_secs(23 * 3600)).await;
                }
            }
        });
    }

    // Spawn Gemini embedding worker (processes pending documents & orders).
    // Auth is dual-mode: `studio` (BYO AI Studio key, open-source default) or
    // `managed` (server-minted Vertex Bearer). See eck_core::ai::AiAuth.
    {
        // Gate on config presence (a key for studio; mint URL + license, or a
        // pinned bearer, for managed). The managed bearer itself is minted
        // lazily inside each worker cycle — see AiAuth::resolve. A node running
        // ECK_EMBED_MODE=local can embed on-device with NO cloud auth at all —
        // it still gets the embedding worker (summarization stays cloud-only).
        let cloud_ai = eck_core::ai::AiAuth::is_enabled_in_env();
        let local_embed = ai::embeddings::embed_mode() == "local";
        if cloud_ai || local_embed {
            // PPRL anonymisation peppers PII with SYNC_SECRET. Local mode needs
            // it too: the deterministic field masking (obfuscate_pii) runs on
            // every embed text regardless of backend. Without the pepper the
            // masking falls back to a public default and becomes reversible —
            // refuse to run AI rather than ship a false privacy guarantee.
            assert!(
                sync_secret.is_some(),
                "AI is configured (AiAuth or ECK_EMBED_MODE=local) but SYNC_SECRET is \
                 unset — refusing to start: PPRL anonymisation would use a \
                 publicly-known default pepper and be reversible. Set SYNC_SECRET to enable AI."
            );
            let emb_db = db.clone();
            // Missing GEMINI_* names are hard config errors in cloud mode (fail
            // fast); a local-embed-only node legitimately has none of them set —
            // the worker never makes a cloud call in local mode.
            let need = |key: &str| -> String {
                match std::env::var(key) {
                    Ok(v) => v,
                    Err(_) if !cloud_ai => String::new(),
                    Err(_) => panic!("{key} must be set in .env"),
                }
            };
            let gen_model = need("GEMINI_GENERATION_MODEL");
            let sum_model = need("GEMINI_SUMMARY_MODEL");
            let emb_model = need("GEMINI_EMBEDDING_MODEL");
            // Symmetric mesh (default): every AI-enabled node runs the embed
            // worker; whoever meets a vector-less row first authors its vector
            // and the mesh adopts it. Asymmetric (one AI-dedicated box): set
            // ECK_EMBED_WORKER=0 on the other nodes — they consume vectors
            // from the mesh and never call the embed API themselves.
            let embed_worker_on = std::env::var("ECK_EMBED_WORKER")
                .map(|v| v.trim() != "0")
                .unwrap_or(true);
            if embed_worker_on {
                tokio::spawn(ai::embeddings::start_embedding_worker(emb_db, gen_model, emb_model, instance_id.clone()));
            } else {
                info!("Embedding worker disabled (ECK_EMBED_WORKER=0) — vectors arrive via mesh sync");
            }
            // Summarization resolves each ticket's address inline at summary time
            // (free zip/city/Vorwahl, then AI grounding if enabled) — no separate
            // address pass. It is cloud-only: a local-embed-only node skips it.
            // Same consumer switch as embeddings: ECK_SUMMARY_WORKER=0 turns
            // this node into a pure consumer (summaries arrive via mesh).
            // Since managed auto-mint, EVERY full node can reach Gemini, so
            // without this gate (and the source_instance_id gate inside the
            // worker) each node re-summarizes the whole mesh-synced backlog
            // in parallel — N-fold spend from the one shared wallet
            // (2026-07-23: kiosk duplicated an entire re-import wave).
            let summary_worker_on = std::env::var("ECK_SUMMARY_WORKER")
                .map(|v| v.trim() != "0")
                .unwrap_or(true);
            if cloud_ai && summary_worker_on {
                let sum_db = db.clone();
                tokio::spawn(ai::summarization::start_summarization_worker(sum_db, sum_model, instance_id.clone()));
                let mode = std::env::var("ECK_AI_MODE").unwrap_or_else(|_| "studio".into());
                info!("Embedding + Summarization workers spawned (AI mode={mode})");
            } else if cloud_ai {
                info!("Summarization worker disabled (ECK_SUMMARY_WORKER=0) — summaries arrive via mesh sync");
            } else {
                info!("Local-embed node without cloud AI: embedding worker on-device; summarization disabled");
            }
        } else {
            warn!("AI auth not configured — AI workers disabled");
        }
    }

    // One-shot: distil any legacy `shipment` blob rows into the meshed delivery
    // models (stock_picking_delivery/delivery_tracking/delivery_carrier). Runs
    // once on the node that still has them, no-ops everywhere else. Unconditional
    // (not gated by ENABLE_SCRAPERS) so an existing blob table converges even on
    // a node where scrapers are off.
    {
        let mig_db = db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            services::delivery::migrate_from_shipment_if_needed(&mig_db).await;
        });
    }

    // Spawn scraper scheduler (hourly: OPAL/DHL/Zoho, daily 06:00: Excel/Exact Online)
    if std::env::var("ENABLE_SCRAPERS").unwrap_or_default() == "true" {
        let sched_db = db.clone();
        let sched_iid = instance_id.clone();
        tokio::spawn(services::scheduler::start_cron_jobs(sched_db, sched_iid));
        info!("Scraper scheduler spawned");
    } else {
        info!("Scrapers disabled (ENABLE_SCRAPERS != true). Run on edge node only.");
    }

    // Self-diagnosis: sample loop counters (eck_core::metrics) + CPU/RSS into a
    // rolling ~1h history, surfaced at GET /api/health/deep. Always on, negligible
    // cost — the cheap way to catch (and time-stamp) a runaway loop in prod.
    tokio::spawn(services::health_monitor::start_health_monitor());

    // No published fallback: env wins, else a per-node secret is generated
    // and persisted (data/jwt_secret) — a mis-provisioned kiosk still boots,
    // but nobody can forge its tokens offline anymore.
    let jwt_secret =
        eck_core::auth::load_or_generate_jwt_secret(std::path::Path::new("data/jwt_secret"));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3210);

    // Server identity for device pairing (Ed25519 keypair)
    let server_identity = eck_core::utils::identity::load_or_generate_identity(&instance_id);
    info!("Server identity loaded (instance: {})", instance_id);

    // Seed temporary setup account if no users exist. Real accounts live in
    // the mesh `db`; the setup row itself stays in the node-local users_db.
    let setup_password = handlers::auth::seed_setup_account(&db, &users_db).await;
    if let Some(ref pw) = setup_password {
        info!("=================================================");
        info!("  FIRST RUN: Setup account created");
        info!("  Email: admin@setup.local");
        info!("  Password: {}", pw);
        info!("  Create your own account, then this one will be removed.");
        info!("=================================================");
    }

    let (ws_tx, _) = tokio::sync::broadcast::channel(256);
    let hedera = HederaClient::from_env();

    // Build the xelixir AgentController before AppState so it can be shared
    // with /X/ handlers via Arc<AppState>.
    let agent_controller = services::agent_manager::AgentController::new(
        db.clone(),
        ws_tx.clone(),
        instance_id.clone(),
        server_identity.public_key.clone(),
    );

    let node_role = std::env::var("MESH_NODE_ROLE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "full".to_string());
    if node_role == "cache" {
        info!(
            "MESH_NODE_ROLE=cache — periodic merkle sync disabled, this node \
             serves as pull-through cache only."
        );
    }

    let app_state = Arc::new(AppState {
        db,
        users_db,
        sync_engine,
        hedera,
        jwt_secret,
        sync_secret,
        server_identity,
        instance_id,
        mesh_id,
        port,
        setup_password: RwLock::new(setup_password),
        ws_tx,
        agent_controller: agent_controller.clone(),
        node_role,
        i18n_lang_jobs: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        pii_reveal: Arc::new(crate::mcp::reveal::PiiRevealStore::new()),
    });

    // On-demand content-translation queue (support-ticket summaries → the
    // viewer's language). Gated on AI being enabled; consumes db + ws_tx.
    if eck_core::ai::AiAuth::is_enabled_in_env() {
        ai::translation::init(
            app_state.db.clone(),
            app_state.ws_tx.clone(),
            app_state.instance_id.clone(),
        );
    }

    // Audit-chain anchor scheduler: Merkle-batch un-anchored events → Hedera
    // twice a day (steady-state heartbeat for the WMS chain).
    {
        let db_anchor = app_state.db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(12 * 3600));
            loop {
                interval.tick().await;
                match eck_core::audit::anchor_pending(&db_anchor).await {
                    Ok(Some(a)) => info!("WMS audit anchor #{} sealed {} events", a.anchor_seq, a.count),
                    Ok(None) => {}
                    Err(e) => warn!("WMS audit anchor failed: {}", e),
                }
            }
        });
    }

    // Backfill Hedera consensus seq/timestamp into audit_anchor every 5 min.
    {
        let db_bf = app_state.db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                match eck_core::audit::backfill_anchor_consensus(&db_bf).await {
                    Ok(n) if n > 0 => info!("WMS audit anchor backfill: {} row(s) got consensus data", n),
                    Ok(_) => {}
                    Err(e) => warn!("WMS audit anchor backfill failed: {}", e),
                }
            }
        });
    }

    // Spawn AI Observer (system anomaly detection, every 6h)
    {
        let obs_state = app_state.clone();
        tokio::spawn(async move {
            ai::observer::start_observer_worker(obs_state).await;
        });
    }

    // Spawn AI Orchestrator (Central Brain — event-sourced ReAct loop, Phase 1 stub)
    {
        let orch_state = app_state.clone();
        tokio::spawn(async move {
            ai::orchestrator::start_orchestrator(orch_state).await;
        });
    }

    // Let the AI attachment ladder hydrate a blob it doesn't hold locally.
    // A process has exactly one sync engine, and threading an Arc through the
    // provider-generic orchestrator + every rig `Tool` struct would spread mesh
    // plumbing across two crates to express that one fact — so it is installed
    // once here and read by `ai::attachments`. Unset (tests) = old disk-only
    // behaviour.
    ai::attachments::install_mesh_hydrator(app_state.sync_engine.clone());

    // Spawn Image Optimizer Worker (AVIF transcoding)
    {
        let opt_state = app_state.clone();
        tokio::spawn(async move {
            ai::image_optimizer::start_optimizer_worker(opt_state).await;
        });
    }

    // Spawn SOP Optimizer (Phase 5 — self-learning from human-in-the-loop tasks)
    {
        let opt_state = app_state.clone();
        tokio::spawn(async move {
            ai::optimizer::start_optimizer_worker(opt_state).await;
        });
    }

    // Spawn Repair-Lesson Distiller ("Pass C") — distills resolved support
    // tickets into PII-stripped repair lessons; pushes only confirmed-fixed ones
    // to the xelixir KB (gated). See ai/repair_distiller.rs.
    {
        let rd_state = app_state.clone();
        tokio::spawn(async move {
            ai::repair_distiller::start_repair_distiller_worker(rd_state).await;
        });
    }

    // Spawn Geocoder Worker
    {
        let geo_db = app_state.db.clone();
        tokio::spawn(async move {
            services::geocoder::start_geocoder_worker(geo_db).await;
        });
    }

    // Geo self-heal sweep: periodically retries the office-pinned pile through
    // the free levers (customer-DB copy, phone area code) plus a small bounded
    // slice of paid grounding (which itself honours the grounding switch and
    // the budget circuit-breaker). Terminal `geo_source` markers make each pass
    // self-limiting, so a converged pile costs two near-empty selects.
    //
    // OPT-IN per node via ECK_GEO_SWEEP_SECS (unset/0 = off): documents are
    // mesh-replicated, so an unconditional sweep on every fleet node would
    // multiply the same Nominatim/Gemini lookups by the node count. Enable it
    // on ONE node (the ticket home node).
    {
        let sweep_secs: u64 = std::env::var("ECK_GEO_SWEEP_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if sweep_secs > 0 {
            let secs = sweep_secs.max(600); // floor: never tighter than 10 min
            let sweep_db = app_state.db.clone();
            tokio::spawn(async move {
                // Let boot (summarizer, index builds) settle before the first pass.
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                info!("[GeoSweep] enabled, every {secs}s (customer-db -> attachment-ocr -> vorwahl -> grounding)");
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(secs));
                loop {
                    interval.tick().await;
                    services::customer_geo::customer_fill_batch(sweep_db.clone(), 500).await;
                    services::attachment_geo::attachment_fill_batch(sweep_db.clone(), 500).await;
                    services::vorwahl::vorwahl_fill_batch(sweep_db.clone(), 500).await;
                    // Paid stage: hard-capped per pass; no-ops when the operator
                    // switch is off or the AI budget hits Halt.
                    if let Ok(model) = std::env::var("GEMINI_GENERATION_MODEL") {
                        ai::address_discovery::discover_addresses_batch(
                            sweep_db.clone(),
                            model,
                            25,
                        )
                        .await;
                    }
                }
            });
        }
    }

    // Attachment-extraction sweep: persist the OCR/text-layer ladder onto every
    // byte-owning file_resource row so the extract mesh-syncs (blobs do not) and
    // remote nodes gain attachment text without pulling bytes. DISABLED by default
    // (ECK_EXTRACT_SWEEP_SECS unset/0); self-gated inside. Enable on ONE
    // blob-holding node only, and only after the fleet carries this code.
    {
        let sweep_db = app_state.db.clone();
        tokio::spawn(async move {
            services::scheduler::start_extract_sweep(sweep_db).await;
        });
    }

    // Attachment-classification sweep (proposal 51, layer 2): one cheap-model
    // pass per PDF attachment over the STORED extract, persisting a `doc_class`
    // card. DISABLED by default (ECK_CLASSIFY_SWEEP_SECS unset/0); metered and
    // budget-halt gated. Enable on ONE node only (card + row mesh-replicate).
    {
        let classify_db = app_state.db.clone();
        tokio::spawn(async move {
            services::scheduler::start_classify_sweep(classify_db).await;
        });
    }

    // Spawn Cell Resolver Worker (PDA trip cell-tower geocoding + GoBD sealing)
    {
        let cell_db = app_state.db.clone();
        let cell_hedera = app_state.hedera.clone();
        tokio::spawn(async move {
            services::cell_resolver::start_cell_resolver_worker(cell_db, cell_hedera).await;
        });
    }

    // Spawn the Xelixir AgentController: ensures self-row + config and
    // (if `auto_start`) auto-spawns the agent. The legacy mesh LIVE SELECT
    // watcher inside it remains as a status-only mirror — it is NOT used
    // for command delivery anymore (the relay-routed xelixir_router is).
    {
        let ctrl = agent_controller.clone();
        tokio::spawn(async move {
            ctrl.bootstrap_and_run().await;
        });
    }

    // Spawn the cross-mesh xelixir router poller — pulls signed commands
    // queued for our UUID from the eck relay and drives the local
    // AgentController. Independent from data-mesh `SYNC_SECRET`.
    {
        let poller_state = app_state.clone();
        tokio::spawn(async move {
            services::xelixir_router::start_poller(poller_state).await;
        });
    }

    // Spawn the cross-NAT mesh task receiver — polls /E/m/poll/<my_uuid> for
    // pull/push tasks routed via the relay queue when direct P2P HTTP between
    // two NAT'd peers fails. Complementary to the direct merkle path: peers
    // that can reach each other directly still use that fast path; only the
    // unreachable pairs fall back to relay.
    {
        let mesh_poller_state = app_state.clone();
        tokio::spawn(async move {
            services::mesh_relay_poller::start_poller(mesh_poller_state).await;
        });
    }

    // Spawn the paid relay-carried MCP receiver — polls /E/c/poll/<my_uuid>,
    // re-verifies each SubscriptionCert, and serves the MCP request through the
    // same handler as direct /mcp. No-op unless ECK_SUB_ROOT_PUBKEY is set.
    {
        let cmcp_state = app_state.clone();
        tokio::spawn(async move {
            services::client_mcp_poller::start_poller(cmcp_state).await;
        });
    }

    // Cache LRU eviction worker — only meaningful on node_role=cache. Runs
    // every 5 min and trims is_cache rows down to the configured budget so
    // a busy public VPS doesn't drift toward holding the full data set.
    {
        let cache_state = app_state.clone();
        let budget = std::env::var("MESH_CACHE_BUDGET_ROWS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(10_000);
        if cache_state.node_role == "cache" {
            info!("Cache eviction worker armed (budget={} rows, every 5 min)", budget);
        }
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            interval.tick().await; // skip first immediate fire
            loop {
                interval.tick().await;
                if cache_state.node_role != "cache" {
                    continue;
                }
                if let Err(e) = cache_state.sync_engine.evict_cache_lru(budget).await {
                    warn!("Cache LRU eviction failed: {}", e);
                }
            }
        });
    }

    // Spawn heartbeat task (every 5 min) — register with relay so other nodes discover us
    {
        let heartbeat_relay = RelayClient::new_multi(
            &RelayClient::relay_urls_from_env(),
            &app_state.instance_id,
            &app_state.mesh_id,
        );
        let base_url = std::env::var("BASE_URL").unwrap_or_default();
        let heartbeat_port = app_state.port;
        let node_role_owned = std::env::var("MESH_NODE_ROLE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "full".to_string());
        info!(
            "Heartbeat task started (every 5 min) as '{}' node, relay: {}",
            node_role_owned,
            heartbeat_relay.relay_url()
        );
        tokio::spawn(async move {
            let role_str = node_role_owned.as_str();
            // Send first heartbeat immediately
            let (ip, p, hb_base_url) = heartbeat_announce(&base_url, heartbeat_port);
            match heartbeat_relay
                .send_heartbeat(&ip, p, None, hb_base_url.as_deref(), Some(role_str))
                .await
            {
                Ok(r) => info!("Initial heartbeat OK: {}", r.status),
                Err(e) => {
                    let mut chain = format!("{}", e);
                    let mut src = std::error::Error::source(&e);
                    while let Some(s) = src {
                        chain.push_str(&format!(" / caused by: {}", s));
                        src = s.source();
                    }
                    warn!("Initial heartbeat failed: {}", chain);
                }
            }
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let (ip, p, hb_base_url) = heartbeat_announce(&base_url, heartbeat_port);
                match heartbeat_relay
                    .send_heartbeat(&ip, p, None, hb_base_url.as_deref(), Some(role_str))
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        let mut chain = format!("{}", e);
                        let mut src = std::error::Error::source(&e);
                        while let Some(s) = src {
                            chain.push_str(&format!(" / caused by: {}", s));
                            src = s.source();
                        }
                        warn!("Heartbeat failed: {}", chain);
                    }
                }
            }
        });
    }

    // POS module gate (ecKasse — the paid tier). Interim env flag; will move
    // to an eck_core::licensing scope check once product licenses are minted
    // per-tenant.
    let pos_enabled = std::env::var("POS_ENABLED")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);

    // Protected API routes
    let protected_routes = Router::new()
        // Items CRUD
        .route("/items", get(handlers::items::list).post(handlers::items::create))
        .route("/items/:id", get(handlers::items::get).put(handlers::items::update).delete(handlers::items::delete))
        // Products & Partners (Edge Sync Layer — Odoo/Twenty CRM mappable via source_system+external_id)
        .route("/products", get(handlers::products::list).post(handlers::products::create))
        .route("/products/:id", get(handlers::products::get).put(handlers::products::update).delete(handlers::products::delete))
        .route("/partners", get(handlers::partners::list).post(handlers::partners::create))
        .route("/partners/:id", get(handlers::partners::get).put(handlers::partners::update).delete(handlers::partners::delete))
        // Warehouse Operations (Quants & Pickings)
        .route("/quants", get(handlers::quants::list).post(handlers::quants::create))
        .route("/quants/:id", get(handlers::quants::get).put(handlers::quants::update).delete(handlers::quants::delete))
        .route("/pickings", get(handlers::pickings::list).post(handlers::pickings::create))
        .route("/pickings/:id", get(handlers::pickings::get).put(handlers::pickings::update).delete(handlers::pickings::delete))
        .route("/move-lines", get(handlers::pickings::list_lines).post(handlers::pickings::create_line))
        .route("/move-lines/:id", put(handlers::pickings::update_line))
        // Warehouse & Racks
        .route("/warehouse", get(handlers::warehouse::list).post(handlers::warehouse::create))
        .route("/warehouse/racks", get(handlers::warehouse::list_racks).post(handlers::warehouse::create_rack))
        .route("/warehouse/racks/:id", put(handlers::warehouse::update_rack).delete(handlers::warehouse::delete_rack))
        .route("/warehouse/put-away", post(handlers::warehouse::put_away))
        .route("/warehouse/bin", get(handlers::warehouse::bin_contents))
        .route("/warehouse/reconcile", get(handlers::warehouse::reconcile))
        .route("/warehouse/inventory", get(handlers::warehouse::inventory))
        .route("/warehouse/:id", get(handlers::warehouse::get))
        // RMA / Orders
        .route("/rma", get(handlers::rma::list_orders).post(handlers::rma::create_order))
        .route("/rma/search", post(handlers::rma::search_orders))
        .route("/rma/:id", get(handlers::rma::get_order).put(handlers::rma::update_order).delete(handlers::rma::delete_order))
        .route("/rma/:id/generate-link", post(handlers::rma::generate_agreement_link))
        // Menu (categories + items)
        .route("/menu/categories", get(handlers::menu::list_categories).post(handlers::menu::create_category))
        .route("/menu/categories/:id", put(handlers::menu::update_category).delete(handlers::menu::delete_category))
        .route("/menu/items", get(handlers::menu::list_items).post(handlers::menu::create_item))
        .route("/menu/items/:id", put(handlers::menu::update_item).delete(handlers::menu::delete_item))
        // Mesh Status (frontend uses these via JWT)
        .route("/mesh/status", get(handlers::mesh::status))
        .route("/mesh/nodes", get(handlers::mesh::nodes))
        // Internal
        .route("/internal/pairing-qr", get(handlers::device::generate_pairing_qr))
        // Print / Labels
        .route("/print/labels", post(handlers::print::generate_labels))
        // Action Proofs
        .route("/proofs", post(handlers::proofs::submit_proof))
        // Tamper-evident audit chain (9eck:wms:<instance_id>)
        .route("/audit/verify", get(handlers::audit::verify))
        .route("/audit/chain", get(handlers::audit::chain))
        .route("/audit/anchor", post(handlers::audit::anchor))
        // FileStore (CAS) & Attachments
        .route("/files/upload", post(handlers::files::upload))
        .route("/files/:id", get(handlers::files::download))
        .route("/files/attachments", get(handlers::files::list_attachments))
        .route("/files/attach", post(handlers::files::attach))
        .route("/files/attachments/:edge_id", delete(handlers::files::detach))
        // Temp (user-parked) photos: re-home onto an open order / delete for good
        .route("/files/redirect", post(handlers::files::redirect))
        .route("/files/temp/:id", delete(handlers::files::delete_temp))
        // Support (Zoho Desk import + read)
        .route("/support/import-ticket", post(handlers::support::import_ticket))
        .route("/support/import-tickets", post(handlers::support::import_tickets))
        .route("/support/import-thread", post(handlers::support::import_thread))
        .route("/support/tickets", get(handlers::support::list_tickets))
        .route("/support/debug/:ticket_id", get(handlers::support::debug_ticket))
        .route("/support/tickets/:ticket_id/threads", get(handlers::support::get_ticket_threads))
        .route("/support/tickets/:ticket_id/threads/:thread_id/payload", get(handlers::support::get_thread_payload))
        .route("/support/tickets/:ticket_id/summary", post(handlers::support::summarize_ticket))
        .route("/support/tickets/:ticket_id/similar", get(handlers::support::find_similar))
        // AI orchestrator — operator inbox + replies to paused tasks
        .route("/ai/tasks", get(handlers::ai::list_tasks))
        .route("/ai/tasks/:id/reply", post(handlers::ai::reply_to_task))
        // AI batch CSV enrichment (Analysis dashboard)
        .route("/ai/enrich-csv", post(handlers::ai::enrich_csv))
        // AI token-spend gauge (last 24h, local estimate from ai_telemetry)
        .route("/ai/usage", get(handlers::ai::get_ai_usage))
        // Voice command resolution (movFast PDA) — Gemini fallback on a local miss
        .route("/voice/resolve", post(handlers::voice::resolve_voice))
        // Operator geo override (reset-to-HQ / edit zip+city)
        .route("/geo/fix", post(handlers::geo::fix_location))
        // One-shot rescue of the HQ-fallback pile (re-geocode from summaries)
        .route("/geo/regeocode-fallback", post(handlers::geo::regeocode_fallback))
        // Lever 2A — AI address discovery (toggle + run) for office-pinned tickets
        .route("/geo/grounding-config", get(handlers::geo::get_grounding_config).post(handlers::geo::set_grounding_config))
        .route("/geo/discover-addresses", post(handlers::geo::discover_addresses))
        // Lever 2B — place residual tickets by landline area code (local ONB table)
        .route("/geo/vorwahl-fill", post(handlers::geo::vorwahl_fill))
        .route("/geo/customer-fill", post(handlers::geo::customer_fill))
        // Server-side cached geocoding (browser never calls Nominatim directly)
        .route("/geo/resolve", get(handlers::geo::resolve_location))
        // Exact Online manual imports
        .route("/exact/import-items", post(handlers::exact::import_items))
        .route("/exact/import-customers", post(handlers::exact::import_customers))
        .route("/exact/import-stock-positions", post(handlers::exact::import_stock_positions))
        .route("/exact/import-quotations", post(handlers::exact::import_quotations))
        .route("/exact/import-sales-orders", post(handlers::exact::import_sales_orders))
        // PDA (movFast Android client) — heartbeat, scan, repair, picking, explorer
        .route("/status", get(handlers::pda::status))
        .route("/scan", post(handlers::pda::handle_scan))
        .route("/repair/event", post(handlers::pda::repair_event))
        .route("/repair/consume", post(handlers::pda::consume))
        .route("/upload/image", post(handlers::files::upload))
        .route("/users/active", get(handlers::pda::active_users))
        .route("/users/verify-pin", post(handlers::pda::verify_pin))
        .route("/pickings/active", get(handlers::pda::active_pickings))
        .route("/pickings/:id/route", get(handlers::pda::picking_route))
        .route("/pickings/:id/lines/:line_id/confirm", post(handlers::pda::confirm_pick_line))
        .route("/pickings/:id/validate", post(handlers::pda::validate_picking))
        .route("/explorer/locations", get(handlers::pda::explorer_locations))
        .route("/explorer/locations/:id/contents", get(handlers::pda::explorer_location_contents))
        .route("/explorer/products", get(handlers::pda::explorer_products))
        .route("/explorer/products/:id/locations", get(handlers::pda::explorer_product_locations))
        .route("/sync/pull", post(handlers::pda::sync_pull))
        .route("/crm/update", post(handlers::pda::crm_update))
        .route("/crm/:entity_type/:id", get(handlers::pda::crm_get))
        // Trips (PDA Fahrtenbuch — cell-tower tracks + odometer)
        .route("/trips", post(handlers::trips::upload_trip).get(handlers::trips::list_trips))
        .route("/trips/export", get(handlers::trips::export_trips))
        .route("/trips/purpose-candidates", get(handlers::trips::purpose_candidates))
        .route("/trips/destinations", get(handlers::trips::destinations))
        // Ephemeral live position of an in-progress trip → TRIP_LIVE WS event
        // (consent-gated, never persisted; static path wins over /trips/:id).
        .route("/trips/live", post(handlers::trips::trip_live))
        .route("/trips/:id", get(handlers::trips::get_trip))
        .route("/trips/:id/verify", get(handlers::trips::verify_trip))
        .route("/cells/cache", get(handlers::trips::cell_cache))
        // Vehicle registry (Fahrtenbuch — plate/Kennzeichen, photographed once)
        .route("/vehicles", get(handlers::vehicles::list_vehicles).post(handlers::vehicles::create_vehicle))
        .route("/vehicles/:id", axum::routing::put(handlers::vehicles::update_vehicle))
        // Visit tasks (check-in/check-out model — see .eck/PRIVACY_BY_DESIGN.md)
        .route("/visits", get(handlers::visits::list_visits).post(handlers::visits::create_visit))
        .route("/visits/:id/checkin", post(handlers::visits::checkin))
        .route("/visits/:id/checkout", post(handlers::visits::checkout))
        // Odoo connector (external JSON-RPC) — the tenant's warehouse master data
        .route("/odoo/ping", get(handlers::odoo::ping))
        .route("/odoo/sync", post(handlers::odoo::sync))
        // Bridge the odoo_product mirror into 9eck's native product catalog
        .route("/odoo/bridge-products", post(handlers::odoo::bridge_products))
        // Stubs (not yet ported from the legacy system)
        .route("/odoo/pickings", get(handlers::stubs::odoo_pickings))
        .route("/delivery/shipments", get(handlers::stubs::list_shipments).post(handlers::stubs::create_shipment))
        .route("/delivery/config", get(handlers::stubs::delivery_config))
        .route("/delivery/shipments/:id/cancel", post(handlers::stubs::cancel_shipment))
        .route("/delivery/shipments/:id/resolve", post(handlers::stubs::resolve_shipment))
        .route("/delivery/shipments/:id/ai-match", get(handlers::stubs::ai_match_shipment))
        .route("/delivery/import/opal", post(handlers::stubs::import_opal))
        .route("/delivery/import/dhl", post(handlers::stubs::import_dhl))
        .route("/delivery/sync/history", get(handlers::stubs::delivery_sync_history))
        .route("/delivery/carriers", get(handlers::stubs::delivery_carriers))
        .route("/analysis/support-dump", get(handlers::stubs::analysis_support_dump))
        // Auth (protected)
        .route("/auth/me", get(handlers::auth::me))
        // Self-service password change (acts on the caller's own token subject).
        .route("/auth/change-password", post(handlers::auth::change_password))
        // i18n language set — any authenticated token (incl. kiosk observer);
        // NOT admin-only so the kiosk language button can read it.
        .route("/i18n/languages", get(handlers::i18n::languages))
        // Config GET is dashboard-wide (SLA scale colors every operator's map);
        // the POST halves enforce role=admin inside the handlers.
        .route("/admin/config/kiosk", get(handlers::auth::get_kiosk_config).post(handlers::auth::set_kiosk_config))
        .route("/admin/config/dashboard_sla", get(handlers::auth::get_dashboard_sla).post(handlers::auth::set_dashboard_sla))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::auth::auth_middleware));

    // Admin-only routes: JWT + role=admin, enforced by LAYERS (auth outermost,
    // then require_admin), not per-handler ifs. Before this split any operator
    // JWT could create admin users (privilege escalation), restore DB backups,
    // mint device pair-codes, replay mesh tables, or write back into Odoo.
    let admin_routes = Router::new()
        // Backup / restore (destructive)
        .route("/admin/db/backups", get(handlers::backup::list_backups))
        .route("/admin/db/backup", post(handlers::backup::create_backup))
        .route("/admin/db/restore/:filename", post(handlers::backup::restore_backup))
        // Force-sync (manual trigger for all scraper providers)
        .route("/admin/force-sync", post(handlers::admin::force_sync))
        // Mesh-replay (one-shot backfill of an entity_type to all peers)
        .route("/admin/mesh-replay/:entity_type", post(handlers::admin::mesh_replay))
        // GDPR Art.17 erasure of AI-derived vectors (audit-logged)
        .route("/admin/gdpr/erase", post(handlers::gdpr::erase_subject))
        // Cross-mesh node registry (lists kiosks regardless of mesh)
        .route("/admin/known-nodes", get(handlers::mesh::known_nodes))
        // Users (create/update includes role assignment — admin-only or any
        // operator can mint themselves an admin account)
        .route("/admin/users", get(handlers::users::list).post(handlers::users::create))
        .route("/admin/users/:id", put(handlers::users::update).delete(handlers::users::delete))
        // Devices
        .route("/admin/pair-code", post(handlers::device::mint_pair_code))
        .route("/admin/devices", get(handlers::device::list_devices))
        .route("/admin/devices/:id/status", put(handlers::device::update_device_status))
        .route("/admin/devices/:id/home", put(handlers::device::update_device_home))
        .route("/admin/devices/:id/restore", post(handlers::device::restore_device))
        .route("/admin/devices/:id", delete(handlers::device::delete_device))
        // Mesh master/home designation (transferable, mesh-synced)
        .route("/admin/mesh/master", post(handlers::mesh::set_master))
        // Arbitrary-SurrealQL diagnostics. This MUST stay behind auth: it
        // previously sat in `public_routes`, i.e. UNAUTHENTICATED read/write
        // SQL reachable on every public node. Handler double-checks admin.
        .route("/admin/query", post(handlers::admin::query))
        // One-shot ops/backfill tools (mesh-wide effects)
        .route("/support/backfill-assignees", post(handlers::support::backfill_assignees))
        .route("/support/backfill-customfields", post(handlers::support::backfill_customfields))
        .route("/support/backfill-meta", post(handlers::support::backfill_meta))
        .route("/support/restamp-thread-hashes", post(handlers::support::restamp_thread_hashes))
        .route("/support/restamp-vclocks", post(handlers::support::restamp_vclocks))
        .route("/support/claim-home", post(handlers::support::claim_home))
        .route("/support/backfill-outbound-times", post(handlers::support::backfill_outbound_times))
        .route("/support/backfill-thread-headers", post(handlers::support::backfill_thread_headers))
        .route("/support/backfill-summary-sync", post(handlers::support::backfill_summary_sync))
        .route("/support/backfill-embedding-sync", post(handlers::support::backfill_embedding_sync))
        .route("/support/requeue-embeddings", post(handlers::support::requeue_embeddings))
        .route("/support/admin/scrub-summaries", post(handlers::support::scrub_summaries))
        .route("/exact/backfill-vclocks", post(handlers::exact::backfill_vclocks))
        // Projection back into Odoo (guarded by ODOO_WRITE_ENABLED, but it
        // writes into the customer's production ERP — admin on top)
        .route("/odoo/project/set-onhand", post(handlers::odoo::set_onhand))
        .route("/odoo/project/run", post(handlers::odoo::project_run))
        // Spawns a local node.js process holding scraper credentials
        .route("/scraper/start", post(handlers::scraper_proxy::start_scraper))
        // Ready-to-run MCP connector bundle (zip): shim binary + pre-filled
        // shim.env (this node's base URL + tier bearer) + README. Admin-only —
        // it mints a live token into the download.
        .route("/admin/mcp-connector", get(mcp::connector::mcp_connector))
        // Customer-addable UI languages: machine-translate the English label set
        // into a new language at runtime (background job + poll-able status).
        .route("/admin/i18n/languages", post(handlers::i18n::add_language))
        .route("/admin/i18n/languages/:lang/status", get(handlers::i18n::add_language_status))
        // Layer order: tower wraps outer→inner with the LAST declared call
        // outermost, so auth (validates JWT, inserts Claims) runs first, then
        // require_admin reads the Claims extension.
        .route_layer(axum_mw::from_fn(middleware::require_admin::require_admin_middleware))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::auth::auth_middleware));

    // Public routes (no JWT required)
    let public_routes = Router::new()
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/setup-status", get(handlers::auth::setup_status))
        .route("/auth/kiosk-token", get(handlers::auth::kiosk_token))
        // Single-use nonce for replay-proof device auth (signed into register-device)
        .route("/auth/device-challenge", get(handlers::auth::device_challenge))
        .route("/public/devices/register", post(handlers::device::register_device))
        // Legacy PDA pairing path (movFast calls /api/internal/register-device)
        .route("/internal/register-device", post(handlers::device::register_device))
        .route("/public/agreement/:token", get(handlers::rma::get_agreement_by_token))
        .route("/public/agreement/:token/sign", post(handlers::rma::sign_agreement))
        // UI label dictionary — public: labels aren't secrets and the login
        // page needs them before any auth. DB (i18n_label) is the runtime truth.
        .route("/i18n/dict/:lang", get(handlers::i18n::dict))
        // POS module availability — public so the login/dashboard shells can
        // decide whether to show the register entry point.
        .route("/pos/status", get(move || async move {
            axum::Json(serde_json::json!({ "enabled": pos_enabled }))
        }));

    // P2P mesh routes (SYNC_SECRET auth, NOT JWT)
    let p2p_routes = Router::new()
        .route("/mesh/merkle/state", get(handlers::mesh::merkle_state))
        .route("/mesh/parity", get(handlers::mesh::parity))
        .route("/mesh/sync/pull", post(handlers::mesh::sync_pull))
        .route("/mesh/sync/push", post(handlers::mesh::sync_push))
        .route("/mesh/file/:hash", get(handlers::mesh::serve_mesh_file))
        .route("/mesh/raw-docs/:ticket_id", get(handlers::mesh::raw_docs))
        .route("/mesh/tasks", get(handlers::mesh::get_tasks))
        .route("/mesh/tasks/nudge", post(handlers::mesh::nudge_tasks))
        .route("/mesh/tasks/:id", delete(handlers::mesh::delete_task))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::mesh_auth::mesh_auth_middleware));

    // Xelixir C2 microservice — strict /X/ prefix (NOT under /api).
    //
    // Routes split into two groups:
    //
    //   * JWT-protected (admin/observer) — UI-facing.
    //   * Public-but-signature-verified (`/X/self/*`) — inter-node only.
    //     The envelope must be Ed25519-signed by a key in this node's
    //     `XELIXIR_ADMIN_PUBKEYS` allow-list; the handler enforces it.
    let xelixir_jwt_routes = Router::new()
        .route("/config", get(handlers::xelixir::get_config).post(handlers::xelixir::set_config))
        .route("/approve", post(handlers::xelixir::approve))
        .route("/devices/:id/start", post(handlers::xelixir::start_device))
        .route("/devices/:id/stop", post(handlers::xelixir::stop_device))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::auth::auth_middleware));

    let xelixir_self_routes = Router::new()
        .route("/self/start", post(handlers::xelixir::self_start))
        .route("/self/stop", post(handlers::xelixir::self_stop));

    // Server-initiated activation. Sibling services (xelixir.service) hit
    // this with a shared service token to dispatch start/stop commands
    // through xelixir_router (same plumbing as the JWT-gated admin path).
    let xelixir_internal_routes = Router::new()
        .route("/internal/dispatch", post(handlers::xelixir::internal_dispatch))
        .route("/internal/result/:task_id", get(handlers::xelixir::internal_result))
        .route_layer(axum_mw::from_fn(middleware::service_token::require_service_token));

    // Extended ops vocabulary — xelixir's autonomous ops loop calls these.
    // Same service-token auth as /X/internal/*. Per-verb endpoints by design
    // (see .eck/XELIXIR_OPS_VOCABULARY.md). Each new ops verb lands as a
    // route under here, not as a new value in some polymorphic command field.
    let xelixir_ops_routes = Router::new()
        .route("/ops/journal", get(handlers::ops::journal))
        .route("/ops/service_status", get(handlers::ops::service_status))
        .route("/ops/system_health", get(handlers::ops::system_health))
        .route("/ops/health_check", get(handlers::ops::health_check))
        // Self-diagnosis: per-loop rates + CPU/RSS history (eck_core::metrics).
        // Same service-token auth + ops_audit as the other ops verbs.
        .route("/ops/loop_metrics", get(services::health_monitor::deep_health_handler))
        .route("/ops/file_read", get(handlers::ops::file_read))
        .route("/ops/file_write", post(handlers::ops::file_write))
        .route("/ops/surrealql_read", post(handlers::ops::surrealql_read))
        .route("/ops/surrealql_write", post(handlers::ops::surrealql_write))
        .route("/ops/restart_service", post(handlers::ops::restart_service))
        // Tier-2: long-running. Return task_id immediately; caller polls
        // /ops/task/:task_id until state != "running".
        .route("/ops/git_pull", post(handlers::ops::git_pull))
        .route("/ops/cargo_build", post(handlers::ops::cargo_build))
        .route("/ops/deploy", post(handlers::ops::deploy))
        .route("/ops/task/:task_id", get(handlers::ops::task_status))
        // Tier-3: ops utilities.
        .route("/ops/nginx_test_reload", post(handlers::ops::nginx_test_reload))
        .route("/ops/package_install", post(handlers::ops::package_install))
        // Layer order (tower applies them outer→inner in the order they
        // were declared, last call = outermost). We want:
        //   audit (outermost) → token-check → handler
        // so the audit row is written even for 403-rejected requests.
        .route_layer(axum_mw::from_fn(middleware::service_token::require_service_token))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::ops_audit::ops_audit_middleware));

    let xelixir_routes = xelixir_jwt_routes
        .merge(xelixir_self_routes)
        .merge(xelixir_internal_routes)
        .merge(xelixir_ops_routes);

    let api_router = public_routes.merge(protected_routes).merge(admin_routes).merge(p2p_routes)
        .fallback(|| async {
            (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"success": false, "error": "API route not found"}))
            )
        })
        // Raise the Axum body-size ceiling to 50 MiB. Default is 2 MiB,
        // which caused 413s on /support/import-thread when Zoho threads
        // carry inline HTML bodies or attachment metadata for the
        // largest tickets (#25206, #25357, #25162).
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024));

    let app = Router::new()
        .route("/E/health", get(health_check))
        .route("/E/ws", get(handlers::ws::ws_handler))
        // MCP agent surface (business-graph tools) — Bearer-gated inside the
        // handler (Master vs Agent tier), so it is mounted outside the /X
        // service-token layer. See wms/src/mcp/.
        .route("/mcp", post(mcp::mcp_handler))
        // Direct twin of the relay `/E/c/*` channel: accepts the same paid
        // SubscriptionCert-signed request straight from the LAN / a public node,
        // no relay hop. Cert-gated inside the handler (no bearer), so mounted
        // outside the /X service-token layer. See wms/src/services/client_mcp.rs.
        .route("/mcp/signed", post(mcp::mcp_signed_handler))
        .route("/E/auth/setup-status", get(handlers::auth::setup_status))
        .route("/E/auth/login", post(handlers::auth::login))
        // Scraper reverse proxy: /S/* → http://127.0.0.1:$SCRAPER_PORT/*
        .route("/S", any(handlers::scraper_proxy::proxy_handler))
        .route("/S/*path", any(handlers::scraper_proxy::proxy_handler))
        .nest("/X", xelixir_routes)
        .nest("/api", api_router.clone())
        // Legacy PDA base URLs end in /E (pairing QR candidates are
        // "http://ip:port/E"), so movFast calls /E/api/*. Same router,
        // second mount point.
        .nest("/E/api", api_router)
        .fallback(web::static_handler)
        .with_state(app_state.clone());

    // POS module (ecKasse) — the paid register, one process / one DB / one
    // mesh node with the WMS. When disabled, /K/* simply falls through to the
    // WMS SPA fallback above. Compiled behind the `pos-module` feature (default
    // on); an open-core build without the feature drops the ecKasse dependency
    // entirely and the /K routes just never exist.
    #[cfg(feature = "pos-module")]
    let app = if pos_enabled {
        let pos_state = pos::AppState::embedded(
            app_state.db.clone(),
            // Cashier accounts are real users → the mesh DB (users_db now
            // holds only the node-local setup-admin bootstrap row).
            app_state.db.clone(),
            Arc::clone(&app_state.sync_engine),
            app_state.server_identity.clone(),
            app_state.jwt_secret.clone(),
            app_state.sync_secret.clone(),
        );
        // Table DDL + fiscal-WAL replay (crash between TSE sign and business
        // write) — must finish before the first sale is accepted.
        pos::init_fiscal(&pos_state).await;
        info!("POS module enabled — ecKasse mounted at /K/");
        app.merge(pos::router(pos_state))
    } else {
        app
    };

    // `pos-module` feature compiled out: no ecKasse dependency, /K/* is a plain
    // 404 through the SPA fallback. `POS_ENABLED` may still be set in the env,
    // but there is no module to mount.
    #[cfg(not(feature = "pos-module"))]
    let app = {
        if pos_enabled {
            info!("POS_ENABLED is set but the `pos-module` feature was not compiled in — /K/ is unavailable");
        }
        app
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    info!("eckWMS listening on {}", addr);
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        server: "wms".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Extract IP/hostname and port from a base URL string.
/// Outbound LAN IP of this box right now (UDP connect() picks the routing
/// interface; no packet is sent). None on a box with no route.
fn detect_outbound_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("1.1.1.1:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        None
    } else {
        Some(ip.to_string())
    }
}

/// What this heartbeat should announce. A configured BASE_URL wins (stable
/// boxes: kiosk, cloud). When BASE_URL is EMPTY the outbound IP is detected
/// FRESH per heartbeat, so a roaming box (the dev laptop) always announces
/// its current address instead of a stale one (TECH_DEBT 2026-07-18).
fn heartbeat_announce(base_url: &str, port: u16) -> (String, u16, Option<String>) {
    if !base_url.is_empty() {
        let (ip, p) = parse_base_url(base_url, port);
        return (ip, p, Some(base_url.to_string()));
    }
    match detect_outbound_ip() {
        Some(ip) => {
            let base = format!("http://{}:{}", ip, port);
            (ip, port, Some(base))
        }
        None => ("0.0.0.0".to_string(), port, None),
    }
}

fn parse_base_url(base_url: &str, default_port: u16) -> (String, u16) {
    if base_url.is_empty() {
        return ("0.0.0.0".to_string(), default_port);
    }
    let url = base_url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    if let Some(colon_pos) = url.rfind(':') {
        let ip = &url[..colon_pos];
        let port = url[colon_pos + 1..].parse().unwrap_or(default_port);
        (ip.to_string(), port)
    } else {
        (url.to_string(), default_port)
    }
}
