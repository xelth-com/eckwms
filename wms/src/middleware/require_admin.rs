use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Role gate for admin-sensitive routes. Runs INSIDE `auth_middleware` (which
/// validates the JWT and inserts `Claims`), so by the time this executes the
/// caller is authenticated — this only checks the role.
///
/// Before this layer existed the only role distinction was observer-vs-rest:
/// any operator JWT could create admin users (privilege escalation), restore
/// DB backups, mint device pair-codes, etc. Route-level gating beats
/// per-handler checks because a new admin route added tomorrow is protected
/// by where it's mounted, not by remembering an `if` in its handler.
pub async fn require_admin_middleware(
    req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let is_admin = req
        .extensions()
        .get::<eck_core::auth::Claims>()
        .map(|c| c.role == "admin")
        .unwrap_or(false);

    if !is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "Admin role required", "code": "ADMIN_ONLY" })),
        )
            .into_response());
    }

    Ok(next.run(req).await)
}
