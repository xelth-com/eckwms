use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub email: String,
    pub password: String,
    pub name: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
    pub pin: Option<String>,
    #[serde(rename = "isActive", default = "default_true")]
    pub is_active: bool,
    /// Primary UI language; defaults to "en" to mirror prior hardcoded behavior.
    #[serde(rename = "preferredLanguage", default = "default_language")]
    pub preferred_language: String,
    /// Full set of languages the user speaks (e.g. ["de","ko"]).
    #[serde(rename = "languages", default)]
    pub languages: Option<Vec<String>>,
    /// Force the user to set their own password on first login (bulk-seeded
    /// accounts get a shared generated password).
    #[serde(rename = "mustChangePassword", default)]
    pub must_change_password: bool,
}

#[derive(Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<String>,
    pub email: Option<String>,
    pub password: Option<String>,
    pub pin: Option<String>,
    #[serde(rename = "isActive")]
    pub is_active: Option<bool>,
    #[serde(rename = "preferredLanguage")]
    pub preferred_language: Option<String>,
    #[serde(rename = "languages")]
    pub languages: Option<Vec<String>>,
    #[serde(rename = "mustChangePassword")]
    pub must_change_password: Option<bool>,
}

fn default_role() -> String { "user".into() }
fn default_true() -> bool { true }
fn default_language() -> String { "en".into() }

/// GET /api/admin/users — list all active users (no password/pin hashes)
pub async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Value>>, (StatusCode, String)> {
    let mut result = state
        .db
        .query(
            "SELECT record::id(id) AS id, username, email, name, role, userType, isActive, \
             pin != '' AND pin IS NOT NONE AS hasPin, mustChangePassword, \
             preferredLanguage, languages, lastLogin, createdAt, updatedAt \
             FROM user WHERE deleted_at IS NONE ORDER BY createdAt DESC",
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let users: Vec<Value> = result
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(users))
}

/// POST /api/admin/users — create a new user
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    if payload.username.is_empty() || payload.email.is_empty() || payload.password.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "username, email and password are required".into(),
        ));
    }

    let hashed_password = eck_core::auth::hash_password(&payload.password)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let pin = match &payload.pin {
        Some(p) if !p.is_empty() => {
            eck_core::auth::hash_password(p)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        _ => String::new(),
    };

    let username = payload.username.clone();

    // CREATE + SELECT in one query to avoid Thing serialization and timing issues.
    // Record id = the USERNAME (type::record binds it injection-safe): the user
    // table is mesh-synced, and a deterministic id makes the same account created
    // on two nodes converge via LWW instead of duplicating. `_vclock` starts at
    // this node's component so the row propagates as a causal event.
    let mut result = state
        .db
        .query(
            "CREATE type::record($tb, $username) SET
                username = $username,
                _vclock = $vclock,
                password = $password,
                email = $email,
                name = $name,
                role = $role,
                pin = $pin,
                userType = 'individual',
                isActive = $is_active,
                mustChangePassword = $must_change_password,
                failed_login_attempts = 0,
                preferredLanguage = $preferred_language,
                languages = $languages,
                createdAt = time::now(),
                updatedAt = time::now();
            SELECT record::id(id) AS id, username, email, name, role, isActive, \
                preferredLanguage, languages, createdAt, updatedAt \
                FROM user WHERE username = $username AND deleted_at IS NONE \
                ORDER BY createdAt DESC LIMIT 1;",
        )
        .bind(("tb", "user"))
        .bind(("username", payload.username))
        .bind(("vclock", eck_core::sync::conflict::next_local_vclock(None, &state.instance_id)))
        .bind(("password", hashed_password))
        .bind(("email", payload.email))
        .bind(("name", payload.name))
        .bind(("role", payload.role))
        .bind(("pin", pin))
        .bind(("is_active", payload.is_active))
        .bind(("must_change_password", payload.must_change_password))
        .bind(("preferred_language", payload.preferred_language))
        .bind(("languages", payload.languages))
        .await
        .map_err(|e| {
            let msg = e.to_string();
            // "already exists" = CREATE on the deterministic user:⟨username⟩ id;
            // "unique"/"duplicate" = the username unique index.
            if msg.contains("duplicate") || msg.contains("unique") || msg.contains("already exists") {
                (StatusCode::CONFLICT, "User already exists (check username/email)".into())
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        })?;

    // Statement 0 = CREATE (skip), Statement 1 = SELECT
    let created: Option<Value> = result
        .take(1)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Remove setup account now that a real user exists
    super::auth::cleanup_setup_account(&state).await;
    info!("New user '{}' created, setup account cleanup triggered", username);

    // Real users DO sync across the mesh since 2026-07-09 (`user` is in
    // SYNC_ENTITY_TYPES; record id = username; the engine's merkle sweep
    // picks this row up — no explicit push needed here). Only the
    // install-time setup-admin stays node-local in `users_db`.

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": created.as_ref().and_then(|v| v.get("id")).cloned(),
            "username": username,
            "message": "User created"
        })),
    ))
}

/// PUT /api/admin/users/:id — update user fields
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Verify user exists
    let exists: Option<Value> = state
        .db
        .query("SELECT record::id(id) AS id FROM user WHERE record::id(id) = $id AND deleted_at IS NONE LIMIT 1")
        .bind(("id", id.clone()))
        .await
        .and_then(|mut r| r.take(0))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if exists.is_none() {
        return Err((StatusCode::NOT_FOUND, "User not found".into()));
    }

    // Build the update object — only set provided fields
    let mut update_obj = json!({ "updatedAt": chrono::Utc::now() });
    let map = update_obj.as_object_mut().unwrap();

    if let Some(name) = payload.name {
        map.insert("name".into(), json!(name));
    }
    if let Some(ref role) = payload.role {
        if !role.is_empty() {
            map.insert("role".into(), json!(role));
        }
    }
    if let Some(ref email) = payload.email {
        if !email.is_empty() {
            map.insert("email".into(), json!(email));
        }
    }
    if let Some(is_active) = payload.is_active {
        map.insert("isActive".into(), json!(is_active));
    }
    if let Some(must_change) = payload.must_change_password {
        map.insert("mustChangePassword".into(), json!(must_change));
    }
    if let Some(ref preferred_language) = payload.preferred_language {
        if !preferred_language.is_empty() {
            map.insert("preferredLanguage".into(), json!(preferred_language));
        }
    }
    if let Some(ref languages) = payload.languages {
        map.insert("languages".into(), json!(languages));
    }
    if let Some(ref pin) = payload.pin {
        let hashed_pin = if pin.is_empty() {
            String::new()
        } else {
            eck_core::auth::hash_password(pin)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        };
        map.insert("pin".into(), json!(hashed_pin));
    }
    if let Some(ref password) = payload.password {
        if !password.is_empty() {
            let hashed = eck_core::auth::hash_password(password)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            map.insert("password".into(), json!(hashed));
        }
    }

    state
        .db
        .query("UPDATE user MERGE $data WHERE record::id(id) = $id AND deleted_at IS NONE")
        .bind(("id", id.clone()))
        .bind(("data", update_obj))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Advance causality so the edit propagates across the mesh.
    if let Err(e) = eck_core::sync::conflict::bump_local_vclock_by_leaf(
        &state.db, "user", &id, &state.instance_id,
    ).await {
        tracing::warn!("user update: vclock bump failed for {}: {}", id, e);
    }

    Ok(Json(json!({
        "id": id,
        "message": "User updated"
    })))
}

/// DELETE /api/admin/users/:id — soft delete via deleted_at
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut result = state
        .db
        .query(
            "UPDATE user SET deleted_at = time::now(), updatedAt = time::now() \
             WHERE record::id(id) = $id AND deleted_at IS NONE",
        )
        .bind(("id", id.clone()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let updated: Option<Value> = result
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match updated {
        Some(_) => {
            // Soft delete is a content change — bump causality so peers adopt it.
            if let Err(e) = eck_core::sync::conflict::bump_local_vclock_by_leaf(
                &state.db, "user", &id, &state.instance_id,
            ).await {
                tracing::warn!("user delete: vclock bump failed for {}: {}", id, e);
            }
            Ok(Json(json!({ "message": "User deleted" })))
        }
        None => Err((StatusCode::NOT_FOUND, "User not found".into())),
    }
}
