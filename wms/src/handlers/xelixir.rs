//! Backend endpoints for the on-demand xelixir C2 microservice.
//!
//! Routes mount under the `/X/` prefix (NOT `/api/`):
//!
//! * `GET /X/config`           — read local `system_config:xelixir`
//! * `POST /X/config`          — admin updates local `system_config:xelixir`
//! * `POST /X/approve`         — local kiosk operator approves a pending session
//! * `POST /X/devices/:id/start|stop`
//!     — cloud admin issues a command targeting another node; the
//!       command is signed and routed via [`xelixir_router::dispatch`]
//!       (cross-mesh: relay-resolved direct POST, with relay-queue
//!       fallback for NAT'd targets). NO local DB mutation, NO mesh sync.
//! * `POST /X/self/start|stop`
//!     — inter-node receive endpoints. Body is a `SignedEnvelope`. The
//!       signer's pubkey must be in `XELIXIR_ADMIN_PUBKEYS`. Replay
//!       protection (timestamp + nonce) is handled in the router for the
//!       poll-based path; for direct POST we trust the timestamp window.
//! * `POST /X/internal/dispatch`
//!     — server-initiated activation: `xelixir.service` (or any sibling
//!       service on the same trust zone) tells this cloud WMS to dispatch
//!       a command to a target node. Authed by `X-Xelixir-Service-Token`
//!       header against `XELIXIR_SERVICE_TOKEN` env. Body:
//!       `{"target_uuid": "...", "command": "start"|"stop"}`. Delegates to
//!       [`xelixir_router::dispatch`] — the WMS signs with *its own*
//!       identity, so the target's `XELIXIR_ADMIN_PUBKEYS` doesn't need
//!       to know about xelixir.service at all.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use eck_core::auth::Claims;
use eck_core::xelixir::envelope::{
    read_admin_pubkeys_from_env, SignedEnvelope, DEFAULT_MAX_AGE_SECS,
};

use crate::services::xelixir_router;
use crate::AppState;

// ─── GET /X/config ─────────────────────────────────────────────────────────

pub async fn get_config(
    Extension(_claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    let cfg: Option<Value> = state
        .db
        .query("SELECT auto_start, auto_accept, updated_at FROM system_config:xelixir")
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    match cfg {
        Some(v) => (StatusCode::OK, Json(v)),
        None => (
            StatusCode::OK,
            Json(json!({
                "id": "system_config:xelixir",
                "auto_start": true,
                "auto_accept": true
            })),
        ),
    }
}

// ─── POST /X/config ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct XelixirConfigRequest {
    pub auto_start: Option<bool>,
    pub auto_accept: Option<bool>,
}

pub async fn set_config(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<XelixirConfigRequest>,
) -> (StatusCode, Json<Value>) {
    if claims.role != "admin" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "Admin required" })),
        );
    }

    let current: Option<Value> = state
        .db
        .query("SELECT auto_start, auto_accept FROM system_config:xelixir")
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();

    let cur_auto_start = current
        .as_ref()
        .and_then(|v| v.get("auto_start")?.as_bool())
        .unwrap_or(true);
    let cur_auto_accept = current
        .as_ref()
        .and_then(|v| v.get("auto_accept")?.as_bool())
        .unwrap_or(true);

    let new_auto_start = body.auto_start.unwrap_or(cur_auto_start);
    let new_auto_accept = body.auto_accept.unwrap_or(cur_auto_accept);

    let now = chrono::Utc::now().to_rfc3339();
    let res = state
        .db
        .query(
            "UPSERT system_config:xelixir MERGE { \
                auto_start: $auto_start, \
                auto_accept: $auto_accept, \
                updated_at: $now \
            };",
        )
        .bind(("auto_start", new_auto_start))
        .bind(("auto_accept", new_auto_accept))
        .bind(("now", now))
        .await;

    match res {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "auto_start": new_auto_start,
                "auto_accept": new_auto_accept
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e.to_string() })),
        ),
    }
}

// ─── POST /X/approve ───────────────────────────────────────────────────────
// Local kiosk operator (any authenticated role, incl. observer) approves a
// pending xelixir request — bypasses `auto_accept`.

pub async fn approve(
    Extension(_claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    match state.agent_controller.approve().await {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "xelixir_status": "running",
                "session_url": format!(
                    "{}?token={}",
                    std::env::var("XELTH_SESSION_BASE")
                        .unwrap_or_else(|_| "".into()),
                    token
                )
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

// ─── POST /X/devices/:id/start|stop ────────────────────────────────────────
// Cloud admin: sign + route a command to the target node via
// `xelixir_router::dispatch`. Cross-mesh, NAT-friendly.

pub async fn start_device(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if claims.role != "admin" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "Admin required" })),
        );
    }
    dispatch_response(&state, &id, "start").await
}

pub async fn stop_device(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if claims.role != "admin" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "Admin required" })),
        );
    }
    dispatch_response(&state, &id, "stop").await
}

async fn dispatch_response(
    state: &Arc<AppState>,
    target_uuid: &str,
    command: &str,
) -> (StatusCode, Json<Value>) {
    match xelixir_router::dispatch(state, target_uuid, command).await {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::to_value(result).unwrap_or(json!({"success": true}))),
        ),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

// ─── POST /X/self/start|stop ───────────────────────────────────────────────
// Inter-node endpoints. Body is `SignedEnvelope`. Verified locally against
// the env allow-list. NOT JWT-gated — they must be reachable by any peer
// that holds an authorised signing key.

pub async fn self_start(
    State(state): State<Arc<AppState>>,
    Json(signed): Json<SignedEnvelope>,
) -> (StatusCode, Json<Value>) {
    handle_self_envelope(&state, signed, "start").await
}

pub async fn self_stop(
    State(state): State<Arc<AppState>>,
    Json(signed): Json<SignedEnvelope>,
) -> (StatusCode, Json<Value>) {
    handle_self_envelope(&state, signed, "stop").await
}

async fn handle_self_envelope(
    state: &Arc<AppState>,
    signed: SignedEnvelope,
    expected_command: &str,
) -> (StatusCode, Json<Value>) {
    if signed.envelope.command != expected_command {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("envelope command '{}' != endpoint '{}'", signed.envelope.command, expected_command)
            })),
        );
    }
    let allowed = read_admin_pubkeys_from_env();
    let fleet_root = eck_core::xelixir::envelope::read_fleet_root_from_env();
    if allowed.is_empty() && fleet_root.is_none() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "neither XELIXIR_ADMIN_PUBKEYS nor ECK_FLEET_ROOT_PUBKEY is configured on this node"
            })),
        );
    }
    if let Err(e) = signed.verify(&state.instance_id, &allowed, fleet_root.as_deref(), DEFAULT_MAX_AGE_SECS) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": e })),
        );
    }
    match xelixir_router::execute_local(state, expected_command).await {
        Ok(body) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "xelixir_status": body.get("xelixir_status"),
                "xelixir_session_url": body.get("xelixir_session_url"),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

// ─── POST /X/internal/dispatch ─────────────────────────────────────────────
// Server-initiated activation channel for sibling services (xelixir.service
// is the main caller). The route-level `require_service_token` middleware
// has already verified the token by the time we get here.

/// Request shape — accepts either the legacy `{target_uuid, command}`
/// for agent start/stop, or the new `{target_uuid, verb, args}` for
/// cross-mesh ops dispatch. The two are mutually exclusive on the wire.
#[derive(Deserialize)]
pub struct InternalDispatchRequest {
    pub target_uuid: String,
    /// Legacy field: `"start"` or `"stop"`. Mutually exclusive with `verb`.
    #[serde(default)]
    pub command: Option<String>,
    /// New field: ops verb name (e.g., `"deploy"`, `"journal"`).
    /// Mutually exclusive with `command`. Routes through `dispatch_ops`.
    #[serde(default)]
    pub verb: Option<String>,
    /// Args for the ops verb. Defaults to `{}` when omitted.
    #[serde(default)]
    pub args: Option<Value>,
}

pub async fn internal_dispatch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InternalDispatchRequest>,
) -> (StatusCode, Json<Value>) {
    match (body.command.as_deref(), body.verb.as_deref()) {
        (Some(cmd), None) => {
            if cmd != "start" && cmd != "stop" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "success": false,
                        "error": format!("unsupported command '{}'", cmd)
                    })),
                );
            }
            match xelixir_router::dispatch(&state, &body.target_uuid, cmd).await {
                Ok(result) => (
                    StatusCode::OK,
                    Json(serde_json::to_value(result).unwrap_or(json!({"success": true}))),
                ),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "success": false, "error": e })),
                ),
            }
        }
        (None, Some(verb)) => {
            let args = body.args.unwrap_or_else(|| json!({}));
            match xelixir_router::dispatch_ops(&state, &body.target_uuid, verb, args).await {
                Ok(result) => (
                    StatusCode::OK,
                    Json(serde_json::to_value(result).unwrap_or(json!({"success": true}))),
                ),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "success": false, "error": e })),
                ),
            }
        }
        (Some(_), Some(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "specify exactly one of `command` or `verb`, not both"
            })),
        ),
        (None, None) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "must specify either `command` (start/stop) or `verb` (ops.*)"
            })),
        ),
    }
}

// ─── GET /X/internal/result/:task_id ───────────────────────────────────────
// Proxy to the relay's /E/x/result/<task_id>. Lets xelixir.service poll
// for the outcome of a queued cross-mesh dispatch without holding a token
// to the relay directly. The relay is co-located on antigravity, but
// going through us means the service-token middleware audits the read.

pub async fn internal_result(
    Path(task_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let relay_url = std::env::var("RELAY_URL")
        .unwrap_or_else(|_| "http://localhost:3200".into())
        .trim_end_matches('/')
        .to_string();
    let url = format!("{}/E/x/result/{}", relay_url, task_id);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e.to_string() })),
            );
        }
    };
    match client.get(&url).send().await {
        Ok(r) => {
            let status = r.status();
            match r.json::<Value>().await {
                Ok(b) => (
                    StatusCode::from_u16(status.as_u16())
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    Json(b),
                ),
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "success": false, "error": format!("relay body: {}", e) })),
                ),
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "success": false, "error": format!("relay reach: {}", e) })),
        ),
    }
}
