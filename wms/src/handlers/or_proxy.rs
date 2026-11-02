//! `POST /api/ai/or` — the 9eck managed OpenRouter proxy (commerce heart).
//!
//! Owner architecture decision (2026-08-30): billing and AI distribution live in
//! 9eck, not xelixir. This is the relocation of the former xelixir `/ai/or`
//! endpoint onto the new 9eck-wms "xelth" commerce instance. A keyless paid
//! client (the WMS OpenRouter shim in managed mode) sends an OpenAI-chat request
//! with its paid mesh license as `Authorization: Bearer <license>`; this node
//! verifies + gates it, forwards the JSON body VERBATIM to OpenRouter under OUR
//! master key, and returns the upstream response verbatim (the client parses
//! `usage.cost` / `provider` itself).
//!
//! Hard invariants:
//! * Mounted ONLY when `ECK_OR_SERVE=1` (see `wms/src/main.rs`) — the route does
//!   not exist otherwise. This is the single node that serves AI distribution.
//! * AUTH reuses the relay-paidgate primitive `eck_core::licensing::verify` +
//!   `is_paid()` against the embedded issuer pubkey (`node_issuer_pubkey`); the
//!   client identity is the license `sub`. No node-mesh binding — the caller is
//!   a DIFFERENT mesh presenting its own paid license.
//! * GATE (transitional, no wallet yet): per-client USD/month caps from
//!   `ECK_OR_CLIENT_CAPS`, month-to-date spend tracked in the node-local
//!   `or_spend_counter` table; over cap => 402.
//! * Model allowlist `ECK_OR_SERVE_MODELS` (cost control) + a per-client
//!   prefix deny policy (`or_client_policy` row) => 400 / 403.
//! * No content logging: sizes / latency / model / cost only.
//!
//! Storage: `or_spend_counter` (cap counter) and `or_usage_ledger` (billing
//! seed) are node-LOCAL SurrealDB tables — this node is the home node for AI
//! distribution and neither table is in `SYNC_ENTITY_TYPES`, so they never mesh-
//! sync (cheapest choice for v1; a proper wallet entity supersedes them later).

use crate::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Default per-client cap when neither the client nor `default=` is listed.
const DEFAULT_CAP_USD: f64 = 5.0;
/// Default upstream timeout (seconds); floored at 120 (glm can take 30-60s).
const DEFAULT_TIMEOUT_SECS: u64 = 180;

/// Exact-match model allowlist from `ECK_OR_SERVE_MODELS` (our cost control),
/// default the two benched flash models. A client can only proxy models WE priced.
fn or_allowed_models() -> Vec<String> {
    std::env::var("ECK_OR_SERVE_MODELS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "glm-5.3-flash,z-ai/glm-5.3-flash,qwen/qwen3.5-flash-02-23,deepseek/deepseek-v4.1-flash-1017,deepseek/deepseek-v4.1-flash,xiaomi/mimo-v2.6-pro,xiaomi/mimo-v2.6-flash".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Exact-match allowlist test (pure).
fn or_model_allowed(model: &str, allow: &[String]) -> bool {
    allow.iter().any(|m| m == model)
}

/// Per-client model policy (security setting): `model` is denied when it starts
/// with any vendor prefix in `denied` (e.g. "z-ai/"). Pure so the UI wave reuses
/// it; empty deny-list => everything allowed.
fn or_model_denied_by_policy(model: &str, denied: &[String]) -> bool {
    denied
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .any(|p| model.starts_with(p))
}

/// Parse `ECK_OR_CLIENT_CAPS="<client>=<usd>,default=<usd>"` => (per-client map,
/// default cap). Unparsable entries are skipped; a missing `default=` falls back
/// to [`DEFAULT_CAP_USD`]. Pure (spec passed in) for testability.
fn parse_client_caps(spec: &str) -> (HashMap<String, f64>, f64) {
    let mut caps = HashMap::new();
    let mut default = DEFAULT_CAP_USD;
    for entry in spec.split(',') {
        let Some((k, v)) = entry.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let Ok(usd) = v.parse::<f64>() else { continue };
        if k == "default" {
            default = usd;
        } else if !k.is_empty() {
            caps.insert(k.to_string(), usd);
        }
    }
    (caps, default)
}

/// The cap that applies to `client`: its own entry, else the default.
fn cap_for(caps: &HashMap<String, f64>, default: f64, client: &str) -> f64 {
    caps.get(client).copied().unwrap_or(default)
}

/// Gate decision (BEFORE the call, like the wallet gate): reject when the
/// month-to-date spend already reaches the cap. A single in-flight call may
/// overshoot by its own cost — acceptable for the transitional cap gate.
fn over_cap(spent_usd: f64, cap: f64) -> bool {
    spent_usd >= cap
}

/// Parse `ECK_OR_PRICES="<model>=<in_per_1M>:<out_per_1M>,..."` => per-model
/// (input, output) USD-per-1M-token rates. Unparsable entries skipped. Pure.
fn parse_price_table(spec: &str) -> HashMap<String, (f64, f64)> {
    let mut m = HashMap::new();
    for entry in spec.split(',') {
        let Some((model, prices)) = entry.split_once('=') else { continue };
        let Some((inp, outp)) = prices.split_once(':') else { continue };
        let (Ok(i), Ok(o)) = (inp.trim().parse::<f64>(), outp.trim().parse::<f64>()) else { continue };
        let model = model.trim();
        if !model.is_empty() {
            m.insert(model.to_string(), (i, o));
        }
    }
    m
}

/// The price table from `ECK_OR_PRICES`, defaulting to glm-5.3-flash rates
/// ($0.075 in / $0.25 out per 1M) under both the z.ai-direct and OR-prefixed
/// names. Used ONLY to estimate cost when the upstream omits `usage.cost` (z.ai
/// direct does); a real upstream `usage.cost` always wins.
fn or_price_table() -> HashMap<String, (f64, f64)> {
    parse_price_table(
        &std::env::var("ECK_OR_PRICES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "glm-5.3-flash=0.075:0.25,z-ai/glm-5.3-flash=0.075:0.25,deepseek/deepseek-v4.1-flash-1017=0.15:0.60,deepseek/deepseek-v4.1-flash=0.045:0.50,xiaomi/mimo-v2.6-pro=0.435:0.87,xiaomi/mimo-v2.6-flash=0.14:0.28".to_string()),
    )
}

/// Slug upstream split for the SERVING side (`.eck/OPENROUTER_TIER.md` §slug),
/// mirroring the client `slug_upstream_for`. A slug model (name contains `/`)
/// can't be served by a vendor-direct upstream (e.g. Z.ai via
/// `ECK_OR_UPSTREAM_URL`), so when `ECK_OR_SLUG_MASTER_KEY` is set it routes to
/// a SECOND upstream (`ECK_OR_SLUG_UPSTREAM_URL`, default the public OpenRouter
/// endpoint — formed like `ECK_OR_UPSTREAM_URL`, incl. `/chat/completions`)
/// under that key. Returns `Some((upstream_url, master_key))` for the slug
/// upstream, or `None` for "unchanged". Pure; env read by the caller.
fn serve_slug_upstream_for(model: &str, slug_key: &str, slug_upstream: &str) -> Option<(String, String)> {
    if !model.contains('/') || slug_key.trim().is_empty() {
        return None;
    }
    let url = if slug_upstream.trim().is_empty() {
        "https://openrouter.ai/api/v1/chat/completions".to_string()
    } else {
        slug_upstream.trim().to_string()
    };
    Some((url, slug_key.trim().to_string()))
}

/// Estimate USD from token counts via the price table (per-1M rates). `None`
/// when `model` is absent from the table — the caller then warns and records $0.
fn estimate_cost_usd(model: &str, prompt: i64, completion: i64, table: &HashMap<String, (f64, f64)>) -> Option<f64> {
    table.get(model).map(|(inp, outp)| {
        (prompt.max(0) as f64 / 1_000_000.0) * inp + (completion.max(0) as f64 / 1_000_000.0) * outp
    })
}

/// `YYYY-MM` bucket key for a unix-seconds instant (UTC). The spend counter is
/// keyed by this, so month rollover is automatic: a new month = a fresh row that
/// starts at zero, with no sweep needed.
fn month_key(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap())
        .format("%Y-%m")
        .to_string()
}

/// Read the per-client OR model deny-list from the node-local `or_client_policy`
/// row. Absent/unreadable => empty (all allowed) — a policy read must never
/// fail-open into a BLOCK. NOTE: no writer wires this yet — the admin-UI security
/// toggle (a later wave) will set `models_denied`; the row lives beside the caps.
async fn fetch_denied_prefixes<C: surrealdb::Connection>(db: &surrealdb::Surreal<C>, client: &str) -> Vec<String> {
    let rows: Result<Vec<Value>, _> = db
        .query("SELECT models_denied FROM type::record('or_client_policy', $c)")
        .bind(("c", client.to_string()))
        .await
        .and_then(|mut r| r.take(0));
    match rows {
        Ok(rows) => rows
            .into_iter()
            .next()
            .and_then(|row| row.get("models_denied").cloned())
            .and_then(|v| v.as_array().cloned())
            .map(|arr| {
                arr.into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!("[or-serve] policy read failed for {client}: {e} — treating as no policy");
            Vec::new()
        }
    }
}

/// Month-to-date USD spend for `client` in `month`, from the node-local counter.
/// A miss / unreadable row => 0.0 (the gate then admits; the row is created on
/// the first successful debit).
async fn month_spend_usd<C: surrealdb::Connection>(db: &surrealdb::Surreal<C>, client: &str, month: &str) -> f64 {
    let id = format!("{client}:{month}");
    let rows: Result<Vec<Value>, _> = db
        .query("SELECT spent_usd FROM type::record('or_spend_counter', $id)")
        .bind(("id", id))
        .await
        .and_then(|mut r| r.take(0));
    rows.ok()
        .and_then(|r| r.into_iter().next())
        .and_then(|row| row.get("spent_usd").and_then(|v| v.as_f64()))
        .unwrap_or(0.0)
}

/// Add `cost_usd` (and one call) to the month-to-date counter (node-local,
/// UPSERT so the first debit of a month creates the row). Best-effort: a failure
/// is logged, never surfaced to the client (the OR call already succeeded).
async fn add_spend<C: surrealdb::Connection>(db: &surrealdb::Surreal<C>, client: &str, month: &str, cost_usd: f64) {
    let id = format!("{client}:{month}");
    let res = db
        .query(
            "UPSERT type::record('or_spend_counter', $id) SET \
             spent_usd = (spent_usd ?? 0) + $cost, \
             calls = (calls ?? 0) + 1, \
             client = $c, month = $m, updated_at = time::now()",
        )
        .bind(("id", id))
        .bind(("cost", cost_usd))
        .bind(("c", client.to_string()))
        .bind(("m", month.to_string()))
        .await
        .and_then(|r| r.check());
    if let Err(e) = res {
        tracing::warn!("[or-serve] spend-counter update failed for {client}: {e}");
    }
}

/// Append one usage-ledger row (node-local, the seed of the 9eck billing ledger).
#[allow(clippy::too_many_arguments)]
async fn record_ledger<C: surrealdb::Connection>(
    db: &surrealdb::Surreal<C>,
    client: &str,
    model: &str,
    provider: &str,
    prompt: i64,
    completion: i64,
    total: i64,
    cost_usd: f64,
    estimated: bool,
) {
    let res = db
        .query(
            "CREATE or_usage_ledger CONTENT { \
             client: $c, model: $m, provider: $prov, \
             prompt: $p, completion: $cp, total: $t, cost_usd: $cost, estimated: $est, ts: time::now() }",
        )
        .bind(("c", client.to_string()))
        .bind(("m", model.to_string()))
        .bind(("prov", provider.to_string()))
        .bind(("p", prompt))
        .bind(("cp", completion))
        .bind(("t", total))
        .bind(("cost", cost_usd))
        .bind(("est", estimated))
        .await
        .and_then(|r| r.check());
    if let Err(e) = res {
        tracing::warn!("[or-serve] usage_ledger write failed for {client}: {e}");
    }
}

/// Internal error shape: `(status, {"error": msg})`. Both transports (the HTTP
/// handler and the relay consumer) build their reply from this same pair.
fn err_val(code: StatusCode, msg: &str) -> (StatusCode, Value) {
    (code, json!({ "error": msg }))
}

/// `POST /api/ai/or`. Thin HTTP transport over [`serve_or_request`]: lift the
/// paid license out of `Authorization: Bearer`, run the shared serving logic,
/// and render its `(status, json)` as an axum `Response`.
pub async fn serve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // AUTH: the client's paid mesh license rides in `Authorization: Bearer`
    // (the body must reach OpenRouter verbatim, so the credential can't live in
    // it). The relay transport carries the same token inside its envelope; both
    // hand it to `serve_or_request`, which verifies it identically.
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let (code, val) = serve_or_request(&state, &token, body).await;
    (code, Json(val)).into_response()
}

/// Verify a paid mesh-license bearer EXACTLY as [`serve_or_request`] does — the
/// same `eck_core::licensing::verify` against the embedded issuer pubkey plus the
/// `is_paid()` tier gate — returning the [`LicenseClaims`] (`sub` = client id) on
/// success or the identical `(status, message)` rejection the OpenRouter proxy
/// uses (`401 missing/rejected`, `403 not a paid tier`).
///
/// Shared so other license-gated routes (e.g. `POST /api/pii/spans`, which a weak
/// node calls with its paid license) authenticate on this ONE seam instead of
/// re-implementing the checks and drifting from the rejections.
pub fn verify_paid_license(
    license_bearer: &str,
) -> Result<eck_core::licensing::LicenseClaims, (StatusCode, &'static str)> {
    let token = license_bearer.trim();
    if token.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "missing Authorization: Bearer <license>"));
    }
    let now = chrono::Utc::now().timestamp();
    match eck_core::licensing::verify(
        &eck_core::licensing::node_issuer_pubkey(),
        token,
        now,
        eck_core::licensing::DEFAULT_GRACE_SECS,
    ) {
        Ok(c) if c.is_paid() => Ok(c),
        Ok(_) => Err((StatusCode::FORBIDDEN, "license is not a paid tier")),
        Err(_) => Err((StatusCode::UNAUTHORIZED, "license rejected")),
    }
}

/// Transport-agnostic serving core (`.eck/OPENROUTER_TIER.md` §10/§11). Verifies
/// the paid `license_bearer` EXACTLY as the HTTP handler always did
/// (`eck_core::licensing::verify` + `is_paid` against the embedded issuer
/// pubkey), applies the allowlist / per-client deny policy / month-cap gate,
/// forwards the body VERBATIM upstream, meters, and returns `(status, json)`.
///
/// Called by BOTH the `POST /ai/or` HTTP handler and the relay consumer
/// (`services::or_relay_poller`): the credential is verified here, so where the
/// request arrived from — a LAN HTTP POST or a relay `ai_task` envelope — never
/// changes the decision. The returned `json` is the upstream OpenRouter/Z.ai
/// response verbatim on success (the client parses `usage.cost`/`provider`), or
/// `{"error": …}` on any gate rejection.
pub async fn serve_or_request(
    state: &Arc<AppState>,
    license_bearer: &str,
    body: Value,
) -> (StatusCode, Value) {
    let started = std::time::Instant::now();

    let claims = match verify_paid_license(license_bearer) {
        Ok(c) => c,
        Err((code, msg)) => return err_val(code, msg),
    };
    let client = claims.sub.clone();
    // Wall clock for the per-client month-cap gate below (the license validity
    // window was already checked inside `verify_paid_license`).
    let now = chrono::Utc::now().timestamp();

    // Our master OpenRouter key (never in code, never on cloud relays).
    let or_key = match std::env::var("ECK_OR_MASTER_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => return err_val(StatusCode::SERVICE_UNAVAILABLE, "OpenRouter proxy not configured (ECK_OR_MASTER_KEY unset)"),
    };

    // Model: from the verbatim body. Allowlist (cost control) then per-client policy.
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if model.is_empty() {
        return err_val(StatusCode::BAD_REQUEST, "request body missing `model`");
    }
    if !or_model_allowed(&model, &or_allowed_models()) {
        return err_val(StatusCode::BAD_REQUEST, "model not in the OpenRouter allowlist");
    }
    let denied = fetch_denied_prefixes(&state.db, &client).await;
    if or_model_denied_by_policy(&model, &denied) {
        return err_val(StatusCode::FORBIDDEN, "model_denied_by_client_policy");
    }

    // GATE: per-client USD/month cap (transitional, no wallet yet).
    let (caps, default_cap) = parse_client_caps(&std::env::var("ECK_OR_CLIENT_CAPS").unwrap_or_default());
    let cap = cap_for(&caps, default_cap, &client);
    let month = month_key(now);
    let spent = month_spend_usd(&state.db, &client, &month).await;
    if over_cap(spent, cap) {
        tracing::warn!("[or-serve] BLOCK {client} — month-to-date ${spent:.4} >= cap ${cap:.4}");
        return err_val(StatusCode::PAYMENT_REQUIRED, "monthly OpenRouter cap reached — contact support");
    }

    // Forward the body VERBATIM to OpenRouter under our key.
    let timeout = std::time::Duration::from_secs(
        std::env::var("ECK_OR_TIMEOUT_SECS").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(DEFAULT_TIMEOUT_SECS).max(120),
    );
    let http = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("[or-serve] http client build failed: {e}");
            return err_val(StatusCode::INTERNAL_SERVER_ERROR, "proxy client unavailable");
        }
    };
    // Upstream endpoint + bearer. Default = OpenRouter; `ECK_OR_UPSTREAM_URL`
    // lets the xelth commerce node point serving straight at Z.ai's
    // OpenAI-compatible endpoint (https://api.z.ai/api/paas/v4/chat/completions)
    // — no US transit. A SLUG model (name contains `/`) can't be served by that
    // vendor-direct upstream, so with `ECK_OR_SLUG_MASTER_KEY` set it routes to
    // the SECOND upstream (`ECK_OR_SLUG_UPSTREAM_URL`) under the slug key; the
    // metering below is identical (usage.cost still wins over the price table).
    let (upstream, or_key) = match serve_slug_upstream_for(
        &model,
        &std::env::var("ECK_OR_SLUG_MASTER_KEY").unwrap_or_default(),
        &std::env::var("ECK_OR_SLUG_UPSTREAM_URL").unwrap_or_default(),
    ) {
        Some((url, key)) => (url, key),
        None => (
            std::env::var("ECK_OR_UPSTREAM_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://openrouter.ai/api/v1/chat/completions".to_string()),
            or_key,
        ),
    };
    let req_bytes = serde_json::to_vec(&body).map(|v| v.len()).unwrap_or(0);
    let res = http
        .post(&upstream)
        .header("Content-Type", "application/json")
        .bearer_auth(&or_key)
        .json(&body)
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("[or-serve] upstream call failed for {client} (model={model}): {e}");
            return err_val(StatusCode::BAD_GATEWAY, "OpenRouter request failed");
        }
    };
    let status = res.status();
    let out: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("[or-serve] unparsable upstream body for {client} (model={model}): {e}");
            return err_val(StatusCode::BAD_GATEWAY, "OpenRouter returned an unreadable response");
        }
    };

    // Meter only a successful generation. On an upstream error, pass the status
    // and body straight back and charge nothing.
    if status.is_success() {
        let usage = out.get("usage");
        let prompt = usage.and_then(|u| u.get("prompt_tokens")).and_then(|v| v.as_i64()).unwrap_or(0);
        let completion = usage.and_then(|u| u.get("completion_tokens")).and_then(|v| v.as_i64()).unwrap_or(0);
        let total = usage
            .and_then(|u| u.get("total_tokens"))
            .and_then(|v| v.as_i64())
            .unwrap_or(prompt + completion);
        let real_cost = usage.and_then(|u| u.get("cost")).and_then(|v| v.as_f64()).filter(|c| *c >= 0.0);
        let provider = out.get("provider").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        // Real upstream `usage.cost` (OpenRouter) always wins. Z.ai-direct omits
        // it => estimate from the price table so the USD cap actually accumulates;
        // the ledger row is flagged `estimated`. Only when the model is ALSO
        // absent from the price table do we warn and record $0.
        let (cost, estimated) = match real_cost {
            Some(c) => (c, false),
            None => match estimate_cost_usd(&model, prompt, completion, &or_price_table()) {
                Some(est) => (est, true),
                None => {
                    tracing::warn!("[or-serve] no usage.cost and model '{model}' not in ECK_OR_PRICES for {client} — recording $0");
                    (0.0, false)
                }
            },
        };
        record_ledger(&state.db, &client, &model, &provider, prompt, completion, total, cost, estimated).await;
        add_spend(&state.db, &client, &month, cost).await;
        tracing::info!(
            "[or-serve] ok client={client} model={model} prov={provider} req_bytes={req_bytes} prompt={prompt} completion={completion} cost_usd={cost:.6} latency_ms={}",
            started.elapsed().as_millis()
        );
    } else {
        tracing::warn!(
            "[or-serve] upstream {status} for {client} (model={model}, req_bytes={req_bytes}, latency_ms={}) — no charge",
            started.elapsed().as_millis()
        );
    }

    // Response passed through verbatim (client needs usage.cost + provider).
    let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_exact_match_only() {
        let allow = vec!["z-ai/glm-5.3-flash".to_string(), "qwen/qwen3.5-flash-02-23".to_string()];
        assert!(or_model_allowed("z-ai/glm-5.3-flash", &allow));
        assert!(or_model_allowed("qwen/qwen3.5-flash-02-23", &allow));
        // Not an exact match => rejected (our cost control).
        assert!(!or_model_allowed("z-ai/glm-5.3-pro", &allow));
        assert!(!or_model_allowed("openai/gpt-5", &allow));
        assert!(!or_model_allowed("z-ai/", &allow));
        // Default list (env unset) accepts the unprefixed Z.ai-direct name too.
        let dflt = or_allowed_models();
        assert!(or_model_allowed("glm-5.3-flash", &dflt), "default allows z.ai-direct glm-5.3-flash");
        assert!(or_model_allowed("z-ai/glm-5.3-flash", &dflt));
    }

    #[test]
    fn serve_slug_upstream_split() {
        // Unprefixed model ⇒ never (primary upstream path, byte-identical).
        assert_eq!(serve_slug_upstream_for("glm-5.3-flash", "sk-slug", ""), None);
        // Slug model but no slug key ⇒ never.
        assert_eq!(serve_slug_upstream_for("deepseek/deepseek-v4.1-flash", " ", "x"), None);
        // Slug model + key, empty upstream ⇒ public OpenRouter endpoint.
        assert_eq!(
            serve_slug_upstream_for("deepseek/deepseek-v4.1-flash", "sk-slug", ""),
            Some(("https://openrouter.ai/api/v1/chat/completions".to_string(), "sk-slug".to_string()))
        );
        // Explicit upstream trimmed and carried with the key.
        assert_eq!(
            serve_slug_upstream_for("deepseek/deepseek-v4.1-flash", " sk ", " https://or.example/v1/chat/completions "),
            Some(("https://or.example/v1/chat/completions".to_string(), "sk".to_string()))
        );
        // The default allowlist + price table cover the alt slug model.
        assert!(or_model_allowed("deepseek/deepseek-v4.1-flash-1017", &or_allowed_models()));
        assert!(or_model_allowed("deepseek/deepseek-v4.1-flash", &or_allowed_models()), "undated slug stays allowed for mixed-version fleets");
        assert_eq!(
            or_price_table().get("deepseek/deepseek-v4.1-flash-1017").copied(),
            Some((0.15, 0.60))
        );
    }

    #[test]
    fn client_policy_prefix_deny() {
        let denied = vec!["z-ai/".to_string(), "qwen/".to_string()];
        assert!(or_model_denied_by_policy("z-ai/glm-5.3-flash", &denied));
        assert!(or_model_denied_by_policy("qwen/qwen3.5-flash-02-23", &denied));
        assert!(!or_model_denied_by_policy("openai/gpt-5", &denied));
        // Empty / whitespace-only deny-list => nothing denied.
        assert!(!or_model_denied_by_policy("z-ai/glm-5.3-flash", &[]));
        assert!(!or_model_denied_by_policy("z-ai/glm-5.3-flash", &["  ".to_string()]));
    }

    #[test]
    fn caps_parse_and_lookup() {
        let (caps, default) = parse_client_caps("mesh-abc=20, default=10, garbage, bad=oops");
        assert_eq!(cap_for(&caps, default, "mesh-abc"), 20.0);
        assert_eq!(cap_for(&caps, default, "unlisted-client"), 10.0, "falls back to default=");
        // Unparsable entries skipped, `bad=oops` did not poison the map.
        assert!(caps.get("bad").is_none());
        // No default= entry => DEFAULT_CAP_USD.
        let (caps2, default2) = parse_client_caps("mesh-x=3");
        assert_eq!(default2, DEFAULT_CAP_USD);
        assert_eq!(cap_for(&caps2, default2, "nobody"), DEFAULT_CAP_USD);
        // Empty spec => empty map + default cap.
        let (caps3, default3) = parse_client_caps("");
        assert!(caps3.is_empty());
        assert_eq!(default3, DEFAULT_CAP_USD);
    }

    #[test]
    fn over_cap_boundary() {
        assert!(!over_cap(0.0, 5.0));
        assert!(!over_cap(4.99, 5.0));
        assert!(over_cap(5.0, 5.0), "at the cap blocks");
        assert!(over_cap(9.0, 5.0));
        // A zero cap blocks everything (kill-switch via ECK_OR_CLIENT_CAPS).
        assert!(over_cap(0.0, 0.0));
    }

    #[test]
    fn month_key_bucket_and_rollover() {
        // A late-August 2026 instant and an early-August one share one bucket…
        let aug = 1787000000; // within August 2026 (UTC)
        let aug_key = month_key(aug);
        assert_eq!(&aug_key, "2026-08");
        // …but an instant ~40 days later rolls over to a NEW bucket => a fresh
        // counter row that starts at zero (no sweep; the id carries the month).
        let later = aug + 40 * 24 * 3600;
        let later_key = month_key(later);
        assert_eq!(&later_key, "2026-09");
        assert_ne!(aug_key, later_key, "month rollover => different counter key => reset");
    }

    #[test]
    fn price_table_parse_and_estimate() {
        let t = parse_price_table("glm-5.3-flash=0.075:0.25, z-ai/glm-5.3-flash=0.075:0.25, junk, bad=x:y");
        assert_eq!(t.get("glm-5.3-flash").copied(), Some((0.075, 0.25)));
        assert_eq!(t.get("z-ai/glm-5.3-flash").copied(), Some((0.075, 0.25)));
        assert!(t.get("bad").is_none(), "unparsable price skipped");
        // 1M in + 1M out => 0.075 + 0.25 = 0.325.
        let c = estimate_cost_usd("glm-5.3-flash", 1_000_000, 1_000_000, &t).unwrap();
        assert!((c - 0.325).abs() < 1e-9, "got {c}");
        // A realistic small call: 2452 in, 717 out.
        let c2 = estimate_cost_usd("glm-5.3-flash", 2452, 717, &t).unwrap();
        let want = 2452.0 / 1e6 * 0.075 + 717.0 / 1e6 * 0.25;
        assert!((c2 - want).abs() < 1e-12);
        // Unknown model => None (caller warns + records $0).
        assert!(estimate_cost_usd("openai/gpt-5", 100, 100, &t).is_none());
        // The default table (env unset) prices both glm names.
        let dflt = or_price_table();
        assert!(dflt.contains_key("glm-5.3-flash") && dflt.contains_key("z-ai/glm-5.3-flash"));
    }

    /// Live DB-backed check against an embedded mem SurrealDB (same pattern as
    /// the embeddings tests): the queries must actually EXECUTE — this is what
    /// caught the `type::thing` -> `type::record` v3 parse error. Covers the
    /// UPSERT spend counter (incl. read-back + month isolation), the ledger
    /// CREATE (incl. the `estimated` flag), and the policy SELECT.
    #[tokio::test]
    async fn db_ops_execute_on_mem_surreal() {
        let db = surrealdb::Surreal::new::<surrealdb::engine::local::Mem>(())
            .await
            .unwrap();
        db.use_ns("test").use_db("test").await.unwrap();

        // A client id with the special chars that broke the naive query shape:
        // a UUID (dashes) plus the `:month` suffix inside the record id.
        let client = "6f38a1c2-dead-beef-0000-000000000001";
        let month = "2026-08";

        // Fresh counter reads 0 (no row yet).
        assert_eq!(month_spend_usd(&db, client, month).await, 0.0);

        // Two UPSERT debits accumulate.
        add_spend(&db, client, month, 0.0125).await;
        add_spend(&db, client, month, 0.0075).await;
        let spent = month_spend_usd(&db, client, month).await;
        assert!((spent - 0.02).abs() < 1e-9, "two debits accumulate; got {spent}");

        // A different month is a SEPARATE row (rollover reset).
        assert_eq!(month_spend_usd(&db, client, "2026-09").await, 0.0);

        // Ledger CREATE executes and the row carries the estimated flag + cost.
        record_ledger(&db, client, "glm-5.3-flash", "z.ai", 2452, 717, 3169, 0.000363, true).await;
        let rows: Vec<Value> = db
            .query("SELECT cost_usd, estimated, model, prompt FROM or_usage_ledger")
            .await
            .unwrap()
            .take(0)
            .unwrap();
        assert_eq!(rows.len(), 1, "one ledger row");
        assert_eq!(rows[0].get("estimated").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(rows[0].get("model").and_then(|v| v.as_str()), Some("glm-5.3-flash"));
        assert_eq!(rows[0].get("prompt").and_then(|v| v.as_i64()), Some(2452));

        // Policy SELECT: absent row => empty; a written row => its prefixes.
        assert!(fetch_denied_prefixes(&db, client).await.is_empty());
        db.query("CREATE type::record('or_client_policy', $c) CONTENT { models_denied: ['z-ai/', 'qwen/'] }")
            .bind(("c", client.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap();
        let denied = fetch_denied_prefixes(&db, client).await;
        assert_eq!(denied, vec!["z-ai/".to_string(), "qwen/".to_string()]);
    }
}
