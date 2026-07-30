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
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// CAS integrity gate: does `data` hash to `expected_hex`? Case-insensitive on
/// the hex. An empty `expected_hex` passes (no claim to check) — mirrors the
/// tolerance in [`FileStore::write_verified`]. Used by the relay `file_fetch`
/// path to reject a blob a blind relay / untrusted peer returned under the
/// wrong content-address before it's trusted.
pub fn verify_sha256(data: &[u8], expected_hex: &str) -> bool {
    expected_hex.is_empty() || sha256_hex(data).eq_ignore_ascii_case(expected_hex)
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

    /// Check whether a file exists on disk given a relative storage path.
    pub fn exists(&self, storage_path: &str) -> bool {
        PathBuf::from(&self.base_dir).join(storage_path).exists()
    }

    /// Land bytes fetched FROM A MESH PEER at the storage path the synced
    /// `file_resource` row already names, after verifying they hash to
    /// `expected_sha`. The path is a pure function of the content sha (see
    /// `save`), so the origin node's `storage_path` reproduces byte-for-byte
    /// here — no rewrite of the row is needed.
    ///
    /// The sha check is the point: a peer is a remote party, and silently
    /// caching whatever it returned under a content-addressed name would let
    /// one bad node poison every other node's CAS. On mismatch nothing is
    /// written and the caller falls through to its 404.
    pub async fn write_verified(
        &self,
        storage_path: &str,
        content: &[u8],
        expected_sha: &str,
    ) -> Result<(), String> {
        let actual = sha256_hex(content);
        if !expected_sha.is_empty() && actual != expected_sha {
            return Err(format!(
                "hash mismatch: expected {}, got {} ({} bytes)",
                expected_sha,
                actual,
                content.len()
            ));
        }
        let abs_path = PathBuf::from(&self.base_dir).join(storage_path);
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
        fs::write(&abs_path, content)
            .await
            .map_err(|e| format!("Failed to write file: {}", e))?;
        info!(
            "FileStore: hydrated {} ({} bytes) from mesh peer",
            storage_path,
            content.len()
        );
        Ok(())
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

    #[test]
    fn verify_sha256_rejects_wrong_bytes() {
        let data = b"the quick brown fox";
        let good = sha256_hex(data);
        // Correct content passes.
        assert!(verify_sha256(data, &good));
        // Case-insensitive on the hex claim.
        assert!(verify_sha256(data, &good.to_uppercase()));
        // Wrong bytes under the same claimed hash are rejected (CAS poison guard).
        assert!(!verify_sha256(b"a different payload", &good));
        // Empty claim = no check (tolerant, matches write_verified).
        assert!(verify_sha256(data, ""));
    }
}
