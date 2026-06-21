//! Service-token middleware for `/X/internal/*` and `/X/ops/*`.
//!
//! These endpoints are not JWT-gated — they're called by sibling
//! services (xelixir.service) and by xelixir's autonomous-ops loop.
//! A shared bearer token in `XELIXIR_SERVICE_TOKEN` env authenticates
//! every call; comparison is constant-time so partial-match timing
//! does not leak.
//!
//! Mount with `axum_mw::from_fn(require_service_token)`.

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

const SERVICE_TOKEN_HEADER: &str = "x-xelixir-service-token";

pub async fn require_service_token(headers: HeaderMap, req: Request, next: Next) -> Response {
    let expected = match std::env::var("XELIXIR_SERVICE_TOKEN") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "success": false,
                    "error": "XELIXIR_SERVICE_TOKEN is not configured on this node"
                })),
            )
                .into_response();
        }
    };
    let presented = headers
        .get(SERVICE_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "invalid service token" })),
        )
            .into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
