//! MCP agent surface for the WMS data plane.
//!
//! A single `POST /mcp` route on the same axum app (no separate process),
//! speaking just enough of the Model Context Protocol Streamable-HTTP transport
//! (spec rev 2025-03-26) to expose *domain* tools to a remote LLM client —
//! Claude Code / claude.ai over the relay or LAN. Modeled on xelixir's
//! `crates/server/src/mcp.rs`, but this catalog is the **business graph**
//! (customers, tickets, repairs, warehouse), NOT device/infra control.
//!
//! xelixir (infra repair) and 9eck.com (data) stay two decoupled systems: this
//! endpoint never proxies to xelixir. A future narrow agent-to-agent bridge
//! (e.g. xelixir asking "back up X before I touch the disk") rides the existing
//! `mesh_task` queue, not a merged tool catalog.
//!
//! Wire shape per request:
//!   POST /mcp
//!   Authorization: Bearer <token>
//!   body: {"jsonrpc":"2.0","id":N,"method":...,"params":...}
//!   Response: text/event-stream, `event: message\ndata: {jsonrpc}\n\n`
//!
//! PII on this surface is PSEUDONYMIZED for EVERY tier: names/emails/phones/
//! addresses come back as stable `obfuscate_pii` tokens (`Name_<16 hex>` …),
//! never in clear — so a third-party LLM (Gemini/Antigravity, Claude Code)
//! never receives clear customer PII. A master caller restores the real values
//! into a FILE via the shim's `reveal_file` bridge tool, which calls the
//! master-only [`reveal::reveal_tokens_method`] (see `reveal.rs`).
//!
//! Two privilege tiers, mapped onto mechanisms this repo already has:
//!   • Master token → may call `reveal_tokens` and `surrealql_read`.
//!   • Agent token  → automated / external callers: `reveal_tokens` and the
//!     Zone-1 tables (`user`, credentials) stay unreachable, exactly like the
//!     `/X/ops/*` surface. (Tool PII is tokenized for both tiers regardless.)

pub(crate) mod connector;
pub(crate) mod reveal;
pub(crate) mod tools;

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::AppState;

const PROTOCOL_VERSION: &str = "2025-03-26";

/// Which privilege tier a validated MCP bearer maps to.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum McpTier {
    /// Full operator: un-masked PII, every tool.
    Master,
    /// Automated caller (server-side `claude -p`, external agents on the agent
    /// token): PII masked, Zone-1 unreadable.
    Agent,
}

impl McpTier {
    /// Whether this tier may see un-masked customer PII (names, emails, phones).
    pub(crate) fn reveal_pii(self) -> bool {
        matches!(self, McpTier::Master)
    }
}

/// Constant-time byte compare (same primitive as `middleware::service_token`),
/// so token validation is not a timing oracle.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Resolve the bearer to a tier, or an HTTP error response.
///
/// Master token: `ECK_MCP_MASTER_TOKEN`, falling back to `XELIXIR_SERVICE_TOKEN`
/// so the endpoint is usable today without provisioning a new secret. Agent
/// token: `ECK_MCP_AGENT_TOKEN` (optional — unset means no agent tier yet).
fn authenticate(headers: &HeaderMap) -> Result<McpTier, Response> {
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string());
    let Some(provided) = provided else {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response());
    };

    let master = std::env::var("ECK_MCP_MASTER_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("XELIXIR_SERVICE_TOKEN").ok())
        .unwrap_or_default();
    let agent = std::env::var("ECK_MCP_AGENT_TOKEN").unwrap_or_default();

    if master.is_empty() && agent.is_empty() {
        return Err(
            (StatusCode::INTERNAL_SERVER_ERROR, "MCP token not configured").into_response(),
        );
    }
    if !master.is_empty() && ct_eq(provided.as_bytes(), master.as_bytes()) {
        return Ok(McpTier::Master);
    }
    if !agent.is_empty() && ct_eq(provided.as_bytes(), agent.as_bytes()) {
        return Ok(McpTier::Agent);
    }
    Err((StatusCode::UNAUTHORIZED, "Unauthorized").into_response())
}

fn sse_response(payload: Value) -> Response {
    let body = format!("event: message\ndata: {}\n\n", payload);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body.into())
        .expect("static sse response")
}

fn jsonrpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn jsonrpc_error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handle one JSON-RPC request and return the response object — or `None` for
/// notifications (which have no response). Shared by the HTTP transport
/// (`mcp_handler`) and the relay-carried transport (client-MCP poller), so both
/// paths run the identical method dispatch and tier gating.
///
/// `over_relay` marks a request that arrived through the paid relay channel: raw
/// SurrealQL (`surrealql_read`) is refused on that path even for a master cert —
/// the schema-blind DB window never leaves the LAN.
pub(crate) async fn handle_jsonrpc(
    state: &Arc<AppState>,
    tier: McpTier,
    req: &Value,
    over_relay: bool,
) -> Option<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let resp = match method {
        "initialize" => jsonrpc_result(
            &id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "9eck-wms", "version": env!("CARGO_PKG_VERSION")},
                "instructions": tools::INSTRUCTIONS,
            }),
        ),
        // Notification — no response body.
        "notifications/initialized" => return None,
        "ping" => jsonrpc_result(&id, json!({})),
        // Master-only reveal of pseudonym tokens → clear values. A METHOD, not
        // a tool: it never appears in `tools/list` and an agent caller is
        // refused. Runs on BOTH transports (bearer `/mcp` and signed
        // `/mcp/signed` incl. over_relay) — the real values return to the
        // caller's own machine (the shim), never into a normal tool response.
        "reveal_tokens" => match reveal::reveal_tokens_method(&state.pii_reveal, tier, &params) {
            // Tokens the RAM map never saw (baked into stored summaries at
            // distillation) get a second chance via the pii_fingerprints-indexed
            // DB re-derivation — master tier is already proven by the Ok path.
            Ok(v) => jsonrpc_result(&id, reveal::augment_from_db(state, v).await),
            Err((code, msg)) => jsonrpc_error(&id, code, &msg),
        },
        "tools/list" => {
            let mut list = tools::tools_list(tier);
            if over_relay {
                // Never advertise the LAN/server-local tools over the relay:
                // the raw-DB window (`surrealql_read`) and the server-side
                // reveal (`reveal_file_local`, whose file lives on the WMS
                // machine, not the remote caller's) both stay off that path.
                if let Some(arr) = list.as_array_mut() {
                    arr.retain(|t| {
                        let n = t.get("name").and_then(|n| n.as_str());
                        n != Some("surrealql_read") && n != Some("reveal_file_local")
                    });
                }
            }
            jsonrpc_result(&id, json!({ "tools": list }))
        }
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if over_relay && name == "surrealql_read" {
                jsonrpc_result(
                    &id,
                    json!({
                        "isError": true,
                        "content": [{ "type": "text",
                            "text": "surrealql_read is not available to subscription-cert access (relay or direct) — use the master bearer token on /mcp for raw DB access" }],
                    }),
                )
            } else if over_relay && name == "reveal_file_local" {
                jsonrpc_result(
                    &id,
                    json!({
                        "isError": true,
                        "content": [{ "type": "text",
                            "text": "reveal_file_local is not available to subscription-cert access (relay or direct) — use the shim's reveal_file bridge tool instead" }],
                    }),
                )
            } else {
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                jsonrpc_result(&id, tools::dispatch_tool(state, name, &args, tier).await)
            }
        }
        _ => jsonrpc_error(&id, -32601, &format!("method not found: {method}")),
    };
    Some(resp)
}

/// `POST /mcp` — the single MCP transport entry point (direct/LAN).
pub async fn mcp_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    let tier = match authenticate(&headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    match handle_jsonrpc(&state, tier, &req, false).await {
        Some(resp) => sse_response(resp),
        None => (StatusCode::ACCEPTED, "").into_response(),
    }
}

/// `POST /mcp/signed` — the DIRECT (LAN / public) ingress for a paid
/// `SubscriptionCert`-signed MCP request. The direct twin of the relay `/E/c/*`
/// channel: same unforgeable credential, no relay hop and no 10 s poll latency.
///
/// No bearer token — capability comes from the authority-signed cert inside the
/// body, verified by the SAME [`crate::services::client_mcp::serve_signed`] the
/// relay poller uses (so `over_relay = true`: `surrealql_read` stays refused AND
/// hidden — capability follows the credential, not the transport). This route
/// never touches the bearer-gated `/mcp` handler's auth.
///
/// Response contract (mirrors the relay gate): root unset → 503; malformed body
/// → 400; bad cert / replayed nonce → 403; success → 200 with the raw JSON-RPC
/// response body.
pub async fn mcp_signed_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Response {
    use crate::services::client_mcp::{serve_signed, SignedMcpOutcome};

    let outcome = serve_signed(&state, payload).await;
    let status = outcome.http_status();
    match outcome {
        // 200 with the raw JSON-RPC response as application/json — the consumer
        // is the shim proxy, not a Streamable-HTTP MCP client, so we return
        // plain JSON here rather than the SSE framing `/mcp` uses.
        SignedMcpOutcome::Served(resp) => (status, Json(resp)).into_response(),
        SignedMcpOutcome::Disabled => {
            (status, Json(json!({"error": "signed MCP ingress disabled"}))).into_response()
        }
        SignedMcpOutcome::Malformed(e) => {
            (status, format!("malformed SignedClientMcp: {e}")).into_response()
        }
        SignedMcpOutcome::Rejected(_) => (status, "subscription not authorized").into_response(),
        SignedMcpOutcome::Replay => (status, "duplicate nonce (replay)").into_response(),
        SignedMcpOutcome::NonceStoreError(_) => (status, "nonce store unavailable").into_response(),
    }
}
