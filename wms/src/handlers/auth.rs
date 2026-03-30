use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Extension, Json};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use eck_core::auth::Claims;
use eck_core::db::SurrealDb;

use crate::AppState;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub pin: Option<String>,
}

/// Check if any users exist in DB; if not, create a temporary setup admin.
/// Returns the plaintext password if a setup account was created/exists.
pub async fn seed_setup_account(db: &SurrealDb) -> Option<String> {
    // Count all non-deleted users
    let count: i64 = db
        .query("SELECT count() AS c FROM user WHERE deleted_at IS NONE GROUP ALL")
        .await
        .ok()?
        .take::<Option<serde_json::Value>>(0)
        .ok()
        .flatten()
        .and_then(|v| v.get("c")?.as_i64())
        .unwrap_or(0);

    let setup_exists: Option<serde_json::Value> = db
        .query("SELECT username, email FROM user WHERE email = 'admin@setup.local' AND deleted_at IS NONE LIMIT 1")
        .await
        .ok()?
        .take(0)
        .ok()?;

    // Real users exist and no setup account — nothing to do
    if count > 0 && setup_exists.is_none() {
        return None;
    }

    // Setup exists but real users arrived (e.g. via sync) — remove setup
    if let Some(_) = &setup_exists {
        let real_count: i64 = db
            .query("SELECT count() AS c FROM user WHERE email != 'admin@setup.local' AND deleted_at IS NONE GROUP ALL")
            .await
            .ok()
            .and_then(|mut r| r.take::<Option<serde_json::Value>>(0).ok())
            .flatten()
            .and_then(|v| v.get("c")?.as_i64())
            .unwrap_or(0);

        if real_count > 0 {
            let _ = db
                .query("DELETE FROM user WHERE email = 'admin@setup.local'")
                .await;
            info!("Setup account removed — real users exist.");
            return None;
        }
    }

    // Generate random 12-char password
    use rand::Rng;
    let password: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();

    let hash = eck_core::auth::hash_password(&password).ok()?;

    if setup_exists.is_some() {
        // Regenerate password for existing setup account
        let _ = db
            .query("UPDATE user SET password = $hash, updatedAt = $now WHERE email = 'admin@setup.local'")
            .bind(("hash", hash))
            .bind(("now", chrono::Utc::now()))
            .await;
        return Some(password);
    }

    // Create new setup account via SurrealQL
    let result = db
        .query(
            "CREATE user SET
                username = 'setup-admin',
                password = $password,
                email = 'admin@setup.local',
                name = 'Setup Admin',
                role = 'admin',
                userType = 'individual',
                pin = '',
                isActive = true,
                failed_login_attempts = 0,
                preferredLanguage = 'en',
                createdAt = time::now(),
                updatedAt = time::now()"
        )
        .bind(("password", hash))
        .await;
    match &result {
        Ok(_) => info!("Created temporary setup account: admin@setup.local"),
        Err(e) => info!("Failed to create setup account: {}", e),
    }

    info!("Created temporary setup account: admin@setup.local");
    Some(password)
}

/// Remove the setup account once a real admin is created.
pub async fn cleanup_setup_account(state: &AppState) {
    let real_count: i64 = state.db
        .query("SELECT count() AS c FROM user WHERE email != 'admin@setup.local' AND deleted_at IS NONE GROUP ALL")
        .await
        .ok()
        .and_then(|mut r| r.take::<Option<serde_json::Value>>(0).ok())
        .flatten()
        .and_then(|v| v.get("c")?.as_i64())
        .unwrap_or(0);

    if real_count == 0 {
        return;
    }

    let _ = state.db
        .query("DELETE FROM user WHERE email = 'admin@setup.local'")
        .await;

    *state.setup_password.write().await = None;
    info!("Setup account removed — real users exist now.");
}

/// GET /E/auth/setup-status — returns temp credentials if no real users exist
pub async fn setup_status(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    match &*state.setup_password.read().await {
        Some(pw) => Json(json!({
            "needsSetup": true,
            "email": "admin@setup.local",
            "password": pw
        })),
        None => Json(json!({
            "needsSetup": false
        })),
    }
}

/// POST /api/auth/login — verify credentials, return JWT
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Select specific fields to avoid SurrealDB Thing → Value deserialization issues
    let result: Result<Option<serde_json::Value>, _> = state
        .db
        .query("SELECT record::id(id) AS user_id, username, password, email, name, role, pin, isActive FROM user WHERE (username = $username OR email = $username) AND isActive = true AND deleted_at IS NONE LIMIT 1")
        .bind(("username", body.username.clone()))
        .await
        .and_then(|mut r| r.take(0));

    let user = match result {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": "Invalid credentials" })),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e.to_string() })),
            );
        }
    };

    let password_hash = user.get("password").and_then(|v| v.as_str()).unwrap_or("");
    let pin_hash = user.get("pin").and_then(|v| v.as_str()).unwrap_or("");

    let verified = if let Some(ref pin) = body.pin {
        eck_core::auth::verify_password(pin, pin_hash).unwrap_or(false)
    } else if let Some(ref password) = body.password {
        eck_core::auth::verify_password(password, password_hash).unwrap_or(false)
    } else {
        false
    };

    if !verified {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": "Invalid credentials" })),
        );
    }

    let user_id = user.get("user_id").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let role = user.get("role").and_then(|v| v.as_str()).unwrap_or("operator");
    let username = user.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let name = user.get("name").and_then(|v| v.as_str());

    match eck_core::auth::create_token(&user_id, role, &state.jwt_secret) {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "token": token,
                "user": {
                    "id": user_id,
                    "username": username,
                    "name": name,
                    "role": role,
                }
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

/// GET /api/auth/me — return current user from JWT claims
pub async fn me(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let result: Result<Option<serde_json::Value>, _> = state
        .db
        .query("SELECT record::id(id) AS user_id, username, email, name, role, isActive FROM user WHERE record::id(id) = $uid AND deleted_at IS NONE LIMIT 1")
        .bind(("uid", claims.sub.clone()))
        .await
        .and_then(|mut r| r.take(0));

    match result {
        Ok(Some(user)) => (StatusCode::OK, Json(user)),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}
