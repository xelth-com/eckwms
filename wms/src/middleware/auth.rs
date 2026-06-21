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
            if claims.role == "observer" {
                let method = req.method().clone();
                let path = req.uri().path().to_string();
                // Observer is intentionally allowed to POST /X/approve — that
                // is the local kiosk operator approving an inbound xelixir
                // session request, which is a deliberate security action.
                if matches!(method, axum::http::Method::POST | axum::http::Method::PUT | axum::http::Method::DELETE)
                    && !path.starts_with("/api/auth/")
                    && path != "/X/approve"
                {
                    return Err((
                        StatusCode::FORBIDDEN,
                        Json(json!({ "success": false, "error": "Observer role cannot perform mutations", "code": "OBSERVER_FORBIDDEN" })),
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
