//! Generic relay `api_call` tunnel (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §12).
//!
//! An off-LAN PDA cannot reach its home node's `/api/*` HTTP surface directly.
//! Rather than add one typed mesh-task per endpoint, the phone tunnels a whole
//! HTTP request over the relay mesh-queue as a single `api_call` task:
//!
//! ```json
//! { "token": "<device JWT>", "method": "GET|POST|PUT", "path": "/api/...",
//!   "query": { "k": "v" }, "body": { ... }, "request_id": "<client uuid>" }
//! ```
//!
//! The node validates the device JWT (the relay authenticates nobody — the JWT
//! is the only credential, exactly like `trip_upload` / `image_upload`), runs a
//! least-privilege allow-list + path-normalisation + size/time pre-checks, then
//! routes the request through THIS node's own axum `Router` with
//! `tower::ServiceExt::oneshot`. The synthetic request is given a **loopback**
//! `ConnectInfo` and carries ONLY `Authorization`, `Content-Type` and
//! `X-Request-Id` — never `X-Forwarded-For` / `X-Real-IP` / any caller-chosen
//! header — so the relay payload can never pick the request's apparent IP. The
//! inner route's own auth + active-device middleware run as over the LAN.
//!
//! Ack separates transport from HTTP:
//! - transport refusal: `{ok:false, error, request_id?}`
//! - routed (incl. inner 4xx/5xx): `{ok:true, status:<u16>, body:<json|string>, request_id}`
//!
//! The relay delivers at-least-once and the PDA retries, so a bounded in-memory
//! cache keyed on `(token subject, request_id)` returns the prior ack without
//! re-executing — the POST idempotency guarantee. Keying on the subject means a
//! device cannot read another device's cached ack by guessing a `request_id`.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{header, Method, Request},
    Router,
};
use serde_json::{json, Map, Value};
use tower::ServiceExt; // oneshot
use tracing::info;

/// Max serialized request body accepted for a tunnelled call (pre-routing).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Per-call wall-clock budget for the inner route (ack status 504 on timeout).
const TIMEOUT_SECS: u64 = 20;
/// Hard ceiling for reading the inner response into memory. Well above the
/// relay ack budget (default 1.5 MB) so the exact byte count can be reported in
/// the `response_too_large` ack; a body larger than this is refused as 502.
const MAX_RESP_READ: usize = 8 * 1024 * 1024;
/// Replay cache sizing.
const CACHE_CAP: usize = 512;
const CACHE_TTL_SECS: u64 = 600;

/// The ONE place the tunnel surface is defined (least privilege — audit/extend
/// here). A request is tunnelled only if its normalised path starts with one of
/// these prefixes AND its method is in that prefix's list. Everything else —
/// `/api/admin`, `/api/auth`, `/api/internal`, `/api/upload` (multipart stays on
/// `image_upload`), `/api/files`, `/api/mcp`, and any path not listed — is
/// refused with HTTP 403 `{error:"path_not_tunnelled"}`.
const TUNNELLED: &[(&str, &[Method])] = &[
    ("/api/repairs/board", &[Method::GET, Method::POST]),
    ("/api/receiving/", &[Method::GET, Method::POST]),
    ("/api/entity/", &[Method::GET, Method::POST]),
    ("/api/scan", &[Method::GET, Method::POST]),
    ("/api/ai/ocr-hints", &[Method::GET, Method::POST]),
    ("/api/pda/", &[Method::GET, Method::POST]),
    ("/api/rma/", &[Method::GET]),
    ("/api/screens", &[Method::GET]),
];

/// Process-wide replay cache used by the live poller. Tests construct their own
/// `ReplayCache` so they never share state with the poller or each other.
pub fn global_cache() -> &'static ReplayCache {
    static CACHE: OnceLock<ReplayCache> = OnceLock::new();
    CACHE.get_or_init(|| ReplayCache::new(CACHE_CAP, Duration::from_secs(CACHE_TTL_SECS)))
}

// ─── Replay cache ─────────────────────────────────────────────────────────────

struct CacheInner {
    map: HashMap<String, (Instant, Value)>,
    order: VecDeque<String>,
}

/// Bounded, TTL'd `(subject \0 request_id) -> ack` cache. A hit returns the
/// stored ack WITHOUT re-executing the inner route (POST idempotency under the
/// relay's at-least-once redelivery + PDA retries).
pub struct ReplayCache {
    inner: Mutex<CacheInner>,
    cap: usize,
    ttl: Duration,
}

impl ReplayCache {
    pub fn new(cap: usize, ttl: Duration) -> Self {
        ReplayCache {
            inner: Mutex::new(CacheInner { map: HashMap::new(), order: VecDeque::new() }),
            cap: cap.max(1),
            ttl,
        }
    }

    fn get(&self, key: &str) -> Option<Value> {
        let mut g = self.inner.lock().unwrap();
        let hit = match g.map.get(key) {
            Some((t, v)) if t.elapsed() <= self.ttl => Some(v.clone()),
            Some(_) => None, // expired
            None => return None,
        };
        if hit.is_none() {
            g.map.remove(key);
        }
        hit
    }

    fn put(&self, key: String, val: Value) {
        let mut g = self.inner.lock().unwrap();
        let CacheInner { map, order } = &mut *g;
        if map.insert(key.clone(), (Instant::now(), val)).is_none() {
            order.push_back(key);
        }
        while order.len() > self.cap {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
    }
}

// ─── Ack builders ─────────────────────────────────────────────────────────────

fn transport_err(msg: &str, request_id: Option<&str>) -> Value {
    match request_id {
        Some(id) => json!({"ok": false, "error": msg, "request_id": id}),
        None => json!({"ok": false, "error": msg}),
    }
}

/// Routed ack — HTTP 4xx/5xx from the inner route are STILL `ok:true` with that
/// status (the PDA mirrors HTTP semantics); `ok:false` is reserved for a
/// transport-level failure that never reached (or never left) a route.
fn http_ack(status: u16, body: Value, request_id: &str) -> Value {
    json!({"ok": true, "status": status, "body": body, "request_id": request_id})
}

// ─── Pure validators ──────────────────────────────────────────────────────────

/// Only GET/POST/PUT are legal methods; DELETE and anything else → `None`
/// (caller acks 405). The allow-list further restricts which methods reach which
/// prefix.
fn parse_method(s: &str) -> Option<Method> {
    match s.trim().to_ascii_uppercase().as_str() {
        "GET" => Some(Method::GET),
        "POST" => Some(Method::POST),
        "PUT" => Some(Method::PUT),
        _ => None,
    }
}

/// Reject traversal / smuggling before the path ever reaches the router. Must
/// start with `/api/`; no `..`, `//`, backslash, NUL, `?`/`#`, or percent-encoded
/// dot/slash/backslash/NUL (which would defeat the literal checks).
fn normalize_path(raw: &str) -> Result<String, &'static str> {
    let p = raw.trim();
    if p.is_empty() {
        return Err("empty path");
    }
    if !p.starts_with("/api/") {
        return Err("path must start with /api/");
    }
    if p.contains('\0') {
        return Err("NUL in path");
    }
    if p.contains('\\') {
        return Err("backslash in path");
    }
    if p.contains("..") {
        return Err("dot-dot in path");
    }
    if p.contains("//") {
        return Err("double slash in path");
    }
    if p.contains('?') || p.contains('#') {
        return Err("query or fragment in path");
    }
    let lower = p.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") || lower.contains("%00") {
        return Err("percent-encoded separator in path");
    }
    Ok(p.to_string())
}

/// The allow-list gate: normalised path starts with a tunnelled prefix AND the
/// method is permitted for that prefix.
fn path_allowed(method: &Method, path: &str) -> bool {
    TUNNELLED
        .iter()
        .any(|(prefix, methods)| path.starts_with(prefix) && methods.contains(method))
}

// ─── Synthetic request assembly ────────────────────────────────────────────────

fn build_uri(path: &str, query: Option<&Map<String, Value>>) -> String {
    match query {
        Some(q) if !q.is_empty() => {
            let mut parts: Vec<String> = Vec::with_capacity(q.len());
            for (k, v) in q {
                let val = match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                parts.push(format!(
                    "{}={}",
                    urlencoding::encode(k),
                    urlencoding::encode(&val)
                ));
            }
            format!("{path}?{}", parts.join("&"))
        }
        _ => path.to_string(),
    }
}

/// Build the synthetic HTTP request. Forwards ONLY `Authorization: Bearer
/// <token>`, `Content-Type: application/json` (when a body is present) and
/// `X-Request-Id`. The request is stamped with a LOOPBACK `ConnectInfo` and NO
/// proxy headers, so it presents as a local origin and the relay payload can
/// never choose the apparent client IP (there is no field through which an
/// `X-Forwarded-For` / `X-Real-IP` could be injected).
fn build_request(
    method: &Method,
    path: &str,
    query: Option<&Map<String, Value>>,
    body: Option<&Value>,
    token: &str,
    request_id: &str,
) -> Request<Body> {
    let uri = build_uri(path, query);
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("X-Request-Id", request_id);
    let body = match body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(v).unwrap_or_default())
        }
        None => Body::empty(),
    };
    let mut req = builder.body(body).expect("synthetic request build");
    // Loopback origin — no caller-chosen IP. Port 0 is fine; nothing dials back.
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    req
}

fn log_call(method: &Method, path: &str, status: u16, ms: u128, request_id: &str) {
    // path WITHOUT query values; request_id truncated to a prefix; never the
    // token or any body.
    let rid: String = request_id.chars().take(8).collect();
    info!(
        target: "api_tunnel",
        %method, path = %path, status, ms = %ms, request_id = %rid,
        "api_call"
    );
}

// ─── Execution ──────────────────────────────────────────────────────────────

/// Route the synthetic request and encode the HTTP ack. `budget` is the
/// serialized-ack ceiling (`relay_ack_byte_budget()` in production); an ack that
/// would exceed it is downgraded to a 502 `response_too_large` so an unackable
/// ack never poisons the relay queue.
async fn route_and_ack(
    router: &Router,
    method: Method,
    path: &str,
    query: Option<&Map<String, Value>>,
    body: Option<&Value>,
    token: &str,
    request_id: &str,
    budget: usize,
) -> Value {
    let req = build_request(&method, path, query, body, token, request_id);
    let started = Instant::now();

    let resp = match tokio::time::timeout(
        Duration::from_secs(TIMEOUT_SECS),
        router.clone().oneshot(req),
    )
    .await
    {
        Ok(Ok(r)) => r,
        // Router error type is Infallible, so this arm is effectively dead.
        Ok(Err(_)) => {
            log_call(&method, path, 502, started.elapsed().as_millis(), request_id);
            return http_ack(502, json!({"error": "router_error"}), request_id);
        }
        Err(_) => {
            log_call(&method, path, 504, started.elapsed().as_millis(), request_id);
            return http_ack(504, json!({"error": "timeout"}), request_id);
        }
    };

    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let bytes = match axum::body::to_bytes(resp.into_body(), MAX_RESP_READ).await {
        Ok(b) => b,
        Err(_) => {
            log_call(&method, path, 502, started.elapsed().as_millis(), request_id);
            return http_ack(502, json!({"error": "response_too_large"}), request_id);
        }
    };

    let body_val: Value = if bytes.is_empty() {
        Value::Null
    } else if ct.starts_with("application/json") || ct.contains("+json") {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    } else if ct.starts_with("text/") || ct.is_empty() {
        // Text (or a response with no declared type) is tunnelled as a string
        // only when it is valid UTF-8; anything else is treated as binary.
        match std::str::from_utf8(&bytes) {
            Ok(s) => Value::String(s.to_string()),
            Err(_) => {
                log_call(&method, path, 415, started.elapsed().as_millis(), request_id);
                return http_ack(415, json!({"error": "binary_not_tunnelled"}), request_id);
            }
        }
    } else {
        log_call(&method, path, 415, started.elapsed().as_millis(), request_id);
        return http_ack(415, json!({"error": "binary_not_tunnelled"}), request_id);
    };

    let ack = http_ack(status, body_val, request_id);
    let serialized = serde_json::to_string(&ack).map(|s| s.len()).unwrap_or(usize::MAX);
    if serialized > budget {
        log_call(&method, path, 502, started.elapsed().as_millis(), request_id);
        return http_ack(
            502,
            json!({"error": "response_too_large", "bytes": serialized}),
            request_id,
        );
    }

    log_call(&method, path, status, started.elapsed().as_millis(), request_id);
    ack
}

// ─── Entry point ──────────────────────────────────────────────────────────────

/// Handle one `api_call` payload end to end. Production entry — reads the ack
/// byte budget from the relay env.
pub async fn tunnel_call(
    router: &Router,
    cache: &ReplayCache,
    jwt_secret: &str,
    payload: &Value,
) -> Value {
    tunnel_call_with(
        router,
        cache,
        jwt_secret,
        payload,
        crate::services::mesh_relay_poller::relay_ack_byte_budget(),
    )
    .await
}

/// Budget-injectable core (tests pass an explicit ceiling instead of touching
/// the process-wide env var).
pub async fn tunnel_call_with(
    router: &Router,
    cache: &ReplayCache,
    jwt_secret: &str,
    payload: &Value,
    budget: usize,
) -> Value {
    // request_id is MANDATORY and echoed in every ack, so it is read first.
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(request_id) = request_id else {
        return transport_err("request_id required", None);
    };

    // Device JWT (the only credential; the relay authenticates nobody). An
    // invalid/missing token is a routed-style 401 so the PDA mirrors it like the
    // HTTP path — but it is NOT cached (no trusted subject to key on).
    let token = payload.get("token").and_then(|v| v.as_str()).unwrap_or("");
    let subject = match eck_core::auth::validate_token(token, jwt_secret) {
        Ok(claims) => claims.sub,
        Err(_) => {
            return http_ack(
                401,
                json!({"error": "invalid or missing device token"}),
                request_id,
            );
        }
    };

    // Cache key includes the token subject: one device cannot read another's
    // cached ack by guessing a request_id. '\0' is not valid in either half.
    let cache_key = format!("{subject}\u{0}{request_id}");
    if let Some(cached) = cache.get(&cache_key) {
        return cached;
    }

    let ack = compute(router, &token_owned(token), request_id, payload, budget).await;
    // Cache ONLY a successful (2xx) routed ack. A 401/403/4xx/5xx, a 504/502/413/
    // 415, or any transport `{ok:false}` is deliberately NOT cached: the PDA
    // re-authenticates after a 401 and retries with the SAME request_id, and
    // that retry MUST execute for real. Only an idempotent-safe success is
    // memoised against at-least-once redelivery.
    if is_cacheable_success(&ack) {
        cache.put(cache_key, ack.clone());
    }
    ack
}

/// True only for a routed ack with a 2xx inner status. Everything else re-runs
/// on the next (same-request_id) delivery.
fn is_cacheable_success(ack: &Value) -> bool {
    ack.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
        && ack
            .get("status")
            .and_then(|v| v.as_u64())
            .map(|s| (200..=299).contains(&s))
            .unwrap_or(false)
}

// Small owned-copy helper so `compute` can be a plain async fn without borrowing
// across the payload for the whole routed future.
fn token_owned(t: &str) -> String {
    t.to_string()
}

/// Everything after the subject is known: method / body-size / path / allow-list
/// pre-checks, then route. Returns the ack to cache+return.
async fn compute(
    router: &Router,
    token: &str,
    request_id: &str,
    payload: &Value,
    budget: usize,
) -> Value {
    // Method — DELETE and anything that is not GET/POST/PUT → 405.
    let Some(method) = parse_method(payload.get("method").and_then(|v| v.as_str()).unwrap_or(""))
    else {
        return http_ack(405, json!({"error": "method_not_allowed"}), request_id);
    };

    // Body size (serialized) ≤ 256 KB.
    let body = payload.get("body").filter(|v| !v.is_null());
    if let Some(b) = body {
        let sz = serde_json::to_vec(b).map(|v| v.len()).unwrap_or(usize::MAX);
        if sz > MAX_BODY_BYTES {
            return http_ack(413, json!({"error": "body_too_large", "bytes": sz}), request_id);
        }
    }

    // Path normalisation (traversal / smuggling) then the least-privilege
    // allow-list. The inner route's own auth + active-device checks still run.
    let path = match normalize_path(payload.get("path").and_then(|v| v.as_str()).unwrap_or("")) {
        Ok(p) => p,
        Err(_) => return http_ack(400, json!({"error": "bad_path"}), request_id),
    };
    if !path_allowed(&method, &path) {
        return http_ack(403, json!({"error": "path_not_tunnelled"}), request_id);
    }

    let query = payload.get("query").and_then(|v| v.as_object());
    route_and_ack(router, method, &path, query, body, token, request_id, budget).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::middleware::{from_fn, Next};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::http::StatusCode;

    const SECRET: &str = "tunnel-test-secret";

    fn tok(role: &str) -> String {
        eck_core::auth::create_token("device-A", role, "device", SECRET).unwrap()
    }

    // ── pure validators ──

    #[test]
    fn normalize_path_rejects_traversal_and_smuggling() {
        assert!(normalize_path("/api/entity/../admin/query").is_err()); // ..
        assert!(normalize_path("/api/entity//thing").is_err()); // //
        assert!(normalize_path("/api/entity/%2e%2e/x").is_err()); // encoded dot
        assert!(normalize_path("/api/entity/%2fadmin").is_err()); // encoded slash
        assert!(normalize_path("/admin/query").is_err()); // not /api/
        assert!(normalize_path("/api/entity\\x").is_err()); // backslash
        assert!(normalize_path("/api/entity/\0x").is_err()); // NUL
        assert!(normalize_path("/api/entity?x=1").is_err()); // query in path
        assert_eq!(normalize_path("/api/entity/order/o1").unwrap(), "/api/entity/order/o1");
    }

    #[test]
    fn allow_list_accepts_and_denies() {
        // Accepted prefixes/methods.
        assert!(path_allowed(&Method::GET, "/api/repairs/board"));
        assert!(path_allowed(&Method::POST, "/api/receiving/receipts"));
        assert!(path_allowed(&Method::GET, "/api/entity/order/o1"));
        assert!(path_allowed(&Method::POST, "/api/entity/order/o1/capture"));
        assert!(path_allowed(&Method::POST, "/api/scan"));
        assert!(path_allowed(&Method::POST, "/api/ai/ocr-hints"));
        assert!(path_allowed(&Method::GET, "/api/rma/123"));
        assert!(path_allowed(&Method::GET, "/api/screens"));
        // Denied: rma/screens are GET-only; admin/auth/etc. not listed at all.
        assert!(!path_allowed(&Method::POST, "/api/rma/123"));
        assert!(!path_allowed(&Method::POST, "/api/screens"));
        assert!(!path_allowed(&Method::GET, "/api/admin/query"));
        assert!(!path_allowed(&Method::POST, "/api/auth/login"));
        assert!(!path_allowed(&Method::POST, "/api/upload/image"));
        assert!(!path_allowed(&Method::GET, "/api/files/f1"));
    }

    #[test]
    fn parse_method_rejects_delete_and_junk() {
        assert_eq!(parse_method("get"), Some(Method::GET));
        assert_eq!(parse_method("POST"), Some(Method::POST));
        assert_eq!(parse_method("put"), Some(Method::PUT));
        assert_eq!(parse_method("DELETE"), None);
        assert_eq!(parse_method("PATCH"), None);
        assert_eq!(parse_method(""), None);
    }

    #[test]
    fn build_request_forwards_only_allowed_headers_and_loopback_origin() {
        // Even a query/body that mentions proxy headers cannot inject them —
        // there is no field for arbitrary headers at all.
        let q: Map<String, Value> =
            serde_json::from_value(json!({"X-Forwarded-For": "6.6.6.6"})).unwrap();
        let body = json!({"X-Real-IP": "6.6.6.6"});
        let req = build_request(
            &Method::POST,
            "/api/scan",
            Some(&q),
            Some(&body),
            "jwt-token",
            "req-123",
        );
        assert_eq!(
            req.headers().get(header::AUTHORIZATION).unwrap(),
            "Bearer jwt-token"
        );
        assert_eq!(req.headers().get("X-Request-Id").unwrap(), "req-123");
        assert_eq!(
            req.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        // The spoofable proxy headers are NEVER present.
        assert!(req.headers().get("X-Forwarded-For").is_none());
        assert!(req.headers().get("X-Real-IP").is_none());
        // Loopback origin, payload cannot choose it.
        let ci = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .expect("connect info present");
        assert!(ci.0.ip().is_loopback());
    }

    #[test]
    fn replay_cache_put_get_ttl_and_cap() {
        let c = ReplayCache::new(2, Duration::from_secs(60));
        c.put("k1".into(), json!({"v": 1}));
        assert_eq!(c.get("k1"), Some(json!({"v": 1})));
        // Capacity eviction (cap=2): inserting a 3rd evicts the oldest.
        c.put("k2".into(), json!({"v": 2}));
        c.put("k3".into(), json!({"v": 3}));
        assert_eq!(c.get("k1"), None);
        assert_eq!(c.get("k3"), Some(json!({"v": 3})));
        // TTL expiry.
        let z = ReplayCache::new(4, Duration::ZERO);
        z.put("x".into(), json!(1));
        assert_eq!(z.get("x"), None);
    }

    // ── end-to-end through a test Router with an auth layer ──

    async fn auth_layer(req: Request<Body>, next: Next) -> axum::response::Response {
        let ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|t| eck_core::auth::validate_token(t, SECRET).is_ok())
            .unwrap_or(false);
        if ok {
            next.run(req).await
        } else {
            (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "unauthorized"}))).into_response()
        }
    }

    /// Router with a real auth layer and a counter route, so a test can prove a
    /// valid token reaches the handler and replay does not run it twice.
    fn counter_router(counter: Arc<AtomicUsize>) -> Router {
        let c = counter.clone();
        Router::new()
            .route(
                "/api/scan",
                post(move || {
                    let c = c.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"hit": true}))
                    }
                }),
            )
            // A route that returns a non-JSON binary content-type.
            .route(
                "/api/receiving/blob",
                get(|| async {
                    ([(header::CONTENT_TYPE, "application/octet-stream")], vec![0u8, 1, 2, 3])
                }),
            )
            // A route that returns a large JSON body (oversize-ack path).
            .route(
                "/api/receiving/big",
                get(|| async { axum::Json(json!({"blob": "x".repeat(4096)})) }),
            )
            .layer(from_fn(auth_layer))
    }

    fn payload(method: &str, path: &str, token: &str, request_id: &str) -> Value {
        json!({"token": token, "method": method, "path": path, "request_id": request_id})
    }

    #[tokio::test]
    async fn api_call_valid_token_reaches_route_invalid_does_not() {
        let counter = Arc::new(AtomicUsize::new(0));
        let router = counter_router(counter.clone());
        let cache = ReplayCache::new(8, Duration::from_secs(60));

        // Valid token → routed, 200, handler ran once.
        let ack = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("POST", "/api/scan", &tok("operator"), "r1"),
            1_500_000,
        )
        .await;
        assert_eq!(ack["ok"], json!(true));
        assert_eq!(ack["status"], json!(200));
        assert_eq!(ack["body"], json!({"hit": true}));
        assert_eq!(ack["request_id"], json!("r1"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Invalid token → 401 ack, handler NOT run.
        let ack = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("POST", "/api/scan", "garbage.jwt.token", "r2"),
            1_500_000,
        )
        .await;
        assert_eq!(ack["ok"], json!(true));
        assert_eq!(ack["status"], json!(401));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn api_call_replay_returns_cached_without_reexecuting() {
        let counter = Arc::new(AtomicUsize::new(0));
        let router = counter_router(counter.clone());
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        let p = payload("POST", "/api/scan", &tok("operator"), "same-req");

        let first = tunnel_call_with(&router, &cache, SECRET, &p, 1_500_000).await;
        let second = tunnel_call_with(&router, &cache, SECRET, &p, 1_500_000).await;
        assert_eq!(first, second);
        // Executed exactly once despite the duplicate request_id.
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn api_call_401_is_not_cached_valid_retry_executes_and_caches() {
        let counter = Arc::new(AtomicUsize::new(0));
        let router = counter_router(counter.clone());
        let cache = ReplayCache::new(8, Duration::from_secs(60));

        // Same request_id X: first with a bad token → 401, NOT cached.
        let bad = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("POST", "/api/scan", "garbage.jwt.token", "X"),
            1_500_000,
        )
        .await;
        assert_eq!(bad["status"], json!(401));
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // Re-authenticated retry with the SAME request_id must EXECUTE (not hit
        // a cached 401) and its 2xx ack is then cached.
        let good = payload("POST", "/api/scan", &tok("operator"), "X");
        let first = tunnel_call_with(&router, &cache, SECRET, &good, 1_500_000).await;
        assert_eq!(first["status"], json!(200));
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Now the 2xx IS cached — a third identical delivery replays without
        // re-executing.
        let replay = tunnel_call_with(&router, &cache, SECRET, &good, 1_500_000).await;
        assert_eq!(replay, first);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cache_key_isolates_subjects() {
        let counter = Arc::new(AtomicUsize::new(0));
        let router = counter_router(counter.clone());
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        // Two different subjects, SAME request_id.
        let ta = eck_core::auth::create_token("device-A", "operator", "device", SECRET).unwrap();
        let tb = eck_core::auth::create_token("device-B", "operator", "device", SECRET).unwrap();
        tunnel_call_with(&router, &cache, SECRET, &payload("POST", "/api/scan", &ta, "dup"), 1_500_000).await;
        tunnel_call_with(&router, &cache, SECRET, &payload("POST", "/api/scan", &tb, "dup"), 1_500_000).await;
        // Device B's identical request_id must NOT hit device A's cached ack —
        // both executed.
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn api_call_missing_request_id_is_transport_error() {
        let router = counter_router(Arc::new(AtomicUsize::new(0)));
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        let p = json!({"token": tok("operator"), "method": "POST", "path": "/api/scan"});
        let ack = tunnel_call_with(&router, &cache, SECRET, &p, 1_500_000).await;
        assert_eq!(ack["ok"], json!(false));
        assert_eq!(ack["error"], json!("request_id required"));
    }

    #[tokio::test]
    async fn api_call_oversize_body_is_413() {
        let router = counter_router(Arc::new(AtomicUsize::new(0)));
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        let big = "x".repeat(MAX_BODY_BYTES + 100);
        let p = json!({
            "token": tok("operator"), "method": "POST", "path": "/api/scan",
            "request_id": "r", "body": {"blob": big}
        });
        let ack = tunnel_call_with(&router, &cache, SECRET, &p, 1_500_000).await;
        assert_eq!(ack["status"], json!(413));
    }

    #[tokio::test]
    async fn api_call_delete_method_is_405() {
        let router = counter_router(Arc::new(AtomicUsize::new(0)));
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        let ack = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("DELETE", "/api/entity/order/o1", &tok("operator"), "r"),
            1_500_000,
        )
        .await;
        assert_eq!(ack["status"], json!(405));
    }

    #[tokio::test]
    async fn api_call_non_tunnelled_path_is_403() {
        let router = counter_router(Arc::new(AtomicUsize::new(0)));
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        let ack = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("POST", "/api/admin/query", &tok("operator"), "r"),
            1_500_000,
        )
        .await;
        assert_eq!(ack["status"], json!(403));
        assert_eq!(ack["body"], json!({"error": "path_not_tunnelled"}));
    }

    #[tokio::test]
    async fn api_call_binary_response_is_415() {
        let router = counter_router(Arc::new(AtomicUsize::new(0)));
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        let ack = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("GET", "/api/receiving/blob", &tok("operator"), "r"),
            1_500_000,
        )
        .await;
        assert_eq!(ack["status"], json!(415));
        assert_eq!(ack["body"], json!({"error": "binary_not_tunnelled"}));
    }

    #[tokio::test]
    async fn api_call_oversize_response_is_502() {
        let router = counter_router(Arc::new(AtomicUsize::new(0)));
        let cache = ReplayCache::new(8, Duration::from_secs(60));
        // Tiny budget so the ~4 KB JSON response cannot be acked.
        let ack = tunnel_call_with(
            &router,
            &cache,
            SECRET,
            &payload("GET", "/api/receiving/big", &tok("operator"), "r"),
            256,
        )
        .await;
        assert_eq!(ack["status"], json!(502));
        assert_eq!(ack["body"]["error"], json!("response_too_large"));
        assert!(ack["body"]["bytes"].as_u64().unwrap() > 256);
    }
}
