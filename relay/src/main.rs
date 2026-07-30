mod db;
mod handlers;

use axum::{extract::DefaultBodyLimit, routing::{get, post}, Router};
use std::net::SocketAddr;
use tokio::time::{interval, Duration};
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "relay=info".into()))
        .init();

    // SurrealDB (local embedded via SurrealKV)
    let db_path = std::env::var("SURREAL_DB_PATH")
        .unwrap_or_else(|_| "data/relay.db".into());
    let db = db::init_db(&db_path)
        .await
        .expect("Failed to initialize SurrealDB");

    let cleanup_db = db.clone();
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(60));
        loop {
            tick.tick().await;

            match db::cleanup_expired(&cleanup_db).await {
                Ok(n) if n > 0 => tracing::info!("Cleaned up {n} expired packets"),
                Err(e) => tracing::warn!("Cleanup error: {e}"),
                _ => {}
            }

            match db::mark_offline_nodes(&cleanup_db).await {
                Ok(n) if n > 0 => tracing::info!("Marked {n} nodes as offline"),
                Err(e) => tracing::warn!("Offline detection error: {e}"),
                _ => {}
            }

            // GC acked xelixir_task rows older than 1 h. The dispatcher
            // polls `/E/x/result/<task_id>` to read the ack body, so we
            // keep rows around for a window long enough that no caller
            // is still polling, then drop them.
            //
            // `acked_at` is stored as an RFC3339 string (the WMS-side
            // ack handler doesn't have access to SurrealDB native
            // datetime construction without round-tripping). Compare on
            // the string-coerced cutoff so the predicate doesn't blow
            // up on type mismatch — and explicitly check `acked = true`
            // (NOT `acked_at IS NOT NONE`) so non-acked rows never get
            // collected even with a malformed timestamp.
            let cutoff_iso = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
            let res = cleanup_db
                .query("DELETE xelixir_task WHERE acked = true AND acked_at != NONE AND acked_at < $cutoff;")
                .bind(("cutoff", cutoff_iso.clone()))
                .await;
            if let Err(e) = res {
                tracing::warn!("xelixir_task GC error: {e}");
            }

            // Same GC policy for mesh_task — acked rows linger 1 h so the
            // sender's poller has a window to read the result body, then drop.
            let res = cleanup_db
                .query("DELETE mesh_task WHERE acked = true AND acked_at != NONE AND acked_at < $cutoff;")
                .bind(("cutoff", cutoff_iso.clone()))
                .await;
            if let Err(e) = res {
                tracing::warn!("mesh_task GC error: {e}");
            }

            // Same GC policy for the paid client-MCP channel.
            let res = cleanup_db
                .query("DELETE client_mcp_task WHERE acked = true AND acked_at != NONE AND acked_at < $cutoff;")
                .bind(("cutoff", cutoff_iso))
                .await;
            if let Err(e) = res {
                tracing::warn!("client_mcp_task GC error: {e}");
            }

            // Drop expired / burned pair-code rendezvous entries. `expires_at` is
            // an RFC3339 string; compare on the string-coerced now (same format).
            let now_iso = chrono::Utc::now().to_rfc3339();
            let res = cleanup_db
                .query("DELETE pair_rendezvous WHERE expires_at < $now OR used = true;")
                .bind(("now", now_iso))
                .await;
            if let Err(e) = res {
                tracing::warn!("pair_rendezvous GC error: {e}");
            }
        }
    });

    let app = build_router(db);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3200);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    tracing::info!("Eck relay listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Build the relay's axum router. Extracted so integration tests can drive the
/// real handlers + DB via `oneshot` without binding a socket.
fn build_router(db: db::RelayDb) -> Router {
    Router::new()
        .route("/E/health", get(handlers::health))
        .route("/E/register", post(handlers::register))
        .route("/E/push", post(handlers::push))
        .route("/E/pull/{mesh_id}/{instance_id}", get(handlers::pull))
        .route("/E/mesh/{mesh_id}/status", get(handlers::mesh_status))
        .route("/E/mesh/{mesh_id}/resolve/{instance_id}", get(handlers::resolve_node))
        .route("/E/registry", get(handlers::registry))
        // Mesh-agnostic xelixir control plane (UUID-routed, NAT-friendly).
        .route("/E/resolve/{instance_id}", get(handlers::x_resolve))
        .route("/E/x/dispatch/{target_uuid}", post(handlers::x_dispatch))
        .route("/E/x/poll/{self_uuid}", get(handlers::x_poll))
        .route("/E/x/ack/{task_id}", post(handlers::x_ack))
        .route("/E/x/result/{task_id}", get(handlers::x_result))
        // Mesh-sync routing for NAT'd peers — separate queue from xelixir C2.
        .route("/E/m/dispatch/{target_uuid}", post(handlers::m_dispatch))
        .route("/E/m/poll/{self_uuid}", get(handlers::m_poll))
        .route("/E/m/ack/{task_id}", post(handlers::m_ack))
        .route("/E/m/result/{task_id}", get(handlers::m_result))
        // Paid-subscription MCP channel — GATED: c_dispatch admits only a valid
        // authority-signed SubscriptionCert (see handlers::client_mcp).
        .route("/E/c/dispatch/{target_uuid}", post(handlers::c_dispatch))
        .route("/E/c/poll/{self_uuid}", get(handlers::c_poll))
        .route("/E/c/ack/{task_id}", post(handlers::c_ack))
        .route("/E/c/result/{task_id}", get(handlers::c_result))
        // Pair-code onboarding rendezvous (typed code -> ECK$ pairing string).
        .route("/E/pair/announce", post(handlers::pair_announce))
        .route("/E/pair/code", post(handlers::pair_resolve))
        // Mesh payloads are fatter than axum's 2 MB default: /E/m/dispatch
        // carries base64-encoded phone photos and /E/m/ack carries entity
        // batches. Exceeding the limit doesn't 413 cleanly — the connection is
        // cut mid-upload, the sender sees a bare send error, and an unacked
        // task is redelivered forever (poison task). Keep A limit as the DoS
        // bound, just a realistic one (32 MiB ≈ a 24 MB image after base64).
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .with_state(db)
}

#[cfg(test)]
mod client_mcp_gate_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::SigningKey;
    use eck_core::xelixir::subscription::{
        issue, ClientMcpRequest, SignedClientMcp, SubscriptionCert, FEATURE_MCP_RELAY,
    };
    use serde_json::Value;
    use tower::ServiceExt; // oneshot

    fn keypair(seed: u8) -> (String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        (
            STANDARD.encode(sk.to_bytes()),
            STANDARD.encode(sk.verifying_key().to_bytes()),
        )
    }

    fn signed_request(client_priv: &str, client_pub: &str, cert: &str, target: &str) -> Value {
        let request = ClientMcpRequest {
            target_uuid: target.into(),
            mcp: serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            timestamp: chrono::Utc::now().timestamp(),
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let signature =
            eck_core::utils::identity::sign_message(client_priv, &request.canonical()).unwrap();
        serde_json::to_value(SignedClientMcp {
            request,
            signer_pubkey: client_pub.into(),
            signature,
            cert: cert.into(),
        })
        .unwrap()
    }

    async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    // One test: the gate's env var is process-global, so we drive every case
    // sequentially in a single test to avoid a cross-test race.
    #[tokio::test]
    async fn c_dispatch_gate() {
        let (root_priv, root_pub) = keypair(1);
        let (client_priv, client_pub) = keypair(2);
        let (attacker_priv, _attacker_pub) = keypair(9);

        let tmp = std::env::temp_dir().join(format!("relay-cmcp-test-{}", uuid::Uuid::new_v4()));
        let db = db::init_db(tmp.to_str().unwrap()).await.unwrap();
        let app = build_router(db);
        let target = "node-under-test";

        // ── Fail-closed: no root configured ⇒ 503, before we set the key. ──
        std::env::remove_var("ECK_SUB_ROOT_PUBKEY");
        let (st, _) = post(
            &app,
            &format!("/E/c/dispatch/{target}"),
            signed_request(&client_priv, &client_pub, "whatever.sig", target),
        )
        .await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "no root ⇒ channel off");

        std::env::set_var("ECK_SUB_ROOT_PUBKEY", &root_pub);
        let cert = SubscriptionCert {
            client_pubkey: client_pub.clone(),
            subject: "acct-1".into(),
            plan: "pro".into(),
            nodes: vec![target.into()],
            features: vec![FEATURE_MCP_RELAY.into()],
            tier: "agent".into(),
            iat: chrono::Utc::now().timestamp(),
            exp: chrono::Utc::now().timestamp() + 3600,
        };
        let good_token = issue(&root_priv, &cert).unwrap();
        let forged_token = issue(&attacker_priv, &cert).unwrap(); // signed by the wrong root

        let uri = format!("/E/c/dispatch/{target}");

        // 1) Valid subscription → queued.
        let (st, body) = post(&app, &uri, signed_request(&client_priv, &client_pub, &good_token, target)).await;
        assert_eq!(st, StatusCode::OK, "valid cert should be admitted");
        let task_id = body["task_id"].as_str().expect("task_id").to_string();

        // 2) Forged cert (free client signs with its own key) → 403, NOT queued.
        let (st, _) = post(&app, &uri, signed_request(&client_priv, &client_pub, &forged_token, target)).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "forged cert must be refused");

        // 3) Garbage body → 400.
        let (st, _) = post(&app, &uri, serde_json::json!({"nonsense": true})).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        // The node can poll exactly the one admitted task back.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/E/c/poll/{target}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let polled: Value = serde_json::from_slice(&bytes).unwrap();
        let tasks = polled["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1, "only the admitted request is queued");
        assert_eq!(tasks[0]["id"].as_str().unwrap(), task_id);

        std::env::remove_var("ECK_SUB_ROOT_PUBKEY");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
