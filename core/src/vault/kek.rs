//! The node key (KEK) — a random 256-bit key held ONLY on this node, in a file
//! in the data dir (`.eck/PII_VAULT.md` §2). It wraps the safe blob; without it
//! the shares fetched from eck1/2/3 are useless. It never leaves the node and is
//! never logged.

use std::path::PathBuf;

use rand::RngCore;
use zeroize::Zeroizing;

/// Path to the KEK file: `ECK_VAULT_KEK_FILE` (or its alias
/// `ECK_VAULT_KEK_PATH`) if set, else `vault_kek.bin` next to the SurrealDB file
/// (`SURREAL_DB_PATH`, default `data/wms.db`).
///
/// **On a node with an encrypted data volume the KEK MUST be moved out of
/// `data/`** (e.g. `ECK_VAULT_KEK_PATH=/var/lib/9eckwms/vault_kek.bin`): the KEK
/// is read *before* the volume is mounted, and a KEK sealed inside the volume it
/// helps open is a boot deadlock. See `.eck/DATA_AT_REST.md`.
pub fn kek_path() -> PathBuf {
    for key in ["ECK_VAULT_KEK_PATH", "ECK_VAULT_KEK_FILE"] {
        if let Ok(p) = std::env::var(key) {
            let p = p.trim();
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    let db_path = std::env::var("SURREAL_DB_PATH").unwrap_or_else(|_| "data/wms.db".into());
    let dir = PathBuf::from(&db_path)
        .parent()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("vault_kek.bin")
}

/// True if the KEK file already exists on disk.
pub fn kek_exists() -> bool {
    kek_path().exists()
}

/// Load the node KEK, creating a fresh random one (0600 on unix) on first boot.
/// The 32 raw bytes are never logged.
pub fn load_or_create_kek() -> Result<Zeroizing<[u8; 32]>, String> {
    let path = kek_path();
    if path.exists() {
        let bytes = std::fs::read(&path).map_err(|e| format!("read KEK {}: {e}", path.display()))?;
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("KEK {} is not 32 bytes (got {})", path.display(), bytes.len()))?;
        // Wipe the transient read buffer.
        let _ = Zeroizing::new(bytes);
        return Ok(Zeroizing::new(key));
    }

    let mut key = Zeroizing::new([0u8; 32]);
    rand::thread_rng().fill_bytes(key.as_mut());
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create KEK dir {}: {e}", parent.display()))?;
        }
    }
    // Create with 0600 atomically (no world-readable window between write and
    // chmod). Windows dev boxes rely on BitLocker for at-rest protection (P0.5
    // non-goal; see .eck/PII_VAULT.md §2 — Windows ACL is P1).
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("create KEK {}: {e}", path.display()))?;
        f.write_all(key.as_ref())
            .map_err(|e| format!("write KEK {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, key.as_ref())
            .map_err(|e| format!("write KEK {}: {e}", path.display()))?;
    }
    tracing::info!("created node vault KEK at {}", path.display());
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kek_path_honours_env_override() {
        std::env::remove_var("ECK_VAULT_KEK_PATH");
        std::env::set_var("ECK_VAULT_KEK_FILE", "/tmp/custom_kek.bin");
        assert_eq!(kek_path(), PathBuf::from("/tmp/custom_kek.bin"));
        // The datafs-era alias wins when both are set (it is the one the
        // encrypted-volume runbook tells operators to use).
        std::env::set_var("ECK_VAULT_KEK_PATH", "/var/lib/9eckwms/vault_kek.bin");
        assert_eq!(kek_path(), PathBuf::from("/var/lib/9eckwms/vault_kek.bin"));
        std::env::remove_var("ECK_VAULT_KEK_PATH");
        std::env::remove_var("ECK_VAULT_KEK_FILE");
    }

    #[test]
    fn kek_path_defaults_next_to_db() {
        std::env::remove_var("ECK_VAULT_KEK_PATH");
        std::env::remove_var("ECK_VAULT_KEK_FILE");
        std::env::set_var("SURREAL_DB_PATH", "somedir/wms.db");
        assert_eq!(kek_path(), PathBuf::from("somedir").join("vault_kek.bin"));
        std::env::remove_var("SURREAL_DB_PATH");
    }

    #[test]
    fn load_or_create_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("eck_kek_test_{}", std::process::id()));
        let file = dir.join("kek.bin");
        std::env::remove_var("ECK_VAULT_KEK_PATH");
        std::env::set_var("ECK_VAULT_KEK_FILE", &file);
        let _ = std::fs::remove_file(&file);

        assert!(!kek_exists());
        let k1 = load_or_create_kek().unwrap();
        assert!(kek_exists());
        let k2 = load_or_create_kek().unwrap();
        assert_eq!(*k1, *k2, "second load must return the same key");

        std::env::remove_var("ECK_VAULT_KEK_FILE");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}
