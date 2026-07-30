/// Backend-agnostic relay DB handle — embedded SurrealKv by default (edge
/// nodes / kiosks), or the shared remote server when `SURREAL_REMOTE_URL`
/// is set (paid eck1/eck2/eck3 engines). See `eck_core::db::connect_with_db`.
pub type RelayDb = eck_core::db::SurrealDb;

/// Initialize SurrealDB for the relay (database `relay`).
pub async fn init_db(path: &str) -> Result<RelayDb, surrealdb::Error> {
    // Ensure the parent directory exists (embedded mode only cares)
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // No schema definitions — SurrealDB schemaless mode (same approach as WMS)
    // Tables are auto-created on first write
    eck_core::db::connect_with_db(path, "relay").await
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
