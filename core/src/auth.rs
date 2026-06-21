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
