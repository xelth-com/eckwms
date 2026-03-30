mod db;
mod handlers;

use axum::{routing::{get, post}, Router};
use std::net::SocketAddr;
use tokio::time::{interval, Duration};
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "relay=info".into()))
        .init();

    // SurrealDB (local embedded via SurrealKV)
    let db_path = std::env::var("SURREAL_DB_PATH")
        .unwrap_or_else(|_| "data/relay.db".into());
    let db = db::init_db(&db_path)
        .await
        .expect("Failed to initialize SurrealDB");

    let cleanup_db = db.clone();
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(60));
        loop {
            tick.tick().await;

            match db::cleanup_expired(&cleanup_db).await {
                Ok(n) if n > 0 => tracing::info!("Cleaned up {n} expired packets"),
                Err(e) => tracing::warn!("Cleanup error: {e}"),
                _ => {}
            }

            match db::mark_offline_nodes(&cleanup_db).await {
                Ok(n) if n > 0 => tracing::info!("Marked {n} nodes as offline"),
                Err(e) => tracing::warn!("Offline detection error: {e}"),
                _ => {}
            }
        }
    });

    let app = Router::new()
        .route("/E/health", get(handlers::health))
        .route("/E/register", post(handlers::register))
        .route("/E/push", post(handlers::push))
        .route("/E/pull/{mesh_id}/{instance_id}", get(handlers::pull))
        .route("/E/mesh/{mesh_id}/status", get(handlers::mesh_status))
        .route("/E/mesh/{mesh_id}/resolve/{instance_id}", get(handlers::resolve_node))
        .layer(CorsLayer::permissive())
        .with_state(db);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3200);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    tracing::info!("Eck relay listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
