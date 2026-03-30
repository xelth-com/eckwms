use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerIdentity {
    pub instance_id: String,
    pub private_key: String, // Base64
    pub public_key: String,  // Base64
}

impl ServerIdentity {
    /// Returns the public key as uppercase hex (for QR codes)
    pub fn public_key_hex(&self) -> Result<String, String> {
        let bytes = BASE64
            .decode(&self.public_key)
            .map_err(|e| format!("invalid public key base64: {}", e))?;
        Ok(hex::encode(bytes).to_uppercase())
    }
}

/// Load server identity from .eck/server_identity.json, or generate a new one
pub fn load_or_generate_identity(instance_id: &str) -> ServerIdentity {
    let config_dir = ".eck";
    let identity_file = Path::new(config_dir).join("server_identity.json");

    // Try env vars first
    if let (Ok(pub_key), Ok(priv_key)) = (
        std::env::var("SERVER_PUBLIC_KEY"),
        std::env::var("SERVER_PRIVATE_KEY"),
    ) {
        if !pub_key.is_empty() && !priv_key.is_empty() {
            return ServerIdentity {
                instance_id: instance_id.to_string(),
                public_key: pub_key,
                private_key: priv_key,
            };
        }
    }

    // Try file
    if identity_file.exists() {
        if let Ok(data) = std::fs::read_to_string(&identity_file) {
            if let Ok(mut identity) = serde_json::from_str::<ServerIdentity>(&data) {
                if identity.instance_id != instance_id {
                    tracing::info!(
                        "Updating server identity instance_id: {} -> {}",
                        identity.instance_id,
                        instance_id
                    );
                    identity.instance_id = instance_id.to_string();
                    if let Ok(json) = serde_json::to_string_pretty(&identity) {
                        let _ = std::fs::write(&identity_file, json);
                    }
                }
                tracing::info!("Loaded server identity from {}", identity_file.display());
                return identity;
            }
        }
    }

    // Generate new keypair
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    let identity = ServerIdentity {
        instance_id: instance_id.to_string(),
        private_key: BASE64.encode(signing_key.to_bytes()),
        public_key: BASE64.encode(verifying_key.to_bytes()),
    };

    let _ = std::fs::create_dir_all(config_dir);
    if let Ok(data) = serde_json::to_string_pretty(&identity) {
        let _ = std::fs::write(&identity_file, data);
    }

    tracing::info!(
        "Generated new server identity, saved to {}",
        identity_file.display()
    );
    identity
}

/// Ensure instance_id is a valid UUID. If not, generate one and persist it to .env.
/// Ported from legacy eckwmsr/src/config.rs — guarantees stable identity across restarts.
pub fn ensure_uuid_instance_id(raw: &str) -> String {
    if uuid::Uuid::parse_str(raw).is_ok() {
        return raw.to_string();
    }
    let id = uuid::Uuid::new_v4();
    // Persist to .env so the same UUID is used on next startup
    if let Ok(contents) = std::fs::read_to_string(".env") {
        let new_line = format!("INSTANCE_ID={}", id);
        let updated = if contents.contains("INSTANCE_ID=") {
            let mut result = String::new();
            for line in contents.lines() {
                if line.starts_with("INSTANCE_ID=") {
                    result.push_str(&new_line);
                } else {
                    result.push_str(line);
                }
                result.push('\n');
            }
            result
        } else {
            format!("{}\n{}\n", contents.trim_end(), new_line)
        };
        let _ = std::fs::write(".env", updated);
    }
    tracing::info!("Generated INSTANCE_ID={} (saved to .env)", id);
    id.to_string()
}

/// Compute mesh_id from SYNC_NETWORK_KEY.
/// mesh_id = sha256(key)[:8 bytes] = 16 hex characters.
/// All nodes sharing the same network key get the same mesh_id automatically.
pub fn compute_mesh_id(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(key.as_bytes());
    hex::encode(&hash[..8])
}

/// Verify an Ed25519 signature (public key and signature are base64-encoded)
pub fn verify_signature(
    public_key_base64: &str,
    message: &str,
    signature_base64: &str,
) -> Result<bool, String> {
    let pub_bytes = BASE64
        .decode(public_key_base64)
        .map_err(|e| format!("invalid public key: {}", e))?;

    if pub_bytes.len() != 32 {
        return Err(format!(
            "invalid public key size: expected 32, got {}",
            pub_bytes.len()
        ));
    }

    let verifying_key = VerifyingKey::from_bytes(
        pub_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "invalid public key bytes".to_string())?,
    )
    .map_err(|e| format!("invalid public key: {}", e))?;

    let sig_bytes = BASE64
        .decode(signature_base64)
        .map_err(|e| format!("invalid signature: {}", e))?;

    let signature =
        Signature::from_slice(&sig_bytes).map_err(|e| format!("invalid signature format: {}", e))?;

    Ok(verifying_key.verify(message.as_bytes(), &signature).is_ok())
}
