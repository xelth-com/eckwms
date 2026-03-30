use eck_core::db::SurrealDb;
use reqwest::Client as HttpClient;
use serde_json::Value;
use tracing::{debug, info, warn};

const EMBEDDING_MODEL: &str = "gemini-embedding-2-preview";
const EMBEDDING_DIM: usize = 768;
const BATCH_LIMIT: usize = 10;
const LOOP_INTERVAL_SECS: u64 = 10;
const RATE_LIMIT_MS: u64 = 200;

/// Spawns the background embedding worker that processes pending documents and orders.
pub async fn start_embedding_worker(db: SurrealDb, api_key: String) {
    // Initial delay to let the server finish startup
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    info!("[Embeddings] Worker started ({LOOP_INTERVAL_SECS}s interval)");

    let http = HttpClient::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(LOOP_INTERVAL_SECS));

    loop {
        interval.tick().await;

        if let Err(e) = process_table(&db, &http, &api_key, "document").await {
            warn!("[Embeddings] document cycle error: {e}");
        }
        if let Err(e) = process_table(&db, &http, &api_key, "order").await {
            warn!("[Embeddings] order cycle error: {e}");
        }
    }
}

async fn process_table(
    db: &SurrealDb,
    http: &HttpClient,
    api_key: &str,
    table: &str,
) -> Result<(), anyhow::Error> {
    let query = format!(
        "SELECT * FROM {table} WHERE embedding_status = 'pending' LIMIT {BATCH_LIMIT}"
    );

    let rows: Vec<Value> = db
        .query(&query)
        .await?
        .take(0)?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut count = 0u32;
    for row in &rows {
        let id = match row.get("id") {
            Some(v) => v.to_string().trim_matches('"').to_string(),
            None => continue,
        };

        let text = build_embedding_text(table, row);
        if text.is_empty() {
            // Nothing to embed — mark as skipped
            let update_q = format!(
                "UPDATE {id} MERGE {{ embedding_status: 'skipped' }}"
            );
            db.query(&update_q).await?;
            continue;
        }

        match call_gemini_embed(http, api_key, &text).await {
            Ok(embedding) => {
                let update_q = format!(
                    "UPDATE {id} MERGE {{ embedding: $emb, embedding_status: 'complete' }}"
                );
                db.query(&update_q)
                    .bind(("emb", embedding))
                    .await?;
                count += 1;
                debug!("[Embeddings] Embedded {id}");
            }
            Err(e) => {
                warn!("[Embeddings] Failed to embed {id}: {e}");
                let update_q = format!(
                    "UPDATE {id} MERGE {{ embedding_status: 'error', embedding_error: $err }}"
                );
                db.query(&update_q)
                    .bind(("err", e.to_string()))
                    .await?;
            }
        }

        // Rate-limit between API calls
        tokio::time::sleep(std::time::Duration::from_millis(RATE_LIMIT_MS)).await;
    }

    if count > 0 {
        info!("[Embeddings] {table}: {count} records embedded");
    }

    Ok(())
}

/// Build text for embedding by concatenating relevant fields based on table type.
fn build_embedding_text(table: &str, row: &Value) -> String {
    let mut parts = Vec::new();

    match table {
        "document" => {
            if let Some(s) = row.get("type").and_then(|v| v.as_str()) {
                parts.push(format!("Type: {s}"));
            }
            if let Some(s) = row.get("status").and_then(|v| v.as_str()) {
                parts.push(format!("Status: {s}"));
            }
            // Extract text from payload (support threads, workflow results, etc.)
            if let Some(payload) = row.get("payload") {
                if let Some(s) = payload.get("subject").and_then(|v| v.as_str()) {
                    parts.push(format!("Subject: {s}"));
                }
                if let Some(s) = payload.get("content").and_then(|v| v.as_str()) {
                    // Strip HTML tags
                    let plain = strip_html(s);
                    if !plain.is_empty() {
                        parts.push(plain);
                    }
                }
                // Ticket nested content
                if let Some(ticket) = payload.get("ticket") {
                    if let Some(s) = ticket.get("subject").and_then(|v| v.as_str()) {
                        parts.push(format!("Subject: {s}"));
                    }
                }
            }
        }
        "order" => {
            for (key, label) in [
                ("order_number", "Order"),
                ("customer_name", "Customer"),
                ("product_name", "Product"),
                ("serial_number", "Serial"),
                ("issue_description", "Issue"),
                ("diagnosis_notes", "Diagnosis"),
                ("repair_notes", "Repair"),
                ("resolution", "Resolution"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        _ => {}
    }

    let full = parts.join("\n");
    // Truncate to ~8000 chars to stay within embedding model limits
    if full.len() > 8000 {
        full[..8000].to_string()
    } else {
        full
    }
}

/// Call Gemini embedding API and return 768-dim vector.
async fn call_gemini_embed(
    http: &HttpClient,
    api_key: &str,
    text: &str,
) -> Result<Vec<f32>, anyhow::Error> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{EMBEDDING_MODEL}:embedContent?key={api_key}"
    );

    let payload = serde_json::json!({
        "model": format!("models/{EMBEDDING_MODEL}"),
        "content": { "parts": [{ "text": text }] },
        "taskType": "RETRIEVAL_DOCUMENT",
        "outputDimensionality": EMBEDDING_DIM
    });

    let res = http
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let err_text = res.text().await.unwrap_or_default();
        anyhow::bail!("Gemini embedding API error ({status}): {err_text}");
    }

    let body: Value = res.json().await?;

    let values = body["embedding"]["values"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("No embedding values in Gemini response"))?;

    let embedding: Vec<f32> = values
        .iter()
        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
        .collect();

    if embedding.len() != EMBEDDING_DIM {
        anyhow::bail!("Expected {EMBEDDING_DIM}-dim embedding, got {}", embedding.len());
    }

    Ok(embedding)
}

/// Embed a search query using Gemini. Returns 768-dim vector.
pub async fn embed_query(api_key: &str, text: &str) -> Result<Vec<f32>, anyhow::Error> {
    if api_key.is_empty() {
        anyhow::bail!("GEMINI_API_KEY not configured");
    }
    let http = HttpClient::new();
    call_gemini_embed(&http, api_key, text).await
}

/// Naive HTML tag stripper.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Collapse whitespace
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
