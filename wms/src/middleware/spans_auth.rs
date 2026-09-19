//! Auth layer for `POST /api/pii/spans` (the "eck masker" serving endpoint).
//!
//! The spans route serves LOCAL German NER to remote callers. Two kinds of
//! caller are legitimate:
//! * a **dashboard operator / browser** holding a WMS JWT (the original
//!   behavior — the route used to sit behind `middleware::auth::auth_middleware`);
//! * a **weak mesh node** (the kiosk) that has no dashboard account but pays for
//!   the mesh — it authenticates with its PAID LICENSE in `Authorization:
//!   Bearer <license>`, EXACTLY as it does for `POST /api/ai/or`.
//!
//! This middleware accepts EITHER and rejects (401) neither-valid. The license
//! check is not re-implemented here: it calls the same
//! [`crate::handlers::or_proxy::verify_paid_license`] seam the OpenRouter proxy
//! uses, so both routes share one verification and one set of rejections. No
//! request text is logged here; the handler emits the served-line.

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

/// Which credential authorized a `/pii/spans` request. Inserted into the request
/// extensions by [`spans_auth_middleware`] so the handler can log the auth mode
/// (and, for a license, the client id prefix) without re-doing the check.
#[derive(Clone, Debug)]
pub enum SpansAuth {
    /// A valid dashboard JWT (the existing operator / browser path).
    Jwt,
    /// A valid PAID mesh license; carries the license client id (`sub`).
    License(String),
}

/// Pure auth decision for `/pii/spans`: a valid dashboard JWT wins; otherwise a
/// verified paid license authorizes as that client; otherwise `None`
/// (unauthorized). Factored out of the middleware — no headers, no crypto — so
/// the precedence is unit-testable without minting tokens.
pub fn decide_spans_auth(jwt_ok: bool, license_client: Option<String>) -> Option<SpansAuth> {
    if jwt_ok {
        Some(SpansAuth::Jwt)
    } else {
        license_client.map(SpansAuth::License)
    }
}

/// Route layer for `POST /api/pii/spans`. Accepts a valid dashboard JWT OR a
/// valid paid mesh license bearer; 401 on neither. On success the resolved
/// [`SpansAuth`] is placed in the request extensions for the handler to log.
pub async fn spans_auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // Try the dashboard JWT first (cheap HMAC verify); fall back to the paid
    // license on the SAME seam the OpenRouter proxy uses. Never leak which one
    // failed — the response is a single 401 either way.
    let jwt_ok = !token.is_empty()
        && eck_core::auth::validate_token(&token, &state.jwt_secret).is_ok();
    let license_client = if jwt_ok {
        None
    } else {
        crate::handlers::or_proxy::verify_paid_license(&token)
            .ok()
            .map(|c| c.sub)
    };

    match decide_spans_auth(jwt_ok, license_client) {
        Some(auth) => {
            req.extensions_mut().insert(auth);
            Ok(next.run(req).await)
        }
        None => Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": "spans requires a valid dashboard token or paid mesh license"
            })),
        )
            .into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_takes_precedence() {
        // A valid JWT authorizes as Jwt even if a license was also parseable.
        assert!(matches!(
            decide_spans_auth(true, Some("client-x".into())),
            Some(SpansAuth::Jwt)
        ));
        assert!(matches!(decide_spans_auth(true, None), Some(SpansAuth::Jwt)));
    }

    #[test]
    fn license_authorizes_when_no_jwt() {
        match decide_spans_auth(false, Some("kiosk-532e".into())) {
            Some(SpansAuth::License(c)) => assert_eq!(c, "kiosk-532e"),
            other => panic!("expected License, got {other:?}"),
        }
    }

    #[test]
    fn neither_is_unauthorized() {
        assert!(decide_spans_auth(false, None).is_none());
    }
}
