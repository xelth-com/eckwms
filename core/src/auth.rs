use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub role: String,
    pub auth_method: String,
    pub exp: i64,
    pub iat: i64,
}

pub fn create_token(user_id: &str, role: &str, auth_method: &str, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.to_string(),
        role: role.to_string(),
        auth_method: auth_method.to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::hours(24)).timestamp(),
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))
}

pub fn validate_token(token: &str, secret: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}

pub fn hash_password(password: &str) -> Result<String, bcrypt::BcryptError> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST)
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, bcrypt::BcryptError> {
    bcrypt::verify(password, hash)
}

/// Resolve the JWT secret without ever falling back to a published constant.
///
/// Order: `JWT_SECRET` env (non-empty) → persisted per-node secret at
/// `secret_path` → freshly generated 48 random bytes (hex), persisted for
/// subsequent boots. A mis-provisioned node thus still boots (kiosk LAN ops
/// must survive), but its tokens are unforgeable — unlike the old
/// "dev-secret-change-in-production" fallback anyone could mint against.
pub fn load_or_generate_jwt_secret(secret_path: &std::path::Path) -> String {
    if let Ok(s) = std::env::var("JWT_SECRET") {
        if !s.trim().is_empty() {
            return s;
        }
    }

    if let Ok(existing) = std::fs::read_to_string(secret_path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            tracing::warn!(
                "JWT_SECRET env is unset — using the persisted per-node secret at {}",
                secret_path.display()
            );
            return existing;
        }
    }

    use rand::RngCore;
    let mut bytes = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut bytes);
    let secret = hex::encode(bytes);

    if let Some(parent) = secret_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(secret_path, &secret) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(secret_path, std::fs::Permissions::from_mode(0o600));
            }
            tracing::warn!(
                "JWT_SECRET env is unset — generated a persistent per-node secret at {} (set JWT_SECRET in the environment to override)",
                secret_path.display()
            );
        }
        Err(e) => {
            // Still better than a published constant: the secret lives for
            // this process only; tokens die on restart.
            tracing::error!(
                "JWT_SECRET unset and persisting a generated secret to {} FAILED ({}) — using an ephemeral secret, all tokens invalidate on restart",
                secret_path.display(),
                e
            );
        }
    }
    secret
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secret_persists_and_reloads() {
        let dir = std::env::temp_dir().join(format!("eck_jwt_test_{}", std::process::id()));
        let path = dir.join("jwt_secret");
        let _ = std::fs::remove_file(&path);
        // NB: relies on JWT_SECRET not being set in the test environment; if
        // it is, the env value legitimately wins and this test is vacuous.
        if std::env::var("JWT_SECRET").map(|s| !s.trim().is_empty()).unwrap_or(false) {
            return;
        }
        let first = load_or_generate_jwt_secret(&path);
        assert_eq!(first.len(), 96, "48 random bytes hex-encoded");
        let second = load_or_generate_jwt_secret(&path);
        assert_eq!(first, second, "second boot reuses the persisted secret");
        let _ = std::fs::remove_file(&path);
    }
}
