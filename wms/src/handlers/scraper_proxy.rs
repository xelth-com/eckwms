use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::sync::Arc;

use crate::AppState;

/// Local Node.js scraper proxy port. Configurable via `SCRAPER_PORT` so it can
/// be moved off a conflicting port without a rebuild. Default is an unusual
/// high port to avoid collisions. Single source of truth for every caller
/// (scheduler, support backfills, this proxy).
pub fn scraper_port() -> String {
    std::env::var("SCRAPER_PORT").unwrap_or_else(|_| "38211".to_string())
}

/// Base URL of the local scraper proxy, e.g. `http://127.0.0.1:38211`.
pub fn scraper_base() -> String {
    format!("http://127.0.0.1:{}", scraper_port())
}

/// POST /api/scraper/start — spawn Node.js scraper as detached background process
pub async fn start_scraper() -> impl IntoResponse {
    // Check if already running
    if let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        if let Ok(resp) = client.get(format!("{}/health", scraper_base())).send().await {
            if resp.status().is_success() {
                return Json(json!({ "success": true, "message": "Scraper is already running" }));
            }
        }
    }

    let scraper_dir = std::env::var("SCRAPER_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // Try ../eckwmsr/scraper (sibling project), then ./scraper
            let sibling = std::env::current_dir().unwrap_or_default()
                .parent().unwrap_or(std::path::Path::new(".")).join("eckwmsr").join("scraper");
            if sibling.join("server.js").exists() {
                sibling
            } else {
                std::env::current_dir().unwrap_or_default().join("scraper")
            }
        });

    if !scraper_dir.join("server.js").exists() {
        return Json(json!({
            "success": false,
            "error": format!("scraper/server.js not found at {}", scraper_dir.display())
        }));
    }

    match tokio::process::Command::new("node")
        .arg("server.js")
        .current_dir(&scraper_dir)
        .env("PORT", scraper_port())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false)
        .spawn()
    {
        Ok(mut child) => {
            let pid = child.id().unwrap_or(0);
            tokio::spawn(async move { let _ = child.wait().await; });
            tracing::info!("[Scraper] Started scraper process (pid={})", pid);
            Json(json!({ "success": true, "message": "Scraper process started", "pid": pid }))
        }
        Err(e) => {
            tracing::error!("[Scraper] Failed to start: {}", e);
            Json(json!({ "success": false, "error": format!("Failed to spawn node: {}", e) }))
        }
    }
}

/// Reverse proxy: forwards /S/* requests to the Node.js scraper on port 3211.
/// Strips /S prefix: /S/debug → http://127.0.0.1:3211/debug
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>, StatusCode> {
    // Validate JWT
    let token = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_default();
    if eck_core::auth::validate_token(token, &jwt_secret).is_err() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Strip /S prefix
    let path = uri.path();
    let stripped = path.strip_prefix("/S").unwrap_or(path);
    let stripped = if stripped.is_empty() { "/" } else { stripped };
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let target_url = format!("{}{}{}", scraper_base(), stripped, query);

    tracing::debug!("[ScraperProxy] {} {} → {}", method, path, target_url);

    let body_bytes = axum::body::to_bytes(body, 10 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut req_builder = client.request(req_method, &target_url);

    for (name, value) in headers.iter() {
        let n = name.as_str();
        if matches!(n, "host" | "connection" | "transfer-encoding" | "upgrade" | "authorization") {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req_builder = req_builder.header(n, v);
        }
    }

    if !body_bytes.is_empty() {
        req_builder = req_builder.body(body_bytes.to_vec());
    }

    let upstream = req_builder.send().await.map_err(|e| {
        tracing::error!("[ScraperProxy] Upstream unreachable: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = Response::builder().status(status);

    for (name, value) in upstream.headers().iter() {
        let n = name.as_str();
        if matches!(n, "transfer-encoding" | "connection") {
            continue;
        }
        builder = builder.header(n, value.as_bytes());
    }

    let resp_bytes = upstream.bytes().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    builder
        .body(Body::from(resp_bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
