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

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let token = match auth_header {
        Some(ref h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": "Missing or invalid Authorization header" })),
            )
                .into_response());
        }
    };

    match eck_core::auth::validate_token(token, &state.jwt_secret) {
        Ok(claims) => {
            // NOTE on paths: this middleware runs INSIDE `.nest()`ed routers,
            // where axum strips the mount prefix from `req.uri()` — a request
            // to /api/auth/me arrives here as /auth/me, /X/approve as
            // /approve. Match both forms.
            if claims.role == "observer" {
                let method = req.method().clone();
                let path = req.uri().path().to_string();
                // Observer is intentionally allowed to POST /X/approve — that
                // is the local kiosk operator approving an inbound xelixir
                // session request, which is a deliberate security action.
                if matches!(method, axum::http::Method::POST | axum::http::Method::PUT | axum::http::Method::DELETE)
                    && !path.starts_with("/api/auth/")
                    && !path.starts_with("/auth/")
                    && path != "/X/approve"
                    && path != "/approve"
                {
                    return Err((
                        StatusCode::FORBIDDEN,
                        Json(json!({ "success": false, "error": "Observer role cannot perform mutations", "code": "OBSERVER_FORBIDDEN" })),
                    )
                        .into_response());
                }
            }
            // POS credentials stay in the POS. Cashiers work the register
            // (/K/*, which has its own middleware) and get no WMS surface
            // beyond auth self-service; PIN-minted tokens are weaker than
            // passwords, so they don't unlock the WMS for anyone.
            if claims.role == "cashier" || claims.auth_method == "pin" {
                let path = req.uri().path();
                if !path.starts_with("/auth/")
                    && !path.starts_with("/api/auth/")
                    && !path.starts_with("/E/api/auth/")
                {
                    return Err((
                        StatusCode::FORBIDDEN,
                        Json(json!({ "success": false, "error": "POS credentials cannot access the WMS API", "code": "POS_ONLY_FORBIDDEN" })),
                    )
                        .into_response());
                }
            }
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(_) => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": "Invalid or expired token" })),
        )
            .into_response()),
    }
}
