use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::AppState;

/// Authentication middleware for P2P mesh endpoints (server-to-server).
///
/// Unlike `auth_middleware` (which validates JWTs for browser/frontend clients),
/// this validates the `SYNC_SECRET` shared token used by `MeshClient` on peer
/// nodes. If `sync_secret` is `None` (dev mode), all requests are allowed.
pub async fn mesh_auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let Some(ref expected) = state.sync_secret else {
        // Dev mode — no secret configured, allow all P2P requests
        return Ok(next.run(req).await);
    };

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": "Missing mesh auth token" })),
            )
                .into_response());
        }
    };

    if token != expected {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": "Invalid mesh auth token" })),
        )
            .into_response());
    }

    Ok(next.run(req).await)
}
