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
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Gemini 3.x is global-only on Vertex; default the region accordingly.
const DEFAULT_VERTEX_LOCATION: &str = "global";

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
        }
    }

    pub fn mode(&self) -> &'static str {
        match self {
            AiAuth::Studio { .. } => "studio",
            AiAuth::Vertex { .. } => "managed",
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
        }
    }

    fn with_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            AiAuth::Studio { api_key } => req.header("x-goog-api-key", api_key),
            AiAuth::Vertex { bearer, .. } => req.bearer_auth(bearer),
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
        mut body: Value,
    ) -> Result<Value> {
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

        let req = http
            .post(self.generate_url(model))
            .header("Content-Type", "application/json")
            .json(&body);
        let res = send_with_retry(self.with_auth(req)).await?;

        if !res.status().is_success() {
            let status = res.status();
            let err_text = res.text().await.unwrap_or_default();
            anyhow::bail!(
                "Gemini generateContent error ({status}, mode={}): {err_text}",
                self.mode()
            );
        }

        let resp: Value = res.json().await?;
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

    /// Embed `text` → `(vector, usage)`. Studio uses `:embedContent`; Vertex uses
    /// the publisher `text-embedding` model via `:predict` (different shape).
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
            AiAuth::Vertex { .. } => {
                // Managed/keyless nodes can't call Studio directly, and the Gemini
                // Embedding 2 model lives ONLY on Studio — not Vertex. So embeddings
                // are routed through the authority's `/ai/embed` proxy, which embeds
                // with its own key. This keeps the WHOLE fleet on ONE gemini-embedding-2
                // vector space. Usage is metered server-side by the proxy.
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
            }))
            .send()
            .await;
        // The authority answers every usage report with the CURRENT mesh
        // balance — absorb it so the snapshot refreshes on every AI call,
        // not just on the ~hourly mint (keeps all nodes' gauges in step).
        if let Ok(r) = resp {
            if let Ok(j) = r.json::<Value>().await {
                absorb_quota_snapshot(&j).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
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
}
