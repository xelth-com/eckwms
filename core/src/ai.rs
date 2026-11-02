//! Dual-mode Gemini auth — the one place the Studio-vs-Vertex split lives.
//!
//! `studio` (DEFAULT, open-source) = the user's own AI Studio key via
//! `generativelanguage…` + `x-goog-api-key` header. `managed` (paid) = a short-lived Vertex Bearer
//! (minted server-side) hitting `aiplatform.googleapis.com` with `Authorization:
//! Bearer`. Both go through `generate_content` / `embed_content` so every call
//! site stays mode-agnostic. Full design: `.eck/AI_DUAL_PROVIDER_VERTEX.md`.

use anyhow::Result;
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Gemini 3.x is global-only on Vertex; default the region accordingly.
const DEFAULT_VERTEX_LOCATION: &str = "global";

/// THIS node's id (the sync-engine instance UUID), stamped onto every managed
/// usage report so the token authority's `usage_ledger` says WHICH node produced
/// a row. Set once at startup by [`set_ai_node_id`]; unset → reports send
/// `"node": null`.
static AI_NODE_ID: OnceLock<String> = OnceLock::new();

/// Register this node's id for usage attribution. Idempotent — the first value
/// wins (OnceLock semantics), later calls are ignored. Call it once at startup
/// with the sync-engine instance UUID (see wms `main`).
pub fn set_ai_node_id(id: &str) {
    let _ = AI_NODE_ID.set(id.to_string());
}

/// This node's id if [`set_ai_node_id`] has run, else `None` (report `node:null`).
pub fn ai_node_id() -> Option<String> {
    AI_NODE_ID.get().cloned()
}

tokio::task_local! {
    /// The named AI task in scope for the current async task. Stamped onto every
    /// managed usage report (see [`report_managed_usage`]) so the authority's
    /// ledger says WHICH task produced a row. Scoped by [`with_ai_task`]; when
    /// unset a report sends `"task": "unlabelled"`. NOTE: task-locals do NOT
    /// cross `tokio::spawn` — a spawned future must set its own scope inside.
    pub static AI_TASK: String;
}

/// Run `fut` with [`AI_TASK`] set to `task`, so any usage report fired inside it
/// (directly, or via a nested await that is NOT `tokio::spawn`ed) is labelled
/// with `task`. To label work that runs on a spawned task, call this INSIDE the
/// spawned future — the scope does not cross the spawn boundary.
pub async fn with_ai_task<F: Future>(task: &str, fut: F) -> F::Output {
    AI_TASK.scope(task.to_string(), fut).await
}

/// The AI task label for the current scope, or `"unlabelled"` when none is set.
fn current_ai_task() -> String {
    AI_TASK
        .try_with(|t| t.clone())
        .unwrap_or_else(|_| "unlabelled".to_string())
}

/// Refresh the managed token this many seconds before its stated expiry, so a
/// call never races a token that dies mid-flight.
const TOKEN_REFRESH_MARGIN_SECS: u64 = 30;

/// A managed Vertex Bearer cached process-wide. Minted from the token authority
/// (`ECK_VERTEX_MINT_URL`) against the 9eck license; refreshed on near-expiry.
struct CachedToken {
    bearer: String,
    project: String,
    location: String,
    fetched_at: Instant,
    ttl: Duration,
}

impl CachedToken {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() + Duration::from_secs(TOKEN_REFRESH_MARGIN_SECS) < self.ttl
    }
}

/// Process-wide backoff after the token authority refuses a mint. Without this
/// every AI worker (embeddings, summarization, translation, observer, distiller)
/// re-mints on its own poll cycle, so an exhausted-allowance (HTTP 402) or auth
/// failure turns into a fleet-wide hammer — the kiosk once retried every 2–10s
/// for three days against a 402. When the authority says "no", we stop asking
/// for a while (exponential, capped) instead of pounding it; the first success
/// clears the backoff.
struct MintBackoff {
    /// Instant before which `resolve()` must NOT attempt a mint.
    until: Option<Instant>,
    /// Consecutive failures, drives the exponential delay.
    consecutive: u32,
}

fn mint_backoff() -> &'static Mutex<MintBackoff> {
    static BO: OnceLock<Mutex<MintBackoff>> = OnceLock::new();
    BO.get_or_init(|| Mutex::new(MintBackoff { until: None, consecutive: 0 }))
}

/// Record a failed mint and arm the backoff. `hard` = a 402/allowance or auth
/// (401/403) refusal that won't self-heal in seconds → longer floor; otherwise
/// (transient 5xx / network) a short delay just to avoid a tight loop. Delay is
/// exponential in the consecutive-failure count, capped at 1h, with jitter so a
/// restarted fleet doesn't retry in lockstep.
/// Pure exponential-backoff ladder: `base · 2^(consecutive-1)`, capped at 1h.
/// `base` is 60s for a hard (402/auth) refusal, 15s for a transient error. The
/// doubling is capped at 2^6 so the multiply can't overflow before the min().
fn backoff_delay_secs(consecutive: u32, hard: bool) -> u64 {
    let base: u64 = if hard { 60 } else { 15 };
    // Cap the shift well below 64 (avoids a shift-overflow panic); saturating_mul
    // + min(3600) do the real capping, so both ladders climb to the 1h ceiling.
    let shift = consecutive.saturating_sub(1).min(20);
    base.saturating_mul(1u64 << shift).min(3600)
}

/// Send a request with up to 4 attempts on transient failures — network
/// errors and HTTP 429/5xx. 5xx/network waits 0.5s → 1s → 2s; 429 is a
/// RATE limit with minute-scale windows, so it honours `Retry-After` when
/// present and otherwise waits 2s → 8s → 20s (the old 0.5s ladder just
/// burned all attempts inside the same limit window — 2026-07-09: 72 docs
/// went 'error' on a single 429 burst). Non-transient responses return
/// immediately so callers keep their own status handling. Bodies here are
/// small JSON, so `try_clone()` always succeeds; if it ever can't
/// (streaming body), the request simply isn't retried.
async fn send_with_retry(req: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    const ATTEMPTS: u32 = 4;
    let mut current = req;
    for attempt in 1..=ATTEMPTS {
        let next = if attempt < ATTEMPTS { current.try_clone() } else { None };
        let mut delay = Duration::from_millis(500u64 * (1 << (attempt - 1)));
        match current.send().await {
            Ok(resp) => {
                let status = resp.status();
                let transient = status.as_u16() == 429 || status.is_server_error();
                match (transient, next) {
                    (true, Some(n)) => {
                        if status.as_u16() == 429 {
                            // Retry-After (seconds form) wins; else 2s/8s/20s.
                            let ra = resp
                                .headers()
                                .get(reqwest::header::RETRY_AFTER)
                                .and_then(|v| v.to_str().ok())
                                .and_then(|s| s.trim().parse::<u64>().ok());
                            delay = Duration::from_secs(
                                ra.unwrap_or(match attempt { 1 => 2, 2 => 8, _ => 20 }).min(30),
                            );
                        }
                        tracing::debug!(attempt, %status, delay_ms = delay.as_millis() as u64, "AI request transient error; retrying");
                        current = n;
                    }
                    _ => return Ok(resp),
                }
            }
            Err(e) => match next {
                Some(n) => {
                    tracing::debug!(attempt, error = %e, "AI request network error; retrying");
                    current = n;
                }
                None => return Err(e),
            },
        }
        tokio::time::sleep(delay).await;
    }
    unreachable!("every last-attempt branch returns above")
}

async fn note_mint_failure(hard: bool) {
    let mut bo = mint_backoff().lock().await;
    bo.consecutive = bo.consecutive.saturating_add(1);
    let secs = backoff_delay_secs(bo.consecutive, hard);
    // Cheap jitter (0..secs/4) from the clock — avoids pulling in `rand`.
    let jitter = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0))
        % (secs / 4 + 1);
    bo.until = Some(Instant::now() + Duration::from_secs(secs + jitter));
    tracing::warn!(
        "managed AI mint backing off {}s after {} consecutive authority failure(s)",
        secs + jitter,
        bo.consecutive
    );
}

/// Clear the backoff after a successful mint.
async fn clear_mint_backoff() {
    let mut bo = mint_backoff().lock().await;
    bo.until = None;
    bo.consecutive = 0;
}

/// Remaining backoff in seconds if a mint is currently suppressed, else `None`.
async fn mint_backoff_remaining() -> Option<u64> {
    let bo = mint_backoff().lock().await;
    bo.until.and_then(|u| {
        let now = Instant::now();
        (now < u).then(|| (u - now).as_secs())
    })
}

fn token_cache() -> &'static Mutex<Option<CachedToken>> {
    static CACHE: OnceLock<Mutex<Option<CachedToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Authoritative token-balance snapshot returned by the token authority on each
/// mint (managed mode). Cached process-wide so the dashboard can render the real
/// "will the budget last to period end?" forecast instead of a local guess.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct QuotaSnapshot {
    pub tokens_remaining: i64,
    pub monthly_grant: i64,
    pub loan_outstanding: i64,
    /// v3 plan engine: bought extra-usage tokens, drawn only after the monthly
    /// grant hits 0; carries across window rolls. 0 from a v2 authority.
    pub extra_balance: i64,
    /// v3 plan engine: credit ceiling (`loan_outstanding` is the amount drawn).
    /// 0 from a v2 authority.
    pub credit_limit: i64,
    /// RFC3339 instant when the current 30-day window refills.
    pub period_ends_at: String,
    /// Unix seconds when this snapshot was fetched (to age-correct `remaining`
    /// against locally-metered burn since the mint).
    pub fetched_at_unix: i64,
}

fn quota_cache() -> &'static Mutex<Option<QuotaSnapshot>> {
    static CACHE: OnceLock<Mutex<Option<QuotaSnapshot>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Latest token-balance snapshot from the authority, if managed mode has minted
/// at least once this process. `None` in studio mode or before the first mint.
pub async fn last_quota() -> Option<QuotaSnapshot> {
    quota_cache().lock().await.clone()
}

/// Store the balance snapshot the authority piggybacks on mint AND usage
/// responses. `monthly_grant > 0` gates it so an older authority that omits
/// these fields doesn't poison the cache. The wallet is per-MESH (shared by
/// all nodes), so the freshest authority word always wins over local guesses.
async fn absorb_quota_snapshot(j: &Value) {
    if let Some(grant) = j["monthly_grant"].as_i64().filter(|g| *g > 0) {
        let snap = QuotaSnapshot {
            tokens_remaining: j["tokens_remaining"].as_i64().unwrap_or(0),
            monthly_grant: grant,
            loan_outstanding: j["loan_outstanding"].as_i64().unwrap_or(0),
            extra_balance: j["extra_balance"].as_i64().unwrap_or(0),
            credit_limit: j["credit_limit"].as_i64().unwrap_or(0),
            period_ends_at: j["period_ends_at"].as_str().unwrap_or_default().to_string(),
            fetched_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        };
        *quota_cache().lock().await = Some(snap);
    }
}

/// Refresh the balance snapshot from the authority when the cached one is
/// missing or older than `max_age_secs`. Used by the dashboard gauge so every
/// node shows the SAME mesh-wallet figure (the wallet is shared; a node that
/// hasn't minted or called AI recently would otherwise drift on a stale
/// snapshot corrected only by its own local burn). Implemented as a
/// zero-token usage report — the authority debits nothing and returns the
/// current balance. No-op outside managed mode.
pub async fn refresh_quota_if_stale(max_age_secs: i64) -> Option<QuotaSnapshot> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Some(q) = quota_cache().lock().await.clone() {
        if now - q.fetched_at_unix <= max_age_secs {
            return Some(q);
        }
    }
    let url = std::env::var("ECK_VERTEX_USAGE_URL").ok().filter(|s| !s.is_empty())?;
    let license = std::env::var("ECK_LICENSE_TOKEN").ok().filter(|s| !s.is_empty())?;
    let http = reqwest::Client::new();
    let resp = http
        .post(&url)
        .json(&json!({
            "license": license,
            "model": "",
            "kind": "balance_check",
            "prompt_tokens": 0,
            "candidates_tokens": 0,
            "total_tokens": 0,
        }))
        .send()
        .await
        .ok()?;
    let j: Value = resp.json().await.ok()?;
    absorb_quota_snapshot(&j).await;
    quota_cache().lock().await.clone()
}

#[derive(Clone, Debug)]
pub enum AiAuth {
    /// Open-source / self-hosted: the user's own AI Studio key. UNCHANGED behaviour.
    Studio { api_key: String },
    /// Paid/SaaS: a short-lived Vertex Bearer minted by our server (+ routing).
    Vertex {
        bearer: String,
        project: String,
        location: String,
    },
    /// OpenRouter cheap background tier (`.eck/OPENROUTER_TIER.md`). The `model`
    /// travels INSIDE the variant — unlike Studio/Vertex, where the model is a
    /// per-call argument — because an OR *route* is one decision: "this task goes
    /// to auth+model X". Produced ONLY by [`AiAuth::resolve_for_task`], never by
    /// `from_env`/`resolve`, so every existing caller and the mint/backoff logic
    /// stay untouched. The Gemini↔OpenAI-chat translation lives entirely in
    /// `generate_content_raw` so no caller learns this variant exists.
    ///
    /// `proxied` selects the transport: `false` = BYOK, a DIRECT call to
    /// OpenRouter (`api_key` is a node-local OpenRouter key); `true` = the MANAGED
    /// path, where `api_key` is the paid `ECK_LICENSE_TOKEN` and the call goes to
    /// the xelixir `/ai/or` proxy (`ECK_OR_PROXY_URL`), which holds the real OR
    /// key and meters usage.cost at the wallet seam. The flag is decided ONCE in
    /// `resolve_for_task` (where all the env context is in hand) and carried
    /// explicitly rather than re-derived from env at URL-build time — this keeps
    /// the auth value self-describing and immune to an env change mid-flight, and
    /// (crucially) keeps the BYOK URL path byte-identical to before.
    OpenRouter {
        api_key: String,
        model: String,
        proxied: bool,
        /// Relay transport (native paid-mesh AI, `.eck/OPENROUTER_TIER.md` §10):
        /// when `Some(mesh_uuid)`, the request does NOT go out over HTTP — it is
        /// enqueued on the eck relay's `/E/ai/*` queue addressed to that serving
        /// node, which runs the same `serve_or_request` and acks the response.
        /// `api_key` still carries the paid `ECK_LICENSE_TOKEN` (verified on the
        /// serving node). `None` = the HTTP transports (`proxied` then selects
        /// BYOK-direct vs the `ECK_OR_PROXY_URL` proxy) — both BYTE-IDENTICAL to
        /// before this field existed. Chosen ONCE in `resolve_for_task`.
        serve_node: Option<String>,
        /// Per-request BYOK base-URL override (bench only). `None` = read the
        /// host from `ECK_OR_BASE_URL` at call time, BYTE-IDENTICAL to before
        /// this field existed — every production construction passes `None`.
        /// `Some(url)` points ONE request at a different OpenRouter-compatible
        /// endpoint (e.g. openrouter.ai for a bench leg) WITHOUT touching the
        /// process env, so a bench route can never leak into normal traffic.
        /// Ignored on the `proxied`/`serve_node` transports (they don't build a
        /// direct OR URL); only the BYOK-direct path in `generate_url` reads it.
        base_url: Option<String>,
    },
}

impl AiAuth {
    /// Resolve from env. `ECK_AI_MODE=managed` (+ `ECK_VERTEX_*`) → Vertex; else
    /// Studio from `GEMINI_API_KEY`. Studio is the default — the open-source path.
    pub fn from_env() -> Self {
        let mode = std::env::var("ECK_AI_MODE")
            .unwrap_or_default()
            .to_lowercase();
        if mode == "managed" {
            return AiAuth::Vertex {
                bearer: std::env::var("ECK_VERTEX_BEARER").unwrap_or_default(),
                project: std::env::var("ECK_VERTEX_PROJECT").unwrap_or_default(),
                location: std::env::var("ECK_VERTEX_LOCATION")
                    .unwrap_or_else(|_| DEFAULT_VERTEX_LOCATION.to_string()),
            };
        }
        AiAuth::Studio {
            api_key: std::env::var("GEMINI_API_KEY").unwrap_or_default(),
        }
    }

    /// Live auth, minting on demand. Studio → immediate (from `GEMINI_API_KEY`).
    /// Managed → a process-cached Vertex Bearer, freshly minted from the token
    /// authority (`ECK_VERTEX_MINT_URL`, authed by `ECK_LICENSE_TOKEN`) on cache
    /// miss / near-expiry. A static `ECK_VERTEX_BEARER` (manual override / spike)
    /// short-circuits minting. Prefer this over `from_env` for any live call —
    /// `from_env` returns whatever static creds exist and never mints.
    pub async fn resolve(http: &HttpClient) -> Result<Self> {
        let mode = std::env::var("ECK_AI_MODE")
            .unwrap_or_default()
            .to_lowercase();
        if mode != "managed" {
            return Ok(AiAuth::Studio {
                api_key: std::env::var("GEMINI_API_KEY").unwrap_or_default(),
            });
        }

        // Manual override (pinned token / spike): trust env verbatim, no mint.
        let manual = std::env::var("ECK_VERTEX_BEARER").unwrap_or_default();
        if !manual.is_empty() {
            return Ok(AiAuth::Vertex {
                bearer: manual,
                project: std::env::var("ECK_VERTEX_PROJECT").unwrap_or_default(),
                location: std::env::var("ECK_VERTEX_LOCATION")
                    .unwrap_or_else(|_| DEFAULT_VERTEX_LOCATION.to_string()),
            });
        }

        // Cache hit?
        {
            let guard = token_cache().lock().await;
            if let Some(c) = guard.as_ref() {
                if c.is_fresh() {
                    return Ok(AiAuth::Vertex {
                        bearer: c.bearer.clone(),
                        project: c.project.clone(),
                        location: c.location.clone(),
                    });
                }
            }
        }

        // Respect the backoff: if the authority recently refused a mint, fail
        // fast WITHOUT touching it (the whole point — don't hammer). Callers
        // treat this like any mint failure and skip the AI call this cycle.
        if let Some(secs) = mint_backoff_remaining().await {
            anyhow::bail!("managed AI mint suppressed: backing off {}s after repeated authority failures", secs);
        }

        // Mint a fresh token and cache it.
        let minted = Self::mint_managed(http).await?;
        clear_mint_backoff().await;
        let auth = AiAuth::Vertex {
            bearer: minted.bearer.clone(),
            project: minted.project.clone(),
            location: minted.location.clone(),
        };
        *token_cache().lock().await = Some(minted);
        Ok(auth)
    }

    /// Resolve auth for a NAMED AI task (`.eck/OPENROUTER_TIER.md` §2). A task
    /// listed in `ECK_AI_TASK_ROUTES` (comma-separated `task=model`) routes to
    /// OpenRouter with that model; every other task falls through to
    /// [`AiAuth::resolve`] (Studio or managed Vertex, unchanged). Fail-open by
    /// construction: a route pointing at OR while `OPENROUTER_API_KEY` is empty
    /// logs ONE warning process-wide and falls back to the default auth — a
    /// mis-set route can never take the pipeline down, only fail to save money.
    /// Only wave-1 call sites (summarization) pass a routed task; the rest keep
    /// calling `resolve` and are untouched.
    pub async fn resolve_for_task(http: &HttpClient, task: &str) -> Result<Self> {
        if let Some(model) = or_route_for_task(task) {
            return Self::resolve_or_route(http, task, model).await;
        }
        Self::resolve(http).await
    }

    /// Like [`resolve_for_task`] but with the OR `model` PINNED by the caller
    /// (per-request brain engine, `.eck/OPENROUTER_TIER.md` §alt) rather than
    /// looked up in `ECK_AI_TASK_ROUTES`. Keeps the deny-policy check and every
    /// transport (slug upstream / BYOK / managed proxy / relay), so a pinned
    /// alt-engine model resolves exactly as a routed one would. A denied model
    /// falls back to the default auth just like a denied route.
    pub async fn resolve_for_task_with_model(
        http: &HttpClient,
        task: &str,
        model: &str,
    ) -> Result<Self> {
        Self::resolve_or_route(http, task, model.to_string()).await
    }

    /// Shared OR resolution for a KNOWN model (from a route or a pinned caller):
    /// deny-policy → slug upstream → BYOK → managed proxy/relay → fail-open to
    /// the default auth. `task` is used only for the once-per-task warnings.
    async fn resolve_or_route(http: &HttpClient, task: &str, model: String) -> Result<Self> {
        {
            // (Every path in this block returns; the block is a scope, not a value.)
            // Node-side model policy (security setting): a route whose model is
            // denied by `ECK_AI_MODEL_DENY` (comma-separated vendor prefixes) is
            // treated as NO route — fall through to the default auth. Logged once
            // per task so a standing policy doesn't spam every worker cycle. The
            // DB-backed UI toggle (a later wave) will feed the same pure check.
            if model_denied_by_policy(&model, &ai_model_deny_list()) {
                if or_task_denied_warned(task) {
                    tracing::warn!(
                        "[AI] task '{task}' route model '{model}' is denied by ECK_AI_MODEL_DENY — using default auth"
                    );
                }
                return Self::resolve(http).await;
            }

            // Slug upstream (`.eck/OPENROUTER_TIER.md` §slug), checked BEFORE the
            // `OPENROUTER_API_KEY`-empty/managed branch: a slug model (name
            // contains `/`) on a node with an `ECK_OR_SLUG_API_KEY` routes to a
            // SECOND, per-model BYOK upstream carrying its OWN key + base URL —
            // needed where the primary upstream is a vendor-direct endpoint (e.g.
            // Z.ai-direct) that can't serve arbitrary slugs. Checked before the
            // key branch so even a managed/relay node with a local slug key
            // reaches slug models BYOK. This upstream is paid in USD by the node
            // owner, so we carry `base_url: Some(..)` — which (as on the bench
            // leg) SUPPRESSES the managed-wallet report in `openrouter_generate`;
            // reporting would double-charge at the unknown-model ×10 multiplier.
            // Local telemetry (chat_turn cost, `[brain]` log) is unaffected — it
            // reads the returned usage, not the wallet seam. Unprefixed model or
            // no slug key ⇒ `None`, and everything below is BYTE-IDENTICAL.
            if let Some((base, slug_key)) = slug_upstream(&model) {
                return Ok(AiAuth::OpenRouter {
                    api_key: slug_key,
                    model,
                    proxied: false,
                    serve_node: None,
                    base_url: Some(base),
                });
            }

            let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
            if key.trim().is_empty() {
                // No node-local BYOK key. Managed path: in managed mode with a
                // proxy URL configured, route through the authority's `/ai/or`
                // proxy, carrying the paid license as the bearer (the proxy holds
                // the real OR key and meters usage.cost at the wallet seam).
                let managed = std::env::var("ECK_AI_MODE")
                    .unwrap_or_default()
                    .to_lowercase()
                    == "managed";
                let proxy = std::env::var("ECK_OR_PROXY_URL").unwrap_or_default();
                let serve_node = std::env::var("ECK_OR_SERVE_NODE").unwrap_or_default();
                if managed {
                    let license = std::env::var("ECK_LICENSE_TOKEN").unwrap_or_default();
                    if !license.trim().is_empty() {
                        // Transport precedence: an explicit HTTP proxy URL wins
                        // (it's the direct, lowest-latency managed path); else a
                        // configured serving-node mesh id routes over the relay
                        // (`/E/ai/*`) — the NATIVE paid-mesh path for nodes with
                        // no public proxy to dial.
                        if !proxy.trim().is_empty() {
                            return Ok(AiAuth::OpenRouter {
                                api_key: license,
                                model,
                                proxied: true,
                                serve_node: None,
                                base_url: None,
                            });
                        }
                        if !serve_node.trim().is_empty() {
                            return Ok(AiAuth::OpenRouter {
                                api_key: license,
                                model,
                                proxied: false,
                                serve_node: Some(serve_node.trim().to_string()),
                                base_url: None,
                            });
                        }
                    }
                    // managed but no license, or no proxy AND no serve-node →
                    // fall through + warn.
                }
                // Empty proxy env (or not managed / no license) → fail-open to
                // the default auth, exactly as before. Warn once process-wide.
                if !or_fallback_warned().swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "[AI] task '{task}' routes to OpenRouter (model={model}) but no OPENROUTER_API_KEY and no managed transport (ECK_AI_MODE=managed + ECK_LICENSE_TOKEN + one of ECK_OR_PROXY_URL / ECK_OR_SERVE_NODE) — falling back to default auth"
                    );
                }
                return Self::resolve(http).await;
            }
            // BYOK: direct OpenRouter call with the node-local key.
            return Ok(AiAuth::OpenRouter {
                api_key: key,
                model,
                proxied: false,
                serve_node: None,
                base_url: None,
            });
        }
    }

    /// The OpenRouter shim: translate a Gemini-shaped request into an OpenAI
    /// chat request, POST it, and translate the response back into the exact
    /// Gemini shape `generate_content_raw` promises its callers. Metering rides
    /// the SAME `report_usage` seam as Vertex (§4, kind "generate", model
    /// as-is). The per-model reasoning quirk and its 400-retry are handled here.
    async fn openrouter_generate(&self, http: &HttpClient, body: Value) -> Result<Value> {
        let (_api_key, model) = match self {
            AiAuth::OpenRouter { api_key, model, .. } => (api_key.as_str(), model.as_str()),
            _ => unreachable!("openrouter_generate on non-OR auth"),
        };
        // Request translation (incl. function-calling tools/history, if any).
        let base_req = or_build_request(model, &body)?;
        // Reasoning field (bench-verified per-model quirks; env-overridable).
        let reasoning = or_reasoning_field(model);
        let (first_req, retry_on_400) = match &reasoning {
            OrReasoning::Field(v, retry) => {
                let mut r = base_req.clone();
                r["reasoning"] = v.clone();
                (r, *retry)
            }
            // Top-level scalar (z.ai-direct glm): the ONLY control it honours.
            OrReasoning::ReasoningEffort(v) => {
                let mut r = base_req.clone();
                r["reasoning_effort"] = v.clone();
                (r, false)
            }
            OrReasoning::Omit => (base_req.clone(), false),
        };
        let (status, val, text) = self.or_dispatch(http, model, &first_req).await?;
        let resp_val = if status.is_success() {
            val
        } else if status.as_u16() == 400 && retry_on_400 {
            // Unknown model rejected `reasoning` (400) — retry ONCE without it.
            // Works over BOTH transports: the relay serving node returns the
            // upstream 400 status the same way a direct call does.
            tracing::debug!("openrouter: 400 with reasoning field for {model}; retrying without it");
            let (s2, v2, t2) = self.or_dispatch(http, model, &base_req).await?;
            if !s2.is_success() {
                anyhow::bail!("OpenRouter chat error ({s2}, model={model}): {t2}");
            }
            v2
        } else {
            anyhow::bail!("OpenRouter chat error ({status}, model={model}): {text}");
        };
        let translated = or_translate_response(&resp_val)?;
        dump_ai_exchange("or-generate", "openrouter", &self.generate_url(model), model, &first_req, &translated);
        // Meter every call here (§4) — same shape as the Vertex arm, so the
        // spend counters / mesh-wallet snapshot keep working. Weighted-unit mult
        // (default ×1 for OR) resolves server-side via XELTH_MODEL_TOKEN_MULT.
        if let Some(u) = translated.get("usageMetadata") {
            if !u.is_null() {
                // A per-request `base_url` override is a BENCH LEG dialing a
                // non-managed OpenRouter endpoint directly. Reporting that to the
                // token authority bills it to the managed wallet — a double
                // charge (2026-09-06 incident). Skip the report for override
                // calls; keep it for the normal relay/managed/BYOK path.
                let is_bench_leg = matches!(self, AiAuth::OpenRouter { base_url: Some(_), .. });
                if !is_bench_leg {
                    Self::report_usage(model, "generate", u);
                }
            }
        }
        Ok(translated)
    }

    /// Send ONE OpenAI-chat request over whichever transport this OR auth
    /// selected, returning the uniform `(status, parsed_json, raw_text)` the
    /// reasoning-retry logic branches on. `serve_node = None` → the HTTP
    /// transports (BYOK-direct / `ECK_OR_PROXY_URL` proxy), byte-identical to
    /// before. `serve_node = Some(uuid)` → the native relay `/E/ai/*` channel.
    async fn or_dispatch(
        &self,
        http: &HttpClient,
        model: &str,
        req: &Value,
    ) -> Result<(reqwest::StatusCode, Value, String)> {
        match self {
            AiAuth::OpenRouter { serve_node: Some(node), api_key, .. } => {
                self.or_send_via_relay(http, node, api_key, req).await
            }
            _ => {
                let url = self.generate_url(model);
                self.or_send(http, &url, req).await
            }
        }
    }

    /// Relay transport for the managed OR path (`.eck/OPENROUTER_TIER.md` §10).
    /// Enqueue the OpenAI-chat request as a transport-neutral envelope on the eck
    /// relay's `/E/ai/dispatch/<node>` queue (carrying the paid license the
    /// serving node verifies), then poll `/E/ai/result/<task_id>` until the node
    /// acks `{status, body}`. The `body` is the SAME upstream OpenAI-chat JSON a
    /// direct call returns, so `openrouter_generate` runs the identical response
    /// shim on it. Returns `(status, body, raw_text)` mirroring [`Self::or_send`].
    async fn or_send_via_relay(
        &self,
        http: &HttpClient,
        node: &str,
        license: &str,
        req: &Value,
    ) -> Result<(reqwest::StatusCode, Value, String)> {
        let relay = std::env::var("RELAY_URL")
            .unwrap_or_else(|_| "http://localhost:3200".into())
            .trim_end_matches('/')
            .to_string();
        // The per-request patience budget (serving-side upstream timeout) plus a
        // network/queue margin. Also the envelope's own validity window — the
        // serving node drops an envelope past `expires_at` instead of burning an
        // upstream call on a request the client already abandoned.
        let timeout_secs = std::env::var("ECK_OR_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(180)
            .max(120);
        let margin_secs: u64 = 30;
        let deadline_secs = timeout_secs + margin_secs;
        let now = chrono::Utc::now().timestamp();
        let nonce = uuid::Uuid::new_v4().to_string();
        let envelope = or_build_relay_envelope(node, license, req, &nonce, now, deadline_secs as i64);

        let dispatch_url = format!("{}/E/ai/dispatch/{}", relay, node);
        let resp = http
            .post(&dispatch_url)
            .header("Content-Type", "application/json")
            .json(&envelope)
            .send()
            .await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            anyhow::bail!("relay AI dispatch failed ({s}): {t}");
        }
        let dj: Value = resp.json().await?;
        let task_id = dj
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("relay AI dispatch response missing task_id"))?
            .to_string();

        let deadline = Instant::now() + Duration::from_secs(deadline_secs);
        let result = self.fetch_result(http, &relay, &task_id, deadline).await?;

        // The serving node acks `{status, body}`; unwrap into the (status, json,
        // text) shape the reasoning-retry + response shim expect.
        let code = result
            .get("status")
            .and_then(|v| v.as_u64())
            .and_then(|s| reqwest::StatusCode::from_u16(s as u16).ok())
            .unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
        let body = result.get("body").cloned().unwrap_or(Value::Null);
        let text = body.to_string();
        Ok((code, body, text))
    }

    /// Poll the relay for a queued AI task's result until it acks or the deadline
    /// passes. TIGHT fixed cadence by design (owner direction 2026-08-30): 1 s for
    /// the first 60 s, then 5 s — NOT an adaptive backoff (adaptive 7-300 s
    /// backoffs were the latency pain). `ECK_OR_POLL_MS` overrides the fast tick.
    ///
    /// LONG-POLL UPGRADE SEAM: this is the ONLY place the poll cadence lives. A
    /// future relay-side held response (`GET /E/ai/result/<id>?wait=30`) slots in
    /// by changing only this fn — the envelope and every caller stay as-is.
    async fn fetch_result(
        &self,
        http: &HttpClient,
        relay: &str,
        task_id: &str,
        deadline: Instant,
    ) -> Result<Value> {
        let fast_ms = std::env::var("ECK_OR_POLL_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&m| m >= 100)
            .unwrap_or(1000);
        let url = format!("{}/E/ai/result/{}", relay, task_id);
        let started = Instant::now();
        loop {
            if Instant::now() >= deadline {
                anyhow::bail!("relay AI result timed out for task {task_id}");
            }
            let interval = if started.elapsed() < Duration::from_secs(60) {
                Duration::from_millis(fast_ms)
            } else {
                Duration::from_secs(5)
            };
            tokio::time::sleep(interval).await;
            let resp = match http.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    // Transient relay blip — keep polling until the deadline.
                    tracing::debug!("relay AI result poll error for {task_id}: {e}");
                    continue;
                }
            };
            if !resp.status().is_success() {
                tracing::debug!("relay AI result poll non-success for {task_id}: {}", resp.status());
                continue;
            }
            let j: Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            match j.get("status").and_then(|v| v.as_str()).unwrap_or("") {
                "completed" | "failed" => {
                    // Both carry the serving node's `{status, body}` result — the
                    // caller maps a non-2xx `status` to an error itself.
                    return Ok(j.get("result").cloned().unwrap_or(Value::Null));
                }
                _ => continue, // "pending" — keep polling
            }
        }
    }

    /// POST one OpenAI-chat request. Returns `(status, parsed_json_or_null,
    /// raw_text)` so the caller can branch on 400 (reasoning retry) without the
    /// body being consumed. Rides `send_with_retry` for 429/5xx (400 is
    /// non-transient and returns at once).
    async fn or_send(
        &self,
        http: &HttpClient,
        url: &str,
        req: &Value,
    ) -> Result<(reqwest::StatusCode, Value, String)> {
        let request = http
            .post(url)
            .header("Content-Type", "application/json")
            .json(req);
        let res = send_with_retry(self.with_auth(request)).await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let val = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        Ok((status, val, text))
    }

    /// POST the 9eck license to the token authority and parse a fresh bearer.
    async fn mint_managed(http: &HttpClient) -> Result<CachedToken> {
        let url = std::env::var("ECK_VERTEX_MINT_URL").map_err(|_| {
            anyhow::anyhow!("managed AI mode but ECK_VERTEX_MINT_URL is unset")
        })?;
        let license = std::env::var("ECK_LICENSE_TOKEN").map_err(|_| {
            anyhow::anyhow!("managed AI mode but ECK_LICENSE_TOKEN is unset")
        })?;

        let res = match http
            .post(&url)
            .json(&json!({ "license": license }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Network/transport failure — short backoff so we don't spin.
                note_mint_failure(false).await;
                return Err(e.into());
            }
        };
        if !res.status().is_success() {
            let status = res.status();
            // 402 (allowance exhausted) and 401/403 (auth) won't clear in
            // seconds — arm the long backoff. Anything else gets the short one.
            let hard = matches!(status.as_u16(), 401 | 402 | 403);
            note_mint_failure(hard).await;
            let body = res.text().await.unwrap_or_default();
            anyhow::bail!("token mint failed ({status}): {body}");
        }
        let j: Value = res.json().await?;
        let bearer = j["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("mint response missing `token`"))?
            .to_string();
        let ttl = j["expires_in_secs"].as_u64().unwrap_or(300);
        let project = j["project"].as_str().unwrap_or_default().to_string();
        let location = j["location"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_VERTEX_LOCATION)
            .to_string();

        // Capture the balance snapshot the authority piggybacks on the mint.
        absorb_quota_snapshot(&j).await;

        Ok(CachedToken {
            bearer,
            project,
            location,
            fetched_at: Instant::now(),
            ttl: Duration::from_secs(ttl),
        })
    }

    /// Sync config gate for deciding whether to spawn AI workers at startup.
    /// `studio` → a key is present. `managed` → either a pinned bearer or the
    /// mint URL + license are present (the bearer is fetched lazily later).
    pub fn is_enabled_in_env() -> bool {
        let mode = std::env::var("ECK_AI_MODE")
            .unwrap_or_default()
            .to_lowercase();
        if mode == "managed" {
            let has_pinned = std::env::var("ECK_VERTEX_BEARER")
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let has_mint = std::env::var("ECK_VERTEX_MINT_URL")
                .map(|s| !s.is_empty())
                .unwrap_or(false)
                && std::env::var("ECK_LICENSE_TOKEN")
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
            has_pinned || has_mint
        } else {
            std::env::var("GEMINI_API_KEY")
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        }
    }

    /// Fire-and-forget usage report to the metering authority (`ECK_VERTEX_USAGE_URL`).
    /// No-op outside managed mode or when unconfigured. Token counts are read from
    /// a Gemini/Vertex `usageMetadata` (or estimate) Value. Thin wrapper over the
    /// free [`report_managed_usage`] — kept so existing `Self::report_usage(...)`
    /// call sites (the live generate paths) stay unchanged.
    fn report_usage(model: &str, kind: &str, usage: &Value) {
        report_managed_usage(model, kind, usage);
    }

    /// Explicit Studio auth (used where a key is already in hand, e.g. POS state).
    pub fn studio(api_key: impl Into<String>) -> Self {
        AiAuth::Studio {
            api_key: api_key.into(),
        }
    }

    /// Whether the credential needed for this mode is actually present.
    pub fn is_configured(&self) -> bool {
        match self {
            AiAuth::Studio { api_key } => !api_key.is_empty(),
            AiAuth::Vertex {
                bearer, project, ..
            } => !bearer.is_empty() && !project.is_empty(),
            AiAuth::OpenRouter { api_key, .. } => !api_key.is_empty(),
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            AiAuth::Studio { .. } => "studio",
            AiAuth::Vertex { .. } => "managed",
            AiAuth::OpenRouter { .. } => "openrouter",
        }
    }

    /// The route model when this is an OpenRouter auth, else `None`. Lets the
    /// summarization shadow/telemetry log the OR model without matching the enum
    /// at the call site.
    pub fn or_model(&self) -> Option<&str> {
        match self {
            AiAuth::OpenRouter { model, .. } => Some(model),
            _ => None,
        }
    }

    fn vertex_host(location: &str) -> String {
        if location == "global" {
            "aiplatform.googleapis.com".to_string()
        } else {
            format!("{location}-aiplatform.googleapis.com")
        }
    }

    fn generate_url(&self, model: &str) -> String {
        match self {
            // Key travels in the `x-goog-api-key` header (see `with_auth`), not
            // the query string — URLs end up in proxies/access logs, headers don't.
            AiAuth::Studio { .. } => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
            ),
            AiAuth::Vertex {
                project, location, ..
            } => {
                let host = Self::vertex_host(location);
                format!(
                    "https://{host}/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:generateContent"
                )
            }
            // OpenAI-compatible chat endpoint; the model rides in the JSON body,
            // not the path (`model` arg is ignored — the OR variant carries its
            // own).
            AiAuth::OpenRouter { proxied, base_url, .. } => {
                if *proxied {
                    // MANAGED: `ECK_OR_PROXY_URL` IS the endpoint (the authority's
                    // `POST /ai/or`). Do NOT append `/chat/completions` — the
                    // proxy forwards our OpenAI-chat body verbatim to OpenRouter
                    // itself. Used verbatim (only a trailing slash trimmed).
                    std::env::var("ECK_OR_PROXY_URL")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim_end_matches('/').to_string())
                        // Shouldn't happen (resolve_for_task set proxied only with
                        // the env present), but degrade to a direct OR call rather
                        // than an empty URL if the env vanished mid-flight.
                        .unwrap_or_else(|| {
                            "https://openrouter.ai/api/v1/chat/completions".to_string()
                        })
                } else {
                    // BYOK: direct OpenRouter. A per-request `base_url` override
                    // (bench only) wins; else `ECK_OR_BASE_URL` overrides the host
                    // for a self-test; else the public OR host. `/chat/completions`
                    // is appended. The override lets a bench leg dial a different
                    // OpenRouter-compatible endpoint without mutating process env.
                    let base = base_url
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| {
                            std::env::var("ECK_OR_BASE_URL")
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                        })
                        .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
                    format!("{}/chat/completions", base.trim_end_matches('/'))
                }
            }
        }
    }

    fn embed_url(&self, model: &str) -> String {
        match self {
            AiAuth::Studio { .. } => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:embedContent"
            ),
            AiAuth::Vertex {
                project, location, ..
            } => {
                let host = Self::vertex_host(location);
                format!(
                    "https://{host}/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:predict"
                )
            }
            // Never reached — `embed_content` bails on the OR variant before
            // building a URL (embeddings never route to OpenRouter).
            AiAuth::OpenRouter { .. } => String::new(),
        }
    }

    fn with_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            AiAuth::Studio { api_key } => req.header("x-goog-api-key", api_key),
            AiAuth::Vertex { bearer, .. } => req.bearer_auth(bearer),
            AiAuth::OpenRouter { api_key, .. } => req.bearer_auth(api_key),
        }
    }

    /// `generateContent` returning the FULL response JSON — for callers that
    /// need more than the first text part (function calling: `functionCall`
    /// parts + their `thoughtSignature` must be read and echoed verbatim).
    /// Applies the same request defaults as [`Self::generate_content`]:
    /// Vertex `thinkingBudget=0` (caller's own value wins) and a `user` role
    /// on role-less content entries.
    pub async fn generate_content_raw(
        &self,
        http: &HttpClient,
        model: &str,
        body: Value,
    ) -> Result<Value> {
        self.generate_content_raw_inner(http, model, body, true).await
    }

    /// Inner worker. `allow_cache` gates the Vertex explicit-cache path so a
    /// stale-cache retry can re-run with the head restored and caching disabled.
    async fn generate_content_raw_inner(
        &self,
        http: &HttpClient,
        model: &str,
        mut body: Value,
        allow_cache: bool,
    ) -> Result<Value> {
        // OpenRouter is a different wire protocol (OpenAI chat, not Gemini
        // generateContent). Divert BEFORE the Gemini-shaped defaults/role fixup
        // below run — the shim does its own translation and returns a
        // Gemini-shaped Value, so callers (`generate_content` incl.) never learn
        // the route existed. The passed `model` is ignored: the OR variant
        // carries the route model.
        if let AiAuth::OpenRouter { .. } = self {
            return self.openrouter_generate(http, body).await;
        }
        if let AiAuth::Vertex { .. } = self {
            if let Some(obj) = body.as_object_mut() {
                let gc = obj
                    .entry("generationConfig")
                    .or_insert_with(|| json!({}));
                if let Some(gc_obj) = gc.as_object_mut() {
                    let tc = gc_obj
                        .entry("thinkingConfig")
                        .or_insert_with(|| json!({}));
                    if let Some(tc_obj) = tc.as_object_mut() {
                        tc_obj
                            .entry("thinkingBudget".to_string())
                            .or_insert(json!(0));
                    }
                }
            }
        }

        // Vertex rejects content entries without an explicit role ("Please use a
        // valid role: user, model."), while AI Studio is lenient. Default any
        // role-less entry to "user" — single-turn callers (summary, address
        // discovery) omit it; multi-turn callers already set their own roles, so
        // this only fills the gap and is harmless in studio mode too.
        if let Some(contents) = body.get_mut("contents").and_then(|c| c.as_array_mut()) {
            for entry in contents.iter_mut() {
                if let Some(o) = entry.as_object_mut() {
                    o.entry("role").or_insert_with(|| json!("user"));
                }
            }
        }

        // Vertex explicit head cache (gated, default OFF). Register the stable
        // head (systemInstruction + tools) as a `cachedContents` resource once,
        // then reference it by name and STRIP it from the request. Returns the
        // stripped (name, systemInstruction, tools) so a stale-cache retry can
        // restore them and re-run without the cache. No-op unless enabled AND the
        // body actually carries a tool-bearing head (the brain's signature).
        let mut cache_restore: Option<(String, Value, Value)> = None;
        if allow_cache {
            if let AiAuth::Vertex { .. } = self {
                if gemini_explicit_cache_enabled() {
                    cache_restore = self.apply_head_cache(http, model, &mut body).await;
                }
            }
        }

        let req = http
            .post(self.generate_url(model))
            .header("Content-Type", "application/json")
            .json(&body);
        let res = send_with_retry(self.with_auth(req)).await?;

        if !res.status().is_success() {
            let status = res.status();
            let err_text = res.text().await.unwrap_or_default();
            // A cache that expired between ensure and use → evict, restore the
            // head into the body, and retry ONCE without caching. Next call
            // re-creates it. Only NOT_FOUND/PERMISSION-shaped errors qualify.
            if let Some((name, si, tools)) = cache_restore {
                let cache_miss = status.as_u16() == 403
                    || status.as_u16() == 404
                    || err_text.contains("CachedContent")
                    || err_text.contains("NOT_FOUND");
                if cache_miss {
                    evict_head_cache_by_name(&name).await;
                    tracing::warn!(
                        "[ai-cache] gemini cachedContent stale/rejected ({status}); retrying without cache"
                    );
                    if let Some(obj) = body.as_object_mut() {
                        obj.remove("cachedContent");
                        obj.insert("systemInstruction".to_string(), si);
                        obj.insert("tools".to_string(), tools);
                    }
                    return Box::pin(self.generate_content_raw_inner(http, model, body, false)).await;
                }
            }
            anyhow::bail!(
                "Gemini generateContent error ({status}, mode={}): {err_text}",
                self.mode()
            );
        }

        let resp: Value = res.json().await?;
        dump_ai_exchange("generate", self.mode(), &self.generate_url(model), model, &body, &resp);
        if let Some((name, _, _)) = &cache_restore {
            tracing::debug!(
                "[ai-cache] gemini cachedContent={name} cachedTokens={}",
                resp["usageMetadata"]["cachedContentTokenCount"].as_i64().unwrap_or(0)
            );
        }
        // Managed mode: meter every call against the client's balance right
        // here so multi-turn callers (function-calling loops) can't skip it.
        // The estimated-usage fallback stays in `generate_content`.
        if let AiAuth::Vertex { .. } = self {
            if let Some(u) = resp.get("usageMetadata") {
                if !u.is_null() {
                    Self::report_usage(model, "generate", u);
                }
            }
        }
        Ok(resp)
    }

    /// Vertex explicit-cache seam. If `body` carries BOTH a non-empty
    /// `systemInstruction` and `tools` (the brain's stable head), ensure a
    /// `cachedContents` resource for that head exists, set `body.cachedContent`
    /// to its name, and REMOVE `systemInstruction`+`tools` from `body`. Returns
    /// `Some((cache_name, systemInstruction, tools))` on success so the caller
    /// can restore + retry on a stale-cache error, or `None` when caching didn't
    /// apply (no head, below the token minimum, or a create failure — all
    /// fall back to an unmodified request).
    async fn apply_head_cache(
        &self,
        http: &HttpClient,
        model: &str,
        body: &mut Value,
    ) -> Option<(String, Value, Value)> {
        let si = body.get("systemInstruction").cloned().filter(|v| !v.is_null());
        let tools = body.get("tools").cloned().filter(|v| !v.is_null());
        let (si, tools) = match (si, tools) {
            (Some(s), Some(t)) => (s, t),
            _ => return None, // not a tool-bearing head — leave the request as-is
        };
        let name = self.ensure_head_cache(http, model, &si, &tools).await?;
        if let Some(obj) = body.as_object_mut() {
            obj.remove("systemInstruction");
            obj.remove("tools");
            obj.insert("cachedContent".to_string(), json!(name));
        }
        Some((name, si, tools))
    }

    /// Return a live Vertex `cachedContents` resource name for this head,
    /// creating one on cache miss / near-expiry. `None` if creation fails (e.g.
    /// the head is below the model's cache-token minimum) — caller falls back to
    /// an uncached request.
    async fn ensure_head_cache(
        &self,
        http: &HttpClient,
        model: &str,
        system_instruction: &Value,
        tools: &Value,
    ) -> Option<String> {
        let (project, location, bearer) = match self {
            AiAuth::Vertex { project, location, bearer } => (project, location, bearer),
            _ => return None,
        };
        let key = gemini_cache_key(model, system_instruction, tools);

        // Cache hit (with a refresh margin so we never reference a just-expired
        // entry) — reuse the name.
        {
            let map = gemini_head_cache().lock().await;
            if let Some(entry) = map.get(&key) {
                if entry.expires_at > Instant::now() + Duration::from_secs(TOKEN_REFRESH_MARGIN_SECS) {
                    return Some(entry.name.clone());
                }
            }
        }

        let ttl = gemini_cache_ttl_secs();
        let host = Self::vertex_host(location);
        let url = format!(
            "https://{host}/v1/projects/{project}/locations/{location}/cachedContents"
        );
        let create_body = json!({
            "model": format!("projects/{project}/locations/{location}/publishers/google/models/{model}"),
            "systemInstruction": system_instruction,
            "tools": tools,
            "ttl": format!("{ttl}s"),
        });
        let res = http
            .post(&url)
            .header("Content-Type", "application/json")
            .bearer_auth(bearer)
            .json(&create_body)
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            // Below the cache-token minimum → warn once, disable for this head by
            // returning None (uncached request). Any other error also falls back.
            if !gemini_cache_min_warned().swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "[ai-cache] gemini cachedContents create failed ({status}) — falling back to uncached (head may be below the model's cache-token minimum): {text}"
                );
            }
            return None;
        }
        let j: Value = res.json().await.ok()?;
        let name = j.get("name").and_then(|v| v.as_str())?.to_string();
        let entry = CachedHead {
            name: name.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        };
        gemini_head_cache().lock().await.insert(key, entry);
        tracing::info!("[ai-cache] gemini cachedContents created name={name} ttl={ttl}s model={model}");
        Some(name)
    }

    /// `generateContent`. `body` is the full request JSON
    /// (`systemInstruction`/`contents`/`generationConfig`/`tools`). For Vertex 3.x
    /// "thinking" models `generationConfig.thinkingConfig.thinkingBudget=0` is
    /// injected as a DEFAULT (or the JSON answer gets starved — verified);
    /// a caller that explicitly sets its own thinkingBudget wins.
    /// Returns `(text, usageMetadata)`; usage is estimated if the API omits it.
    pub async fn generate_content(
        &self,
        http: &HttpClient,
        model: &str,
        body: Value,
    ) -> Result<(String, Value)> {
        // Estimate BEFORE the request consumes the body (used only when the
        // API omits usageMetadata).
        let body_len_estimate = serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0);
        let resp = self.generate_content_raw(http, model, body).await?;
        let text = resp["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No text in Gemini response"))?
            .to_string();

        let usage = match resp.get("usageMetadata") {
            // Real usage was already metered inside `generate_content_raw`.
            Some(u) if !u.is_null() => u.clone(),
            _ => {
                let pt = body_len_estimate / 4;
                let ct = text.len() / 4;
                let est = json!({
                    "promptTokenCount": pt,
                    "candidatesTokenCount": ct,
                    "totalTokenCount": pt + ct,
                    "estimated": true,
                });
                // Managed mode: still meter the call when the API omitted usage.
                if let AiAuth::Vertex { .. } = self {
                    Self::report_usage(model, "generate", &est);
                }
                est
            }
        };
        Ok((text, usage))
    }

    /// Task-routed generation with an expected-time budget (GLM-first, Gemini
    /// fallback). Resolves the OR route for `task`; when routed and configured,
    /// runs OpenRouter with a retry-within-budget loop
    /// ([`or_generate_with_retry`]). Outcomes:
    ///   - OR answers → `path="or"`.
    ///   - OR errors out (retries exhausted, or a non-retryable 401/403) BEFORE
    ///     the budget elapses → Gemini immediately, `path="gemini"`,
    ///     `fallback_reason` = last OR error.
    ///   - the budget elapses with an OR call still in flight → HEDGE: start
    ///     Gemini in parallel, first `Ok` wins, abort the loser
    ///     (`path="or"` or `path="gemini-hedge"`).
    /// When `task` is NOT routed to OR, behaves exactly like a plain
    /// `generate_content` on the default auth (`path="gemini"`). `budget_override`
    /// lets a test pin the budget; production passes `None` (looks up
    /// [`task_budget_secs`]). Usage metering is unchanged (rides
    /// `generate_content`).
    pub async fn generate_for_task(
        http: &HttpClient,
        task: &str,
        gemini_model: &str,
        body: Value,
        budget_override: Option<Duration>,
    ) -> Result<TaskGeneration> {
        // Scope the whole routed generation under `task` so every usage report it
        // fires is attributed WITHOUT any caller having to wrap. The inline paths
        // (Gemini fallback / unrouted Gemini) inherit this scope; the spawned
        // sub-futures (the OR attempt, the hedge's Gemini leg) re-establish the
        // scope INSIDE the spawn — a task-local does not cross `tokio::spawn`.
        with_ai_task(
            task,
            Self::generate_for_task_inner(http, task, gemini_model, body, budget_override),
        )
        .await
    }

    async fn generate_for_task_inner(
        http: &HttpClient,
        task: &str,
        gemini_model: &str,
        body: Value,
        budget_override: Option<Duration>,
    ) -> Result<TaskGeneration> {
        let or_auth = Self::resolve_for_task(http, task).await?;

        // Not routed to OR (or OR not configured) → plain Gemini, exactly
        // today's behavior. `resolve_for_task` already fell back to the default
        // auth on an unrouted/misconfigured node.
        let or_model = match or_auth.or_model() {
            Some(m) if or_auth.is_configured() => m.to_string(),
            _ => {
                let (text, usage) = or_auth.generate_content(http, gemini_model, body).await?;
                return Ok(TaskGeneration {
                    text,
                    usage,
                    model: gemini_model.to_string(),
                    path: "gemini",
                    fallback_reason: None,
                });
            }
        };

        let budget =
            budget_override.unwrap_or_else(|| Duration::from_secs(task_budget_secs(task)));
        let deadline = tokio::time::Instant::now() + budget;

        // OR attempt loop as a spawned task so the hedge can let it keep running
        // in parallel with Gemini once the budget elapses.
        let mut or_handle = tokio::spawn({
            let http = http.clone();
            let or_auth = or_auth.clone();
            let or_model = or_model.clone();
            let body = body.clone();
            // Re-establish the task scope inside the spawned future — the OR call
            // reports usage from here and task-locals don't cross the spawn.
            let task = task.to_string();
            async move {
                with_ai_task(&task, or_generate_with_retry(&http, &or_auth, &or_model, body, deadline)).await
            }
        });

        let sleep = tokio::time::sleep_until(deadline);
        tokio::pin!(sleep);

        let or_err: String = tokio::select! {
            joined = &mut or_handle => {
                match joined {
                    Ok(Ok((text, usage))) => {
                        return Ok(TaskGeneration {
                            text, usage, model: or_model, path: "or", fallback_reason: None,
                        });
                    }
                    Ok(Err(e)) => e,
                    Err(join_e) => format!("openrouter task panicked: {join_e}"),
                }
            }
            _ = &mut sleep => {
                tracing::info!("[ai-hedge] task={task} after={}s → gemini started", budget.as_secs());
                return Self::hedge_or_vs_gemini(http, task, gemini_model, body, or_model, or_handle).await;
            }
        };

        // OR failed before the budget elapsed → Gemini immediately.
        tracing::warn!("[ai-fallback] task={task} glm→gemini: {or_err}");
        let gem_auth = Self::resolve(http).await?;
        let (text, usage) = gem_auth.generate_content(http, gemini_model, body).await?;
        Ok(TaskGeneration {
            text,
            usage,
            model: gemini_model.to_string(),
            path: "gemini",
            fallback_reason: Some(or_err),
        })
    }

    /// Hedge race: OR is still running past the budget; run Gemini in parallel
    /// and return whichever finishes with `Ok` first, aborting the loser. If
    /// both fail, error out with both messages.
    async fn hedge_or_vs_gemini(
        http: &HttpClient,
        task: &str,
        gemini_model: &str,
        body: Value,
        or_model: String,
        mut or_handle: tokio::task::JoinHandle<std::result::Result<(String, Value), String>>,
    ) -> Result<TaskGeneration> {
        let gem_auth = Self::resolve(http).await?;
        let mut gem_handle = tokio::spawn({
            let http = http.clone();
            let gemini_model = gemini_model.to_string();
            let body = body.clone();
            // Re-establish the task scope inside the spawned Gemini leg.
            let task = task.to_string();
            async move {
                with_ai_task(&task, gem_auth.generate_content(&http, &gemini_model, body))
                    .await
                    .map_err(|e| e.to_string())
            }
        });

        let mut or_err: Option<String> = None;
        let mut gem_err: Option<String> = None;
        loop {
            tokio::select! {
                r = &mut or_handle, if or_err.is_none() => {
                    match r {
                        Ok(Ok((text, usage))) => {
                            gem_handle.abort();
                            return Ok(TaskGeneration {
                                text, usage, model: or_model, path: "or", fallback_reason: None,
                            });
                        }
                        Ok(Err(e)) => or_err = Some(e),
                        Err(je) => or_err = Some(format!("openrouter task panicked: {je}")),
                    }
                }
                r = &mut gem_handle, if gem_err.is_none() => {
                    match r {
                        Ok(Ok((text, usage))) => {
                            or_handle.abort();
                            return Ok(TaskGeneration {
                                text,
                                usage,
                                model: gemini_model.to_string(),
                                path: "gemini-hedge",
                                fallback_reason: None,
                            });
                        }
                        Ok(Err(e)) => gem_err = Some(e),
                        Err(je) => gem_err = Some(format!("gemini task panicked: {je}")),
                    }
                }
            }
            if let (Some(oe), Some(ge)) = (&or_err, &gem_err) {
                anyhow::bail!("both engines failed: openrouter: {oe}; gemini: {ge}");
            }
        }
    }

    /// Embed `text` → `(vector, usage)`. Studio and Vertex both use `:embedContent`
    /// (Vertex: `gemini-embedding-2-preview` in us-central1, bearer auth, usage
    /// self-reported to the authority — see the `Vertex` arm).
    pub async fn embed_content(
        &self,
        http: &HttpClient,
        model: &str,
        text: &str,
        dim: usize,
    ) -> Result<(Vec<f32>, Value)> {
        match self {
            AiAuth::Studio { .. } => {
                let body = json!({
                    "model": format!("models/{model}"),
                    "content": { "parts": [{ "text": text }] },
                    "taskType": "RETRIEVAL_DOCUMENT",
                    "outputDimensionality": dim,
                });
                let res = send_with_retry(self.with_auth(
                    http.post(self.embed_url(model))
                        .header("Content-Type", "application/json")
                        .json(&body),
                ))
                .await?;
                if !res.status().is_success() {
                    let status = res.status();
                    let err = res.text().await.unwrap_or_default();
                    anyhow::bail!("Gemini embedContent error ({status}): {err}");
                }
                let resp: Value = res.json().await?;
                let values = resp["embedding"]["values"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("No embedding values in Gemini response"))?;
                let v: Vec<f32> = values
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                let usage = resp.get("usageMetadata").cloned().unwrap_or(Value::Null);
                Ok((v, usage))
            }
            AiAuth::Vertex { bearer, project, .. } => {
                // Managed nodes embed DIRECTLY on Vertex with the minted bearer —
                // same client-direct pattern as generation, nothing flows through
                // the authority any more. Gemini Embedding 2 is served on Vertex as
                // `gemini-embedding-2-preview` through `:embedContent` (NOT the
                // `text-embedding-*` `:predict` shape), currently in us-central1
                // only. Verified 2026-09-06 against the eckwms project: the vectors
                // are identical to Studio `gemini-embedding-2` (cos = 1.0000), so
                // the whole fleet stays on ONE vector space — no re-embed.
                //
                // Rollback hatch: `ECK_EMBED_VIA_AUTHORITY=1` restores the legacy
                // authority `/ai/embed` proxy (metered server-side there).
                if env_flag("ECK_EMBED_VIA_AUTHORITY") {
                    return self.embed_via_authority(http, text).await;
                }
                let vmodel = vertex_embedding_model(model);
                let vloc = std::env::var("ECK_VERTEX_EMBED_LOCATION")
                    .ok()
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| "us-central1".to_string());
                let host = if vloc == "global" {
                    "aiplatform.googleapis.com".to_string()
                } else {
                    format!("{vloc}-aiplatform.googleapis.com")
                };
                let url = format!(
                    "https://{host}/v1/projects/{project}/locations/{vloc}/publishers/google/models/{vmodel}:embedContent"
                );
                let body = json!({
                    "content": { "parts": [{ "text": text }] },
                    "taskType": "RETRIEVAL_DOCUMENT",
                    "outputDimensionality": dim,
                });
                let res = send_with_retry(
                    http.post(&url)
                        .bearer_auth(bearer)
                        .header("Content-Type", "application/json")
                        .json(&body),
                )
                .await?;
                if !res.status().is_success() {
                    let status = res.status();
                    let err = res.text().await.unwrap_or_default();
                    anyhow::bail!("Vertex embedContent error ({status}, {vmodel}@{vloc}): {err}");
                }
                let resp: Value = res.json().await?;
                let values = resp["embedding"]["values"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("No embedding values in Vertex response"))?;
                let v: Vec<f32> = values
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                // Meter the call ourselves (the proxy used to). Vertex reports
                // prompt/total token counts; fall back to a length estimate.
                let usage = match resp.get("usageMetadata") {
                    Some(u) if !u.is_null() => u.clone(),
                    _ => json!({
                        "promptTokenCount": text.len() / 4,
                        "totalTokenCount": text.len() / 4,
                        "estimated": true,
                    }),
                };
                Self::report_usage(&vmodel, "embedding", &usage);
                Ok((v, usage))
            }
            // Embeddings NEVER route to OpenRouter (§1/§9): OR has no embedding
            // model in our vector space, and mixing spaces would silently poison
            // HNSW recall. Fail loud so a mis-wired caller is caught in test.
            AiAuth::OpenRouter { .. } => {
                anyhow::bail!("no OpenRouter embeddings route — embeddings stay on Gemini/local")
            }
        }
    }
}

/// Module-level shorthand for [`AiAuth::resolve_for_task`] — resolve auth for a
/// NAMED AI task (`ECK_AI_TASK_ROUTES`), OpenRouter when routed, else the default
/// Studio/Vertex auth. Lets call sites write `eck_core::ai::resolve_for_task(...)`.
/// `1`/`true`/`yes` → on. Unset or anything else → off.
fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    )
}

/// Map the configured (Studio-named) embedding model to its Vertex publisher id.
/// `ECK_VERTEX_EMBEDDING_MODEL` overrides; otherwise the Gemini Embedding 2
/// family maps to the Vertex preview id (that is where the model lives on
/// Vertex today; drop the suffix once Google promotes it to GA).
pub fn vertex_embedding_model(model: &str) -> String {
    if let Ok(m) = std::env::var("ECK_VERTEX_EMBEDDING_MODEL") {
        if !m.trim().is_empty() {
            return m.trim().to_string();
        }
    }
    if model.starts_with("gemini-embedding-2") && !model.ends_with("-preview") {
        return "gemini-embedding-2-preview".to_string();
    }
    model.to_string()
}

impl AiAuth {
    /// Legacy managed embedding path: the authority's `/ai/embed` proxy embeds
    /// with its own Studio key and meters server-side. Only reachable with
    /// `ECK_EMBED_VIA_AUTHORITY=1` (rollback hatch for the direct-Vertex path).
    async fn embed_via_authority(&self, http: &HttpClient, text: &str) -> Result<(Vec<f32>, Value)> {
        let embed_url = std::env::var("ECK_EMBED_URL")
            .unwrap_or_else(|_| "".to_string());
        let license = std::env::var("ECK_LICENSE_TOKEN").unwrap_or_default();
        if license.is_empty() {
            anyhow::bail!("ECK_LICENSE_TOKEN unset — cannot embed via authority proxy");
        }
        let body = json!({ "license": license, "text": text, "task_type": "RETRIEVAL_DOCUMENT" });
        let res = send_with_retry(
            http.post(&embed_url)
                .header("Content-Type", "application/json")
                .json(&body),
        )
        .await?;
        if !res.status().is_success() {
            let status = res.status();
            let err = res.text().await.unwrap_or_default();
            anyhow::bail!("authority /ai/embed error ({status}): {err}");
        }
        let resp: Value = res.json().await?;
        let values = resp["embedding"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("No embedding in /ai/embed response"))?;
        let v: Vec<f32> = values
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect();
        // Usage already metered server-side by /ai/embed.
        Ok((v, Value::Null))
    }
}

pub async fn resolve_for_task(http: &HttpClient, task: &str) -> Result<AiAuth> {
    AiAuth::resolve_for_task(http, task).await
}

/// Module-level shorthand for [`AiAuth::resolve_for_task_with_model`] — resolve
/// auth for a NAMED task with the OR `model` PINNED by the caller (the alt brain
/// engine), keeping the deny-policy check and every transport.
pub async fn resolve_for_task_with_model(
    http: &HttpClient,
    task: &str,
    model: &str,
) -> Result<AiAuth> {
    AiAuth::resolve_for_task_with_model(http, task, model).await
}

/// Build a BYOK OpenRouter auth for a SHADOW/soak call — slug-aware, but NEVER
/// managed/relay and NEVER deny-policy-gated (the soak is BYOK by construction,
/// independent of `ECK_AI_TASK_ROUTES`). A slug model (name contains `/`) with
/// `ECK_OR_SLUG_API_KEY` set takes the per-model slug upstream (`base_url: Some`
/// ⇒ carried its own key + base, and — like the bench leg — NOT reported to the
/// managed wallet); every other model uses `byok_key` on the node's primary
/// upstream, byte-identical to a hand-built `OpenRouter { proxied:false,
/// serve_node:None, base_url:None }`.
pub fn openrouter_byok_for_model(model: &str, byok_key: &str) -> AiAuth {
    let (api_key, base_url) = match slug_upstream(model) {
        Some((base, slug_key)) => (slug_key, Some(base)),
        None => (byok_key.to_string(), None),
    };
    AiAuth::OpenRouter {
        api_key,
        model: model.to_string(),
        proxied: false,
        serve_node: None,
        base_url,
    }
}

/// Fire-and-forget usage report to the metering authority (`ECK_VERTEX_USAGE_URL`,
/// authed by `ECK_LICENSE_TOKEN`). No-op when either env is unset. `usage` is a
/// Gemini/Vertex `usageMetadata` (or estimate) Value.
///
/// Shared by the in-process live meter ([`AiAuth::report_usage`], called per
/// `generateContent`) and out-of-band meters — notably the batch summarizer,
/// whose generate calls run server-side inside a Vertex batch job and so never
/// pass through `generate_content_raw`. It reports ONE aggregated usage figure
/// per completed job. The authority answers with the current mesh balance, which
/// we absorb so the dashboard gauge refreshes on every report.
pub fn report_managed_usage(model: &str, kind: &str, usage: &Value) {
    // Global kill-switch: `ECK_AI_USAGE_REPORT=0` disables all usage reporting
    // (spend counters / wallet debits stop). Warn ONCE so a standing override is
    // visible in the log without spamming every AI call.
    if std::env::var("ECK_AI_USAGE_REPORT")
        .map(|v| v.trim() == "0")
        .unwrap_or(false)
    {
        static WARNED: OnceLock<()> = OnceLock::new();
        if WARNED.set(()).is_ok() {
            tracing::warn!("[AI] ECK_AI_USAGE_REPORT=0 — managed usage reporting disabled");
        }
        return;
    }
    let url = match std::env::var("ECK_VERTEX_USAGE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => return,
    };
    let license = match std::env::var("ECK_LICENSE_TOKEN") {
        Ok(l) if !l.is_empty() => l,
        _ => return,
    };
    let prompt = usage
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let candidates = usage
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = usage
        .get("totalTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt + candidates);
    let (model, kind) = (model.to_string(), kind.to_string());
    // Attribution: WHICH node + WHICH task produced this usage. Read the
    // task-local HERE, before `tokio::spawn` — a task-local does NOT propagate
    // across the spawn boundary, so reading it inside the spawned future would
    // always yield "unlabelled".
    let node = ai_node_id();
    let task = current_ai_task();
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let resp = http
            .post(&url)
            .json(&json!({
                "license": license,
                "model": model,
                "kind": kind,
                "prompt_tokens": prompt,
                "candidates_tokens": candidates,
                "total_tokens": total,
                "node": node,
                "task": task,
            }))
            .send()
            .await;
        // The authority answers every usage report with the CURRENT mesh
        // balance — absorb it so the snapshot refreshes on every AI call,
        // not just on the ~hourly mint (keeps all nodes' gauges in step).
        if let Ok(r) = resp {
            if let Ok(j) = r.json::<Value>().await {
                // SPEND-SIDE GATE. The mint gate only bites when a bearer is
                // issued; a node holding a still-fresh bearer can burn past its
                // credit_limit for the rest of the bearer's lifetime (2026-08-27:
                // 7.5M-unit overdraft on a 2M limit in under an hour). The
                // authority now flags exhaustion on every usage report — drop
                // the cached bearer and arm the hard backoff, so the very next
                // AI call re-mints, hits the 402, and the burn stops within one
                // report instead of at bearer expiry.
                if j.get("exhausted").and_then(|v| v.as_bool()) == Some(true) {
                    let had_token = token_cache().lock().await.take().is_some();
                    if had_token {
                        tracing::warn!(
                            "[AI] authority reports allowance exhausted — dropping cached bearer, further AI calls suspended until top-up"
                        );
                        note_mint_failure(true).await;
                    }
                }
                absorb_quota_snapshot(&j).await;
            }
        }
    });
}

// ── OpenRouter shim (pure helpers) ──────────────────────────────────────────
//
// These are the translation guts of the OR variant, kept as free, side-effect-
// free functions (no HTTP, no env except the two documented overrides) so the
// Gemini↔OpenAI-chat mapping is unit-testable without a network. `openrouter_
// generate` above is the only caller; it wraps these with the POST + metering.

/// One process-wide latch so the "route→OR but key empty" fallback warns ONCE
/// instead of on every worker cycle.
fn or_fallback_warned() -> &'static AtomicBool {
    static W: OnceLock<AtomicBool> = OnceLock::new();
    W.get_or_init(|| AtomicBool::new(false))
}

/// Per-task latch for the "route model denied by policy" warning, so a standing
/// deny doesn't log on every worker cycle. Returns `true` the FIRST time it sees
/// a given task (caller should warn), `false` afterwards.
fn or_task_denied_warned(task: &str) -> bool {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let set = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    // `try_lock` keeps this sync (the caller is async but we don't want to hold
    // an await point just to dedup a log line); on contention we simply warn.
    match set.try_lock() {
        Ok(mut g) => g.insert(task.to_string()),
        Err(_) => true,
    }
}

/// Node-side AI model deny-list from `ECK_AI_MODEL_DENY` (comma-separated vendor
/// prefixes, e.g. `"z-ai/,qwen/"`). Empty/unset ⇒ nothing denied. A later
/// DB-backed security setting will feed the SAME pure [`model_denied_by_policy`]
/// check — this is only the env source.
fn ai_model_deny_list() -> Vec<String> {
    std::env::var("ECK_AI_MODEL_DENY")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Pure model-policy test: `model` is denied when it starts with any prefix in
/// `denied`. Kept parameterised (deny-list passed in, not read from env) so the
/// UI/DB wave can reuse it against a stored policy. Empty list ⇒ never denied.
fn model_denied_by_policy(model: &str, denied: &[String]) -> bool {
    denied
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .any(|p| model.starts_with(p))
}

/// Pure "slug upstream" decision (`.eck/OPENROUTER_TIER.md` §slug). A model name
/// containing `/` is an OpenRouter *slug* (`vendor/model`); an unprefixed name is
/// vendor-direct. On a node whose primary OpenAI-compatible upstream is a
/// vendor-direct endpoint that can't serve arbitrary slugs (e.g. Z.ai-direct,
/// where `OPENROUTER_API_KEY` is the Z.ai key and `*=glm-5.3-flash`), a slug
/// model needs a SECOND, per-model upstream. Given the model plus the slug env
/// values, return `Some((base_url, api_key))` for that upstream, or `None` for
/// "unchanged — resolve as today". `None` unless the model contains `/` AND the
/// slug key is non-empty, so an unprefixed name, or a node with no slug key, is
/// BYTE-IDENTICAL to before. Empty base ⇒ the public OpenRouter host.
fn slug_upstream_for(model: &str, slug_key: &str, slug_base: &str) -> Option<(String, String)> {
    if !model.contains('/') || slug_key.trim().is_empty() {
        return None;
    }
    let base = if slug_base.trim().is_empty() {
        "https://openrouter.ai/api/v1".to_string()
    } else {
        slug_base.trim().to_string()
    };
    Some((base, slug_key.trim().to_string()))
}

/// Env wrapper over [`slug_upstream_for`]: read `ECK_OR_SLUG_API_KEY` /
/// `ECK_OR_SLUG_BASE_URL` and decide the slug upstream for `model`.
fn slug_upstream(model: &str) -> Option<(String, String)> {
    let key = std::env::var("ECK_OR_SLUG_API_KEY").unwrap_or_default();
    let base = std::env::var("ECK_OR_SLUG_BASE_URL").unwrap_or_default();
    slug_upstream_for(model, &key, &base)
}

/// Build the TRANSPORT-NEUTRAL relay AI envelope (`.eck/OPENROUTER_TIER.md`
/// §10). Deliberately free of any polling-specific field so a future relay
/// long-poll or xelixir-style WS burst carries the identical shape. `expires_at`
/// is the client's own validity window (>= 10 min in practice), which the
/// serving node honours instead of a short signature age — the property that
/// disqualified the ops channel. The paid `license` rides inside (verified on
/// the serving node), and `request` is the untouched OpenAI-chat body. Pure, so
/// the envelope shape + window are unit-tested without a relay.
fn or_build_relay_envelope(
    node: &str,
    license: &str,
    request: &Value,
    nonce: &str,
    now: i64,
    ttl_secs: i64,
) -> Value {
    json!({
        "envelope": {
            "target_uuid": node,
            "kind": "ai.or.request",
            "nonce": nonce,
            "created_at": now,
            "expires_at": now + ttl_secs,
        },
        "license": license,
        "request": request,
    })
}

/// Public route probe: the OR model `task` is routed to in `ECK_AI_TASK_ROUTES`,
/// or `None` for "not routed / use the default auth". Lets a caller decide
/// whether to take the tier path WITHOUT calling [`resolve_for_task`] (which, on
/// an unrouted managed node, would mint a Vertex bearer as a side effect).
pub fn task_route(task: &str) -> Option<String> {
    or_route_for_task(task)
}

/// Look up the OR model for `task` in `ECK_AI_TASK_ROUTES` (`task=model`,
/// comma-separated). Precedence:
///   1. an explicit `task=model` entry wins (returns that model);
///   2. an explicit `task=` (empty) or `task=off` entry OPTS THE TASK OUT — it
///      returns `None` even when a wildcard is present;
///   3. a `*=model` (or `default=model`) wildcard applies to any task with no
///      explicit entry;
///   4. otherwise `None` ("use the default auth").
/// `None` also when the var is unset.
fn or_route_for_task(task: &str) -> Option<String> {
    let raw = std::env::var("ECK_AI_TASK_ROUTES").ok()?;
    // An explicit entry for this task wins over the wildcard (including opt-out).
    for pair in raw.split(',') {
        if let Some((t, m)) = pair.split_once('=') {
            if t.trim() == task {
                let m = m.trim();
                if m.is_empty() || m.eq_ignore_ascii_case("off") {
                    return None;
                }
                return Some(m.to_string());
            }
        }
    }
    task_map_wildcard(&raw).filter(|m| !m.eq_ignore_ascii_case("off"))
}

/// Explicit `task=value` lookup in a comma-separated `task=value` map. Returns
/// `None` for an absent task OR an explicit empty value (`task=`). Pure — the
/// caller supplies the raw string, so it is unit-testable without env.
fn task_map_explicit(raw: &str, task: &str) -> Option<String> {
    for pair in raw.split(',') {
        if let Some((t, m)) = pair.split_once('=') {
            if t.trim() == task {
                let m = m.trim();
                return if m.is_empty() { None } else { Some(m.to_string()) };
            }
        }
    }
    None
}

/// The wildcard (`*=value` / `default=value`) value in a `task=value` map, or
/// `None`. Pure.
fn task_map_wildcard(raw: &str) -> Option<String> {
    for pair in raw.split(',') {
        if let Some((t, m)) = pair.split_once('=') {
            let t = t.trim();
            if t == "*" || t == "default" {
                let m = m.trim();
                if !m.is_empty() {
                    return Some(m.to_string());
                }
            }
        }
    }
    None
}

/// Result of [`AiAuth::generate_for_task`]. `path` records which engine served
/// the text: `"or"` (OpenRouter/GLM), `"gemini"` (fell back before the budget
/// elapsed — see `fallback_reason`), or `"gemini-hedge"` (Gemini won the hedge
/// race started when the budget elapsed with OR still running).
#[derive(Debug, Clone)]
pub struct TaskGeneration {
    pub text: String,
    pub usage: Value,
    pub model: String,
    pub path: &'static str,
    pub fallback_reason: Option<String>,
}

/// The EXPECTED-TIME BUDGET (seconds) for `task` — the interval within which we
/// keep trying to reach GLM over OpenRouter before spending money on Gemini.
/// Source of truth: built-in defaults, overridable per task via
/// `ECK_AI_TASK_BUDGET="task=secs,…"` (same syntax as `ECK_AI_TASK_ROUTES`,
/// `*=N` wildcard allowed). For `brain`, the legacy `ECK_AI_HEDGE_SECS` still
/// overrides the default (but not an explicit `ECK_AI_TASK_BUDGET` entry).
/// Precedence: explicit budget entry > `ECK_AI_HEDGE_SECS` (brain only) >
/// wildcard budget > built-in default.
pub fn task_budget_secs(task: &str) -> u64 {
    let raw = std::env::var("ECK_AI_TASK_BUDGET").unwrap_or_default();
    if let Some(v) = task_map_explicit(&raw, task).and_then(|s| s.trim().parse::<u64>().ok()) {
        return v;
    }
    if task == "brain" {
        if let Some(v) = std::env::var("ECK_AI_HEDGE_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            return v;
        }
    }
    if let Some(v) = task_map_wildcard(&raw).and_then(|s| s.trim().parse::<u64>().ok()) {
        return v;
    }
    default_task_budget(task)
}

/// Built-in expected-time budgets (seconds). Values are rough — the owner tunes
/// them via `ECK_AI_TASK_BUDGET`. Unknown task → a conservative 120s.
fn default_task_budget(task: &str) -> u64 {
    match task {
        "brain" => 25,
        // Rolling chat-history compaction: a single cheap lite-model summary
        // of the oldest turns. Short budget — it's one small generation.
        "chat_compact" => 60,
        "vision" => 120,
        "classify" => 120,
        "i18n" => 60,
        "translation" => 300,
        "attachments" => 300,
        "trips" => 60,
        "voice" => 30,
        "observer" => 300,
        "optimizer" => 300,
        "distiller" => 600,
        "summarization" => 1800,
        _ => 120,
    }
}

/// Max OpenRouter attempts inside one task budget before giving up on OR (so a
/// hard-broken key can't spin for the whole budget window).
const OR_MAX_ATTEMPTS: usize = 6;

/// Backoff before OR retry `attempt` (1-based): 2s, 5s, 10s, then 20s steps.
fn or_retry_delay(attempt: usize) -> Duration {
    Duration::from_secs(match attempt {
        1 => 2,
        2 => 5,
        3 => 10,
        _ => 20,
    })
}

/// Whether an OR error is worth retrying within the budget. Auth/policy
/// failures (HTTP 401/403) will not self-heal — bail to Gemini at once.
/// Everything else (5xx, 429, timeouts, empty answer) is transient → retry.
fn or_error_is_retryable(err: &str) -> bool {
    !(err.contains("401") || err.contains("403"))
}

/// OR attempt loop honouring the task budget: retry transient OR failures with
/// [`or_retry_delay`] backoff until the budget deadline (or [`OR_MAX_ATTEMPTS`])
/// is reached; a non-retryable (401/403) error returns at once. `Err` carries
/// the last OR error string. A single in-flight call MAY run past the deadline —
/// the caller races it against a hedge timer.
async fn or_generate_with_retry(
    http: &HttpClient,
    or_auth: &AiAuth,
    or_model: &str,
    body: Value,
    deadline: tokio::time::Instant,
) -> std::result::Result<(String, Value), String> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match or_auth.generate_content(http, or_model, body.clone()).await {
            Ok(ok) => return Ok(ok),
            Err(e) => {
                let last = e.to_string();
                if !or_error_is_retryable(&last) {
                    return Err(last);
                }
                if attempt >= OR_MAX_ATTEMPTS {
                    return Err(last);
                }
                let delay = or_retry_delay(attempt);
                let now = tokio::time::Instant::now();
                if now >= deadline || now + delay >= deadline {
                    return Err(last);
                }
                tracing::debug!(
                    "[ai-or-retry] model={or_model} attempt={attempt} err={last}; retrying in {}s",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// How the `reasoning` field is set for a given model. `Field(v, retry_on_400)`
/// injects `v`; `retry_on_400` means "if the model rejects it with HTTP 400,
/// resend once without the field" (only the unknown-model default asks for
/// this). `Omit` sends no field at all.
#[derive(Debug, Clone, PartialEq)]
enum OrReasoning {
    Field(Value, bool),
    /// A TOP-LEVEL `"reasoning_effort": <value>` scalar (NOT the nested
    /// `reasoning` object). Z.ai-direct glm SILENTLY IGNORES the OpenRouter-style
    /// `reasoning` object and 400s on `thinking:{type:disabled}` — the only
    /// working control is top-level `reasoning_effort` (owner-verified live).
    ReasoningEffort(Value),
    Omit,
}

/// Env-facing wrapper: read `ECK_OR_REASONING` once and delegate to the pure
/// [`or_reasoning_for`].
fn or_reasoning_field(model: &str) -> OrReasoning {
    let ovr = std::env::var("ECK_OR_REASONING").ok();
    or_reasoning_for(model, ovr.as_deref())
}

/// Per-model reasoning config (`.eck/OPENROUTER_TIER.md` §3.3), bench-verified:
///   - `qwen/`               → `{"enabled":false}` (accepts off; else burns budget on thinking)
///   - `z-ai/`,`google/`,`stepfun/` → `{"effort":"low"}` (`enabled:false` → HTTP 400)
///   - `deepseek/`           → `{"enabled":false}`, retry-once-without on 400 (accepts off; owner-verified live on v4.1-flash)
///   - `anthropic/`          → omit (thinking off by default)
///   - unknown               → `{"effort":"low"}`, retry-once-without on 400
/// `override_str` (`ECK_OR_REASONING="<prefix>=<mode>,…"`) wins when a prefix
/// matches: mode `off`→`{"enabled":false}`, `omit`/`none`→omit, anything else is
/// taken as an effort level → `{"effort":"<mode>"}`. An explicit override never
/// asks for the 400-retry (the operator chose it deliberately).
fn or_reasoning_for(model: &str, override_str: Option<&str>) -> OrReasoning {
    if let Some(s) = override_str {
        for pair in s.split(',') {
            if let Some((prefix, mode)) = pair.split_once('=') {
                if model.starts_with(prefix.trim()) {
                    let mode = mode.trim();
                    // Z.ai-direct glm (unprefixed name) only honours a TOP-LEVEL
                    // `reasoning_effort`; an effort value routes there instead of
                    // the ignored `reasoning` object. `omit`/`none`/empty still
                    // means "send nothing".
                    if model.starts_with("glm") {
                        return match mode {
                            "omit" | "none" | "" => OrReasoning::Omit,
                            eff => OrReasoning::ReasoningEffort(json!(eff)),
                        };
                    }
                    return match mode {
                        "off" => OrReasoning::Field(json!({ "enabled": false }), false),
                        "omit" | "none" | "" => OrReasoning::Omit,
                        eff => OrReasoning::Field(json!({ "effort": eff }), false),
                    };
                }
            }
        }
    }
    if model.starts_with("qwen/") {
        OrReasoning::Field(json!({ "enabled": false }), false)
    } else if model.starts_with("glm") {
        // Z.ai-direct glm (`glm-5.3-flash`, no `z-ai/` prefix): the OpenRouter
        // `reasoning` object is silently ignored and `thinking:{type:disabled}`
        // 400s (code 1210) — top-level `reasoning_effort:"low"` is the confirmed
        // working control (owner-verified live 2026-08-30).
        OrReasoning::ReasoningEffort(json!("low"))
    } else if model.starts_with("z-ai/")
        || model.starts_with("google/")
        || model.starts_with("stepfun/")
    {
        OrReasoning::Field(json!({ "effort": "low" }), false)
    } else if model.starts_with("deepseek/") {
        // DeepSeek (OpenRouter slug): accepts `{"enabled":false}` to turn thinking
        // off (owner-verified live on deepseek-v4.1-flash). Keep the 400-retry so
        // an upstream that rejects the field degrades to a plain call.
        OrReasoning::Field(json!({ "enabled": false }), true)
    } else if model.starts_with("anthropic/") {
        OrReasoning::Omit
    } else {
        // Unknown model: try the safe default, but be ready to drop it on a 400.
        OrReasoning::Field(json!({ "effort": "low" }), true)
    }
}

/// Concatenate the `.parts[].text` of a Gemini content/systemInstruction entry.
fn collect_parts_text(entry: &Value) -> String {
    entry
        .get("parts")
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Vision shim (`.eck/OPENROUTER_TIER.md` §3.4): build an OpenAI-chat message
/// `content` from a Gemini content entry, translating `inlineData
/// {mimeType,data}` image parts into `{"type":"image_url","image_url":{"url":
/// "data:<mime>;base64,<data>"}}` blocks. Returns a **plain string** when the
/// entry has no image part (pure-text is byte-for-byte unchanged — so every
/// wave-1 text caller and its golden tests keep the exact string shape); returns
/// a mixed **content array** (text blocks + image_url blocks, in source order)
/// only when at least one inline image is present. Gemini's REST spelling is
/// `inlineData`/`mimeType` (camelCase); we also accept the proto snake_case
/// `inline_data`/`mime_type` defensively.
fn collect_parts_content(entry: &Value) -> Value {
    let parts = match entry.get("parts").and_then(|p| p.as_array()) {
        Some(p) => p,
        None => return json!(""),
    };
    let inline = |p: &Value| p.get("inlineData").or_else(|| p.get("inline_data")).cloned();
    let has_image = parts.iter().any(|p| inline(p).is_some());
    if !has_image {
        // Pure text: preserve the exact concatenated-string shape of wave 1.
        return json!(collect_parts_text(entry));
    }
    let mut blocks: Vec<Value> = Vec::new();
    for p in parts {
        if let Some(text) = p.get("text").and_then(|t| t.as_str()) {
            if !text.is_empty() {
                blocks.push(json!({ "type": "text", "text": text }));
            }
        } else if let Some(idata) = inline(p) {
            let mime = idata
                .get("mimeType")
                .or_else(|| idata.get("mime_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("image/webp");
            let data = idata.get("data").and_then(|v| v.as_str()).unwrap_or("");
            blocks.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{mime};base64,{data}") },
            }));
        }
    }
    json!(blocks)
}

/// Pop the OpenAI `tool_call_id` to key a Gemini `functionResponse` onto. Gemini
/// matches a response to its call only by position + name, OpenAI by id — so as
/// we translate the history we queue each synthesized call id with its function
/// name and here dequeue in that order. Prefer the oldest pending id with the
/// SAME name (parallel calls in one turn are answered in the same order); else
/// the oldest pending id; else a stable synthesized fallback (orphan response).
fn take_pending_id(pending: &mut std::collections::VecDeque<(String, String)>, name: &str) -> String {
    if let Some(pos) = pending.iter().position(|(_, n)| n == name) {
        return pending.remove(pos).map(|(id, _)| id).unwrap_or_default();
    }
    if let Some((id, _)) = pending.pop_front() {
        return id;
    }
    format!("call_{name}")
}

/// Providers whose OpenAI-compatible endpoint accepts only `tool_choice:"auto"`
/// (Meta's Muse Spark family on OpenRouter, verified 2026-09-07). Matched on
/// the OpenRouter model id prefix.
/// The `max_tokens` floor for an OpenRouter model: an `ECK_OR_MAX_TOKENS`
/// entry whose prefix matches wins, else 12000 for MiMo (`xiaomi/`, whose
/// thinking counts against the same budget), else 3000. Pure.
fn or_max_tokens_floor(model: &str, override_str: Option<&str>) -> u64 {
    if let Some(s) = override_str {
        for pair in s.split(',') {
            if let Some((prefix, n)) = pair.split_once('=') {
                if model.starts_with(prefix.trim()) {
                    if let Ok(n) = n.trim().parse::<u64>() {
                        return n;
                    }
                }
            }
        }
    }
    if model.starts_with("xiaomi/") {
        12000
    } else {
        3000
    }
}

fn or_tool_choice_auto_only(model: &str) -> bool {
    let m = model.trim().trim_start_matches("openrouter:").to_ascii_lowercase();
    m.starts_with("meta/")
}

/// Gemini request JSON → OpenAI-chat request JSON (`.eck/OPENROUTER_TIER.md`
/// §3.1). Does NOT add the `reasoning` field (the caller injects it so the
/// 400-retry can resend a reasoning-free copy).
///
/// Function calling is translated BOTH directions: `tools[].functionDeclarations`
/// → OpenAI `tools:[{type:"function",…}]`, `toolConfig.functionCallingConfig.mode`
/// → `tool_choice`, and history `functionCall`/`functionResponse` parts →
/// assistant `tool_calls` / `role:"tool"` messages. A request WITHOUT tools is
/// byte-identical to the pre-tools shim (the message loop only diverges when a
/// part carries `functionCall`/`functionResponse`). `thoughtSignature` is a
/// Gemini-only field and is never copied onto the OR path.
fn or_build_request(model: &str, body: &Value) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::new();
    // systemInstruction → one system message (skipped when empty).
    if let Some(si) = body.get("systemInstruction") {
        let text = collect_parts_text(si);
        if !text.is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }
    // contents[] → user/assistant/tool messages. role `model`→`assistant`;
    // anything else (incl. role-less entries) defaults to `user`, matching the
    // Vertex fixup. Function-call and function-response parts get their own
    // OpenAI shapes; plain text / vision entries are unchanged.
    let mut call_seq = 0u64;
    let mut pending: std::collections::VecDeque<(String, String)> =
        std::collections::VecDeque::new();
    if let Some(contents) = body.get("contents").and_then(|c| c.as_array()) {
        for entry in contents {
            let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let parts = entry.get("parts").and_then(|p| p.as_array());
            let has_fc = parts
                .map(|ps| ps.iter().any(|p| p.get("functionCall").is_some()))
                .unwrap_or(false);
            let has_fr = parts
                .map(|ps| ps.iter().any(|p| p.get("functionResponse").is_some()))
                .unwrap_or(false);

            if has_fc {
                // model turn with tool calls → assistant message with tool_calls.
                let ps = parts.unwrap();
                let mut tool_calls: Vec<Value> = Vec::new();
                for p in ps {
                    if let Some(fc) = p.get("functionCall") {
                        call_seq += 1;
                        let id = format!("call_{call_seq}");
                        let name =
                            fc.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                        let args_str =
                            serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": args_str },
                        }));
                        pending.push_back((id, name));
                    }
                }
                // Assistant text alongside the calls is preserved; OpenAI accepts
                // null content when tool_calls are present.
                let text: String = ps
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("");
                messages.push(json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                    "tool_calls": tool_calls,
                }));
            } else if has_fr {
                // tool-result turn → one `role:"tool"` message per response.
                let ps = parts.unwrap();
                for p in ps {
                    if let Some(fr) = p.get("functionResponse") {
                        let name = fr.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let response = fr.get("response").cloned().unwrap_or_else(|| json!({}));
                        let response_str =
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                        let id = take_pending_id(&mut pending, name);
                        messages.push(json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": response_str,
                        }));
                    }
                }
            } else {
                let orole = match role {
                    "model" => "assistant",
                    "system" => "system",
                    _ => "user",
                };
                messages.push(json!({ "role": orole, "content": collect_parts_content(entry) }));
            }
        }
    }

    let mut req = json!({
        "model": model,
        "messages": messages,
        // OR returns the exact upstream `cost` (USD) when asked.
        "usage": { "include": true },
    });

    // tools[].functionDeclarations → OpenAI tools:[{type:"function",function}].
    // A tool with no functionDeclarations (e.g. googleSearch) has nothing to
    // translate and simply yields no `tools` field.
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let mut fns: Vec<Value> = Vec::new();
        for t in tools {
            if let Some(decls) = t.get("functionDeclarations").and_then(|d| d.as_array()) {
                for d in decls {
                    let mut f = serde_json::Map::new();
                    if let Some(n) = d.get("name") {
                        f.insert("name".to_string(), n.clone());
                    }
                    if let Some(desc) = d.get("description") {
                        f.insert("description".to_string(), desc.clone());
                    }
                    if let Some(params) = d.get("parameters") {
                        f.insert("parameters".to_string(), params.clone());
                    }
                    fns.push(json!({ "type": "function", "function": Value::Object(f) }));
                }
            }
        }
        if !fns.is_empty() {
            req["tools"] = Value::Array(fns);
        }
    }
    // toolConfig.functionCallingConfig.mode → tool_choice. NONE is the one the
    // brain relies on (last-turn "answer now"): expressed by DROPPING the tool
    // list instead of `tool_choice:"none"`, because not every OpenAI-compatible
    // provider accepts the latter — Meta (Muse Spark via OpenRouter) rejects
    // everything but "auto" with a 400 (2026-09-07 bench: two tickets lost on
    // the final turn). No tools + no tool_choice means "answer now" everywhere.
    // ANY→"required" for the same reason falls back to "auto" on providers that
    // reject it (model id prefix list below); AUTO mapped for parity.
    if let Some(mode) = body
        .get("toolConfig")
        .and_then(|tc| tc.get("functionCallingConfig"))
        .and_then(|c| c.get("mode"))
        .and_then(|m| m.as_str())
    {
        let auto_only = or_tool_choice_auto_only(model);
        match mode {
            "NONE" => {
                if let Some(o) = req.as_object_mut() {
                    o.remove("tools");
                    o.remove("tool_choice");
                }
            }
            "ANY" => req["tool_choice"] = json!(if auto_only { "auto" } else { "required" }),
            "AUTO" => req["tool_choice"] = json!("auto"),
            _ => {}
        }
    }

    let gc = body.get("generationConfig");
    // maxOutputTokens → max_tokens, floored per model (3000 by default: 512
    // truncated ~60% of our German-format outputs in the bench). Reasoning
    // models spend the same budget on thinking first — MiMo-V2.6 answered a
    // 98k-token chat turn with empty content + reasoning at 3000 (2026-10-01),
    // so its floor is higher; `ECK_OR_MAX_TOKENS="<prefix>=<n>,…"` overrides.
    let max_tokens = gc
        .and_then(|g| g.get("maxOutputTokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .max(or_max_tokens_floor(model, std::env::var("ECK_OR_MAX_TOKENS").ok().as_deref()));
    req["max_tokens"] = json!(max_tokens);
    if let Some(t) = gc.and_then(|g| g.get("temperature")) {
        req["temperature"] = t.clone();
    }
    if let Some(p) = gc.and_then(|g| g.get("topP")) {
        req["top_p"] = p.clone();
    }
    Ok(req)
}

/// OpenAI-chat response JSON → Gemini-shaped response JSON
/// (`.eck/OPENROUTER_TIER.md` §3.2). Empty/null content WITH a nonempty
/// `reasoning` field is a thinking-ate-the-budget failure → `Err` (callers
/// already retry/back off on an error).
fn or_translate_response(resp: &Value) -> Result<Value> {
    let choice = resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow::anyhow!("OpenRouter response missing choices[0]"))?;
    let msg = choice.get("message");
    let content = msg
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let reasoning = msg
        .and_then(|m| m.get("reasoning"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls = msg
        .and_then(|m| m.get("tool_calls"))
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty());

    // Candidate parts: tool calls → Gemini `functionCall` parts (with any
    // assistant text kept as a leading text part); otherwise a single text part
    // (unchanged from the text-only shim, incl. the empty-content error paths).
    let parts: Vec<Value> = if let Some(tcs) = tool_calls {
        let mut ps: Vec<Value> = Vec::new();
        if !content.trim().is_empty() {
            ps.push(json!({ "text": content }));
        }
        for tc in tcs {
            let f = tc.get("function");
            let name = f
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args_val = match f.and_then(|f| f.get("arguments")) {
                // OpenAI dialect: `arguments` is a JSON STRING.
                Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                    Ok(v) => v,
                    // Parse failure: surface the raw string under `_raw_args` so
                    // the caller's typed deserialize errors instead of silently
                    // dropping a malformed call.
                    Err(_) => json!({ "_raw_args": s }),
                },
                // glm/z.ai-direct quirk: `arguments` already an object.
                Some(Value::Object(o)) => Value::Object(o.clone()),
                Some(other) => other.clone(),
                None => json!({}),
            };
            ps.push(json!({ "functionCall": { "name": name, "args": args_val } }));
        }
        ps
    } else {
        if content.trim().is_empty() {
            if !reasoning.trim().is_empty() {
                anyhow::bail!(
                    "OpenRouter returned empty content with nonempty reasoning (thinking exhausted the token budget)"
                );
            }
            anyhow::bail!("OpenRouter returned empty content");
        }
        vec![json!({ "text": content })]
    };

    let finish_mapped: String = match choice.get("finish_reason").and_then(|v| v.as_str()) {
        Some("length") => "MAX_TOKENS".to_string(),
        Some("stop") | Some("tool_calls") | None => "STOP".to_string(),
        Some(other) => other.to_uppercase(),
    };

    let usage = resp.get("usage");
    let prompt = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt + completion);
    let mut um = json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": completion,
        "totalTokenCount": total,
    });
    // Cache observability: OpenRouter reports the cache-hit slice of the prompt
    // under `usage.prompt_tokens_details.cached_tokens`; the shim used to drop it.
    // Surface it under the Gemini-shaped `cachedContentTokenCount` so the brain
    // path can report cache ratios identically for both providers. Mapped even
    // when 0 so downstream can distinguish "0% hit" from "provider didn't report".
    if let Some(cached) = usage
        .and_then(|u| u.get("prompt_tokens_details"))
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
    {
        um["cachedContentTokenCount"] = json!(cached);
    }
    // Extra fields (spec §3.2) — exact upstream USD + the serving provider.
    if let Some(cost) = usage.and_then(|u| u.get("cost")).and_then(|v| v.as_f64()) {
        um["openrouterCostUsd"] = json!(cost);
    }
    if let Some(provider) = resp.get("provider").and_then(|v| v.as_str()) {
        um["openrouterProvider"] = json!(provider);
    }

    Ok(json!({
        "candidates": [{
            "content": { "parts": parts, "role": "model" },
            "finishReason": finish_mapped,
        }],
        "usageMetadata": um,
    }))
}

// ── Debug: outgoing generateContent dump (ECK_AI_DUMP_REQUESTS=1) ────────────
//
// When enabled, every native generateContent exchange (Studio/Vertex) and every
// OpenRouter exchange is written to `ECK_AI_DUMP_DIR` (default `./ai-dumps`) as a
// numbered JSON file: `NNNN-<kind>.json` carrying the FINAL outgoing request body,
// the endpoint URL, the model, and the response `usageMetadata` + `modelVersion`.
// The bearer/api-key never appears in the body or URL (they ride request headers),
// so nothing needs redacting. Used to diff turn-to-turn prefixes and confirm what
// the provider actually cached (`cachedContentTokenCount`).
fn ai_dump_enabled() -> bool {
    std::env::var("ECK_AI_DUMP_REQUESTS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn ai_dump_seq() -> u64 {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Write one AI exchange to the dump dir. `mode` = studio|managed|openrouter.
/// `request_body` is the exact JSON put on the wire; `resp` the parsed response.
fn dump_ai_exchange(kind: &str, mode: &str, url: &str, model: &str, request_body: &Value, resp: &Value) {
    if !ai_dump_enabled() {
        return;
    }
    let dir = std::env::var("ECK_AI_DUMP_DIR").unwrap_or_else(|_| "./ai-dumps".to_string());
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let seq = ai_dump_seq();
    let record = json!({
        "seq": seq,
        "kind": kind,
        "mode": mode,
        "url": url,
        "model": model,
        "modelVersion": resp.get("modelVersion"),
        "response_usageMetadata": resp.get("usageMetadata"),
        "request_body": request_body,
    });
    let path = format!("{}/{:04}-{}.json", dir.trim_end_matches('/'), seq, kind);
    if let Ok(s) = serde_json::to_string_pretty(&record) {
        let _ = std::fs::write(&path, s);
        tracing::debug!("[ai-dump] wrote {path} (mode={mode} model={model})");
    }
}

// ── Gemini explicit context caching (Vertex) ────────────────────────────────
//
// Managed-Vertex IMPLICIT caching does not fire on this node: gemini-3.x is
// global-only on Vertex (`DEFAULT_VERTEX_LOCATION = "global"`), and the global
// endpoint load-balances across backends so back-to-back requests lose the warm-
// cache affinity implicit caching depends on — verified live: cachedContentToken
// Count = 0 across four consecutive turns with a byte-stable ~2.1k-token prefix.
// We cannot move gemini-3.x to a regional endpoint to restore affinity, so the
// robust lever is EXPLICIT caching: register the brain's stable head
// (systemInstruction + tool declarations) as a Vertex `cachedContents` resource
// and reference it by name from generateContent. Env-gated (default OFF) so no
// other Vertex caller's wire shape changes unless opted in.

struct CachedHead {
    /// `projects/<p>/locations/<l>/cachedContents/<id>`
    name: String,
    expires_at: Instant,
}

fn gemini_head_cache() -> &'static Mutex<std::collections::HashMap<String, CachedHead>> {
    static C: OnceLock<Mutex<std::collections::HashMap<String, CachedHead>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// One process-wide latch so a "head below the cache minimum" warning fires once.
fn gemini_cache_min_warned() -> &'static AtomicBool {
    static W: OnceLock<AtomicBool> = OnceLock::new();
    W.get_or_init(|| AtomicBool::new(false))
}

fn gemini_explicit_cache_enabled() -> bool {
    std::env::var("ECK_AI_GEMINI_EXPLICIT_CACHE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn gemini_cache_ttl_secs() -> u64 {
    std::env::var("ECK_AI_GEMINI_CACHE_TTL_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n >= 60)
        .unwrap_or(3600)
}

/// Cache key = model + a hash of the stable head (systemInstruction + tools). A
/// change to either the prompt or the tool set naturally rotates the entry.
fn gemini_cache_key(model: &str, system_instruction: &Value, tools: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(model.as_bytes());
    h.update(serde_json::to_vec(system_instruction).unwrap_or_default());
    h.update(serde_json::to_vec(tools).unwrap_or_default());
    format!("{model}:{:x}", h.finalize())
}

async fn evict_head_cache_by_name(name: &str) {
    let mut map = gemini_head_cache().lock().await;
    map.retain(|_, v| v.name != name);
}

#[cfg(test)]
mod tests {
    #[test]
    fn vertex_embedding_model_maps_gemini_embedding_2_to_preview() {
        std::env::remove_var("ECK_VERTEX_EMBEDDING_MODEL");
        assert_eq!(super::vertex_embedding_model("gemini-embedding-2"), "gemini-embedding-2-preview");
        assert_eq!(super::vertex_embedding_model("gemini-embedding-2-preview"), "gemini-embedding-2-preview");
        // Non-Gemini-2 names pass through untouched.
        assert_eq!(super::vertex_embedding_model("gemini-embedding-001"), "gemini-embedding-001");
    }

    use super::*;

    #[test]
    fn backoff_hard_ladder_doubles_then_caps_at_one_hour() {
        // 402/auth: 60 → 120 → 240 → 480 → 960 → 1920 → 3600(cap) → 3600 …
        let expected = [60u64, 120, 240, 480, 960, 1920, 3600, 3600, 3600];
        for (i, &want) in expected.iter().enumerate() {
            assert_eq!(backoff_delay_secs(i as u32 + 1, true), want, "n={}", i + 1);
        }
    }

    #[test]
    fn backoff_soft_ladder_starts_lower() {
        // Transient errors recover faster: 15 → 30 → 60 → 120 …
        assert_eq!(backoff_delay_secs(1, false), 15);
        assert_eq!(backoff_delay_secs(2, false), 30);
        assert_eq!(backoff_delay_secs(3, false), 60);
        assert_eq!(backoff_delay_secs(99, false), 3600); // capped
    }

    #[test]
    fn backoff_never_overflows_at_large_counts() {
        // The shift cap keeps the multiply safe for any consecutive count.
        assert_eq!(backoff_delay_secs(u32::MAX, true), 3600);
        assert_eq!(backoff_delay_secs(u32::MAX, false), 3600);
    }

    #[tokio::test]
    async fn note_failure_arms_backoff_and_clear_resets_it() {
        // Uses the process-wide static; reset first so ordering can't taint it.
        clear_mint_backoff().await;
        assert_eq!(mint_backoff_remaining().await, None);
        note_mint_failure(true).await;
        let remaining = mint_backoff_remaining().await.expect("backoff armed");
        assert!(remaining > 0 && remaining <= 3600 + 3600 / 4);
        clear_mint_backoff().await;
        assert_eq!(mint_backoff_remaining().await, None);
    }

    // ── OpenRouter shim ──────────────────────────────────────────────────────

    /// Request translation: systemInstruction → one system message, contents →
    /// user/assistant by role, a role-less entry defaults to user, gen-config
    /// maps + the 3000 floor + the `usage.include` extra, no reasoning field.
    #[test]
    fn or_request_translates_system_multiturn_and_roleless() {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": "You are a summarizer." }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "hello" }] },
                { "role": "model", "parts": [{ "text": "hi there" }] },
                { "parts": [{ "text": "again" }] } // role-less ⇒ user
            ],
            "generationConfig": { "temperature": 0.2, "maxOutputTokens": 512, "topP": 0.9 }
        });
        let req = or_build_request("z-ai/glm-5.3-flash", &body).unwrap();
        assert_eq!(req["model"], "z-ai/glm-5.3-flash");
        let msgs = req["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are a summarizer.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hello");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "hi there");
        assert_eq!(msgs[3]["role"], "user"); // role-less defaulted
        assert_eq!(msgs[3]["content"], "again");
        // maxOutputTokens 512 floored to 3000.
        assert_eq!(req["max_tokens"], 3000);
        assert_eq!(req["temperature"], 0.2);
        assert_eq!(req["top_p"], 0.9);
        assert_eq!(req["usage"]["include"], true);
        // reasoning is injected by the caller, not by or_build_request.
        assert!(req.get("reasoning").is_none());
    }

    /// maxOutputTokens above the floor is preserved; absent gen-config still gets
    /// the 3000 floor.
    #[test]
    fn or_request_max_tokens_floor_and_passthrough() {
        let hi = or_build_request(
            "z-ai/glm-5.3-flash",
            &json!({ "contents": [], "generationConfig": { "maxOutputTokens": 4096 } }),
        )
        .unwrap();
        assert_eq!(hi["max_tokens"], 4096);
        let none = or_build_request("z-ai/glm-5.3-flash", &json!({ "contents": [] })).unwrap();
        assert_eq!(none["max_tokens"], 3000);
    }

    /// Reasoning models get a higher floor (their thinking eats the budget);
    /// an `ECK_OR_MAX_TOKENS` prefix entry overrides either default.
    #[test]
    fn or_max_tokens_floor_per_model() {
        assert_eq!(or_max_tokens_floor("xiaomi/mimo-v2.6-flash", None), 12000);
        assert_eq!(or_max_tokens_floor("deepseek/deepseek-v4.1-flash", None), 3000);
        assert_eq!(or_max_tokens_floor("xiaomi/mimo-v2.6-pro", Some("xiaomi/=20000,deepseek/=6000")), 20000);
        assert_eq!(or_max_tokens_floor("deepseek/deepseek-v4.1-flash", Some("xiaomi/=20000,deepseek/=6000")), 6000);
        assert_eq!(or_max_tokens_floor("glm-5.3-flash", Some("xiaomi/=x")), 3000);
        let mimo = or_build_request("xiaomi/mimo-v2.6-flash", &json!({ "contents": [] })).unwrap();
        assert_eq!(mimo["max_tokens"], 12000);
    }

    /// A non-functionDeclarations tool (e.g. googleSearch) has nothing to
    /// translate ⇒ no OpenAI `tools` field (no longer a hard bail).
    #[test]
    fn or_request_google_search_tool_ignored() {
        let req = or_build_request(
            "z-ai/glm-5.3-flash",
            &json!({
                "contents": [{ "parts": [{ "text": "x" }] }],
                "tools": [{ "googleSearch": {} }]
            }),
        )
        .unwrap();
        assert!(req.get("tools").is_none());
    }

    /// A request WITHOUT any tools/toolConfig emits neither `tools` nor
    /// `tool_choice` — byte-identical to the pre-tools shim (backward compat).
    #[test]
    fn or_request_no_tools_has_no_tool_fields() {
        let req = or_build_request(
            "z-ai/glm-5.3-flash",
            &json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] }),
        )
        .unwrap();
        assert!(req.get("tools").is_none());
        assert!(req.get("tool_choice").is_none());
        assert_eq!(req["messages"][0]["content"], "hi");
    }

    /// (a) functionDeclarations + a functionCall/functionResponse history
    /// round-trip into valid OpenAI JSON: tools[], an assistant `tool_calls`
    /// message (thoughtSignature stripped, args JSON-stringified), and a matching
    /// `role:"tool"` reply keyed to the synthesized id.
    #[test]
    fn or_request_translates_tools_and_history() {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": "sys" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "how many tickets?" }] },
                { "role": "model", "parts": [
                    { "functionCall": { "name": "get_ticket_count", "args": { "status": "open" } },
                      "thoughtSignature": "SIG-should-be-stripped" }
                ]},
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "get_ticket_count", "response": { "count": 7 } } }
                ]}
            ],
            "tools": [{ "functionDeclarations": [
                { "name": "get_ticket_count", "description": "count tickets",
                  "parameters": { "type": "object", "properties": { "status": { "type": "string" } } } }
            ]}]
        });
        let req = or_build_request("glm-5.3-flash", &body).unwrap();

        // tools[] shape.
        let tools = req["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_ticket_count");
        assert_eq!(tools[0]["function"]["description"], "count tickets");
        assert_eq!(tools[0]["function"]["parameters"]["type"], "object");

        // messages: system, user, assistant(tool_calls), tool.
        let msgs = req["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "how many tickets?");

        assert_eq!(msgs[2]["role"], "assistant");
        assert!(msgs[2]["content"].is_null(), "no text ⇒ null content");
        let tcs = msgs[2]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        let cid = tcs[0]["id"].as_str().unwrap().to_string();
        assert_eq!(tcs[0]["type"], "function");
        assert_eq!(tcs[0]["function"]["name"], "get_ticket_count");
        // arguments is a JSON STRING (OpenAI dialect).
        assert_eq!(tcs[0]["function"]["arguments"], "{\"status\":\"open\"}");
        // thoughtSignature never leaks onto the OR path.
        assert!(msgs[2].get("thoughtSignature").is_none());

        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], cid, "response keyed to the call id");
        assert_eq!(msgs[3]["content"], "{\"count\":7}");
    }

    /// (c) toolConfig NONE → NO tools and NO tool_choice (the last-turn "answer
    /// now", portable across providers); ANY → "required", but "auto" on
    /// auto-only providers (meta/*).
    #[test]
    fn or_request_toolconfig_none_maps_tool_choice() {
        let body = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "x" }] }],
            "tools": [{ "functionDeclarations": [
                { "name": "f", "description": "d", "parameters": { "type": "object" } }
            ]}],
            "toolConfig": { "functionCallingConfig": { "mode": "NONE" } }
        });
        let req = or_build_request("glm-5.3-flash", &body).unwrap();
        assert!(req.get("tool_choice").is_none(), "NONE must not emit tool_choice: {req}");
        assert!(req.get("tools").is_none(), "NONE must drop the tool list: {req}");
        let any_meta = or_build_request(
            "openrouter:meta/muse-spark-1.3-contributor",
            &json!({ "contents": [], "toolConfig": { "functionCallingConfig": { "mode": "ANY" } } }),
        )
        .unwrap();
        assert_eq!(any_meta["tool_choice"], "auto", "meta accepts only auto");
        // AUTO/ANY parity mappings.
        let auto = or_build_request(
            "glm-5.3-flash",
            &json!({ "contents": [], "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } } }),
        )
        .unwrap();
        assert_eq!(auto["tool_choice"], "auto");
        let any = or_build_request(
            "glm-5.3-flash",
            &json!({ "contents": [], "toolConfig": { "functionCallingConfig": { "mode": "ANY" } } }),
        )
        .unwrap();
        assert_eq!(any["tool_choice"], "required");
    }

    /// (b) OpenAI `tool_calls` response → Gemini `functionCall` parts.
    #[test]
    fn or_response_translates_tool_calls() {
        let resp = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        { "id": "call_abc", "type": "function",
                          "function": { "name": "get_ticket_count", "arguments": "{\"status\":\"open\"}" } }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25 }
        });
        let g = or_translate_response(&resp).unwrap();
        let parts = g["candidates"][0]["content"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionCall"]["name"], "get_ticket_count");
        // string args parsed into an object.
        assert_eq!(parts[0]["functionCall"]["args"]["status"], "open");
        // tool_calls finish maps to STOP so the driver loop sees a normal turn.
        assert_eq!(g["candidates"][0]["finishReason"], "STOP");
        assert_eq!(g["usageMetadata"]["totalTokenCount"], 25);
    }

    /// Response tool-call arg dialects: glm sometimes returns `arguments` as an
    /// OBJECT (not a string); a malformed JSON string is surfaced under
    /// `_raw_args` for the caller's error path.
    #[test]
    fn or_response_tool_call_args_object_and_parse_failure() {
        let obj = json!({
            "choices": [{ "message": { "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "f", "arguments": { "a": 1 } } }
            ]}, "finish_reason": "tool_calls" }]
        });
        let g = or_translate_response(&obj).unwrap();
        assert_eq!(g["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["a"], 1);

        let bad = json!({
            "choices": [{ "message": { "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{not json" } }
            ]}, "finish_reason": "tool_calls" }]
        });
        let g = or_translate_response(&bad).unwrap();
        assert_eq!(
            g["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["_raw_args"],
            "{not json"
        );
    }

    /// Vision shim (§3.4): a single image + text part becomes a mixed content
    /// array with one text block and one `image_url` data-URL block.
    #[test]
    fn or_request_vision_single_image_and_text() {
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "text": "Beschreibe dieses Bild." },
                    { "inlineData": { "mimeType": "image/webp", "data": "AAAB" } }
                ]
            }]
        });
        let req = or_build_request("z-ai/glm-5.3-flash", &body).unwrap();
        let content = req["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Beschreibe dieses Bild.");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/webp;base64,AAAB");
    }

    /// Vision shim: multiple images in one message → multiple image_url blocks in
    /// source order (proto snake_case `inline_data`/`mime_type` also accepted).
    #[test]
    fn or_request_vision_multiple_images() {
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "text": "Transkribiere diese Ausschnitte exakt" },
                    { "inline_data": { "mime_type": "image/webp", "data": "IMG1" } },
                    { "inlineData": { "mimeType": "image/png", "data": "IMG2" } }
                ]
            }]
        });
        let req = or_build_request("z-ai/glm-5.3-flash", &body).unwrap();
        let content = req["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["image_url"]["url"], "data:image/webp;base64,IMG1");
        assert_eq!(content[2]["image_url"]["url"], "data:image/png;base64,IMG2");
    }

    /// Brain image-passthrough turn shape (§3): a functionResponse turn stays a
    /// STRING `role:"tool"` message, while the SEPARATE user turn that follows it
    /// carrying text + a JPEG `inlineData` part becomes a multipart user message
    /// (text block + image_url data-URL). This is the exact wire shape
    /// `drive_orchestrator_native` emits when image passthrough is on.
    #[test]
    fn or_request_passthrough_image_user_turn_after_tool_result() {
        let body = json!({
            "contents": [
                { "role": "model", "parts": [
                    { "functionCall": { "name": "ocr_attachment", "args": { "file_ids": ["cas-1"] } } }
                ]},
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "ocr_attachment", "response": { "text": "hello" } } }
                ]},
                { "role": "user", "parts": [
                    { "text": "Attachment image for scan.jpg (page 0) — the same page the tool above described." },
                    { "inlineData": { "mimeType": "image/jpeg", "data": "SkZJRg==" } }
                ]}
            ]
        });
        let req = or_build_request("z-ai/glm-5.3-flash", &body).unwrap();
        let msgs = req["messages"].as_array().unwrap();
        // [assistant tool_calls, tool result, user image message]
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "assistant");
        assert!(msgs[0]["tool_calls"].is_array());
        // Tool result MUST remain a plain string (no image blocks leak in).
        assert_eq!(msgs[1]["role"], "tool");
        assert!(msgs[1]["content"].is_string());
        // The image rides a SEPARATE user turn, translated to a multipart array.
        assert_eq!(msgs[2]["role"], "user");
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/jpeg;base64,SkZJRg==");
    }

    /// Vision shim: a message with NO image part keeps the exact wave-1
    /// concatenated-STRING content shape (byte-for-byte unchanged).
    #[test]
    fn or_request_pure_text_content_unchanged() {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": "sys" }] },
            "contents": [{ "role": "user", "parts": [{ "text": "a" }, { "text": "b" }] }]
        });
        let req = or_build_request("z-ai/glm-5.3-flash", &body).unwrap();
        // system + user both remain plain strings, not arrays.
        assert_eq!(req["messages"][0]["content"], "sys");
        assert_eq!(req["messages"][1]["content"], "ab");
        assert!(req["messages"][1]["content"].is_string());
    }

    /// Reasoning table + override (§3.3).
    #[test]
    fn or_reasoning_table_matches_bench() {
        assert_eq!(
            or_reasoning_for("qwen/qwen3.5-flash-02-23", None),
            OrReasoning::Field(json!({ "enabled": false }), false)
        );
        assert_eq!(
            or_reasoning_for("z-ai/glm-5.3-flash", None),
            OrReasoning::Field(json!({ "effort": "low" }), false),
            "OR path (z-ai/ prefix) unchanged: still the nested reasoning object"
        );
        assert_eq!(
            or_reasoning_for("deepseek/deepseek-v4.1-flash", None),
            OrReasoning::Field(json!({ "enabled": false }), true),
            "deepseek/ slug: thinking off, keep the 400-retry"
        );
        // Z.ai-direct glm (unprefixed) → TOP-LEVEL reasoning_effort, not the object.
        assert_eq!(
            or_reasoning_for("glm-5.3-flash", None),
            OrReasoning::ReasoningEffort(json!("low"))
        );
        // Override for glm routes an effort value to reasoning_effort too…
        assert_eq!(
            or_reasoning_for("glm-5.3-flash", Some("glm=high")),
            OrReasoning::ReasoningEffort(json!("high"))
        );
        // …and omit still means send nothing.
        assert_eq!(
            or_reasoning_for("glm-5.3-flash", Some("glm=omit")),
            OrReasoning::Omit
        );
        assert_eq!(
            or_reasoning_for("google/gemini-3.7-flash", None),
            OrReasoning::Field(json!({ "effort": "low" }), false)
        );
        assert_eq!(or_reasoning_for("anthropic/claude-haiku-4.5", None), OrReasoning::Omit);
        // unknown ⇒ effort low + retry-on-400 armed.
        assert_eq!(
            or_reasoning_for("acme/mystery-1", None),
            OrReasoning::Field(json!({ "effort": "low" }), true)
        );
        // Override wins, never asks for the 400-retry.
        assert_eq!(
            or_reasoning_for("z-ai/glm-5.3-flash", Some("z-ai/=off")),
            OrReasoning::Field(json!({ "enabled": false }), false)
        );
        assert_eq!(
            or_reasoning_for("z-ai/glm-5.3-flash", Some("z-ai/=omit")),
            OrReasoning::Omit
        );
        assert_eq!(
            or_reasoning_for("z-ai/glm-5.3-flash", Some("z-ai/=high")),
            OrReasoning::Field(json!({ "effort": "high" }), false)
        );
    }

    /// Serialize env-mutating tests — `cargo test` runs them in parallel and the
    /// process env is global, so two tests toggling the same var would race.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static L: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Clear every env var the routing/resolution tests touch, so one test's
    /// setup can't leak into another that runs later under the same guard.
    fn clear_ai_env() {
        for k in [
            "ECK_AI_TASK_ROUTES",
            "ECK_AI_MODEL_DENY",
            "OPENROUTER_API_KEY",
            "ECK_AI_MODE",
            "ECK_OR_PROXY_URL",
            "ECK_OR_SERVE_NODE",
            "ECK_OR_BASE_URL",
            "ECK_OR_SLUG_API_KEY",
            "ECK_OR_SLUG_BASE_URL",
            "ECK_LICENSE_TOKEN",
            "ECK_VERTEX_MINT_URL",
            "ECK_VERTEX_BEARER",
            "GEMINI_API_KEY",
            "ECK_AI_TASK_BUDGET",
            "ECK_AI_HEDGE_SECS",
        ] {
            std::env::remove_var(k);
        }
    }

    /// ECK_AI_TASK_ROUTES wildcard + opt-out precedence.
    #[test]
    fn or_route_wildcard_and_optout() {
        let _g = env_guard();
        clear_ai_env();

        // Wildcard applies to any task with no explicit entry; an explicit entry
        // wins over the wildcard.
        std::env::set_var("ECK_AI_TASK_ROUTES", "*=glm-5.3-flash,vision=google/gemini-3.7-flash");
        assert_eq!(or_route_for_task("translation").as_deref(), Some("glm-5.3-flash"));
        assert_eq!(or_route_for_task("summarization").as_deref(), Some("glm-5.3-flash"));
        assert_eq!(or_route_for_task("vision").as_deref(), Some("google/gemini-3.7-flash"));

        // `default=` is an accepted spelling of the wildcard.
        std::env::set_var("ECK_AI_TASK_ROUTES", "default=glm-5.3-flash");
        assert_eq!(or_route_for_task("brain").as_deref(), Some("glm-5.3-flash"));

        // Explicit opt-out (`task=off` and `task=`) beats the wildcard.
        std::env::set_var("ECK_AI_TASK_ROUTES", "*=glm-5.3-flash,voice=off,trips=");
        assert_eq!(or_route_for_task("voice"), None);
        assert_eq!(or_route_for_task("trips"), None);
        assert_eq!(or_route_for_task("brain").as_deref(), Some("glm-5.3-flash"));

        // Unset ⇒ None.
        std::env::remove_var("ECK_AI_TASK_ROUTES");
        assert_eq!(or_route_for_task("brain"), None);
        clear_ai_env();
    }

    /// task_budget_secs: defaults, explicit override, wildcard, and the
    /// ECK_AI_HEDGE_SECS brain back-compat override + its precedence.
    #[test]
    fn task_budget_precedence() {
        let _g = env_guard();
        clear_ai_env();

        // Built-in defaults.
        assert_eq!(task_budget_secs("brain"), 25);
        assert_eq!(task_budget_secs("summarization"), 1800);
        assert_eq!(task_budget_secs("unknown-task"), 120);

        // Wildcard applies to tasks with no explicit entry.
        std::env::set_var("ECK_AI_TASK_BUDGET", "*=42,summarization=999");
        assert_eq!(task_budget_secs("translation"), 42);
        assert_eq!(task_budget_secs("summarization"), 999);

        // ECK_AI_HEDGE_SECS overrides the brain DEFAULT but not an explicit
        // ECK_AI_TASK_BUDGET brain= entry.
        clear_ai_env();
        std::env::set_var("ECK_AI_HEDGE_SECS", "7");
        assert_eq!(task_budget_secs("brain"), 7);
        assert_eq!(task_budget_secs("translation"), 300); // hedge secs is brain-only
        std::env::set_var("ECK_AI_TASK_BUDGET", "brain=3");
        assert_eq!(task_budget_secs("brain"), 3, "explicit budget beats hedge secs");
        clear_ai_env();
    }

    /// OR retry classification: 401/403 are non-retryable, everything else is.
    #[test]
    fn or_retry_error_classification() {
        assert!(!or_error_is_retryable("OpenRouter chat error (401 Unauthorized, model=glm): bad key"));
        assert!(!or_error_is_retryable("OpenRouter chat error (403 Forbidden, model=glm): denied"));
        assert!(or_error_is_retryable("OpenRouter chat error (429 Too Many Requests, model=glm)"));
        assert!(or_error_is_retryable("OpenRouter chat error (500, model=glm)"));
        assert!(or_error_is_retryable("error sending request: operation timed out"));
        assert!(or_error_is_retryable("OpenRouter returned empty content"));
    }

    /// Slug upstream (§slug) pure decision: only a slug model (name with `/`) AND
    /// a non-empty slug key select the second upstream; empty base ⇒ public host.
    #[test]
    fn slug_upstream_split() {
        // Unprefixed model ⇒ never (byte-identical to today, even with a key).
        assert_eq!(slug_upstream_for("glm-5.3-flash", "sk-slug", ""), None);
        // Slug model but no slug key ⇒ never.
        assert_eq!(slug_upstream_for("deepseek/deepseek-v4.1-flash", "  ", "x"), None);
        // Slug model + key, empty base ⇒ public OpenRouter host.
        assert_eq!(
            slug_upstream_for("deepseek/deepseek-v4.1-flash", "sk-slug", ""),
            Some(("https://openrouter.ai/api/v1".to_string(), "sk-slug".to_string()))
        );
        // Explicit base is trimmed and carried through with the key.
        assert_eq!(
            slug_upstream_for("deepseek/deepseek-v4.1-flash", " sk-slug ", " https://or.example/api "),
            Some(("https://or.example/api".to_string(), "sk-slug".to_string()))
        );
    }

    /// OR backoff schedule: 2s, 5s, 10s, then 20s cap.
    #[test]
    fn or_backoff_schedule() {
        assert_eq!(or_retry_delay(1), Duration::from_secs(2));
        assert_eq!(or_retry_delay(2), Duration::from_secs(5));
        assert_eq!(or_retry_delay(3), Duration::from_secs(10));
        assert_eq!(or_retry_delay(4), Duration::from_secs(20));
        assert_eq!(or_retry_delay(6), Duration::from_secs(20));
    }

    /// ECK_AI_TASK_ROUTES parsing.
    #[test]
    fn or_route_parsing() {
        let _g = env_guard();
        clear_ai_env();
        std::env::set_var(
            "ECK_AI_TASK_ROUTES",
            "summarization=z-ai/glm-5.3-flash, doc_classify=z-ai/glm-5.3-flash",
        );
        assert_eq!(
            or_route_for_task("summarization").as_deref(),
            Some("z-ai/glm-5.3-flash")
        );
        assert_eq!(
            or_route_for_task("doc_classify").as_deref(),
            Some("z-ai/glm-5.3-flash")
        );
        assert_eq!(or_route_for_task("translation"), None);
        std::env::remove_var("ECK_AI_TASK_ROUTES");
        assert_eq!(or_route_for_task("summarization"), None);
    }

    /// Node-side model policy filter (`ECK_AI_MODEL_DENY`) — pure prefix match.
    #[test]
    fn model_policy_deny_prefix() {
        let denied = vec!["z-ai/".to_string(), "qwen/".to_string()];
        assert!(model_denied_by_policy("z-ai/glm-5.3-flash", &denied));
        assert!(model_denied_by_policy("qwen/qwen3.5-flash-02-23", &denied));
        assert!(!model_denied_by_policy("anthropic/claude-haiku-4.5", &denied));
        assert!(!model_denied_by_policy("google/gemini-3.7-flash", &denied));
        // Empty / whitespace-only entries deny nothing.
        assert!(!model_denied_by_policy("z-ai/glm-5.3-flash", &[]));
        assert!(!model_denied_by_policy("z-ai/glm-5.3-flash", &["   ".to_string()]));
    }

    /// `ECK_AI_MODEL_DENY` env parsing → deny-list.
    #[test]
    fn ai_model_deny_list_parses_env() {
        let _g = env_guard();
        clear_ai_env();
        assert!(ai_model_deny_list().is_empty(), "unset ⇒ empty");
        std::env::set_var("ECK_AI_MODEL_DENY", "z-ai/, ,qwen/");
        assert_eq!(ai_model_deny_list(), vec!["z-ai/".to_string(), "qwen/".to_string()]);
        clear_ai_env();
    }

    /// generate_url: managed proxy uses ECK_OR_PROXY_URL verbatim (no
    /// `/chat/completions`); BYOK appends it (and honours ECK_OR_BASE_URL).
    #[test]
    fn or_generate_url_proxy_vs_direct() {
        let _g = env_guard();
        clear_ai_env();

        // Managed proxy: URL is the env value verbatim, trailing slash trimmed,
        // NOTHING appended (the proxy IS the endpoint).
        std::env::set_var("ECK_OR_PROXY_URL", "https://xelth.com:3221/ai/or");
        let managed = AiAuth::OpenRouter {
            api_key: "lic-123".to_string(),
            model: "z-ai/glm-5.3-flash".to_string(),
            proxied: true,
            serve_node: None,
            base_url: None,
        };
        assert_eq!(managed.generate_url("ignored"), "https://xelth.com:3221/ai/or");
        std::env::set_var("ECK_OR_PROXY_URL", "https://xelth.com:3221/ai/or/");
        assert_eq!(
            managed.generate_url("ignored"),
            "https://xelth.com:3221/ai/or",
            "trailing slash trimmed, still no /chat/completions"
        );
        clear_ai_env();

        // BYOK direct: default host + /chat/completions.
        let byok = AiAuth::OpenRouter {
            api_key: "sk-or-key".to_string(),
            model: "z-ai/glm-5.3-flash".to_string(),
            proxied: false,
            serve_node: None,
            base_url: None,
        };
        assert_eq!(
            byok.generate_url("ignored"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        // BYOK honours ECK_OR_BASE_URL (self-test override).
        std::env::set_var("ECK_OR_BASE_URL", "http://127.0.0.1:9099/api/v1");
        assert_eq!(
            byok.generate_url("ignored"),
            "http://127.0.0.1:9099/api/v1/chat/completions"
        );
        clear_ai_env();
    }

    /// Slug upstream resolution (§slug): a slug model wins the second upstream
    /// (own key + base, `base_url: Some`) EVEN when `OPENROUTER_API_KEY` holds a
    /// vendor-direct (Z.ai) key; an unprefixed model on the same node stays BYOK
    /// on the primary key, byte-identical to before.
    #[tokio::test]
    async fn resolve_slug_upstream_second_key() {
        let _g = env_guard();
        clear_ai_env();
        let http = reqwest::Client::new();
        // Z.ai-direct node: primary key + base, wildcard glm, plus a slug key.
        std::env::set_var("ECK_AI_TASK_ROUTES", "*=glm-5.3-flash");
        std::env::set_var("OPENROUTER_API_KEY", "zai-key");
        std::env::set_var("ECK_OR_BASE_URL", "https://api.z.ai/api/paas/v4");
        std::env::set_var("ECK_OR_SLUG_API_KEY", "sk-or-slug");

        // Pinned slug model → slug upstream (second key, base_url:Some, direct).
        let a = AiAuth::resolve_for_task_with_model(&http, "brain", "deepseek/deepseek-v4.1-flash")
            .await
            .unwrap();
        match a {
            AiAuth::OpenRouter { ref api_key, ref model, proxied, ref serve_node, ref base_url } => {
                assert_eq!(api_key, "sk-or-slug", "slug model carries its OWN key, not the Z.ai key");
                assert_eq!(model, "deepseek/deepseek-v4.1-flash");
                assert!(!proxied && serve_node.is_none(), "slug upstream is BYOK-direct");
                assert_eq!(
                    base_url.as_deref(),
                    Some("https://openrouter.ai/api/v1"),
                    "default slug base = public OpenRouter; base_url:Some ⇒ unreported to wallet"
                );
            }
            other => panic!("expected slug OpenRouter, got {other:?}"),
        }

        // Wildcard-routed unprefixed model on the SAME node → primary BYOK,
        // base_url:None (byte-identical to today; Z.ai key + host).
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        match a {
            AiAuth::OpenRouter { ref api_key, ref model, proxied, ref serve_node, ref base_url } => {
                assert_eq!(api_key, "zai-key");
                assert_eq!(model, "glm-5.3-flash");
                assert!(!proxied && serve_node.is_none());
                assert!(base_url.is_none(), "unprefixed model untouched by the slug split");
            }
            other => panic!("expected primary BYOK OpenRouter, got {other:?}"),
        }
        clear_ai_env();
    }

    /// resolve_for_task resolution matrix: route set × key present/absent × mode
    /// managed/studio (+ proxy present). No network is hit: BYOK and managed-proxy
    /// return synchronously, and the studio fallback returns Studio without a mint.
    #[tokio::test]
    async fn resolve_for_task_managed_matrix() {
        let _g = env_guard();
        clear_ai_env();
        let http = reqwest::Client::new();
        std::env::set_var("ECK_AI_TASK_ROUTES", "summarization=z-ai/glm-5.3-flash");

        // 1) BYOK key present → direct OpenRouter (proxied=false), regardless of mode.
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-byok");
        std::env::set_var("ECK_AI_MODE", "managed");
        std::env::set_var("ECK_OR_PROXY_URL", "https://xelth.com:3221/ai/or");
        std::env::set_var("ECK_LICENSE_TOKEN", "lic-abc");
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        match a {
            AiAuth::OpenRouter { ref api_key, ref model, proxied, ref serve_node, .. } => {
                assert_eq!(api_key, "sk-or-byok");
                assert_eq!(model, "z-ai/glm-5.3-flash");
                assert!(!proxied, "node-local key ⇒ BYOK direct");
                assert!(serve_node.is_none(), "BYOK never uses the relay transport");
            }
            other => panic!("expected BYOK OpenRouter, got {other:?}"),
        }

        // 2) key absent + managed + proxy + license → managed proxy (proxied=true,
        //    api_key = license).
        std::env::remove_var("OPENROUTER_API_KEY");
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        match a {
            AiAuth::OpenRouter { ref api_key, ref model, proxied, ref serve_node, .. } => {
                assert_eq!(api_key, "lic-abc", "managed carries the license as bearer");
                assert_eq!(model, "z-ai/glm-5.3-flash");
                assert!(proxied, "managed proxy path");
                assert!(serve_node.is_none(), "proxy path ⇒ no relay node");
            }
            other => panic!("expected managed OpenRouter, got {other:?}"),
        }

        // 2b) key absent + managed + NO proxy + serve-node + license → the native
        //     relay transport (proxied=false, serve_node=Some, api_key=license).
        std::env::remove_var("ECK_OR_PROXY_URL");
        std::env::set_var("ECK_OR_SERVE_NODE", "node-mesh-uuid-1");
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        match a {
            AiAuth::OpenRouter { ref api_key, ref model, proxied, ref serve_node, .. } => {
                assert_eq!(api_key, "lic-abc", "relay path still carries the license");
                assert_eq!(model, "z-ai/glm-5.3-flash");
                assert!(!proxied, "relay path is not the HTTP proxy");
                assert_eq!(serve_node.as_deref(), Some("node-mesh-uuid-1"));
            }
            other => panic!("expected relay OpenRouter, got {other:?}"),
        }
        // Proxy URL WINS over serve-node when both are set (direct beats relay).
        std::env::set_var("ECK_OR_PROXY_URL", "https://xelth.com:3221/ai/or");
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        match a {
            AiAuth::OpenRouter { proxied, ref serve_node, .. } => {
                assert!(proxied && serve_node.is_none(), "proxy precedence over relay");
            }
            other => panic!("expected proxy OpenRouter, got {other:?}"),
        }
        std::env::remove_var("ECK_OR_SERVE_NODE");

        // 3) key absent + STUDIO mode → fall through to default auth (Studio, no
        //    network mint). Proxy env is irrelevant outside managed mode.
        std::env::set_var("ECK_AI_MODE", "studio");
        std::env::set_var("GEMINI_API_KEY", "studio-key-xyz");
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        assert_eq!(a.mode(), "studio", "studio fallback, not the OR proxy");

        // 4) key absent + managed but NO proxy → fall through. In managed mode the
        //    fallback resolve() tries to mint; with no mint URL it errs fast (no
        //    network), which is the observable "did not take the OR proxy" signal.
        std::env::set_var("ECK_AI_MODE", "managed");
        std::env::remove_var("ECK_OR_PROXY_URL");
        std::env::remove_var("ECK_VERTEX_MINT_URL");
        let r = AiAuth::resolve_for_task(&http, "summarization").await;
        assert!(r.is_err(), "managed + no proxy + no mint URL ⇒ default-auth mint error, not an OR proxy auth");

        // 5) route present but model denied by node policy → treated as no route,
        //    falls through to Studio (studio mode, no network).
        std::env::set_var("ECK_AI_MODE", "studio");
        std::env::set_var("ECK_AI_MODEL_DENY", "z-ai/");
        let a = AiAuth::resolve_for_task(&http, "summarization").await.unwrap();
        assert_eq!(a.mode(), "studio", "denied model ⇒ default auth, never OpenRouter");

        clear_ai_env();
    }

    /// Relay envelope (native paid-mesh transport): transport-neutral shape +
    /// the client-set validity window that lets a minutes-long AI call survive
    /// (the property that killed the ops channel). No polling-specific fields, so
    /// a future WS burst carries it unchanged.
    #[test]
    fn or_relay_envelope_is_transport_neutral_with_10min_window() {
        let req = json!({ "model": "z-ai/glm-5.3-flash", "messages": [] });
        let now = 1_800_000_000i64;
        let ttl = 210i64; // 180 s upstream + 30 s margin ≥ our >=10 min floor once maxed
        let env = or_build_relay_envelope("node-uuid-9", "lic-xyz", &req, "nonce-1", now, ttl);

        // Routing + auth + payload live where the relay/serving node expect them.
        assert_eq!(env["envelope"]["target_uuid"], "node-uuid-9");
        assert_eq!(env["envelope"]["kind"], "ai.or.request");
        assert_eq!(env["envelope"]["nonce"], "nonce-1");
        assert_eq!(env["license"], "lic-xyz");
        assert_eq!(env["request"]["model"], "z-ai/glm-5.3-flash");

        // Validity window is the envelope's OWN expires_at, not a 60 s sig age.
        assert_eq!(env["envelope"]["created_at"], now);
        assert_eq!(env["envelope"]["expires_at"], now + ttl);

        // Transport-neutral: no poll interval / deadline / task_id leaked in.
        let e = env["envelope"].as_object().unwrap();
        for forbidden in ["poll_interval", "deadline", "task_id", "wait", "next_poll_in_seconds"] {
            assert!(!e.contains_key(forbidden), "envelope must not carry '{forbidden}'");
        }
        // With ECK_OR_TIMEOUT_SECS floored at 120 + 30 margin the real deadline is
        // >= 150 s; a caller can set a >= 10 min budget via the env — assert the
        // arithmetic is the window, not a fixed short age.
        let big = or_build_relay_envelope("n", "l", &req, "x", now, 600);
        assert_eq!(big["envelope"]["expires_at"], now + 600);
    }

    /// Response translation: content → parts[0].text, finish_reason mapping,
    /// usage → usageMetadata incl. the OR extras.
    #[test]
    fn or_response_translates_text_and_usage() {
        let resp = json!({
            "choices": [{
                "message": { "content": "=== SUMMARY ===\nDone." },
                "finish_reason": "stop"
            }],
            "provider": "Alibaba",
            "usage": { "prompt_tokens": 2452, "completion_tokens": 717, "total_tokens": 3169, "cost": 0.00016677 }
        });
        let g = or_translate_response(&resp).unwrap();
        assert_eq!(g["candidates"][0]["content"]["parts"][0]["text"], "=== SUMMARY ===\nDone.");
        assert_eq!(g["candidates"][0]["finishReason"], "STOP");
        assert_eq!(g["usageMetadata"]["promptTokenCount"], 2452);
        assert_eq!(g["usageMetadata"]["candidatesTokenCount"], 717);
        assert_eq!(g["usageMetadata"]["totalTokenCount"], 3169);
        assert_eq!(g["usageMetadata"]["openrouterCostUsd"], 0.00016677);
        assert_eq!(g["usageMetadata"]["openrouterProvider"], "Alibaba");
    }

    /// Z.ai-direct response shape: NO `usage.cost`, NO `provider` field. The
    /// translation must degrade gracefully — token counts still map, and the
    /// two OR-only extras are simply omitted (never a panic).
    #[test]
    fn or_response_tolerates_missing_cost_and_provider() {
        let resp = json!({
            "choices": [{
                "message": { "role": "assistant", "content": "OK" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 1, "total_tokens": 13 }
        });
        let g = or_translate_response(&resp).unwrap();
        assert_eq!(g["candidates"][0]["content"]["parts"][0]["text"], "OK");
        assert_eq!(g["candidates"][0]["finishReason"], "STOP");
        assert_eq!(g["usageMetadata"]["promptTokenCount"], 12);
        assert_eq!(g["usageMetadata"]["candidatesTokenCount"], 1);
        assert_eq!(g["usageMetadata"]["totalTokenCount"], 13);
        // The OR-only extras are absent, not null or zero.
        assert!(g["usageMetadata"].get("openrouterCostUsd").is_none());
        assert!(g["usageMetadata"].get("openrouterProvider").is_none());
    }

    /// GLM engine (OpenRouter shim): the translated OpenAI-chat wire request has
    /// a byte-identical cacheable head — the system message + the `tools` block —
    /// turn-to-turn, and the leading `messages` (all but the new user turn) are a
    /// stable byte-prefix. This is the property z.ai's automatic prefix cache keys
    /// on (verified live: 0% turn 1 → ~97% turn 2+).
    #[test]
    fn or_wire_request_prefix_is_stable() {
        let sys = json!({ "parts": [{ "text": "SYSTEM PROMPT (stable head)" }] });
        let tools = json!([{ "functionDeclarations": [
            { "name": "a_tool", "description": "d", "parameters": { "type": "object" } },
            { "name": "b_tool", "description": "d", "parameters": { "type": "object" } },
        ] }]);
        let mk = |contents: Value| -> Value {
            json!({ "systemInstruction": sys, "contents": contents, "tools": tools })
        };
        // Turn N: [q1, a1, q2]. Turn N+1 appends a2 + q3.
        let cn = json!([
            { "role": "user", "parts": [{ "text": "q1" }] },
            { "role": "model", "parts": [{ "text": "a1" }] },
            { "role": "user", "parts": [{ "text": "q2" }] },
        ]);
        let cn1 = json!([
            { "role": "user", "parts": [{ "text": "q1" }] },
            { "role": "model", "parts": [{ "text": "a1" }] },
            { "role": "user", "parts": [{ "text": "q2" }] },
            { "role": "model", "parts": [{ "text": "a2" }] },
            { "role": "user", "parts": [{ "text": "q3" }] },
        ]);
        let rn = or_build_request("glm-5.3-flash", &mk(cn)).unwrap();
        let rn1 = or_build_request("glm-5.3-flash", &mk(cn1)).unwrap();

        // Head byte-identical: system message [0] + the tools block.
        assert_eq!(rn["messages"][0], rn1["messages"][0]);
        assert_eq!(rn["messages"][0]["role"], "system");
        assert_eq!(rn["tools"], rn1["tools"]);

        // Leading messages (all but turn N's new user question) prefix N+1's.
        let concat = |m: &Value| -> String {
            m.as_array()
                .unwrap()
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mn = rn["messages"].as_array().unwrap();
        let head_n = concat(&json!(mn[..mn.len() - 1]));
        let full_n1 = concat(&rn1["messages"]);
        assert!(full_n1.starts_with(&head_n), "or messages head must prefix N+1");
    }

    /// finish_reason `length` → MAX_TOKENS.
    #[test]
    fn or_response_length_maps_to_max_tokens() {
        let resp = json!({
            "choices": [{ "message": { "content": "partial" }, "finish_reason": "length" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3000, "total_tokens": 3010 }
        });
        let g = or_translate_response(&resp).unwrap();
        assert_eq!(g["candidates"][0]["finishReason"], "MAX_TOKENS");
        // total falls back to prompt+completion when absent — here it's present.
        assert_eq!(g["usageMetadata"]["totalTokenCount"], 3010);
    }

    /// Empty content WITH nonempty reasoning ⇒ Err (thinking ate the budget).
    #[test]
    fn or_response_empty_with_reasoning_errs() {
        let resp = json!({
            "choices": [{
                "message": { "content": "", "reasoning": "Let me think step by step..." },
                "finish_reason": "length"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3000, "total_tokens": 3010 }
        });
        assert!(or_translate_response(&resp).is_err());
        // Empty content with NO reasoning is also an error (nothing usable).
        let resp2 = json!({ "choices": [{ "message": { "content": "" }, "finish_reason": "stop" }] });
        assert!(or_translate_response(&resp2).is_err());
    }

    /// Golden: a recorded glm response (real OR shape, incl. the nested
    /// `usage.cost_details`/`prompt_tokens_details` the shim ignores) maps to the
    /// exact Gemini-shaped Value a caller expects.
    #[test]
    fn or_golden_glm_response() {
        let recorded = json!({
            "id": "gen-1788034450-kxzIOgb8Bono1LnRzDGh",
            "provider": "Alibaba",
            "model": "z-ai/glm-5.3-flash",
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "=== LOGISTIK & KONTAKTE ===\n**Gerät / Modell:** ZetaBody 770"
                }
            }],
            "usage": {
                "prompt_tokens": 2452,
                "completion_tokens": 717,
                "total_tokens": 3169,
                "cost": 0.00016677,
                "is_byok": false,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "cost_details": { "upstream_inference_cost": 0.00016677 },
                "completion_tokens_details": { "reasoning_tokens": 0 }
            }
        });
        let g = or_translate_response(&recorded).unwrap();
        let expected = json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": "=== LOGISTIK & KONTAKTE ===\n**Gerät / Modell:** ZetaBody 770" }],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 2452,
                "candidatesTokenCount": 717,
                "totalTokenCount": 3169,
                "cachedContentTokenCount": 0,
                "openrouterCostUsd": 0.00016677,
                "openrouterProvider": "Alibaba"
            }
        });
        assert_eq!(g, expected);
    }

    /// Cache observability: a nonzero `prompt_tokens_details.cached_tokens`
    /// maps to `usageMetadata.cachedContentTokenCount` (was silently dropped).
    #[test]
    fn or_maps_cached_tokens_to_gemini_shape() {
        let resp = json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 2000,
                "completion_tokens": 10,
                "total_tokens": 2010,
                "prompt_tokens_details": { "cached_tokens": 1700 }
            }
        });
        let g = or_translate_response(&resp).unwrap();
        assert_eq!(g["usageMetadata"]["cachedContentTokenCount"], 1700);
        // Absent details → field omitted (not forced to 0).
        let resp2 = json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
        });
        let g2 = or_translate_response(&resp2).unwrap();
        assert!(g2["usageMetadata"].get("cachedContentTokenCount").is_none());
    }

    /// The Gemini text wrapper `generate_content` reads the shim's output shape
    /// unchanged: parts[0].text extraction + usageMetadata both work.
    #[test]
    fn or_translated_shape_feeds_generate_content_extraction() {
        let resp = json!({
            "choices": [{ "message": { "content": "hallo welt" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
        });
        let g = or_translate_response(&resp).unwrap();
        // Mirror generate_content's extraction path.
        let text = g["candidates"][0]["content"]["parts"][0]["text"].as_str().unwrap();
        assert_eq!(text, "hallo welt");
        assert!(!g["usageMetadata"].is_null());
    }

    /// Keyed live smoke test (§8): hits glm with "Reply OK" (~$0.000002).
    /// `cargo test -p eck_core or_smoke -- --ignored` with OPENROUTER_API_KEY set.
    #[tokio::test]
    #[ignore]
    async fn or_smoke() {
        let key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY unset");
        let auth = AiAuth::OpenRouter { api_key: key, model: "z-ai/glm-5.3-flash".to_string(), proxied: false, serve_node: None, base_url: None };
        let http = reqwest::Client::new();
        let body = json!({
            "systemInstruction": { "parts": [{ "text": "Answer with exactly: OK" }] },
            "contents": [{ "role": "user", "parts": [{ "text": "Reply OK" }] }],
            "generationConfig": { "temperature": 0.0, "maxOutputTokens": 3000 }
        });
        let (text, usage) = auth.generate_content(&http, "z-ai/glm-5.3-flash", body).await.unwrap();
        assert!(text.to_uppercase().contains("OK"), "got: {text}");
        assert!(usage["totalTokenCount"].as_u64().unwrap_or(0) > 0);
    }

    /// LIVE function-calling smoke against Z.ai-direct glm (§5): one tool
    /// (`get_ticket_count`), assert the model emits a functionCall, feed the
    /// result back, assert a final text answer that quotes the count. Spend
    /// ~<$0.002. Run with the direct key + base URL set:
    ///   XELTH_ZAI_KEY=… ECK_OR_BASE_URL=https://api.z.ai/api/paas/v4 \
    ///   cargo test -p eck_core or_tool_smoke -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn or_tool_smoke() {
        let key = std::env::var("XELTH_ZAI_KEY")
            .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
            .expect("XELTH_ZAI_KEY (or OPENROUTER_API_KEY) unset");
        if std::env::var("ECK_OR_BASE_URL").is_err() {
            std::env::set_var("ECK_OR_BASE_URL", "https://api.z.ai/api/paas/v4");
        }
        let auth = AiAuth::OpenRouter {
            api_key: key,
            model: "glm-5.3-flash".to_string(),
            proxied: false,
            serve_node: None,
            base_url: None,
        };
        let http = reqwest::Client::new();

        let tools = json!([{ "functionDeclarations": [{
            "name": "get_ticket_count",
            "description": "Return the number of open support tickets.",
            "parameters": {
                "type": "object",
                "properties": { "status": { "type": "string", "description": "ticket status filter" } },
                "required": ["status"]
            }
        }]}]);

        // Turn 1: model should call the tool.
        let body1 = json!({
            "systemInstruction": { "parts": [{ "text": "You answer using the provided tools. Do not guess." }] },
            "contents": [{ "role": "user", "parts": [{ "text": "How many OPEN tickets are there right now? Use the tool." }] }],
            "tools": tools,
            "generationConfig": { "temperature": 0.0, "maxOutputTokens": 3000 }
        });
        let r1 = auth.generate_content_raw(&http, "glm-5.3-flash", body1).await.unwrap();
        let parts1 = r1["candidates"][0]["content"]["parts"].as_array().cloned().unwrap_or_default();
        let call = parts1
            .iter()
            .find_map(|p| p.get("functionCall"))
            .expect("model did not emit a functionCall on turn 1");
        assert_eq!(call["name"], "get_ticket_count", "unexpected tool: {call:?}");
        eprintln!("[or_tool_smoke] turn1 functionCall = {}", call);

        // Turn 2: echo the model turn back VERBATIM + a functionResponse, forbid
        // further tool calls, assert a text answer that quotes the count (42).
        let body2 = json!({
            "systemInstruction": { "parts": [{ "text": "You answer using the provided tools. Do not guess." }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "How many OPEN tickets are there right now? Use the tool." }] },
                r1["candidates"][0]["content"].clone(),
                { "role": "user", "parts": [{ "functionResponse": { "name": "get_ticket_count", "response": { "count": 42 } } }] }
            ],
            "tools": tools,
            "toolConfig": { "functionCallingConfig": { "mode": "NONE" } },
            "generationConfig": { "temperature": 0.0, "maxOutputTokens": 3000 }
        });
        let (text, usage) = auth.generate_content(&http, "glm-5.3-flash", body2).await.unwrap();
        eprintln!("[or_tool_smoke] final text = {text}");
        eprintln!("[or_tool_smoke] usage = {usage}");
        assert!(text.contains("42"), "final answer should quote the count 42; got: {text}");
        // Spend cap sanity: this tiny exchange must be well under $0.20.
        if let Some(cost) = usage["openrouterCostUsd"].as_f64() {
            assert!(cost < 0.20, "spend cap exceeded: ${cost}");
        }
    }
}
