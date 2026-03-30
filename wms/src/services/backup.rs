use std::path::PathBuf;

use chrono::{DateTime, Utc};
use eck_core::db::SurrealDb;
use serde_json::json;
use tracing::info;

const BACKUP_DIR: &str = "data/backups";
const MAX_BACKUPS: usize = 7;

/// Create a .surql backup of the database.
/// Returns the generated filename.
pub async fn create_backup(db: &SurrealDb) -> anyhow::Result<String> {
    let dir = PathBuf::from(BACKUP_DIR);
    tokio::fs::create_dir_all(&dir).await?;

    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("wms_backup_{}.surql", timestamp);
    let file_path = dir.join(&filename);

    // SurrealDB export to file
    db.export(&file_path).await?;

    info!("Backup created: {}", filename);

    // Cleanup old backups
    if let Err(e) = cleanup_old_backups().await {
        tracing::warn!("Backup cleanup failed: {}", e);
    }

    Ok(filename)
}

/// List all .surql backups sorted by creation time (newest first).
pub async fn list_backups() -> anyhow::Result<Vec<serde_json::Value>> {
    let dir = PathBuf::from(BACKUP_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut backups = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("surql") {
            continue;
        }

        let metadata = entry.metadata().await?;
        let size = metadata.len();
        let created_at = metadata.modified()
            .or_else(|_| metadata.created())
            .map(|t| {
                let dt: DateTime<Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_default();

        let filename = path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        backups.push(json!({
            "filename": filename,
            "sizeBytes": size,
            "createdAt": created_at,
        }));
    }

    // Sort newest first
    backups.sort_by(|a, b| {
        let a_date = a["createdAt"].as_str().unwrap_or("");
        let b_date = b["createdAt"].as_str().unwrap_or("");
        b_date.cmp(a_date)
    });

    Ok(backups)
}

/// Restore a database from a .surql backup file.
pub async fn restore_backup(db: &SurrealDb, filename: &str) -> anyhow::Result<()> {
    // Path traversal protection
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        anyhow::bail!("Invalid filename");
    }

    let file_path = PathBuf::from(BACKUP_DIR).join(filename);
    if !file_path.exists() {
        anyhow::bail!("Backup file '{}' not found", filename);
    }

    // Read and execute the .surql file
    let content = tokio::fs::read_to_string(&file_path).await?;
    db.query(&content).await?;

    info!("Backup restored: {}", filename);
    Ok(())
}

/// Keep only the most recent MAX_BACKUPS files.
pub async fn cleanup_old_backups() -> anyhow::Result<()> {
    let dir = PathBuf::from(BACKUP_DIR);
    if !dir.exists() {
        return Ok(());
    }

    let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("surql") {
            continue;
        }
        let modified = entry.metadata().await?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        files.push((path, modified));
    }

    // Sort newest first
    files.sort_by(|a, b| b.1.cmp(&a.1));

    // Delete everything beyond MAX_BACKUPS
    for (path, _) in files.into_iter().skip(MAX_BACKUPS) {
        info!("Removing old backup: {}", path.display());
        let _ = tokio::fs::remove_file(&path).await;
    }

    Ok(())
}
