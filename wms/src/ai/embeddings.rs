use eck_core::db::SurrealDb;
use eck_core::utils::anonymizer::obfuscate_pii;
use reqwest::Client as HttpClient;
use serde_json::Value;
use tracing::{debug, info, warn};

const EMBEDDING_DIM: usize = 768;
const BATCH_LIMIT: usize = 10;
const LOOP_INTERVAL_SECS: u64 = 10;
const RATE_LIMIT_MS: u64 = 200;

/// Spawns the background embedding worker that processes pending documents and orders.
pub async fn start_embedding_worker(db: SurrealDb, api_key: String, gen_model: String, emb_model: String) {
    // Initial delay to let the server finish startup
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    info!("[Embeddings] Worker started ({LOOP_INTERVAL_SECS}s interval)");

    let http = HttpClient::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(LOOP_INTERVAL_SECS));

    loop {
        interval.tick().await;

        for table in &["document", "order", "partner", "product", "picking"] {
            if let Err(e) = process_table(&db, &http, &api_key, table, &gen_model, &emb_model).await {
                warn!("[Embeddings] {table} cycle error: {e}");
            }
        }
    }
}

async fn process_table(
    db: &SurrealDb,
    http: &HttpClient,
    api_key: &str,
    table: &str,
    gen_model: &str,
    emb_model: &str,
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
            let update_q = format!(
                "UPDATE {id} MERGE {{ embedding_status: 'skipped' }}"
            );
            db.query(&update_q).await?;
            continue;
        }

        // Anonymize PII before embedding
        let (anonymized_text, fingerprints) = match table {
            "document" => {
                match extract_and_anonymize(http, api_key, &text, gen_model).await {
                    Ok(result) => result,
                    Err(e) => {
                        debug!("[Embeddings] PII extraction failed for {id}, using raw text: {e}");
                        (text.clone(), vec![])
                    }
                }
            }
            "order" => anonymize_order_fields(row, &text),
            "partner" => anonymize_partner_fields(row, &text),
            "picking" => anonymize_picking_fields(row, &text),
            _ => (text.clone(), vec![]),
        };

        match call_gemini_embed(http, api_key, &anonymized_text, emb_model).await {
            Ok(embedding) => {
                let update_q = format!(
                    "UPDATE {id} MERGE {{ embedding: $emb, embedding_status: 'complete', pii_fingerprints: $fingerprints }}"
                );
                db.query(&update_q)
                    .bind(("emb", embedding))
                    .bind(("fingerprints", fingerprints))
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

// ── PII Anonymization ─────────────────────────────────────────────

/// Use Gemini to extract PII from unstructured document text, then replace with SimHash tokens.
async fn extract_and_anonymize(
    http: &HttpClient,
    api_key: &str,
    text: &str,
    model: &str,
) -> Result<(String, Vec<String>), anyhow::Error> {
    let prompt = format!(
        "Analyze the following support ticket text. Extract personal names and street addresses. \
         DO NOT extract cities, countries, or zip codes. Summarize the technical issue. \
         Return ONLY a valid JSON object with this structure: \
         {{ \"summary\": \"<cleaned summary>\", \"entities\": [ {{ \"original\": \"<extracted text>\", \"type\": \"Name\" | \"Address\" }} ] }}\n\n\
         Text:\n{text}"
    );

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
    );

    let payload = serde_json::json!({
        "contents": [{ "parts": [{ "text": prompt }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "temperature": 0.1,
            "maxOutputTokens": 2048,
        }
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
        anyhow::bail!("Gemini generation API error ({status}): {err_text}");
    }

    let body: Value = res.json().await?;

    // Extract the text from the Gemini response
    let response_text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No text in Gemini generation response"))?;

    let parsed: Value = serde_json::from_str(response_text)
        .map_err(|e| anyhow::anyhow!("Failed to parse PII extraction JSON: {e}"))?;

    let _summary = parsed["summary"]
        .as_str()
        .unwrap_or(text)
        .to_string();

    let entities = parsed["entities"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut anonymized = text.to_string();
    let mut fingerprints = Vec::new();

    for entity in &entities {
        let original = match entity["original"].as_str() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let pii_type = entity["type"].as_str().unwrap_or("PII");
        let token = obfuscate_pii(original, pii_type);
        anonymized = anonymized.replace(original, &token);
        fingerprints.push(token);
    }

    Ok((anonymized, fingerprints))
}

/// Anonymize structured order fields (customer_name) directly — no LLM needed.
fn anonymize_order_fields(row: &Value, text: &str) -> (String, Vec<String>) {
    let mut result = text.to_string();
    let mut fingerprints = Vec::new();

    if let Some(name) = row.get("customer_name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            let token = obfuscate_pii(name, "Name");
            result = result.replace(name, &token);
            fingerprints.push(token);
        }
    }

    (result, fingerprints)
}

/// Anonymize structured partner fields (name, street) directly.
fn anonymize_partner_fields(row: &Value, text: &str) -> (String, Vec<String>) {
    let mut result = text.to_string();
    let mut fingerprints = Vec::new();

    if let Some(name) = row.get("name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            let token = obfuscate_pii(name, "Name");
            result = result.replace(name, &token);
            fingerprints.push(token);
        }
    }
    if let Some(street) = row.get("street").and_then(|v| v.as_str()) {
        if !street.is_empty() {
            let token = obfuscate_pii(street, "Address");
            result = result.replace(street, &token);
            fingerprints.push(token);
        }
    }

    (result, fingerprints)
}

/// Anonymize structured picking fields (recipient_name) directly.
fn anonymize_picking_fields(row: &Value, text: &str) -> (String, Vec<String>) {
    let mut result = text.to_string();
    let mut fingerprints = Vec::new();

    if let Some(name) = row.get("recipient_name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            let token = obfuscate_pii(name, "Name");
            result = result.replace(name, &token);
            fingerprints.push(token);
        }
    }

    (result, fingerprints)
}

// ── Embedding text construction ───────────────────────────────────

/// Build text for embedding by concatenating relevant fields based on table type.
fn build_embedding_text(table: &str, row: &Value) -> String {
    let mut parts = Vec::new();

    match table {
        "document" => {
            // Prefer ai_summary (clean, structured text from summarization worker)
            if let Some(s) = row.get("ai_summary").and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    parts.push(s.to_string());
                    // ai_summary is self-contained — skip raw payload
                    let full = parts.join("\n");
                    return if full.len() > 8000 {
                        full[..8000].to_string()
                    } else {
                        full
                    };
                }
            }

            // Fallback: raw payload extraction (for non-ticket documents)
            if let Some(s) = row.get("type").and_then(|v| v.as_str()) {
                parts.push(format!("Type: {s}"));
            }
            if let Some(s) = row.get("status").and_then(|v| v.as_str()) {
                parts.push(format!("Status: {s}"));
            }
            if let Some(payload) = row.get("payload") {
                if let Some(s) = payload.get("subject").and_then(|v| v.as_str()) {
                    parts.push(format!("Subject: {s}"));
                }
                if let Some(s) = payload.get("content").and_then(|v| v.as_str()) {
                    let plain = strip_html(s);
                    if !plain.is_empty() {
                        parts.push(plain);
                    }
                }
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
        "partner" => {
            for (key, label) in [
                ("name", "Firma/Name"),
                ("email", "Email"),
                ("phone", "Telefon"),
                ("street", "Adresse"),
                ("zip", "PLZ"),
                ("city", "Stadt"),
                ("vat", "USt-IdNr"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        "product" => {
            for (key, label) in [
                ("name", "Produkt"),
                ("default_code", "SKU"),
                ("barcode", "Barcode"),
            ] {
                if let Some(s) = row.get(key).and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        parts.push(format!("{label}: {s}"));
                    }
                }
            }
        }
        "picking" => {
            for (key, label) in [
                ("tracking_number", "Tracking"),
                ("recipient_name", "Empfänger"),
                ("recipient_city", "Stadt"),
                ("status", "Status"),
                ("picking_type", "Typ"),
                ("origin", "Herkunft"),
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

// ── Embedding API ─────────────────────────────────────────────────

/// Call Gemini embedding API and return 768-dim vector.
async fn call_gemini_embed(
    http: &HttpClient,
    api_key: &str,
    text: &str,
    model: &str,
) -> Result<Vec<f32>, anyhow::Error> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:embedContent?key={api_key}"
    );

    let payload = serde_json::json!({
        "model": format!("models/{model}"),
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

/// Embed a search query using Gemini. Anonymizes PII before embedding to match the database vectors. Returns 768-dim vector.
pub async fn embed_query(api_key: &str, text: &str) -> Result<Vec<f32>, anyhow::Error> {
    if api_key.is_empty() {
        anyhow::bail!("GEMINI_API_KEY not configured");
    }

    let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
        .unwrap_or_else(|_| "gemini-3.1-flash-lite-preview".to_string());
    let emb_model = std::env::var("GEMINI_EMBEDDING_MODEL")
        .unwrap_or_else(|_| "gemini-embedding-2-preview".to_string());

    let http = HttpClient::new();

    // 1. Anonymize the search query (extract PII to Keyed SimHash tokens)
    let (anonymized_query, _) = match extract_and_anonymize(&http, api_key, text, &gen_model).await {
        Ok(res) => {
            tracing::debug!("[Embeddings] Query anonymized: {}", res.0);
            res
        },
        Err(e) => {
            tracing::warn!("[Embeddings] Failed to anonymize search query, falling back to raw text: {}", e);
            (text.to_string(), vec![])
        }
    };

    // 2. Embed the anonymized query
    call_gemini_embed(&http, api_key, &anonymized_query, &emb_model).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_live_pii_extraction() {
        dotenvy::dotenv().ok();

        let api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                println!("⚠ GEMINI_API_KEY not set — skipping live PII test");
                return;
            }
        };

        std::env::set_var("SYNC_SECRET", "test_secret_123");

        let sample_text = "Ticket from Hans Müller. Address: Alexanderplatz 1, 10115 Berlin. \
            Issue: My InBody 770 device is sparking and won't turn on. \
            Please call me at +49 123 456789.";

        println!("\n=== PII Anonymization E2E Test ===");
        println!("\nOriginal text:\n  {sample_text}");

        let client = HttpClient::new();
        let (anonymized, fingerprints) = extract_and_anonymize(&client, &api_key, sample_text, "gemini-3.1-flash-lite-preview")
            .await
            .expect("extract_and_anonymize failed");

        println!("\nExtracted fingerprints:");
        for fp in &fingerprints {
            println!("  {fp}");
        }

        println!("\nAnonymized summary:\n  {anonymized}");

        // Verify that original PII is not present in the anonymized text
        assert!(
            !anonymized.contains("Hans Müller"),
            "Anonymized text must not contain the original name"
        );
        assert!(
            !anonymized.contains("Alexanderplatz"),
            "Anonymized text must not contain the original address"
        );

        // Verify fingerprints were generated
        assert!(!fingerprints.is_empty(), "Should have extracted at least one PII entity");

        // Verify tokens are in the expected format
        for fp in &fingerprints {
            assert!(
                fp.starts_with("Name_") || fp.starts_with("Address_"),
                "Token should start with Name_ or Address_, got: {fp}"
            );
        }

        println!("\n✓ All assertions passed");
    }

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot_product / (norm_a * norm_b) }
    }

    #[tokio::test]
    #[ignore]
    async fn test_vector_search_with_pii() {
        dotenvy::dotenv().ok();

        let api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                println!("⚠ GEMINI_API_KEY not set — skipping vector search PII test");
                return;
            }
        };

        std::env::set_var("SYNC_SECRET", "test_secret_123");

        let gen_model = "gemini-2.5-flash";
        let emb_model = "gemini-embedding-2-preview";

        let doc_a = "Customer Hans Müller from Alexanderplatz 1, Berlin reports that the display on his InBody 770 is completely broken and black.";
        let doc_b = "Customer Julia Weber from Munich reports a broken display on her InBody 770.";
        let query = "Broken display for Hans Müller";

        let client = HttpClient::new();

        // Anonymize all three strings
        let (anon_doc_a, _) = extract_and_anonymize(&client, &api_key, doc_a, gen_model)
            .await
            .expect("Failed to anonymize doc_a");
        let (anon_doc_b, _) = extract_and_anonymize(&client, &api_key, doc_b, gen_model)
            .await
            .expect("Failed to anonymize doc_b");
        let (anon_query, _) = extract_and_anonymize(&client, &api_key, query, gen_model)
            .await
            .expect("Failed to anonymize query");

        println!("\n=== Vector Search with PII Anonymization ===");
        println!("\nOriginal Doc A: {doc_a}");
        println!("Anonymized Doc A: {anon_doc_a}");
        println!("\nOriginal Doc B: {doc_b}");
        println!("Anonymized Doc B: {anon_doc_b}");
        println!("\nOriginal Query: {query}");
        println!("Anonymized Query: {anon_query}");

        // Embed all three
        let doc_a_emb = call_gemini_embed(&client, &api_key, &anon_doc_a, emb_model)
            .await
            .expect("Failed to embed doc_a");
        let doc_b_emb = call_gemini_embed(&client, &api_key, &anon_doc_b, emb_model)
            .await
            .expect("Failed to embed doc_b");
        let query_emb = call_gemini_embed(&client, &api_key, &anon_query, emb_model)
            .await
            .expect("Failed to embed query");

        let sim_a = cosine_similarity(&query_emb, &doc_a_emb);
        let sim_b = cosine_similarity(&query_emb, &doc_b_emb);

        println!("\nCosine similarity (query ↔ doc_a): {sim_a:.6}");
        println!("Cosine similarity (query ↔ doc_b): {sim_b:.6}");

        assert!(sim_a > sim_b, "The query should match Doc A better than Doc B (sim_a={sim_a}, sim_b={sim_b})");

        println!("\n✓ Vector search assertion passed: sim_a ({sim_a:.6}) > sim_b ({sim_b:.6})");
    }
}
