//! Personal browser MCP sessions.
//!
//! An employee with no AI agent logs in ONCE inside the shim's Mesh Chat page
//! with their WMS username+password; the node mints a personal MCP bearer and
//! remembers only its `sha256` in the node-local `mcp_session` table (Zone 1 —
//! never mesh-synced, like the setup-admin). Subsequent `/mcp` calls carry that
//! bearer plus a per-install `X-Eck-Client` id; [`authenticate_session`] maps
//! them back to a tier. The password is never stored, logged, or echoed.
//!
//! The bearer is one-way: only its hash is persisted, so a DB read cannot
//! recover a usable token, and a stolen bearer is refused on a different install
//! (the `client_hash` binding). Sessions expire after `ECK_MCP_SESSION_DAYS`
//! (default 30) and can be revoked (logout, or the admin cut-off list).

use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{header, HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::handlers::auth::{verify_credentials, CredCheck};
use crate::AppState;

use super::McpTier;

/// Default session lifetime when `ECK_MCP_SESSION_DAYS` is unset/invalid.
const DEFAULT_SESSION_DAYS: i64 = 30;
/// Sweep grace: expired/revoked rows are deleted once they are this old.
const SWEEP_GRACE_SECS: i64 = 7 * 86_400;

/// Lowercase hex `sha256` of a string. Used for BOTH the bearer id (so only the
/// hash is stored) and the client-binding hash.
pub(crate) fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let out = h.finalize();
    let mut hex = String::with_capacity(64);
    for b in out {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Configured session lifetime in seconds (floored at 1 day so a typo can't mint
/// instantly-dead sessions).
fn session_ttl_secs() -> i64 {
    let days = std::env::var("ECK_MCP_SESSION_DAYS")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_SESSION_DAYS);
    days.max(1) * 86_400
}

// ─── pure auth decision (unit-tested) ───────────────────────────────────────

/// The minimal row [`decide`] reasons over.
#[derive(Debug, Clone)]
pub(crate) struct SessionRow {
    pub tier: String,
    pub client_hash: String,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
}

/// The verdict for one bearer lookup.
#[derive(Debug, PartialEq)]
pub(crate) enum SessionDecision {
    /// Usable — carries the tier string ("master" / "agent").
    Valid,
    /// No such session id.
    NotFound,
    /// Past `expires_at`.
    Expired,
    /// Explicitly revoked (logout / admin cut-off).
    Revoked,
    /// Bearer presented from a different install than it was minted on.
    ClientMismatch,
}

/// Pure gate over a fetched row: revocation, then expiry, then client binding.
/// No DB, no clock — the caller supplies `now` and the presented client hash so
/// this is fully unit-testable.
pub(crate) fn decide(row: Option<&SessionRow>, now: i64, provided_client_hash: &str) -> SessionDecision {
    let Some(r) = row else {
        return SessionDecision::NotFound;
    };
    if r.revoked_at.is_some() {
        return SessionDecision::Revoked;
    }
    if r.expires_at <= now {
        return SessionDecision::Expired;
    }
    if r.client_hash != provided_client_hash {
        return SessionDecision::ClientMismatch;
    }
    SessionDecision::Valid
}

/// What [`authenticate_session`] hands back to the MCP `authenticate()` gate.
pub(crate) enum SessionAuth {
    /// A valid session at this tier.
    Tier(McpTier),
    /// Found, but presented from a different install → a distinct 401 message.
    ClientMismatch,
    /// No usable session (absent / expired / revoked) → generic 401.
    None,
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Resolve a session bearer to a tier. Looks up `sha256(bearer)` in the
/// node-local table, applies [`decide`] against `sha256(client_header)`, and on
/// success bumps `last_used_at` fire-and-forget. The bearer itself is only ever
/// hashed here — never logged.
pub(crate) async fn authenticate_session(
    state: &Arc<AppState>,
    bearer: &str,
    client_header: &str,
) -> SessionAuth {
    let id = sha256_hex(bearer);
    let row: Option<SessionRow> = fetch_row(state, &id).await;
    let provided = sha256_hex(client_header);

    match decide(row.as_ref(), now_ts(), &provided) {
        SessionDecision::Valid => {
            let r = row.expect("Valid implies Some");
            // Fire-and-forget last-used bump — never block the request on it.
            let db = state.users_db.clone();
            let idc = id.clone();
            let now = now_ts();
            tokio::spawn(async move {
                let _ = db
                    .query("UPDATE type::record('mcp_session', $id) SET last_used_at = $now")
                    .bind(("id", idc))
                    .bind(("now", now))
                    .await;
            });
            debug!("[mcp] session auth user=<hidden> tier={}", r.tier);
            SessionAuth::Tier(if r.tier == "master" {
                McpTier::Master
            } else {
                McpTier::Agent
            })
        }
        SessionDecision::ClientMismatch => SessionAuth::ClientMismatch,
        _ => SessionAuth::None,
    }
}

/// Fetch one session row by its hashed id from the node-local `users_db`.
async fn fetch_row(state: &Arc<AppState>, id: &str) -> Option<SessionRow> {
    let res: Result<Option<Value>, _> = state
        .users_db
        .query("SELECT username, tier, client_hash, expires_at, revoked_at FROM type::record('mcp_session', $id)")
        .bind(("id", id.to_string()))
        .await
        .and_then(|mut r| r.take(0));
    let v = res.ok().flatten()?;
    Some(SessionRow {
        tier: v.get("tier").and_then(|x| x.as_str()).unwrap_or("agent").to_string(),
        client_hash: v.get("client_hash").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        expires_at: v.get("expires_at").and_then(|x| x.as_i64()).unwrap_or(0),
        revoked_at: v.get("revoked_at").and_then(|x| x.as_i64()),
    })
}

// ─── mint / login (POST /api/auth/mcp-session) ──────────────────────────────

#[derive(Deserialize)]
pub struct McpSessionRequest {
    pub username: String,
    pub password: String,
}

/// The `X-Eck-Client` header value, or "" when absent.
fn client_header(headers: &HeaderMap) -> String {
    headers
        .get("x-eck-client")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// POST /api/auth/mcp-session — verify WMS credentials (SAME path as
/// `/api/auth/login`) and mint a personal MCP bearer. Body `{username, password}`,
/// header `X-Eck-Client` binds the session to the caller's install. The password
/// is never stored/logged; only `sha256(bearer)` is persisted.
pub async fn mcp_session_login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<McpSessionRequest>,
) -> (StatusCode, Json<Value>) {
    // Same brute-force guard as /api/auth/login.
    if let Err(retry) = eck_core::ratelimit::auth_limiter().check(&addr.ip().to_string()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "Too many login attempts", "retry_after": retry })),
        );
    }

    let user = match verify_credentials(&state, &body.username, &body.password).await {
        CredCheck::Ok(u) => *u,
        CredCheck::Invalid => {
            return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Invalid credentials" })));
        }
        CredCheck::Error(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e })));
        }
    };

    let must_change = user.get("mustChangePassword").and_then(|v| v.as_bool()).unwrap_or(false);
    if must_change {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "password change required — sign in to the web app and change your password first"
            })),
        );
    }

    let username = user.get("username").and_then(|v| v.as_str()).unwrap_or(&body.username).to_string();
    let role = user.get("role").and_then(|v| v.as_str()).unwrap_or("operator");
    // A read-only observer (kiosk/exhibition) gets NO agent tool surface — the
    // MCP tools include physical printing (`print_labels`) and other operator
    // actions. dispatch_tool sees only the McpTier (master/agent), never the
    // role, so the observer gate has to live here at mint time: refuse to issue
    // an MCP bearer to an observer account at all.
    if role == "observer" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "observer role has no MCP agent surface" })),
        );
    }
    // Same fence as auth_middleware's POS rule: a cashier works the register
    // and has no WMS surface, so it must not mint an Agent-tier bearer either
    // (MCP dispatch never passes through that middleware — review 2026-09-30,
    // F10-7/F11-3). Only admin and operator accounts get MCP sessions.
    if role != "admin" && role != "operator" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("role '{role}' has no MCP agent surface") })),
        );
    }
    let tier = if role == "admin" { "master" } else { "agent" };

    // 32 random bytes → base64url. Only its hash is persisted.
    let bearer = {
        use base64::Engine;
        use rand::RngCore;
        let mut buf = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut buf);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
    };
    let id = sha256_hex(&bearer);
    // The binding is only worth something if the client actually identifies
    // itself: an empty header would mint a session bound to sha256("") that any
    // header-less caller could replay. Refuse instead.
    let client_id = client_header(&headers);
    if client_id.len() < 16 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "X-Eck-Client header (>=16 chars) required" })),
        );
    }
    let client_hash = sha256_hex(&client_id);

    let now = now_ts();
    let expires_at = now + session_ttl_secs();

    let created: Result<Vec<Value>, _> = state
        .users_db
        .query(
            "CREATE type::record('mcp_session', $id) CONTENT {\
             username: $username, tier: $tier, client_hash: $client_hash,\
             created_at: $now, expires_at: $exp, last_used_at: $now, revoked_at: NONE }",
        )
        .bind(("id", id))
        .bind(("username", username.clone()))
        .bind(("tier", tier.to_string()))
        .bind(("client_hash", client_hash))
        .bind(("now", now))
        .bind(("exp", expires_at))
        .await
        .and_then(|mut r| r.take(0));

    if let Err(e) = created {
        // Never echo the raw DB error to the caller (F9); log it, return generic.
        debug!("[mcp] session mint DB error: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "could not create session" })),
        );
    }

    debug!("[mcp] session minted user=<hidden> tier={tier}");
    (
        StatusCode::OK,
        Json(json!({
            "bearer": bearer,
            "tier": tier,
            "username": username,
            "expires_at": expires_at,
        })),
    )
}

/// POST /api/auth/mcp-session/logout — revoke the presented session bearer
/// (Authorization: Bearer …). Idempotent.
pub async fn mcp_session_logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if bearer.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "missing bearer" })));
    }
    let id = sha256_hex(&bearer);
    let now = now_ts();
    let _ = state
        .users_db
        .query("UPDATE type::record('mcp_session', $id) SET revoked_at = $now")
        .bind(("id", id))
        .bind(("now", now))
        .await;
    (StatusCode::OK, Json(json!({ "ok": true })))
}

// ─── admin (require_admin layer) ────────────────────────────────────────────

/// GET /api/admin/mcp-sessions — active sessions (never expired/revoked). Only
/// hash PREFIXES are exposed (the full bearer is unrecoverable regardless).
pub async fn admin_list_sessions(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let now = now_ts();
    let rows: Result<Vec<Value>, _> = state
        .users_db
        .query(
            "SELECT record::id(id) AS id, username, tier, client_hash, created_at, expires_at, last_used_at \
             FROM mcp_session WHERE revoked_at IS NONE AND expires_at > $now ORDER BY created_at DESC",
        )
        .bind(("now", now))
        .await
        .and_then(|mut r| r.take(0));

    match rows {
        Ok(list) => {
            let out: Vec<Value> = list
                .into_iter()
                .map(|r| {
                    let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let ch = r.get("client_hash").and_then(|v| v.as_str()).unwrap_or("");
                    json!({
                        "id_prefix": id.chars().take(12).collect::<String>(),
                        "username": r.get("username").cloned().unwrap_or(Value::Null),
                        "tier": r.get("tier").cloned().unwrap_or(Value::Null),
                        "created_at": r.get("created_at").cloned().unwrap_or(Value::Null),
                        "expires_at": r.get("expires_at").cloned().unwrap_or(Value::Null),
                        "last_used_at": r.get("last_used_at").cloned().unwrap_or(Value::Null),
                        "client_hash_prefix": ch.chars().take(12).collect::<String>(),
                    })
                })
                .collect();
            (StatusCode::OK, Json(json!({ "sessions": out })))
        }
        Err(e) => {
            debug!("[mcp] admin_list_sessions DB error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "could not list sessions" })),
            )
        }
    }
}

/// DELETE /api/admin/mcp-sessions/:id — revoke a session by its id (or an
/// unambiguous id prefix, e.g. the value the list shows). Lost-laptop cut-off.
pub async fn admin_revoke_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let id = id.trim().to_string();
    if id.len() < 6 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "id (prefix) too short" })));
    }
    let now = now_ts();
    let updated: Result<Vec<Value>, _> = state
        .users_db
        .query(
            "UPDATE mcp_session SET revoked_at = $now \
             WHERE revoked_at IS NONE AND string::starts_with(record::id(id), $id) RETURN AFTER",
        )
        .bind(("id", id))
        .bind(("now", now))
        .await
        .and_then(|mut r| r.take(0));
    match updated {
        Ok(list) => (StatusCode::OK, Json(json!({ "revoked": list.len() }))),
        Err(e) => {
            debug!("[mcp] admin_revoke_session DB error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "could not revoke session" })),
            )
        }
    }
}

// ─── hourly sweep hook ──────────────────────────────────────────────────────

/// Delete expired/revoked sessions once they are older than the grace window.
/// Called from the node's hourly maintenance loop.
pub async fn sweep_expired(users_db: &eck_core::db::SurrealDb) {
    let cutoff = now_ts() - SWEEP_GRACE_SECS;
    let _ = users_db
        .query(
            "DELETE mcp_session WHERE expires_at < $cutoff \
             OR (revoked_at IS NOT NONE AND revoked_at < $cutoff)",
        )
        .bind(("cutoff", cutoff))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(expires_at: i64, revoked_at: Option<i64>, client_hash: &str) -> SessionRow {
        SessionRow {
            tier: "agent".into(),
            client_hash: client_hash.into(),
            expires_at,
            revoked_at,
        }
    }

    #[test]
    fn decide_valid_when_fresh_and_client_matches() {
        let r = row(1000, None, "chash");
        assert_eq!(decide(Some(&r), 500, "chash"), SessionDecision::Valid);
    }

    #[test]
    fn decide_expired_at_or_after_expiry() {
        let r = row(1000, None, "chash");
        assert_eq!(decide(Some(&r), 1000, "chash"), SessionDecision::Expired);
        assert_eq!(decide(Some(&r), 1500, "chash"), SessionDecision::Expired);
    }

    #[test]
    fn decide_revoked_beats_everything() {
        let r = row(9999, Some(10), "chash");
        // Even fresh + matching client: a revoked row is refused.
        assert_eq!(decide(Some(&r), 500, "chash"), SessionDecision::Revoked);
    }

    #[test]
    fn decide_client_mismatch() {
        let r = row(1000, None, "minted-on-A");
        assert_eq!(decide(Some(&r), 500, "presented-from-B"), SessionDecision::ClientMismatch);
    }

    #[test]
    fn decide_not_found() {
        assert_eq!(decide(None, 500, "chash"), SessionDecision::NotFound);
    }

    #[test]
    fn stored_id_is_hash_not_plaintext() {
        let bearer = "super-secret-bearer-value";
        let h = sha256_hex(bearer);
        assert_ne!(h, bearer);
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        // Deterministic.
        assert_eq!(h, sha256_hex(bearer));
    }
}
