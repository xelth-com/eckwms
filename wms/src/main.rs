mod ai;
mod handlers;
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

    // Zone 2 schemaless bootstrap. Note: `user` is intentionally NOT
    // defined here — that table belongs to users_db (Zone 1).
    //
    // `REMOVE TABLE IF EXISTS user` drops any leftover legacy `user`
    // table from before the dual-DB split. Idempotent: on a fresh
    // deployment there's nothing to remove. Important: the `user`
    // table is NOT in SYNC_ENTITY_TYPES anymore, so this won't be
    // recreated by mesh sync.
    if let Err(e) = db
        .query("REMOVE TABLE IF EXISTS user;")
        .await
    {
        tracing::warn!("Failed to drop legacy `user` table from Zone 2 db: {}", e);
    }

    if let Err(e) = db
        .query(
            "DEFINE TABLE IF NOT EXISTS item SCHEMALESS;
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
             DEFINE TABLE IF NOT EXISTS has_attachment TYPE RELATION IN record OUT file_resource SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS contains TYPE RELATION IN location OUT rack SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS ai_telemetry SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS entity_checksum SCHEMALESS;
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
             DEFINE TABLE IF NOT EXISTS peer_health_state SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS scan_log SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS repair_event SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS crm_update_log SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS opportunity SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS trip SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS cell_tower SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS visit_task SCHEMALESS;
             DEFINE TABLE IF NOT EXISTS vehicle SCHEMALESS;",
        )
        .await
    {
        tracing::warn!("Failed to ensure Zone 2 tables: {}", e);
    }

    // Zone 1 schemaless bootstrap — accounts and credentials only.
    // Sidecar PII tables (order_pii, partner_pii, picking_pii) are created
    // lazily by the handlers that own them when those handlers land.
    if let Err(e) = users_db
        .query("DEFINE TABLE IF NOT EXISTS user SCHEMALESS;")
        .await
    {
        tracing::warn!("Failed to ensure Zone 1 tables: {}", e);
    }

    // Ensure search indexes exist (idempotent)
    if let Err(e) = db
        .query(
            "DEFINE ANALYZER IF NOT EXISTS custom_analyzer TOKENIZERS blank,class,camel,punct FILTERS lowercase,ascii;
             DEFINE INDEX IF NOT EXISTS issue_bm25 ON order FIELDS issue_description FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS order_number_bm25 ON order FIELDS order_number FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS customer_name_bm25 ON order FIELDS customer_name FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS embedding_hnsw ON order FIELDS embedding HNSW DIMENSION 768 DIST COSINE;
             DEFINE INDEX IF NOT EXISTS task_state_idx ON ai_task FIELDS state;
             DEFINE INDEX IF NOT EXISTS sop_trigger_bm25 ON ai_sop FIELDS trigger_context FULLTEXT ANALYZER custom_analyzer BM25;
             DEFINE INDEX IF NOT EXISTS sop_embedding_hnsw ON ai_sop FIELDS embedding HNSW DIMENSION 768 DIST COSINE;",
        )
        .await
    {
        tracing::warn!("Failed to ensure search indexes: {}", e);
    }

    // Mesh Sync
    use eck_core::utils::identity::{ensure_uuid_instance_id, compute_mesh_id};
    let raw_id = std::env::var("INSTANCE_ID").unwrap_or_default();
    let instance_id = ensure_uuid_instance_id(&raw_id);
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
    // table. Replaces the scheduled `refresh_checksums()` as the primary
    // up-to-date path — anything that .create/.update/.upsert/.delete's a
    // row in a SYNC_ENTITY_TYPES table updates the merkle tree within
    // ~milliseconds, not at the next 60s tick. Scheduled refresh stays in
    // sync_cycle as a safety net if a stream silently drops events.
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
        // lazily inside each worker cycle — see AiAuth::resolve.
        if eck_core::ai::AiAuth::is_enabled_in_env() {
            let emb_db = db.clone();
            let sum_db = db.clone();
            let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
                .expect("GEMINI_GENERATION_MODEL must be set in .env");
            let sum_model = std::env::var("GEMINI_SUMMARY_MODEL")
                .expect("GEMINI_SUMMARY_MODEL must be set in .env");
            let emb_model = std::env::var("GEMINI_EMBEDDING_MODEL")
                .expect("GEMINI_EMBEDDING_MODEL must be set in .env");
            tokio::spawn(ai::embeddings::start_embedding_worker(emb_db, gen_model, emb_model));
            tokio::spawn(ai::summarization::start_summarization_worker(sum_db, sum_model));
            let mode = std::env::var("ECK_AI_MODE").unwrap_or_else(|_| "studio".into());
            info!("Embedding + Summarization workers spawned (AI mode={mode})");
        } else {
            warn!("AI auth not configured — AI workers disabled");
        }
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

    let jwt_secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "dev-secret-change-in-production".into());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3210);

    // Server identity for device pairing (Ed25519 keypair)
    let server_identity = eck_core::utils::identity::load_or_generate_identity(&instance_id);
    info!("Server identity loaded (instance: {})", instance_id);

    // Seed temporary setup account if no users exist
    let setup_password = handlers::auth::seed_setup_account(&users_db).await;
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
    });

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

    // Spawn Geocoder Worker
    {
        let geo_db = app_state.db.clone();
        tokio::spawn(async move {
            services::geocoder::start_geocoder_worker(geo_db).await;
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
            let (ip, p) = parse_base_url(&base_url, heartbeat_port);
            let hb_base_url = if base_url.is_empty() { None } else { Some(base_url.as_str()) };
            match heartbeat_relay
                .send_heartbeat(&ip, p, None, hb_base_url, Some(role_str))
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
                let (ip, p) = parse_base_url(&base_url, heartbeat_port);
                match heartbeat_relay
                    .send_heartbeat(&ip, p, None, hb_base_url, Some(role_str))
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
        // Admin / Backup
        .route("/admin/db/backups", get(handlers::backup::list_backups))
        .route("/admin/db/backup", post(handlers::backup::create_backup))
        .route("/admin/db/restore/:filename", post(handlers::backup::restore_backup))
        // Admin / Force-sync (manual trigger for all scraper providers)
        .route("/admin/force-sync", post(handlers::admin::force_sync))
        // Admin / Mesh-replay (one-shot backfill of an entity_type to all peers)
        .route("/admin/mesh-replay/:entity_type", post(handlers::admin::mesh_replay))
        // Admin / GDPR Art.17 erasure of AI-derived vectors (audit-logged)
        .route("/admin/gdpr/erase", post(handlers::gdpr::erase_subject))
        // Mesh Status (frontend uses these via JWT)
        .route("/mesh/status", get(handlers::mesh::status))
        .route("/mesh/nodes", get(handlers::mesh::nodes))
        // Admin / cross-mesh node registry (lists kiosks regardless of mesh)
        .route("/admin/known-nodes", get(handlers::mesh::known_nodes))
        // Admin / Users
        .route("/admin/users", get(handlers::users::list).post(handlers::users::create))
        .route("/admin/users/:id", put(handlers::users::update).delete(handlers::users::delete))
        // Admin / Devices
        .route("/admin/devices", get(handlers::device::list_devices))
        .route("/admin/devices/:id/status", put(handlers::device::update_device_status))
        .route("/admin/devices/:id", delete(handlers::device::delete_device))
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
        // Support (Zoho Desk import + read)
        .route("/support/import-ticket", post(handlers::support::import_ticket))
        .route("/support/import-tickets", post(handlers::support::import_tickets))
        .route("/support/import-thread", post(handlers::support::import_thread))
        .route("/support/tickets", get(handlers::support::list_tickets))
        .route("/support/backfill-assignees", post(handlers::support::backfill_assignees))
        .route("/support/backfill-customfields", post(handlers::support::backfill_customfields))
        .route("/support/backfill-meta", post(handlers::support::backfill_meta))
        .route("/support/backfill-outbound-times", post(handlers::support::backfill_outbound_times))
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
        // Voice command resolution (movFast PDA) — Gemini fallback on a local miss
        .route("/voice/resolve", post(handlers::voice::resolve_voice))
        // Operator geo override (reset-to-HQ / edit zip+city)
        .route("/geo/fix", post(handlers::geo::fix_location))
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
        // Stubs (not yet ported from eckwmsr)
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
        // Scraper management
        .route("/scraper/start", post(handlers::scraper_proxy::start_scraper))
        // Auth (protected)
        .route("/auth/me", get(handlers::auth::me))
        .route("/admin/config/kiosk", get(handlers::auth::get_kiosk_config).post(handlers::auth::set_kiosk_config))
        .route("/admin/config/dashboard_sla", get(handlers::auth::get_dashboard_sla).post(handlers::auth::set_dashboard_sla))
        // Arbitrary-SurrealQL diagnostics — admin-only. This MUST stay behind auth:
        // it previously sat in `public_routes`, i.e. UNAUTHENTICATED read/write SQL
        // (data dump / table drop) reachable on every public node via /api + /E/api.
        // Now JWT-gated here; the handler additionally enforces role=admin.
        .route("/admin/query", post(handlers::admin::query))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::auth::auth_middleware));

    // Public routes (no JWT required)
    let public_routes = Router::new()
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/setup-status", get(handlers::auth::setup_status))
        .route("/auth/kiosk-token", get(handlers::auth::kiosk_token))
        .route("/public/devices/register", post(handlers::device::register_device))
        // Legacy PDA pairing path (movFast calls /api/internal/register-device)
        .route("/internal/register-device", post(handlers::device::register_device))
        .route("/public/agreement/:token", get(handlers::rma::get_agreement_by_token))
        .route("/public/agreement/:token/sign", post(handlers::rma::sign_agreement));

    // P2P mesh routes (SYNC_SECRET auth, NOT JWT)
    let p2p_routes = Router::new()
        .route("/mesh/merkle/state", get(handlers::mesh::merkle_state))
        .route("/mesh/sync/pull", post(handlers::mesh::sync_pull))
        .route("/mesh/sync/push", post(handlers::mesh::sync_push))
        .route("/mesh/file/:hash", get(handlers::mesh::serve_mesh_file))
        .route("/mesh/raw-docs/:ticket_id", get(handlers::mesh::raw_docs))
        .route("/mesh/tasks", get(handlers::mesh::get_tasks))
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

    let api_router = public_routes.merge(protected_routes).merge(p2p_routes)
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
        .with_state(app_state);

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
