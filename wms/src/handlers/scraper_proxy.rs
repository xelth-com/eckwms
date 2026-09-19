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

/// True if the scraper answers its /health endpoint.
pub async fn scraper_healthy() -> bool {
    if let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        if let Ok(resp) = client.get(format!("{}/health", scraper_base())).send().await {
            return resp.status().is_success();
        }
    }
    false
}

/// Health-check the scraper and spawn it when it's down. Called from the
/// scheduler at the top of every sync cycle so an edge node survives reboots
/// and scraper crashes without a manual `POST /api/scraper/start`.
pub async fn ensure_scraper_running() -> bool {
    if scraper_healthy().await {
        return true;
    }
    let res = spawn_scraper().await;
    if res.get("success").and_then(|v| v.as_bool()) != Some(true) {
        tracing::warn!("[Scraper] auto-start failed: {}", res);
        return false;
    }
    // Give node a moment to bind before the caller fires requests at it.
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if scraper_healthy().await {
            return true;
        }
    }
    tracing::warn!("[Scraper] auto-start: process spawned but /health never came up");
    false
}

/// POST /api/scraper/start — spawn Node.js scraper as detached background process
pub async fn start_scraper() -> impl IntoResponse {
    if scraper_healthy().await {
        return Json(json!({ "success": true, "message": "Scraper is already running" }));
    }
    Json(spawn_scraper().await)
}

/// Spawn the Node.js scraper detached, without a prior health check.
async fn spawn_scraper() -> serde_json::Value {
    let scraper_dir = std::env::var("SCRAPER_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // Try ../eckwms/scraper (legacy sibling project), then ./scraper
            let sibling = std::env::current_dir().unwrap_or_default()
                .parent().unwrap_or(std::path::Path::new(".")).join("eckwms").join("scraper");
            if sibling.join("server.js").exists() {
                sibling
            } else {
                std::env::current_dir().unwrap_or_default().join("scraper")
            }
        });

    if !scraper_dir.join("server.js").exists() {
        return json!({
            "success": false,
            "error": format!("scraper/server.js not found at {}", scraper_dir.display())
        });
    }

    // Keep the scraper's console in a rotating pair of files next to it —
    // provider diagnostics ([DHL] diag etc.) are unreadable otherwise. Fresh
    // truncation per spawn; the previous life is kept as *.1 for post-mortems.
    let (out_log, err_log) = (scraper_dir.join("scraper.out.log"), scraper_dir.join("scraper.err.log"));
    for f in [&out_log, &err_log] {
        let _ = std::fs::rename(f, f.with_extension("log.1"));
    }
    let out_file = std::fs::File::create(&out_log).ok();
    let err_file = std::fs::File::create(&err_log).ok();
    let stdio = |f: &Option<std::fs::File>| {
        f.as_ref()
            .and_then(|f| f.try_clone().ok())
            .map(std::process::Stdio::from)
            .unwrap_or_else(std::process::Stdio::null)
    };

    // JS runtime: bun by default (faster start, lower RSS — it matters on the
    // 3 GB kiosk), node as the fallback when bun isn't installed. Override with
    // SCRAPER_RUNTIME. Both run scraper/server.js unchanged; Playwright is
    // supported on either.
    let runtime = std::env::var("SCRAPER_RUNTIME").unwrap_or_else(|_| "bun".to_string());
    let mut candidates = vec![runtime.clone()];
    if runtime != "node" {
        candidates.push("node".to_string());
    }
    let mut last_err = String::new();
    for rt in &candidates {
        let (out, err) = (stdio(&out_file), stdio(&err_file));
        match tokio::process::Command::new(rt)
            .arg("server.js")
            .current_dir(&scraper_dir)
            .env("PORT", scraper_port())
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .kill_on_drop(false)
            .spawn()
        {
            Ok(mut child) => {
                let pid = child.id().unwrap_or(0);
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
                tracing::info!("[Scraper] Started scraper process (pid={}, runtime={})", pid, rt);
                return json!({
                    "success": true,
                    "message": "Scraper process started",
                    "pid": pid,
                    "runtime": rt
                });
            }
            Err(e) => {
                last_err = format!("{}: {}", rt, e);
                tracing::warn!("[Scraper] Failed to spawn {}: {}", rt, e);
            }
        }
    }
    tracing::error!("[Scraper] Failed to start: {}", last_err);
    json!({ "success": false, "error": format!("Failed to spawn scraper: {}", last_err) })
}

/// Reverse proxy: forwards /S/* requests to the Node.js scraper on port 38211.
/// Strips /S prefix: /S/debug → http://127.0.0.1:38211/debug
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>, StatusCode> {
    // Validate JWT (state.jwt_secret — NOT a separate env read, which fell
    // back to "" and disagreed with the secret the rest of the app signs with)
    let token = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let claims = eck_core::auth::validate_token(token, &state.jwt_secret)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Mirror auth_middleware's observer rule: this proxy bypasses that layer,
    // and the scraper's POST routes trigger logins/scrapes (mutations).
    if claims.role == "observer" && method != Method::GET {
        return Err(StatusCode::FORBIDDEN);
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

    // The scraper may have exited on idle (SCRAPER_IDLE_EXIT_MIN) — a proxied
    // request is a spawn trigger just like the scheduler cycle, so an unreachable
    // upstream buys one restart plus one retry before it becomes a 502.
    let retry = req_builder.try_clone();
    let upstream = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("[ScraperProxy] Upstream unreachable ({}), (re)starting the scraper", e);
            let retry = retry.ok_or(StatusCode::BAD_GATEWAY)?;
            if !ensure_scraper_running().await {
                return Err(StatusCode::BAD_GATEWAY);
            }
            retry.send().await.map_err(|e| {
                tracing::error!("[ScraperProxy] Upstream still unreachable after restart: {}", e);
                StatusCode::BAD_GATEWAY
            })?
        }
    };

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
