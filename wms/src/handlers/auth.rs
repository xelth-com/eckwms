use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use eck_core::auth::Claims;
use eck_core::db::SurrealDb;

use crate::AppState;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
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
    let real_count: i64 = state.users_db
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

    let _ = state.users_db
        .query("DELETE FROM user WHERE email = 'admin@setup.local'")
        .await;

    *state.setup_password.write().await = None;
    info!("Setup account removed — real users exist now.");
}

/// GET /E/auth/setup-status — returns temp credentials if no real users exist
pub async fn setup_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
) -> Json<serde_json::Value> {
    let client_ip = extract_client_ip(&headers, &ConnectInfo(addr));
    let ua = headers
        .get("User-Agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let referer = headers
        .get("Referer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match &*state.setup_password.read().await {
        Some(pw) => {
            tracing::info!(
                target: "diag::setup_status",
                ip = %client_ip,
                ua_short = ua.chars().take(40).collect::<String>(),
                referer = referer,
                "needsSetup=true returned (password={}…)",
                &pw.chars().take(4).collect::<String>()
            );
            Json(json!({
                "needsSetup": true,
                "email": "admin@setup.local",
                "password": pw
            }))
        },
        None => {
            tracing::info!(
                target: "diag::setup_status",
                ip = %client_ip,
                ua_short = ua.chars().take(40).collect::<String>(),
                referer = referer,
                "needsSetup=false returned (no setup_password in AppState)"
            );
            Json(json!({
                "needsSetup": false
            }))
        },
    }
}

/// POST /api/auth/login — verify credentials, return JWT
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Select specific fields to avoid SurrealDB Thing → Value deserialization issues
    let result: Result<Option<serde_json::Value>, _> = state
        .users_db
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

    let verified = eck_core::auth::verify_password(&body.password, password_hash).unwrap_or(false);

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

    match eck_core::auth::create_token(&user_id, role, "password", &state.jwt_secret) {
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
        .users_db
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

fn is_local_ip(ip: &str) -> bool {
    ip == "127.0.0.1" || ip == "::1" || ip == "::ffff:127.0.0.1"
}

fn extract_client_ip(headers: &HeaderMap, connect_info: &ConnectInfo<std::net::SocketAddr>) -> String {
    if let Some(real_ip) = headers.get("X-Real-IP").and_then(|v| v.to_str().ok()) {
        return real_ip.to_string();
    }
    if let Some(xff) = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            return first.trim().to_string();
        }
    }
    connect_info.0.ip().to_string()
}

pub async fn kiosk_token(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    let client_ip = extract_client_ip(&headers, &ConnectInfo(addr));
    let ua = headers
        .get("User-Agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !is_local_ip(&client_ip) {
        tracing::info!(
            target: "diag::kiosk_token",
            ip = %client_ip,
            ua_short = ua.chars().take(40).collect::<String>(),
            "REJECTED: non-local IP"
        );
        return (StatusCode::FORBIDDEN, Json(json!({ "success": false, "error": "Kiosk token only available from localhost" })));
    }

    let enabled: Option<serde_json::Value> = state
        .db
        .query("SELECT enabled FROM system_config:kiosk")
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    let is_enabled = enabled.as_ref().and_then(|v| v.get("enabled")?.as_bool()).unwrap_or(false);
    if !is_enabled {
        tracing::info!(
            target: "diag::kiosk_token",
            ip = %client_ip,
            raw_config = ?enabled,
            "REJECTED: kiosk mode not enabled"
        );
        return (StatusCode::FORBIDDEN, Json(json!({ "success": false, "error": "Kiosk mode is not enabled" })));
    }

    tracing::info!(
        target: "diag::kiosk_token",
        ip = %client_ip,
        "ISSUED observer JWT for kiosk"
    );

    match eck_core::auth::create_token("kiosk", "observer", "localhost", &state.jwt_secret) {
        Ok(token) => (StatusCode::OK, Json(json!({
            "success": true,
            "token": token,
            "user": { "id": "kiosk", "username": "Kiosk Observer", "name": "Kiosk Observer", "role": "observer" }
        }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
    }
}

pub async fn get_kiosk_config(
    Extension(_claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let config: Option<serde_json::Value> = state
        .db
        .query("SELECT * FROM system_config:kiosk")
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    match config {
        Some(v) => (StatusCode::OK, Json(v)),
        None => (StatusCode::OK, Json(json!({ "id": "system_config:kiosk", "enabled": false }))),
    }
}

// ─── dashboard SLA config ──────────────────────────────────────────────────
// system_config:dashboard_sla is mesh-synced (the system_config table is in
// SYNC_ENTITY_TYPES), so the same scale applies on every operator's browser
// regardless of which node serves the dashboard. Defaults: 7-day "soft"
// aging scale, red reserved for manual/AI escalation only (not time-based).

pub async fn get_dashboard_sla(
    Extension(_claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let config: Option<serde_json::Value> = state
        .db
        .query("SELECT * FROM system_config:dashboard_sla")
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    let mut out = config.unwrap_or_else(|| json!({ "id": "system_config:dashboard_sla" }));
    // Defaults applied at read-time so the API is always populated even if
    // the row was created with a partial set (forward-compat).
    if out.get("aging_scale_days").and_then(|v| v.as_f64()).is_none() {
        out["aging_scale_days"] = json!(7.0);
    }
    if out.get("repair_aging_scale_days").and_then(|v| v.as_f64()).is_none() {
        out["repair_aging_scale_days"] = json!(7.0);
    }
    (StatusCode::OK, Json(out))
}

#[derive(Deserialize)]
pub struct DashboardSlaRequest {
    pub aging_scale_days: Option<f64>,
    pub repair_aging_scale_days: Option<f64>,
}

pub async fn set_dashboard_sla(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<DashboardSlaRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if claims.role != "admin" {
        return (StatusCode::FORBIDDEN, Json(json!({ "success": false, "error": "Admin required" })));
    }
    // Clamp to a sane range: a 0-day scale would divide-by-zero on the
    // client; > 60 days is past the point where coloring is useful.
    let clamp = |v: f64| v.clamp(0.5, 60.0);

    let current: Option<serde_json::Value> = state
        .db
        .query("SELECT aging_scale_days, repair_aging_scale_days FROM system_config:dashboard_sla")
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    let cur_ticket = current
        .as_ref()
        .and_then(|v| v.get("aging_scale_days")?.as_f64())
        .unwrap_or(7.0);
    let cur_repair = current
        .as_ref()
        .and_then(|v| v.get("repair_aging_scale_days")?.as_f64())
        .unwrap_or(7.0);

    let new_ticket = clamp(body.aging_scale_days.unwrap_or(cur_ticket));
    let new_repair = clamp(body.repair_aging_scale_days.unwrap_or(cur_repair));

    let result = state
        .db
        .query(
            "UPSERT system_config:dashboard_sla MERGE { \
                aging_scale_days: $ticket, \
                repair_aging_scale_days: $repair, \
                updated_at: time::now() \
            }",
        )
        .bind(("ticket", new_ticket))
        .bind(("repair", new_repair))
        .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "aging_scale_days": new_ticket,
                "repair_aging_scale_days": new_repair,
            })),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
    }
}

#[derive(Deserialize)]
pub struct KioskConfigRequest {
    pub enabled: bool,
}

pub async fn set_kiosk_config(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<KioskConfigRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if claims.role != "admin" {
        return (StatusCode::FORBIDDEN, Json(json!({ "success": false, "error": "Admin required" })));
    }

    // UPSERT, not UPDATE. SurrealDB v3 `UPDATE record:id` on a non-existent
    // record is a silent no-op (query succeeds, zero rows affected, no row
    // created). The first time the operator enables kiosk auto-login the
    // record doesn't exist yet, so UPDATE returned OK while leaving the DB
    // untouched — every subsequent `/api/auth/kiosk-token` then read NULL
    // for `enabled` and refused to issue the observer token. UPSERT creates
    // the row when missing, updates it when present.
    let result = state
        .db
        .query("UPSERT system_config:kiosk SET enabled = $enabled, updated_at = time::now()")
        .bind(("enabled", body.enabled))
        .await;

    match result {
        Ok(_) => (StatusCode::OK, Json(json!({ "success": true, "enabled": body.enabled }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
    }
}
