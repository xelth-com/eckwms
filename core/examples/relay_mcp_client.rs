//! Minimal relay-MCP client — the embryo of the paid-subscriber shim, and the
//! E2E test client for the paid MCP channel.
//!
//! **Direct-first** (owner preference, 2026-07-17): when the node is reachable
//! directly (same LAN / public node) the shim POSTs the SAME signed request to
//! `<direct>/mcp/signed` and only falls back to the relay `/E/c/*` queue when
//! that fails — no relay load, no 10 s poll latency. The direct ingress accepts
//! the identical `SignedClientMcp` and verifies it with the identical code the
//! relay poller runs, so the credential (not the transport) is what grants
//! access.
//!
//! Deterministic keys (fixed seeds) so the relay + node can be started with the
//! known `ECK_SUB_ROOT_PUBKEY` first, then a fresh-timestamp request is minted
//! at send time (inside the 60 s signature window).
//!
//!   # print the subscription root pubkey to configure on relay + node:
//!   cargo run -p eck_core --example relay_mcp_client -- pubkey
//!
//!   # full round-trip (mint cert, sign, dispatch, poll result):
//!   cargo run -p eck_core --example relay_mcp_client -- \
//!       <relay_url> <node_uuid> [agent|master] [tools/list|tools/call:customer_360:<q>] [direct_url]
//!
//! Direct URL resolution, in priority order:
//!   1. CLI arg 5 (`<direct_url>`), or the `ECK_NODE_DIRECT_URL` env var;
//!   2. else auto-resolve `<relay>/E/resolve/<node_uuid>` → `base_url` (works
//!      only when the relay's resolver is UNGATED — skipped silently otherwise);
//!   3. else relay-only.
//! A known direct URL is probed first (~3 s); on connect error / timeout /
//! 404 / 405 / 503 the shim falls back to the relay dispatch+poll flow.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::SigningKey;
use eck_core::utils::identity::sign_message;
use eck_core::xelixir::subscription::{
    issue, ClientMcpRequest, SignedClientMcp, SubscriptionCert, FEATURE_MCP_RELAY,
};
use serde_json::{json, Value};

fn keypair(seed: u8) -> (String, String) {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    (
        STANDARD.encode(sk.to_bytes()),
        STANDARD.encode(sk.verifying_key().to_bytes()),
    )
}

/// Outcome of a direct `/mcp/signed` probe.
enum DirectResult {
    /// HTTP 200 — the raw JSON-RPC response body.
    Ok(Value),
    /// Node has no signed ingress here (connect refused / timeout / 404/405/503)
    /// — fall back to the relay.
    Fallback(String),
    /// Definitive non-200 (e.g. 403 bad cert, 400 malformed). The relay would
    /// answer the same, so don't double-dispatch.
    Terminal(String),
}

fn short_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(3))
        .build()
        .ok()
}

/// Best-effort UUID → base_url via the relay's ungated resolver. Returns `None`
/// (skip auto-resolve, use the relay) when the resolver is gated / missing / the
/// row has no usable `base_url`.
async fn resolve_direct(relay: &str, target: &str) -> Option<String> {
    let client = short_client()?;
    let url = format!("{}/E/resolve/{}", relay.trim_end_matches('/'), target);
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None; // 401 gated / 404 unknown → explicit URL only
    }
    let body: Value = resp.json().await.ok()?;
    let base = body
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if base.starts_with("http://") || base.starts_with("https://") {
        Some(base)
    } else {
        None
    }
}

async fn try_direct(base: &str, signed: &SignedClientMcp) -> DirectResult {
    // Short CONNECT timeout (is the node reachable at all?), but a generous
    // total timeout: once connected, the node IS serving us — a slow tool
    // (ask_brain) must not get cut mid-execution and bounced to the relay.
    let Some(client) = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(120))
        .build()
        .ok()
    else {
        return DirectResult::Fallback("http client build failed".into());
    };
    let url = format!("{}/mcp/signed", base.trim_end_matches('/'));
    let resp = match client.post(&url).json(signed).send().await {
        Ok(r) => r,
        Err(e) => {
            let why = if e.is_timeout() {
                "timeout".to_string()
            } else if e.is_connect() {
                "connect refused".to_string()
            } else {
                e.to_string()
            };
            return DirectResult::Fallback(why);
        }
    };
    let status = resp.status();
    if status.is_success() {
        return DirectResult::Ok(resp.json().await.unwrap_or(Value::Null));
    }
    if matches!(status.as_u16(), 404 | 405 | 503) {
        return DirectResult::Fallback(format!("HTTP {status}"));
    }
    let body = resp.text().await.unwrap_or_default();
    DirectResult::Terminal(format!("HTTP {status}: {body}"))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (sub_root_priv, sub_root_pub) = keypair(3);
    let (client_priv, client_pub) = keypair(4);

    if args.first().map(|s| s.as_str()) == Some("pubkey") {
        println!("{sub_root_pub}");
        return;
    }

    let relay = args.first().cloned().unwrap_or_else(|| "http://127.0.0.1:3200".into());
    let target = args.get(1).cloned().expect("usage: <relay_url> <node_uuid> [tier] [method] [direct_url]");
    let tier = args.get(2).cloned().unwrap_or_else(|| "agent".into());
    let method = args.get(3).cloned().unwrap_or_else(|| "tools/list".into());
    // Direct URL: CLI arg 5, else ECK_NODE_DIRECT_URL.
    let explicit_direct = args
        .get(4)
        .cloned()
        .or_else(|| std::env::var("ECK_NODE_DIRECT_URL").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Build the MCP JSON-RPC body from the shorthand.
    let mcp: Value = if let Some(rest) = method.strip_prefix("tools/call:") {
        let mut parts = rest.splitn(2, ':');
        let name = parts.next().unwrap_or("");
        let q = parts.next().unwrap_or("");
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":name,"arguments":{"query":q}}})
    } else {
        json!({"jsonrpc":"2.0","id":1,"method":method})
    };

    // Mint the subscription cert (authority side — here we hold the root seed).
    let now = chrono::Utc::now().timestamp();
    let cert = SubscriptionCert {
        client_pubkey: client_pub.clone(),
        subject: "e2e-subscriber".into(),
        plan: "e2e".into(),
        nodes: vec![target.clone()],
        features: vec![FEATURE_MCP_RELAY.into()],
        tier,
        iat: now,
        exp: now + 3600,
    };
    let cert_token = issue(&sub_root_priv, &cert).expect("issue cert");

    // Sign the request (subscriber side). A helper because the relay fallback
    // re-signs with a FRESH nonce + timestamp: the node records the nonce
    // BEFORE executing, so a direct attempt that died mid-flight has already
    // burned it — reusing it on the relay leg would be rejected as a replay.
    let make_signed = |mcp: Value| -> SignedClientMcp {
        let request = ClientMcpRequest {
            target_uuid: target.clone(),
            mcp,
            timestamp: chrono::Utc::now().timestamp(),
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let signature = sign_message(&client_priv, &request.canonical()).expect("sign");
        SignedClientMcp {
            request,
            signer_pubkey: client_pub.clone(),
            signature,
            cert: cert_token.clone(),
        }
    };
    let signed = make_signed(mcp.clone());

    // ── Direct-first: probe the node's /mcp/signed if we know (or can resolve)
    //    a base URL, and only fall back to the relay on unreachable. ──────────
    let direct_base = match explicit_direct {
        Some(u) => Some(u),
        None => resolve_direct(&relay, &target).await,
    };
    if let Some(base) = &direct_base {
        match try_direct(base, &signed).await {
            DirectResult::Ok(resp) => {
                println!("transport=direct ({}/mcp/signed)", base.trim_end_matches('/'));
                println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
                return;
            }
            DirectResult::Terminal(msg) => {
                eprintln!("direct path refused: {msg}");
                std::process::exit(1);
            }
            DirectResult::Fallback(why) => {
                eprintln!("direct path unavailable ({why}); falling back to relay");
            }
        }
    }

    // ── Relay fallback: dispatch through /E/c/* and poll for the result.
    //    Freshly signed (new nonce + timestamp) — see make_signed above. ───────
    let signed = make_signed(mcp.clone());
    println!("transport=relay ({})", relay.trim_end_matches('/'));
    let http = reqwest::Client::new();
    let dispatch_url = format!("{}/E/c/dispatch/{}", relay.trim_end_matches('/'), target);
    let resp = http.post(&dispatch_url).json(&signed).send().await.expect("dispatch");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        eprintln!("dispatch refused ({status}): {body}");
        std::process::exit(1);
    }
    let task_id = body["task_id"].as_str().expect("task_id").to_string();
    println!("dispatched task {task_id}, polling result…");

    let result_url = format!("{}/E/c/result/{}", relay.trim_end_matches('/'), task_id);
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let r: Value = http.get(&result_url).send().await.expect("result").json().await.unwrap_or(Value::Null);
        match r["status"].as_str() {
            Some("pending") => continue,
            _ => {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                return;
            }
        }
    }
    eprintln!("timed out waiting for node ack");
    std::process::exit(1);
}
