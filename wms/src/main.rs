mod ai;
mod handlers;
mod middleware;
mod services;
mod utils;
mod web;

use chrono::Timelike;
use axum::{middleware as axum_mw, routing::{delete, get, post, put}, Json, Router};
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
    pub db: SurrealDb,
    pub sync_engine: Arc<SyncEngine>,
    pub hedera: Option<HederaClient>,
    pub jwt_secret: String,
    pub sync_secret: Option<String>,
    pub server_identity: ServerIdentity,
    pub instance_id: String,
    pub port: u16,
    pub setup_password: RwLock<Option<String>>,
    pub ws_tx: tokio::sync::broadcast::Sender<String>,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();
    info!("Starting eckWMS (9eck.com monorepo edition)");

    // SurrealDB
    let db_path = std::env::var("SURREAL_DB_PATH")
        .unwrap_or_else(|_| "data/wms.db".into());
    let db = eck_core::db::connect(&db_path)
        .await
        .expect("Failed to connect to SurrealDB");

    // Mesh Sync
    use eck_core::utils::identity::{ensure_uuid_instance_id, compute_mesh_id};
    let raw_id = std::env::var("INSTANCE_ID").unwrap_or_default();
    let instance_id = ensure_uuid_instance_id(&raw_id);
    let sync_secret = std::env::var("SYNC_SECRET").ok().filter(|s| !s.is_empty());
    let mesh_id = compute_mesh_id(sync_secret.as_deref().unwrap_or("0"));
    let relay_url = std::env::var("RELAY_URL")
        .unwrap_or_else(|_| "https://9eck.com".into());

    let relay = RelayClient::new(&relay_url, &instance_id, &mesh_id);
    let sync_engine = Arc::new(SyncEngine::new(
        instance_id.clone(),
        mesh_id.clone(),
        relay,
        db.clone(),
        sync_secret.clone(),
    ));

    // Spawn background P2P sync worker (Merkle diff every 60s)
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

    // Spawn Gemini embedding worker (processes pending documents & orders)
    if let Ok(gemini_key) = std::env::var("GEMINI_API_KEY") {
        if !gemini_key.is_empty() {
            let emb_db = db.clone();
            tokio::spawn(ai::embeddings::start_embedding_worker(emb_db, gemini_key));
            info!("Embedding worker spawned");
        } else {
            warn!("GEMINI_API_KEY is empty — embedding worker disabled");
        }
    } else {
        warn!("GEMINI_API_KEY not set — embedding worker disabled");
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
    let setup_password = handlers::auth::seed_setup_account(&db).await;
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

    let app_state = Arc::new(AppState {
        db,
        sync_engine,
        hedera,
        jwt_secret,
        sync_secret,
        server_identity,
        instance_id,
        port,
        setup_password: RwLock::new(setup_password),
        ws_tx,
    });

    // Spawn heartbeat task (every 5 min) — register with relay so other nodes discover us
    {
        let heartbeat_relay = RelayClient::new(
            &std::env::var("RELAY_URL").unwrap_or_else(|_| "https://9eck.com".into()),
            &app_state.instance_id,
            &std::env::var("MESH_ID").unwrap_or_else(|_| "default-mesh".into()),
        );
        let base_url = std::env::var("BASE_URL").unwrap_or_default();
        let heartbeat_port = app_state.port;
        info!("Heartbeat task started (every 5 min), relay: {}", heartbeat_relay.relay_url());
        tokio::spawn(async move {
            // Send first heartbeat immediately
            let (ip, p) = parse_base_url(&base_url, heartbeat_port);
            match heartbeat_relay.send_heartbeat(&ip, p, None).await {
                Ok(r) => info!("Initial heartbeat OK: {}", r.status),
                Err(e) => warn!("Initial heartbeat failed: {}", e),
            }
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let (ip, p) = parse_base_url(&base_url, heartbeat_port);
                match heartbeat_relay.send_heartbeat(&ip, p, None).await {
                    Ok(_) => {}
                    Err(e) => warn!("Heartbeat failed: {}", e),
                }
            }
        });
    }

    // Protected API routes
    let protected_routes = Router::new()
        // Items CRUD
        .route("/items", get(handlers::items::list).post(handlers::items::create))
        .route("/items/{id}", get(handlers::items::get).put(handlers::items::update).delete(handlers::items::delete))
        // Products & Partners (Edge Sync Layer — Odoo/Twenty CRM mappable via source_system+external_id)
        .route("/products", get(handlers::products::list).post(handlers::products::create))
        .route("/products/{id}", get(handlers::products::get).put(handlers::products::update).delete(handlers::products::delete))
        .route("/partners", get(handlers::partners::list).post(handlers::partners::create))
        .route("/partners/{id}", get(handlers::partners::get).put(handlers::partners::update).delete(handlers::partners::delete))
        // Warehouse Operations (Quants & Pickings)
        .route("/quants", get(handlers::quants::list).post(handlers::quants::create))
        .route("/quants/{id}", get(handlers::quants::get).put(handlers::quants::update).delete(handlers::quants::delete))
        .route("/pickings", get(handlers::pickings::list).post(handlers::pickings::create))
        .route("/pickings/{id}", get(handlers::pickings::get).put(handlers::pickings::update).delete(handlers::pickings::delete))
        .route("/move-lines", get(handlers::pickings::list_lines).post(handlers::pickings::create_line))
        .route("/move-lines/{id}", put(handlers::pickings::update_line))
        // Warehouse & Racks
        .route("/warehouse", get(handlers::warehouse::list).post(handlers::warehouse::create))
        .route("/warehouse/racks", get(handlers::warehouse::list_racks).post(handlers::warehouse::create_rack))
        .route("/warehouse/racks/{id}", put(handlers::warehouse::update_rack).delete(handlers::warehouse::delete_rack))
        .route("/warehouse/{id}", get(handlers::warehouse::get))
        // RMA / Orders
        .route("/rma", get(handlers::rma::list_orders).post(handlers::rma::create_order))
        .route("/rma/search", post(handlers::rma::search_orders))
        .route("/rma/{id}", get(handlers::rma::get_order).put(handlers::rma::update_order).delete(handlers::rma::delete_order))
        .route("/rma/{id}/generate-link", post(handlers::rma::generate_agreement_link))
        // Menu (categories + items)
        .route("/menu/categories", get(handlers::menu::list_categories).post(handlers::menu::create_category))
        .route("/menu/categories/{id}", put(handlers::menu::update_category).delete(handlers::menu::delete_category))
        .route("/menu/items", get(handlers::menu::list_items).post(handlers::menu::create_item))
        .route("/menu/items/{id}", put(handlers::menu::update_item).delete(handlers::menu::delete_item))
        // Admin / Backup
        .route("/admin/db/backups", get(handlers::backup::list_backups))
        .route("/admin/db/backup", post(handlers::backup::create_backup))
        .route("/admin/db/restore/{filename}", post(handlers::backup::restore_backup))
        // Mesh Status (frontend uses these via JWT)
        .route("/mesh/status", get(handlers::mesh::status))
        .route("/mesh/nodes", get(handlers::mesh::nodes))
        // Admin / Users
        .route("/admin/users", get(handlers::users::list).post(handlers::users::create))
        .route("/admin/users/{id}", put(handlers::users::update).delete(handlers::users::delete))
        // Admin / Devices
        .route("/admin/devices", get(handlers::device::list_devices))
        .route("/admin/devices/{id}/status", put(handlers::device::update_device_status))
        .route("/admin/devices/{id}", delete(handlers::device::delete_device))
        // Internal
        .route("/internal/pairing-qr", get(handlers::device::generate_pairing_qr))
        // Print / Labels
        .route("/print/labels", post(handlers::print::generate_labels))
        // Action Proofs
        .route("/proofs", post(handlers::proofs::submit_proof))
        // FileStore (CAS) & Attachments
        .route("/files/upload", post(handlers::files::upload))
        .route("/files/{id}", get(handlers::files::download))
        .route("/files/attachments", get(handlers::files::list_attachments))
        .route("/files/attach", post(handlers::files::attach))
        .route("/files/attachments/{edge_id}", delete(handlers::files::detach))
        // Auth (protected)
        .route("/auth/me", get(handlers::auth::me))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::auth::auth_middleware));

    // Public routes (no JWT required)
    let public_routes = Router::new()
        .route("/auth/login", post(handlers::auth::login))
        .route("/auth/setup-status", get(handlers::auth::setup_status))
        .route("/public/devices/register", post(handlers::device::register_device))
        .route("/public/agreement/{token}", get(handlers::rma::get_agreement_by_token))
        .route("/public/agreement/{token}/sign", post(handlers::rma::sign_agreement));

    // P2P mesh routes (SYNC_SECRET auth, NOT JWT)
    let p2p_routes = Router::new()
        .route("/mesh/merkle/state", get(handlers::mesh::merkle_state))
        .route("/mesh/sync/pull", post(handlers::mesh::sync_pull))
        .route("/mesh/sync/push", post(handlers::mesh::sync_push))
        .route("/mesh/file/{hash}", get(handlers::mesh::serve_mesh_file))
        .route_layer(axum_mw::from_fn_with_state(app_state.clone(), middleware::mesh_auth::mesh_auth_middleware));

    let app = Router::new()
        .route("/E/health", get(health_check))
        .route("/E/ws", get(handlers::ws::ws_handler))
        .route("/E/auth/setup-status", get(handlers::auth::setup_status))
        .route("/E/auth/login", post(handlers::auth::login))
        .nest("/api", public_routes.merge(protected_routes).merge(p2p_routes))
        .fallback(web::static_handler)
        .with_state(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    info!("eckWMS listening on {}", addr);
    axum::serve(listener, app).await.unwrap();
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
