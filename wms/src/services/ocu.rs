//! OCU external-API connector (Rust-native OPAL/OCU shipment feed).
//!
//! This is the Rust-native seam to the standalone OCU API product (repo
//! `xelth-com/ocu`), which fronts the same OPAL shipment data the node
//! scraper (`services/scraper`) currently pulls by browser automation. When
//! `OCU_API_URL` is configured, the scheduler talks to this API directly
//! instead of shelling out to the node scraper's `/api/opal/fetch`; the node
//! scraper remains the fallback when `OCU_API_URL` is unset.
//!
//! Config is read from the environment:
//! ```text
//!   OCU_API_URL   base URL, e.g. http://127.0.0.1:38300
//!   OCU_API_KEY   bearer token sent as `Authorization: Bearer <key>`
//! ```
//!
//! Contract (`GET {base}/v1/shipments?limit=N`):
//! ```text
//!   200 { "shipments": [ <opaque JSON object> ], "next_cursor": string|null }
//! ```
//! Each shipment object carries `ocu_number` and/or `tracking_number` plus
//! other fields — same shape as the scraper's order objects today, so it is
//! treated as an opaque `serde_json::Value` and handed straight to
//! `services::delivery::persist`. Non-2xx responses carry
//! `{ "error": { "code", "message" } }`.

use serde_json::Value;

fn get_env(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn base_url() -> Option<String> {
    get_env("OCU_API_URL").map(|s| s.trim_end_matches('/').to_string())
}

/// `true` if `OCU_API_URL` is set (non-empty) — the scheduler's gate between
/// the OCU API path and the node-scraper fallback.
pub fn configured() -> bool {
    base_url().is_some()
}

/// Parse a `/v1/shipments` response body into its shipment array and
/// optional next-page cursor. Split out from [`fetch_shipments`] so the
/// parsing logic is unit-testable without a live HTTP call.
fn parse_shipments_page(body: &Value) -> (Vec<Value>, Option<String>) {
    let shipments = body
        .get("shipments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let next_cursor = body
        .get("next_cursor")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (shipments, next_cursor)
}

/// Build a descriptive error string from a non-2xx response: HTTP status plus
/// the `error.code`/`error.message` when the body parses as the documented
/// error shape, else the raw body text.
fn describe_error(status: reqwest::StatusCode, body_text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body_text) {
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(|c| c.as_str()).unwrap_or("unknown");
            let message = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
            return format!("ocu api error (HTTP {}): {} — {}", status, code, message);
        }
    }
    format!("ocu api error (HTTP {}): {}", status, body_text)
}

async fn get_page(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<(Vec<Value>, Option<String>), String> {
    let mut req = client
        .get(format!("{}/v1/shipments", base))
        .bearer_auth(key)
        .query(&[("limit", limit.to_string())]);
    if let Some(c) = cursor {
        req = req.query(&[("cursor", c)]);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("ocu api request: {}", e))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("ocu api read body (HTTP {}): {}", status, e))?;
    if !status.is_success() {
        return Err(describe_error(status, &text));
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| format!("ocu api parse (HTTP {}): {}", status, e))?;
    Ok(parse_shipments_page(&body))
}

/// Fetch up to `limit` shipments from the OCU API (`GET /v1/shipments`),
/// bearer-authenticated from `OCU_API_KEY`. If the first page returns fewer
/// than `limit` items and carries a `next_cursor`, follows it once (single
/// follow-up max, kept simple — not a full pagination loop) to try to fill
/// out the batch.
pub async fn fetch_shipments(
    client: &reqwest::Client,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let base = base_url().ok_or_else(|| "OCU_API_URL not configured".to_string())?;
    let key = get_env("OCU_API_KEY").unwrap_or_default();

    let (mut shipments, next_cursor) = get_page(client, &base, &key, limit, None).await?;

    if shipments.len() < limit {
        if let Some(cursor) = next_cursor {
            let (more, _) = get_page(client, &base, &key, limit, Some(&cursor)).await?;
            shipments.extend(more);
        }
    }

    Ok(shipments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_shipments_page() {
        let body = json!({
            "shipments": [
                {"ocu_number": "OCU123", "status": "in_transit"},
                {"tracking_number": "TRK456", "status": "delivered"},
            ],
            "next_cursor": "abc",
        });
        let (shipments, cursor) = parse_shipments_page(&body);
        assert_eq!(shipments.len(), 2);
        assert_eq!(shipments[0].get("ocu_number").and_then(|v| v.as_str()), Some("OCU123"));
        assert_eq!(cursor.as_deref(), Some("abc"));
    }

    #[test]
    fn parses_shipments_page_null_cursor() {
        let body = json!({ "shipments": [], "next_cursor": null });
        let (shipments, cursor) = parse_shipments_page(&body);
        assert!(shipments.is_empty());
        assert_eq!(cursor, None);
    }

    #[test]
    fn describes_parseable_error() {
        let msg = describe_error(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"invalid_key","message":"bad token"}}"#,
        );
        assert!(msg.contains("invalid_key"));
        assert!(msg.contains("bad token"));
        assert!(msg.contains("401"));
    }

    #[test]
    fn describes_unparseable_error() {
        let msg = describe_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert!(msg.contains("500"));
        assert!(msg.contains("boom"));
    }
}
