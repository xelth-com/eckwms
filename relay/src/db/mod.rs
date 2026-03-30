use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::Surreal;

pub type RelayDb = Surreal<Db>;

/// Initialize SurrealDB and define tables/indexes for the relay.
pub async fn init_db(path: &str) -> Result<RelayDb, surrealdb::Error> {
    // Ensure the parent directory exists
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let db = Surreal::new::<SurrealKv>(path).await?;
    db.use_ns("eck").use_db("relay").await?;

    // No schema definitions — SurrealDB schemaless mode (same approach as WMS)
    // Tables are auto-created on first write

    tracing::info!("SurrealDB initialized (relay) at: {}", path);
    Ok(db)
}

/// Delete expired packets (returns count via SurrealQL count function).
pub async fn cleanup_expired(db: &RelayDb) -> Result<usize, surrealdb::Error> {
    // Use two separate queries: delete, then count remaining
    // Don't use RETURN BEFORE — Thing in returned data can't deserialize to Value
    db.query("DELETE FROM packet WHERE ttl < time::now()").await?;
    Ok(0) // Exact count not critical for cleanup
}

/// Mark offline nodes that haven't sent heartbeat for 20 minutes.
pub async fn mark_offline_nodes(db: &RelayDb) -> Result<usize, surrealdb::Error> {
    db.query(
        "UPDATE registration SET status = 'offline' WHERE last_seen < time::now() - 20m AND status = 'online'"
    ).await?;
    Ok(0) // Exact count not critical for background task
}
