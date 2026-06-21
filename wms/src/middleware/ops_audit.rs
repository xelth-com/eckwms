//! Audit-log middleware for `/X/ops/*` endpoints.
//!
//! Every `/X/ops/<verb>` request — independent of outcome — writes a row
//! to the Zone 2 `ops_audit_log` table after the handler returns. Captures:
//!
//!   - `verb`        — extracted from the URL path
//!   - `status`      — HTTP status code returned by the handler
//!   - `duration_ms` — wall time from request entry to response
//!   - `created_at`  — wall-clock timestamp
//!   - `request_ip`  — best-effort source IP (usually `127.0.0.1` for
//!                     localhost sibling calls)
//!
//! Body content is NOT captured to keep PII out of the audit log even
//! when verbs eventually touch borderline fields. The verb name + IP +
//! timestamp + outcome are enough to reconstruct what happened later.
//!
//! Failure to write the audit row is logged but does NOT block the
//! response — the user-visible request must always complete.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use tracing::warn;

use crate::AppState;

pub async fn ops_audit_middleware(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    // axum's `.nest("/X", router)` strips the /X prefix before the request
    // reaches our router, so the path here looks like `/ops/<verb>` (no
    // leading /X). Extract the verb from either form to be robust.
    let path = req.uri().path();
    let verb = path
        .strip_prefix("/X/ops/")
        .or_else(|| path.strip_prefix("/ops/"))
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let ip = addr.ip().to_string();

    let response = next.run(req).await;
    let status = response.status().as_u16();
    let duration_ms = started.elapsed().as_millis() as u64;
    let now = chrono::Utc::now().to_rfc3339();

    let db = state.db.clone();
    tokio::spawn(async move {
        let res = db
            .query(
                "INSERT INTO ops_audit_log { \
                    verb: $verb, \
                    status: $status, \
                    duration_ms: $duration, \
                    request_ip: $ip, \
                    created_at: $now \
                }",
            )
            .bind(("verb", verb))
            .bind(("status", status as i64))
            .bind(("duration", duration_ms as i64))
            .bind(("ip", ip))
            .bind(("now", now))
            .await;
        if let Err(e) = res {
            warn!("ops_audit_log insert failed: {}", e);
        }
    });

    response
}
