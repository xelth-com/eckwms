//! xelixir Knowledge Base MCP client.
//!
//! A thin JSON-RPC-over-HTTP client for the xelixir KB MCP endpoint
//! (`XELIXIR_KB_URL` + bearer `XELIXIR_KB_TOKEN`). The endpoint speaks MCP
//! `tools/call` and replies as SSE (`data: {…}` lines); the useful payload is
//! the first content block's `text`, itself a JSON string.
//!
//! Two callers share this: the repair-lesson distiller (`kb_record`, WRITE) and
//! the triage/one-shot brain (`kb_brief`, READ — the recall front door). Both
//! degrade gracefully when the bridge is unconfigured.

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

/// True when BOTH `XELIXIR_KB_URL` and `XELIXIR_KB_TOKEN` are set and non-empty.
pub fn configured() -> bool {
    std::env::var("XELIXIR_KB_URL").map(|u| !u.trim().is_empty()).unwrap_or(false)
        && std::env::var("XELIXIR_KB_TOKEN").map(|t| !t.trim().is_empty()).unwrap_or(false)
}

/// Generic JSON-RPC `tools/call` against the xelixir KB MCP endpoint. Returns
/// the `/result/content/0/text` string (itself usually a JSON document). A
/// JSON-RPC top-level `error`, or an MCP `result.isError` flag, becomes `Err`.
pub async fn call_tool(
    http: &reqwest::Client,
    name: &str,
    args: Value,
    timeout: Duration,
) -> anyhow::Result<String> {
    let url = std::env::var("XELIXIR_KB_URL").unwrap_or_default();
    let token = std::env::var("XELIXIR_KB_TOKEN").unwrap_or_default();
    if url.trim().is_empty() || token.trim().is_empty() {
        anyhow::bail!("xelixir KB not configured (XELIXIR_KB_URL/XELIXIR_KB_TOKEN)");
    }

    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });

    let resp = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .timeout(timeout)
        .send()
        .await?;
    let raw = resp.text().await?;

    // The MCP endpoint replies as SSE: lines prefixed with "data: ".
    for line in raw.lines() {
        let payload = line.strip_prefix("data: ").unwrap_or(line);
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            if let Some(err) = v.get("error") {
                anyhow::bail!("{name} JSON-RPC error: {err}");
            }
            if let Some(result) = v.get("result") {
                if result.get("isError").and_then(|b| b.as_bool()).unwrap_or(false) {
                    let msg = result
                        .pointer("/content/0/text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("tool error");
                    anyhow::bail!("{name} isError: {msg}");
                }
                if let Some(text) = result.pointer("/content/0/text").and_then(|t| t.as_str()) {
                    return Ok(text.to_string());
                }
            }
        }
    }
    anyhow::bail!("{name}: no usable response ({})", raw.chars().take(200).collect::<String>())
}

/// The recall front door — call at the START of a repair/diagnosis. Graph-gated
/// on `model` + `symptom`/`error_code`, trust-ranked. Returns the raw digest
/// text (a JSON document carrying a markdown `brief` + structured rows), or:
///   * `Ok(None)` when the bridge is not configured, OR
///   * `Ok(None)` when `model` AND `symptom` are both empty (nothing to anchor
///     the graph query on) — no call is made.
/// A 10s timeout guards the triage/chat latency budget.
pub async fn kb_brief(
    http: &reqwest::Client,
    model: &str,
    symptom: &str,
    error_code: &str,
    budget: u32,
) -> anyhow::Result<Option<String>> {
    if !configured() {
        return Ok(None);
    }
    if model.trim().is_empty() && symptom.trim().is_empty() {
        return Ok(None);
    }
    let budget = budget.clamp(1, 30);
    let mut args = json!({
        "model": model,
        "symptom": symptom,
        "budget": budget,
    });
    if !error_code.trim().is_empty() {
        args["error_code"] = json!(error_code);
    }
    let text = call_tool(http, "kb_brief", args, Duration::from_secs(10)).await?;
    Ok(Some(text))
}

/// Extract the human-readable `brief` markdown from a raw `kb_brief` response.
/// The response text is a JSON object with a `brief` string (empty/absent when
/// the KB has nothing); if the text is not JSON we treat the whole thing as the
/// brief. Returns `None` when the brief is empty — i.e. "no findings".
pub fn extract_brief(raw: &str) -> Option<String> {
    let brief = match serde_json::from_str::<Value>(raw) {
        Ok(v) => v
            .get("brief")
            .and_then(|b| b.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        Err(_) => raw.to_string(),
    };
    let t = brief.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Build a markdown block carrying the FULL TEXT of the top findings (the
/// `brief` digest truncates each line to a title-length snippet, which is
/// useless for concrete how-to steps). Reads `/findings` from the raw
/// `kb_brief` JSON response: each entry contributes one paragraph
/// `- **<kind>** [<id>] (<model>): <text truncated to max_chars_each>`.
/// Missing `trust`/`relevance`/`model` fields are handled gracefully — a
/// missing `model` renders as "any model". Returns `""` when there are no
/// findings or the response isn't the expected JSON shape.
pub fn extract_details(raw: &str, max_findings: usize, max_chars_each: usize) -> String {
    let findings = match serde_json::from_str::<Value>(raw) {
        Ok(v) => match v.get("findings").and_then(|f| f.as_array()).cloned() {
            Some(f) => f,
            None => return String::new(),
        },
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    for finding in findings.iter().take(max_findings) {
        let id = finding.get("id").and_then(|v| v.as_str()).unwrap_or("kb_finding:unknown");
        let kind = finding.get("kind").and_then(|v| v.as_str()).unwrap_or("finding");
        let model = finding.get("model").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty());
        let text = finding.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            continue;
        }
        let truncated = truncate_at_char_boundary(text, max_chars_each);
        out.push_str(&format!(
            "- **{kind}** [{id}] ({}): {truncated}\n",
            model.unwrap_or("any model")
        ));
    }
    out
}

/// Truncate `s` to at most `max_chars` chars (not bytes), cutting at a char
/// boundary, appending "…" when the text was actually cut.
fn truncate_at_char_boundary(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

/// Pull the `kb_finding:<id>` handles out of a digest, de-duplicated, in order
/// of first appearance. Used to record `kb_brief_ids` on the triage task.
pub fn parse_finding_ids(digest: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"kb_finding:[a-z0-9]+").expect("valid finding-id regex"));
    let mut ids: Vec<String> = Vec::new();
    for m in re.find_iter(digest) {
        let id = m.as_str().to_string();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_finding_ids_dedups_in_order() {
        let digest = "## Known for ZetaBody 770\n\
            - [repair_outcome|t1.15] TRANSPORT-INDUCED DOUBLE-BREAK … (kb_finding:6snivwtraw7do7vsyovk)\n\
            - [repair_outcome|t0.86] FIELD REPAIR (confirmed): … (kb_finding:xuqe88jkspv89hvlkqda)\n\
            - see also kb_finding:6snivwtraw7do7vsyovk";
        let ids = parse_finding_ids(digest);
        assert_eq!(
            ids,
            vec![
                "kb_finding:6snivwtraw7do7vsyovk".to_string(),
                "kb_finding:xuqe88jkspv89hvlkqda".to_string(),
            ]
        );
    }

    #[test]
    fn parse_finding_ids_empty_when_none() {
        assert!(parse_finding_ids("no handles here").is_empty());
    }

    #[test]
    fn extract_brief_reads_field_and_treats_empty_as_none() {
        // Build via serde so the markdown `##` in the brief can't trip raw-string
        // delimiters, then round-trip through extract_brief.
        let brief_md = "## Known\n- [repair_outcome|t0.9] X (kb_finding:abc123)";
        let raw = serde_json::json!({ "brief": brief_md, "rows": [] }).to_string();
        assert_eq!(extract_brief(&raw).as_deref(), Some(brief_md));
        // Empty/absent brief → no findings.
        assert!(extract_brief(r#"{"brief":"","rows":[]}"#).is_none());
        assert!(extract_brief(r#"{"rows":[]}"#).is_none());
    }

    #[test]
    fn extract_details_parses_sample_shape() {
        let raw = serde_json::json!({
            "brief": "## Known for ZetaBody 770\n- [repair_heuristic|t0.86] …",
            "count": 2,
            "findings": [
                {
                    "id": "kb_finding:xxx",
                    "kind": "repair_heuristic",
                    "model": "ZetaBody 770",
                    "text": "Full step-by-step repair text goes here.",
                    "trust": 0.81
                },
                {
                    "id": "kb_finding:yyy",
                    "kind": "repair_outcome",
                    "text": "No model field on this one; still renders."
                }
            ],
            "model": "ZetaBody 770",
            "symptom": "beeps on boot"
        })
        .to_string();
        let details = extract_details(&raw, 6, 900);
        assert!(details.contains("**repair_heuristic** [kb_finding:xxx] (ZetaBody 770): Full step-by-step repair text goes here."));
        assert!(details.contains("**repair_outcome** [kb_finding:yyy] (any model): No model field on this one; still renders."));
    }

    #[test]
    fn extract_details_truncates_at_char_boundary() {
        // "ä"/"ß" are multi-byte in UTF-8; a byte-based truncation would panic
        // or split them. Build text long enough to force a cut mid-multibyte
        // run and verify it lands cleanly with an ellipsis.
        let text = "Prüfe die Baugruppe auf Rißbildung, tausche die Sensorplatine äußerst vorsichtig aus, dann kalibriere neu.";
        let raw = serde_json::json!({
            "brief": "## x",
            "findings": [{ "id": "kb_finding:abc", "kind": "repair_heuristic", "model": "X", "text": text }]
        })
        .to_string();
        let details = extract_details(&raw, 6, 10);
        // Must not panic (char-boundary safe) and must end with the ellipsis marker.
        assert!(details.contains('…'));
        let truncated_part: String = text.chars().take(10).collect();
        assert!(details.contains(&truncated_part));
    }

    #[test]
    fn extract_details_empty_when_no_findings() {
        assert_eq!(extract_details(r#"{"brief":"x","findings":[]}"#, 6, 900), "");
        assert_eq!(extract_details(r#"{"brief":"x"}"#, 6, 900), "");
        assert_eq!(extract_details("not json", 6, 900), "");
    }

    #[tokio::test]
    async fn kb_brief_returns_none_when_unconfigured() {
        // Ensure the bridge looks unconfigured regardless of ambient env.
        std::env::remove_var("XELIXIR_KB_URL");
        std::env::remove_var("XELIXIR_KB_TOKEN");
        assert!(!configured());
        let http = reqwest::Client::new();
        let out = kb_brief(&http, "ZetaBody 770", "beeps on boot", "", 12).await.unwrap();
        assert!(out.is_none());
    }
}
