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

            // GC acked xelixir_task rows older than 1 h. The dispatcher
            // polls `/E/x/result/<task_id>` to read the ack body, so we
            // keep rows around for a window long enough that no caller
            // is still polling, then drop them.
            //
            // `acked_at` is stored as an RFC3339 string (the WMS-side
            // ack handler doesn't have access to SurrealDB native
            // datetime construction without round-tripping). Compare on
            // the string-coerced cutoff so the predicate doesn't blow
            // up on type mismatch — and explicitly check `acked = true`
            // (NOT `acked_at IS NOT NONE`) so non-acked rows never get
            // collected even with a malformed timestamp.
            let cutoff_iso = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
            let res = cleanup_db
                .query("DELETE xelixir_task WHERE acked = true AND acked_at != NONE AND acked_at < $cutoff;")
                .bind(("cutoff", cutoff_iso.clone()))
                .await;
            if let Err(e) = res {
                tracing::warn!("xelixir_task GC error: {e}");
            }

            // Same GC policy for mesh_task — acked rows linger 1 h so the
            // sender's poller has a window to read the result body, then drop.
            let res = cleanup_db
                .query("DELETE mesh_task WHERE acked = true AND acked_at != NONE AND acked_at < $cutoff;")
                .bind(("cutoff", cutoff_iso))
                .await;
            if let Err(e) = res {
                tracing::warn!("mesh_task GC error: {e}");
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
        .route("/E/registry", get(handlers::registry))
        // Mesh-agnostic xelixir control plane (UUID-routed, NAT-friendly).
        .route("/E/resolve/{instance_id}", get(handlers::x_resolve))
        .route("/E/x/dispatch/{target_uuid}", post(handlers::x_dispatch))
        .route("/E/x/poll/{self_uuid}", get(handlers::x_poll))
        .route("/E/x/ack/{task_id}", post(handlers::x_ack))
        .route("/E/x/result/{task_id}", get(handlers::x_result))
        // Mesh-sync routing for NAT'd peers — separate queue from xelixir C2.
        .route("/E/m/dispatch/{target_uuid}", post(handlers::m_dispatch))
        .route("/E/m/poll/{self_uuid}", get(handlers::m_poll))
        .route("/E/m/ack/{task_id}", post(handlers::m_ack))
        .route("/E/m/result/{task_id}", get(handlers::m_result))
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
