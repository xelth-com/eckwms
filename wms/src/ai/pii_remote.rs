//! Remote "eck masker" client (`ECK_PII_NER=remote`).
//!
//! POSTs raw text to a stateless EU span service and returns `(PER, ORG)`
//! entity strings — exactly the shape [`crate::ai::local_ner::extract_entities`]
//! produces, so the subject/distill maskers consume it identically. The token
//! substitution (`Name_<hex>` …) still happens CLIENT-side from these strings;
//! the service never sees the mapping. See `.eck/PII_REMOTE_MASKING.md` §2.
//!
//! Config (env):
//! * `ECK_PII_SPANS_URL`          — base URL of the masker node (required).
//! * `ECK_PII_SPANS_TOKEN`        — bearer token, sent as `Authorization` when set.
//! * `ECK_PII_SPANS_TIMEOUT_SECS` — request timeout, default 20.
//!
//! FAIL-CLOSED: any transport / HTTP / parse error is an `Err` (callers degrade
//! to lexicon/regex masking), NEVER a silent empty `Ok` — an empty `Ok` would be
//! read as "no PII found" and leak. No-content logging: lengths / latency /
//! status / counts only, never the text or the entities.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 20;

/// Response schema of `POST {base}/api/pii/spans`. We only consume `entities`
/// here (the client re-derives byte spans locally when it applies tokens);
/// unknown fields (`model`, `spans`) are ignored.
#[derive(Deserialize)]
struct SpansResponse {
    #[serde(default)]
    entities: Entities,
}

#[derive(Deserialize, Default)]
struct Entities {
    #[serde(default)]
    per: Vec<String>,
    #[serde(default)]
    org: Vec<String>,
}

/// Shared client (connection pool + fixed timeout read once from env). Built
/// lazily; a builder failure degrades to a default client rather than panicking.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let secs = std::env::var("ECK_PII_SPANS_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        reqwest::Client::builder()
            .timeout(Duration::from_secs(secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Call the remote masker and return its `(PER, ORG)` entity strings.
pub async fn extract_entities(text: &str) -> Result<(Vec<String>, Vec<String>)> {
    if text.trim().is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let base = std::env::var("ECK_PII_SPANS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .context("ECK_PII_NER=remote but ECK_PII_SPANS_URL is not set")?;
    let url = format!("{}/api/pii/spans", base.trim().trim_end_matches('/'));

    let mut req = client()
        .post(&url)
        .json(&serde_json::json!({ "text": text }));
    if let Ok(tok) = std::env::var("ECK_PII_SPANS_TOKEN") {
        let tok = tok.trim().to_string();
        if !tok.is_empty() {
            req = req.bearer_auth(tok);
        }
    }

    let req_len = text.len();
    let started = std::time::Instant::now();
    let resp = req
        .send()
        .await
        .context("remote PII spans request failed (transport)")?;
    let status = resp.status();
    let latency_ms = started.elapsed().as_millis() as u64;

    if !status.is_success() {
        // No-content logging.
        tracing::warn!(
            status = status.as_u16(),
            latency_ms,
            req_len,
            "[NER] remote PII spans HTTP error"
        );
        anyhow::bail!("remote PII spans endpoint returned HTTP {status}");
    }

    let body = resp
        .text()
        .await
        .context("read remote PII spans response body")?;
    let parsed: SpansResponse =
        serde_json::from_str(&body).context("parse remote PII spans response JSON")?;

    tracing::debug!(
        status = status.as_u16(),
        latency_ms,
        req_len,
        per = parsed.entities.per.len(),
        org = parsed.entities.org.len(),
        "[NER] remote PII spans ok"
    );
    Ok((parsed.entities.per, parsed.entities.org))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_extracts_per_and_org() {
        let json = r#"{
            "model": "ner-bert-german-r7",
            "entities": { "per": ["Julia Weber", "Hans Müller"], "org": ["ZetaBody GmbH"] },
            "spans": [ { "start": 0, "end": 11, "kind": "PER" } ]
        }"#;
        let parsed: SpansResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.entities.per, vec!["Julia Weber", "Hans Müller"]);
        assert_eq!(parsed.entities.org, vec!["ZetaBody GmbH"]);
    }

    #[test]
    fn parse_response_defaults_missing_fields() {
        // Missing `entities` / missing `org` must default to empty, not error —
        // but note: an EMPTY parse is only ever returned on a 2xx body; a
        // transport/HTTP error is a hard `Err` upstream (fail-closed).
        let parsed: SpansResponse = serde_json::from_str(r#"{"model":"x"}"#).unwrap();
        assert!(parsed.entities.per.is_empty());
        assert!(parsed.entities.org.is_empty());
        let parsed: SpansResponse =
            serde_json::from_str(r#"{"entities":{"per":["A B"]}}"#).unwrap();
        assert_eq!(parsed.entities.per, vec!["A B"]);
        assert!(parsed.entities.org.is_empty());
    }

    #[test]
    fn parse_response_rejects_malformed_json() {
        // Garbage body → parse error at the call site → fail-closed Err.
        assert!(serde_json::from_str::<SpansResponse>("not json").is_err());
    }
}
