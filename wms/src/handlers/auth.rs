use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use eck_core::auth::Claims;
use eck_core::db::SurrealDb;

use crate::AppState;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Check if any REAL users exist in the mesh `db`; if not, create a temporary
/// setup admin in the node-local `users_db` (Zone 1 — deliberately NOT meshed:
/// each install's bootstrap credentials stay on that node only).
/// Returns the plaintext password if a setup account was created/exists.
pub async fn seed_setup_account(db: &SurrealDb, users_db: &SurrealDb) -> Option<String> {
    // Count real (mesh) users — the setup row never lives in `db`.
    let count: i64 = db
        .query("SELECT count() AS c FROM user WHERE deleted_at IS NONE GROUP ALL")
        .await
        .ok()?
        .take::<Option<serde_json::Value>>(0)
        .ok()
        .flatten()
        .and_then(|v| v.get("c")?.as_i64())
        .unwrap_or(0);

    let setup_exists: Option<serde_json::Value> = users_db
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
    if setup_exists.is_some() && count > 0 {
        let _ = users_db
            .query("DELETE FROM user WHERE email = 'admin@setup.local'")
            .await;
        info!("Setup account removed — real users exist.");
        return None;
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
        let _ = users_db
            .query("UPDATE user SET password = $hash, updatedAt = $now WHERE email = 'admin@setup.local'")
            .bind(("hash", hash))
            .bind(("now", chrono::Utc::now()))
            .await;
        return Some(password);
    }

    // Create new setup account via SurrealQL (node-local users_db only)
    let result = users_db
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

/// Remove the setup account once a real admin is created. Real accounts are
/// counted in the mesh `db`; the setup row itself lives in `users_db`.
pub async fn cleanup_setup_account(state: &AppState) {
    let real_count: i64 = state.db
        .query("SELECT count() AS c FROM user WHERE deleted_at IS NONE GROUP ALL")
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

/// Migrate real accounts from the legacy node-local `users_db` into the mesh
/// `user` table (deterministic record id = username, so independently-seeded
/// nodes converge per-account via the sync engine's LWW instead of
/// duplicating). The install-time setup-admin is excluded — it stays
/// node-local by design. Idempotent: a mesh row that is at least as new
/// (by `updatedAt`) is left untouched, so repeated boots no-op. Source rows
/// are kept in users_db as an inert backup; nothing reads them afterwards.
pub async fn migrate_users_to_mesh(
    db: &SurrealDb,
    users_db: &SurrealDb,
    instance_id: &str,
) -> anyhow::Result<usize> {
    // Soft-deleted rows migrate too: deleted_at is content and must propagate.
    let rows: Vec<serde_json::Value> = users_db
        .query("SELECT *, record::id(id) AS __leaf FROM user WHERE email != 'admin@setup.local'")
        .await?
        .take(0)?;

    let parse_ts = |v: &serde_json::Value| -> chrono::DateTime<chrono::Utc> {
        v.get("updatedAt")
            .and_then(|t| t.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
    };

    let mut migrated = 0usize;
    for mut row in rows {
        let username = row
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if username.is_empty() {
            tracing::warn!("users_db migration: skipping row without username");
            continue;
        }

        let existing: Vec<serde_json::Value> = db
            .query("SELECT updatedAt, _vclock FROM user WHERE record::id(id) = $eid LIMIT 1")
            .bind(("eid", username.clone()))
            .await?
            .take(0)?;
        let existing = existing.into_iter().next();

        if let Some(ex) = &existing {
            if parse_ts(ex) >= parse_ts(&row) {
                continue; // mesh copy is at least as new — keep it
            }
        }

        // Stamp causality: advance THIS node's component over whatever the
        // mesh row carries, so the migrated content propagates to peers.
        let next_vc = eck_core::sync::conflict::next_local_vclock(
            existing.as_ref().and_then(|e| e.get("_vclock")),
            instance_id,
        );
        if let Some(obj) = row.as_object_mut() {
            obj.remove("id");
            obj.remove("__leaf");
            obj.insert("_vclock".into(), next_vc);
        }
        let _: Option<serde_json::Value> = db.upsert(("user", username.as_str())).content(row).await?;
        migrated += 1;
    }
    Ok(migrated)
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
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<LoginRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Brute-force guard (per socket-peer IP; direct-port topology = peer is the
    // real client). Replaces the nginx limit_req dropped fleet-wide.
    if let Err(retry) = eck_core::ratelimit::auth_limiter().check(&addr.ip().to_string()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "success": false, "error": "Too many login attempts", "retry_after": retry })),
        );
    }
    // Select specific fields to avoid SurrealDB Thing → Value deserialization issues.
    // Real accounts live in the mesh `db`; the node-local `users_db` holds only the
    // install-time setup-admin, so it is consulted as a fallback on a miss.
    const LOGIN_QUERY: &str = "SELECT record::id(id) AS user_id, username, password, email, name, role, pin, isActive, preferredLanguage, languages, mustChangePassword FROM user WHERE (username = $username OR email = $username) AND isActive = true AND deleted_at IS NONE LIMIT 1";
    let result: Result<Option<serde_json::Value>, _> = state
        .db
        .query(LOGIN_QUERY)
        .bind(("username", body.username.clone()))
        .await
        .and_then(|mut r| r.take(0));

    let user = match result {
        Ok(Some(u)) => u,
        Ok(None) => {
            let fallback: Result<Option<serde_json::Value>, _> = state
                .users_db
                .query(LOGIN_QUERY)
                .bind(("username", body.username.clone()))
                .await
                .and_then(|mut r| r.take(0));
            match fallback {
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
            }
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
    let preferred_language = user
        .get("preferredLanguage")
        .and_then(|v| v.as_str())
        .unwrap_or("en");
    let languages = user.get("languages").cloned().unwrap_or(Value::Null);
    let must_change_password = user
        .get("mustChangePassword")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match eck_core::auth::create_token(&user_id, role, "password", &state.jwt_secret) {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "token": token,
                "mustChangePassword": must_change_password,
                "user": {
                    "id": user_id,
                    "username": username,
                    "name": name,
                    "role": role,
                    "preferredLanguage": preferred_language,
                    "languages": languages,
                    "mustChangePassword": must_change_password,
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
    const ME_QUERY: &str = "SELECT record::id(id) AS user_id, record::id(id) AS id, username, email, name, role, isActive, preferredLanguage, languages, mustChangePassword FROM user WHERE record::id(id) = $uid AND deleted_at IS NONE LIMIT 1";
    let result: Result<Option<serde_json::Value>, _> = state
        .db
        .query(ME_QUERY)
        .bind(("uid", claims.sub.clone()))
        .await
        .and_then(|mut r| r.take(0));

    match result {
        Ok(Some(user)) => (StatusCode::OK, Json(user)),
        Ok(None) => {
            // Setup-admin sessions resolve against the node-local users_db.
            let fallback: Result<Option<serde_json::Value>, _> = state
                .users_db
                .query(ME_QUERY)
                .bind(("uid", claims.sub.clone()))
                .await
                .and_then(|mut r| r.take(0));
            match fallback {
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
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    #[serde(rename = "oldPassword")]
    pub old_password: String,
    #[serde(rename = "newPassword")]
    pub new_password: String,
}

/// Minimum acceptable new-password length. Deliberately modest — the accounts
/// are staff, not internet-facing — but blocks trivially short secrets.
const MIN_PASSWORD_LEN: usize = 8;

/// POST /api/auth/change-password — a user changes THEIR OWN password.
/// Auth'd (acts on `claims.sub`, never a caller-supplied id), verifies the
/// current password, enforces a length floor, and clears `mustChangePassword`.
pub async fn change_password(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChangePasswordRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if body.new_password.chars().count() < MIN_PASSWORD_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": format!("New password must be at least {MIN_PASSWORD_LEN} characters") })),
        );
    }
    if body.new_password == body.old_password {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "New password must differ from the current one" })),
        );
    }

    // Load the caller's own row by their token subject (mesh user table; the
    // setup-admin never changes its password through this endpoint).
    let row: Result<Option<serde_json::Value>, _> = state
        .db
        .query("SELECT record::id(id) AS user_id, password FROM user WHERE record::id(id) = $uid AND deleted_at IS NONE LIMIT 1")
        .bind(("uid", claims.sub.clone()))
        .await
        .and_then(|mut r| r.take(0));
    let user = match row {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "success": false, "error": "User not found" })),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e.to_string() })),
            );
        }
    };

    let current_hash = user.get("password").and_then(|v| v.as_str()).unwrap_or("");
    if !eck_core::auth::verify_password(&body.old_password, current_hash).unwrap_or(false) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": "Current password is incorrect" })),
        );
    }

    let new_hash = match eck_core::auth::hash_password(&body.new_password) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e.to_string() })),
            );
        }
    };

    let update = state
        .db
        .query("UPDATE user SET password = $hash, mustChangePassword = false, updatedAt = time::now() WHERE record::id(id) = $uid")
        .bind(("hash", new_hash))
        .bind(("uid", claims.sub.clone()))
        .await;
    match update {
        Ok(_) => {
            // Advance causality so the new hash propagates across the mesh
            // instead of losing "local wins" against a peer's stale copy.
            if let Err(e) = eck_core::sync::conflict::bump_local_vclock_by_leaf(
                &state.db, "user", &claims.sub, &state.instance_id,
            ).await {
                tracing::warn!("password change: vclock bump failed for {}: {}", claims.sub, e);
            }
            info!("password changed for user {}", claims.sub);
            (StatusCode::OK, Json(json!({ "success": true })))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

fn is_local_ip(ip: &str) -> bool {
    ip == "127.0.0.1" || ip == "::1" || ip == "::ffff:127.0.0.1"
}

fn extract_client_ip(headers: &HeaderMap, connect_info: &ConnectInfo<std::net::SocketAddr>) -> String {
    // Only trust proxy headers when the direct peer IS the local proxy (nginx
    // on this host). WMS also listens on direct public ports, where the remote
    // client controls these headers — trusting them unconditionally let anyone
    // send `X-Real-IP: 127.0.0.1` and mint a kiosk observer token remotely.
    let socket_ip = connect_info.0.ip();
    if socket_ip.is_loopback() {
        if let Some(real_ip) = headers.get("X-Real-IP").and_then(|v| v.to_str().ok()) {
            return real_ip.to_string();
        }
        if let Some(xff) = headers.get("X-Forwarded-For").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                return first.trim().to_string();
            }
        }
    }
    socket_ip.to_string()
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

    // Public-exhibition mode (ECK_PUBLIC_OBSERVER=1, demo nodes only): EVERY
    // anonymous visitor gets an observer session — the env var alone is the
    // gate, deliberately independent of system_config:kiosk (that row may be
    // mesh-synced; an env line in the unit cannot leak to other nodes). Mode
    // is forced to "wms" so a stray kiosk mode="pos" can't bounce visitors
    // into the register, which rejects observer tokens anyway.
    let public_observer = std::env::var("ECK_PUBLIC_OBSERVER")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if public_observer {
        tracing::info!(
            target: "diag::kiosk_token",
            ip = %client_ip,
            ua_short = ua.chars().take(40).collect::<String>(),
            "ISSUED observer JWT (public exhibition mode)"
        );
        return match eck_core::auth::create_token("gast", "observer", "localhost", &state.jwt_secret) {
            Ok(token) => (StatusCode::OK, Json(json!({
                "success": true,
                "token": token,
                "mode": "wms",
                "user": { "id": "gast", "username": "Gast", "name": "Gast", "role": "observer", "preferredLanguage": "en", "languages": [] }
            }))),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
        };
    }

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
        .query("SELECT enabled, mode FROM system_config:kiosk")
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

    // Boot mode "pos": this device is a register, not an observer dashboard.
    // No token is minted — cashiers authenticate on the POS PIN pad; the
    // login shell sees `mode` and redirects to /K/ instead.
    let mode = enabled
        .as_ref()
        .and_then(|v| v.get("mode")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "wms".into());
    if mode == "pos" {
        tracing::info!(
            target: "diag::kiosk_token",
            ip = %client_ip,
            "kiosk boot mode = pos — redirecting shell to the register"
        );
        return (StatusCode::OK, Json(json!({ "success": true, "mode": "pos" })));
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
            "mode": "wms",
            "user": { "id": "kiosk", "username": "Kiosk Observer", "name": "Kiosk Observer", "role": "observer", "preferredLanguage": "en", "languages": [] }
        }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
    }
}

/// GET /api/auth/device-challenge — issue a single-use nonce for replay-proof
/// device registration/auth. The device signs `{deviceId,devicePublicKey,nonce}`
/// with its Ed25519 key and POSTs it to /api/internal/register-device; the server
/// verifies the signature AND consumes the nonce (see device::register_device_core).
///
/// Public (no JWT): it's part of the bootstrap/auth handshake, and the nonce is
/// worthless without the device's private key. Issuance + storage + pruning live
/// in `device::issue_device_challenge` so the NAT'd relay-pairing path (the
/// `device_challenge` mesh-task) shares the exact same logic + TTL.
pub async fn device_challenge(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<serde_json::Value>) {
    match crate::handlers::device::issue_device_challenge(&state.db).await {
        Ok(nonce) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "nonce": nonce,
                "expires_in": crate::handlers::device::DEVICE_CHALLENGE_TTL_SECS,
            })),
        ),
        Err(e) => {
            tracing::error!("device_challenge: failed to issue nonce: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": "could not issue challenge" })),
            )
        }
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
    if out.get("trip_fade_days").and_then(|v| v.as_f64()).is_none() {
        // Days over which a vehicle's trip line fades to nothing on the map.
        out["trip_fade_days"] = json!(3.0);
    }
    (StatusCode::OK, Json(out))
}

#[derive(Deserialize)]
pub struct DashboardSlaRequest {
    pub aging_scale_days: Option<f64>,
    pub repair_aging_scale_days: Option<f64>,
    pub trip_fade_days: Option<f64>,
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
        .query("SELECT aging_scale_days, repair_aging_scale_days, trip_fade_days FROM system_config:dashboard_sla")
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
    let cur_trip_fade = current
        .as_ref()
        .and_then(|v| v.get("trip_fade_days")?.as_f64())
        .unwrap_or(3.0);

    let new_ticket = clamp(body.aging_scale_days.unwrap_or(cur_ticket));
    let new_repair = clamp(body.repair_aging_scale_days.unwrap_or(cur_repair));
    let new_trip_fade = clamp(body.trip_fade_days.unwrap_or(cur_trip_fade));

    let result = state
        .db
        .query(
            "UPSERT system_config:dashboard_sla MERGE { \
                aging_scale_days: $ticket, \
                repair_aging_scale_days: $repair, \
                trip_fade_days: $trip_fade, \
                updated_at: time::now() \
            }",
        )
        .bind(("ticket", new_ticket))
        .bind(("repair", new_repair))
        .bind(("trip_fade", new_trip_fade))
        .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "aging_scale_days": new_ticket,
                "repair_aging_scale_days": new_repair,
                "trip_fade_days": new_trip_fade,
            })),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
    }
}

#[derive(Deserialize)]
pub struct KioskConfigRequest {
    pub enabled: bool,
    /// Boot mode for the kiosk device: "wms" (observer dashboard, default)
    /// or "pos" (boot straight into the register at /K/).
    #[serde(default)]
    pub mode: Option<String>,
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
    let mode = match body.mode.as_deref() {
        None => None,
        Some("wms") | Some("pos") => body.mode.clone(),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": format!("Invalid kiosk mode '{}': expected 'wms' or 'pos'", other) })),
            );
        }
    };

    let result = if let Some(ref m) = mode {
        state
            .db
            .query("UPSERT system_config:kiosk SET enabled = $enabled, mode = $mode, updated_at = time::now()")
            .bind(("enabled", body.enabled))
            .bind(("mode", m.clone()))
            .await
    } else {
        // Mode omitted → leave the stored mode untouched (older clients).
        state
            .db
            .query("UPSERT system_config:kiosk SET enabled = $enabled, updated_at = time::now()")
            .bind(("enabled", body.enabled))
            .await
    };

    match result {
        Ok(_) => (StatusCode::OK, Json(json!({ "success": true, "enabled": body.enabled, "mode": mode }))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "success": false, "error": e.to_string() }))),
    }
}
