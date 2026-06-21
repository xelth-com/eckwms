//! Dual-mode Gemini auth — the one place the Studio-vs-Vertex split lives.
//!
//! `studio` (DEFAULT, open-source) = the user's own AI Studio key via
//! `generativelanguage…?key=`. `managed` (paid) = a short-lived Vertex Bearer
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

fn token_cache() -> &'static Mutex<Option<CachedToken>> {
    static CACHE: OnceLock<Mutex<Option<CachedToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
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

        // Mint a fresh token and cache it.
        let minted = Self::mint_managed(http).await?;
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

        let res = http
            .post(&url)
            .json(&json!({ "license": license }))
            .send()
            .await?;
        if !res.status().is_success() {
            let status = res.status();
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
    /// a Gemini/Vertex `usageMetadata` (or estimate) Value.
    fn report_usage(model: &str, kind: &str, usage: &Value) {
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
            let _ = http
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
        });
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
            AiAuth::Studio { api_key } => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
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
            AiAuth::Studio { api_key } => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{model}:embedContent?key={api_key}"
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
            AiAuth::Studio { .. } => req, // key is in the URL
            AiAuth::Vertex { bearer, .. } => req.bearer_auth(bearer),
        }
    }

    /// `generateContent`. `body` is the full request JSON
    /// (`systemInstruction`/`contents`/`generationConfig`/`tools`). For Vertex 3.x
    /// "thinking" models `generationConfig.thinkingConfig.thinkingBudget=0` is
    /// injected automatically (or the JSON answer gets starved — verified).
    /// Returns `(text, usageMetadata)`; usage is estimated if the API omits it.
    pub async fn generate_content(
        &self,
        http: &HttpClient,
        model: &str,
        mut body: Value,
    ) -> Result<(String, Value)> {
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
                        tc_obj.insert("thinkingBudget".to_string(), json!(0));
                    }
                }
            }
        }

        let req = http
            .post(self.generate_url(model))
            .header("Content-Type", "application/json")
            .json(&body);
        let res = self.with_auth(req).send().await?;

        if !res.status().is_success() {
            let status = res.status();
            let err_text = res.text().await.unwrap_or_default();
            anyhow::bail!(
                "Gemini generateContent error ({status}, mode={}): {err_text}",
                self.mode()
            );
        }

        let resp: Value = res.json().await?;
        let text = resp["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No text in Gemini response"))?
            .to_string();

        let usage = match resp.get("usageMetadata") {
            Some(u) if !u.is_null() => u.clone(),
            _ => {
                let pt = serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0) / 4;
                let ct = text.len() / 4;
                json!({
                    "promptTokenCount": pt,
                    "candidatesTokenCount": ct,
                    "totalTokenCount": pt + ct,
                    "estimated": true,
                })
            }
        };
        // Managed mode: meter this call against the client's balance.
        if let AiAuth::Vertex { .. } = self {
            Self::report_usage(model, "generate", &usage);
        }
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
                let res = http
                    .post(self.embed_url(model))
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
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
                let body = json!({
                    "instances": [{ "content": text, "task_type": "RETRIEVAL_DOCUMENT" }],
                    "parameters": { "outputDimensionality": dim },
                });
                let req = http
                    .post(self.embed_url(model))
                    .header("Content-Type", "application/json")
                    .json(&body);
                let res = self.with_auth(req).send().await?;
                if !res.status().is_success() {
                    let status = res.status();
                    let err = res.text().await.unwrap_or_default();
                    anyhow::bail!("Vertex :predict embed error ({status}): {err}");
                }
                let resp: Value = res.json().await?;
                let values = resp["predictions"][0]["embeddings"]["values"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("No embedding values in Vertex :predict"))?;
                let v: Vec<f32> = values
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                let usage = resp.get("metadata").cloned().unwrap_or(Value::Null);
                // Vertex :predict rarely returns token counts for embeddings;
                // meter a length-based estimate so balance still debits.
                let est = (text.len() / 4).max(1) as u64;
                Self::report_usage(
                    model,
                    "embed",
                    &json!({ "promptTokenCount": est, "totalTokenCount": est }),
                );
                Ok((v, usage))
            }
        }
    }
}
