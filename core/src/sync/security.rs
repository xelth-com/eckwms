use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use rand::RngCore;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use crate::models::sync_packet::{EncryptedSyncPacket, EntityMetadata};

#[derive(Error, Debug)]
pub enum SecurityError {
    #[error("Blind relay cannot create or decrypt packets")]
    RelayRestriction,
    #[error("Invalid key length: expected 32 bytes")]
    InvalidKeyLength,
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("Encryption failed")]
    EncryptionFailed,
    #[error("Decryption failed (invalid auth tag or corrupted data)")]
    DecryptionFailed,
}

/// Node roles in the mesh network
#[derive(Debug, Clone, PartialEq)]
pub enum SyncNodeRole {
    Master,
    Peer,
    Edge,
    BlindRelay,
}

/// SecurityLayer handles AES-256-GCM encryption/decryption of sync packets.
pub struct SecurityLayer {
    node_role: SyncNodeRole,
    shared_secret: Vec<u8>,
    key_id: String,
}

impl SecurityLayer {
    pub fn new(role: SyncNodeRole, secret_hex: &str) -> Self {
        let mut secret = Vec::new();

        if role != SyncNodeRole::BlindRelay {
            if let Ok(decoded) = hex::decode(secret_hex) {
                secret = decoded;
            }
        }

        Self {
            node_role: role,
            shared_secret: secret,
            key_id: "v1".to_string(),
        }
    }

    /// Encrypt a business entity into an EncryptedSyncPacket.
    /// The metadata (entity_type, entity_id, version, vector_clock) remains visible
    /// for routing purposes, but the actual payload is AES-256-GCM encrypted.
    pub fn encrypt_packet<T: Serialize>(
        &self,
        metadata: &EntityMetadata,
        payload: &T,
    ) -> Result<EncryptedSyncPacket, SecurityError> {
        if self.node_role == SyncNodeRole::BlindRelay {
            return Err(SecurityError::RelayRestriction);
        }

        if self.shared_secret.len() != 32 {
            return Err(SecurityError::InvalidKeyLength);
        }

        let json_payload = serde_json::to_vec(payload)?;
        let key = Key::<Aes256Gcm>::from_slice(&self.shared_secret);
        let cipher = Aes256Gcm::new(key);

        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, json_payload.as_ref())
            .map_err(|_| SecurityError::EncryptionFailed)?;

        let vc_value = serde_json::to_value(&metadata.vector_clock)?;

        Ok(EncryptedSyncPacket {
            entity_type: metadata.entity_type.clone(),
            entity_id: metadata.entity_id.clone(),
            version: metadata.version,
            source_instance: metadata.instance_id.clone(),
            vector_clock: vc_value,
            key_id: self.key_id.clone(),
            algorithm: "AES-256-GCM".to_string(),
            encrypted_payload: ciphertext,
            nonce: nonce_bytes.to_vec(),
        })
    }

    /// Decrypt an EncryptedSyncPacket back to a typed payload.
    pub fn decrypt_packet<T: DeserializeOwned>(
        &self,
        packet: &EncryptedSyncPacket,
    ) -> Result<T, SecurityError> {
        if self.node_role == SyncNodeRole::BlindRelay {
            return Err(SecurityError::RelayRestriction);
        }

        if self.shared_secret.len() != 32 {
            return Err(SecurityError::InvalidKeyLength);
        }

        let key = Key::<Aes256Gcm>::from_slice(&self.shared_secret);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&packet.nonce);

        let plaintext = cipher
            .decrypt(nonce, packet.encrypted_payload.as_ref())
            .map_err(|_| SecurityError::DecryptionFailed)?;

        let data: T = serde_json::from_slice(&plaintext)?;
        Ok(data)
    }

    /// Whether this node can encrypt/decrypt packets
    pub fn can_encrypt(&self) -> bool {
        self.node_role != SyncNodeRole::BlindRelay && self.shared_secret.len() == 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::vector_clock::VectorClock;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestPayload {
        name: String,
        value: i64,
    }

    fn test_secret() -> String {
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let layer = SecurityLayer::new(SyncNodeRole::Peer, &test_secret());

        let mut vc = VectorClock::new();
        vc.increment("test_node");

        let metadata = EntityMetadata {
            entity_id: "42".to_string(),
            entity_type: "product".to_string(),
            version: 1,
            updated_at: chrono::Utc::now(),
            source: "test".to_string(),
            source_priority: 10,
            instance_id: "test_node".to_string(),
            device_id: None,
            vector_clock: vc,
        };

        let original = TestPayload {
            name: "Widget".to_string(),
            value: 99,
        };

        let packet = layer.encrypt_packet(&metadata, &original).unwrap();
        assert_eq!(packet.algorithm, "AES-256-GCM");
        assert_eq!(packet.entity_type, "product");

        let decrypted: TestPayload = layer.decrypt_packet(&packet).unwrap();
        assert_eq!(decrypted, original);
    }

    #[test]
    fn test_blind_relay_cannot_encrypt() {
        let layer = SecurityLayer::new(SyncNodeRole::BlindRelay, "");

        let metadata = EntityMetadata {
            entity_id: "1".to_string(),
            entity_type: "test".to_string(),
            version: 1,
            updated_at: chrono::Utc::now(),
            source: "test".to_string(),
            source_priority: 0,
            instance_id: "x".to_string(),
            device_id: None,
            vector_clock: VectorClock::new(),
        };

        let result = layer.encrypt_packet(&metadata, &"secret");
        assert!(result.is_err());
    }
}
