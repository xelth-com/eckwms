use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::PathBuf;
use tokio::fs;
use tracing::info;
use uuid::Uuid;

/// Max size for storing content inline in DB as avatar (50 KB).
const MAX_AVATAR_SIZE: usize = 50 * 1024;

/// Compute a deterministic UUID from file bytes using MurmurHash3 x64_128 (seed=0).
/// Matches the Kotlin/Android ContentHash.uuidFromBytes() and the legacy eckwmsr
/// implementation exactly — keeps CAS keys within UUID (128-bit) bounds.
pub fn content_hash_uuid(data: &[u8]) -> Uuid {
    let hash = murmur3::murmur3_x64_128(&mut Cursor::new(data), 0)
        .expect("murmur3 hash should not fail on in-memory data");
    // murmur3 crate packs u128 as: lower 64 bits = h1, upper 64 bits = h2
    let h1 = hash as u64;
    let h2 = (hash >> 64) as u64;
    // Construct 16 bytes: h1 big-endian || h2 big-endian (matches Kotlin)
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&h1.to_be_bytes());
    bytes[8..16].copy_from_slice(&h2.to_be_bytes());
    Uuid::from_bytes(bytes)
}

/// Compute SHA-256 hex string of raw bytes (used for disk path and backward compat).
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Result of a successful file save.
pub struct SavedFile {
    pub cas_uuid: Uuid,
    pub sha256: String,
    pub storage_path: String,
    pub avatar_data: Option<Vec<u8>>,
    pub size_bytes: i64,
}

/// Content-addressable file storage on disk.
///
/// Files are stored at `{base_dir}/data/filestore/{sha256[0..2]}/{sha256[2..4]}/{sha256}.{ext}`.
/// The primary key (CAS UUID) is a MurmurHash3 x64_128 of the content — identical
/// files always map to the same UUID, enabling deduplication across devices.
pub struct FileStore {
    base_dir: String,
}

impl FileStore {
    pub fn new(base_dir: &str) -> Self {
        Self {
            base_dir: base_dir.to_string(),
        }
    }

    /// Save file content to disk (CAS). Returns metadata needed for DB record.
    ///
    /// - `claimed_id`: optional UUID the client claims for this content (verified against Murmur3).
    /// - `explicit_avatar`: optional client-generated thumbnail (e.g. Smart Crop 224×224).
    pub async fn save(
        &self,
        content: &[u8],
        filename: &str,
        explicit_avatar: Option<&[u8]>,
        claimed_id: Option<&str>,
    ) -> Result<SavedFile, String> {
        let cas_uuid = content_hash_uuid(content);

        // Verify CAS claim if provided
        if let Some(claimed) = claimed_id {
            if !claimed.is_empty() {
                if let Ok(parsed) = Uuid::parse_str(claimed) {
                    if parsed != cas_uuid {
                        return Err(format!(
                            "CAS verification failed: claimed {} != computed {}",
                            claimed, cas_uuid
                        ));
                    }
                }
            }
        }

        let sha = sha256_hex(content);

        // Disk path: data/filestore/aa/bb/{sha}.{ext}
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|os| os.to_str())
            .map(|s| format!(".{}", s))
            .unwrap_or_default();

        let rel_path = format!(
            "data/filestore/{}/{}/{}{}",
            &sha[0..2],
            &sha[2..4],
            sha,
            ext
        );
        let abs_path = PathBuf::from(&self.base_dir).join(&rel_path);

        // Only write if not already on disk (idempotent CAS)
        if !abs_path.exists() {
            if let Some(parent) = abs_path.parent() {
                fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("Failed to create directory: {}", e))?;
            }
            fs::write(&abs_path, content)
                .await
                .map_err(|e| format!("Failed to write file: {}", e))?;
        }

        // Avatar: prefer explicit thumbnail, fall back to inline if small enough
        let avatar_data = if let Some(av) = explicit_avatar.filter(|a| !a.is_empty()) {
            Some(av.to_vec())
        } else if content.len() <= MAX_AVATAR_SIZE {
            Some(content.to_vec())
        } else {
            None
        };

        info!(
            "FileStore: saved {} ({} bytes) as {} → {}",
            filename,
            content.len(),
            cas_uuid,
            rel_path
        );

        Ok(SavedFile {
            cas_uuid,
            sha256: sha,
            storage_path: rel_path,
            avatar_data,
            size_bytes: content.len() as i64,
        })
    }

    /// Read file bytes from disk given a relative storage path.
    pub async fn read(&self, storage_path: &str) -> Result<Vec<u8>, String> {
        let abs_path = PathBuf::from(&self.base_dir).join(storage_path);
        fs::read(&abs_path)
            .await
            .map_err(|e| format!("Failed to read file: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_hash_uuid_vectors() {
        // Cross-platform reference vectors (must match Kotlin ContentHash.uuidFromBytes)
        assert_eq!(
            content_hash_uuid(b"test").to_string(),
            "ac7d28cc-74bd-e19d-9a12-8231f9bd4d82"
        );
        assert_eq!(
            content_hash_uuid(b"hello").to_string(),
            "cbd8a7b3-41bd-9b02-5b1e-906a48ae1d19"
        );
        assert_eq!(
            content_hash_uuid(b"").to_string(),
            "00000000-0000-0000-0000-000000000000"
        );
        // Determinism
        assert_eq!(content_hash_uuid(b"test"), content_hash_uuid(b"test"));
    }
}
