use eck_core::db::SurrealDb;
use reqwest::Client as HttpClient;
use serde_json::Value;
use tracing::{debug, info, warn};

const BATCH_LIMIT: usize = 5;
const LOOP_INTERVAL_SECS: u64 = 10;
const RATE_LIMIT_MS: u64 = 500;

const TICKET_PROMPT: &str = r#"You are an expert Level 3 Technical Support Engineer and Logistics Coordinator for InBody devices.
Your task is to analyze a raw, noisy customer support email thread and extract the core technical facts AND all logistics/contact footprints.

CRITICAL INSTRUCTIONS:
- Ignore greetings and emotional complaints.
- Synthesize the entire thread into a single, cohesive summary.
- Output the result in German.

Extract the information into the following strict structure:

=== LOGISTIK & KONTAKTE ===
**Firma / Einrichtung:** (Extract company names, clinic names, or practices. If multiple, list them).
**Kontaktpersonen:** (List ALL distinct names, emails, and phone numbers found in the text and email signatures. This is crucial for matching future physical packages).
**Adressen:** (Extract any physical street addresses, ZIP codes, and cities mentioned).

=== TECHNISCHE DETAILS ===
**Gerät / Modell:** (Extract device model or serial number, e.g., "InBody 770", "SN: 12345").
**Hauptproblem (Symptom):** (Briefly describe the technical failure in 1-2 sentences).
**Durchgeführte Schritte:** (Troubleshooting steps already taken).
**Lösung / Status:** (Current status, e.g., "RMA needed", "Waiting for customer", "Resolved")."#;

const INVOICE_PROMPT: &str = r#"You are an expert AI assistant for an ERP and Warehouse Management System.
Your task is to analyze a raw invoice (Rechnung) document and extract the core logistical and product data.

Extract the information into the following strict structure in German:

=== KÄUFER & ADRESSEN ===
**Rechnungsadresse:** (Extract the billing company, name, and address)
**Lieferadresse:** (Extract the shipping/delivery address if different)
**Kontaktdaten:** (Email, phone numbers)

=== POSITIONEN & SERIENNUMMERN ===
**Gekaufte Artikel:** (List the models/products purchased)
**Seriennummern:** (Extract ALL serial numbers mentioned in the invoice. This is CRITICAL for warranty tracking)."#;

/// Spawns the background summarization worker that processes pending ticket documents.
pub async fn start_summarization_worker(db: SurrealDb, api_key: String, model: String) {
    // Delay to let the server finish startup
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    info!("[Summarization] Worker started ({LOOP_INTERVAL_SECS}s interval, model={model})");

    let http = HttpClient::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(LOOP_INTERVAL_SECS));

    loop {
        interval.tick().await;

        if let Err(e) = process_pending(&db, &http, &api_key, &model).await {
            warn!("[Summarization] cycle error: {e}");
        }
    }
}

async fn process_pending(db: &SurrealDb, http: &HttpClient, api_key: &str, model: &str) -> Result<(), anyhow::Error> {
    // Process both support_ticket and invoice document types
    let docs: Vec<Value> = db
        .query(&format!(
            "SELECT * FROM document WHERE summary_status = 'pending' AND type IN ['support_ticket', 'invoice'] LIMIT {BATCH_LIMIT}"
        ))
        .await?
        .take(0)?;

    if docs.is_empty() {
        return Ok(());
    }

    let mut count = 0u32;
    for doc in &docs {
        let id = match doc.get("id") {
            Some(v) => v.to_string().trim_matches('"').to_string(),
            None => continue,
        };

        let doc_type = doc
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let (raw_text, prompt) = match doc_type {
            "support_ticket" => {
                let ticket_id = id.split(':').last().unwrap_or(&id).to_string();
                let text = build_ticket_text(db, doc, ticket_id).await?;
                (text, TICKET_PROMPT)
            }
            "invoice" => {
                let text = build_invoice_text(doc);
                (text, INVOICE_PROMPT)
            }
            _ => continue,
        };

        if raw_text.is_empty() {
            db.query("UPDATE type::record($id) MERGE { summary_status: 'skipped' }")
                .bind(("id", id.clone()))
                .await?;
            continue;
        }

        match summarize(http, api_key, model, prompt, &raw_text).await {
            Ok(summary) => {
                db.query(
                    "UPDATE type::record($id) MERGE { ai_summary: $summary, summary_status: 'completed', embedding_status: 'pending' }",
                )
                .bind(("id", id.clone()))
                .bind(("summary", summary.clone()))
                .await?;
                count += 1;
                debug!("[Summarization] Summarized {id} ({doc_type}, {} chars)", summary.len());
            }
            Err(e) => {
                warn!("[Summarization] Failed to summarize {id}: {e}");
                db.query(
                    "UPDATE type::record($id) MERGE { summary_status: 'error', summary_error: $err }",
                )
                .bind(("id", id.clone()))
                .bind(("err", e.to_string()))
                .await?;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(RATE_LIMIT_MS)).await;
    }

    if count > 0 {
        info!("[Summarization] {count} documents summarized");
    }

    Ok(())
}

/// Build the raw text for summarization by combining ticket metadata and all thread contents.
async fn build_ticket_text(
    db: &SurrealDb,
    ticket: &Value,
    ticket_id: String,
) -> Result<String, anyhow::Error> {
    let mut parts = Vec::new();

    // Ticket metadata
    if let Some(payload) = ticket.get("payload") {
        if let Some(t) = payload.get("ticket") {
            if let Some(s) = t.get("subject").and_then(|v| v.as_str()) {
                parts.push(format!("Subject: {s}"));
            }
            if let Some(s) = t.get("status").and_then(|v| v.as_str()) {
                parts.push(format!("Status: {s}"));
            }
            // Custom fields contain device/serial/address data
            if let Some(cf) = t.get("cf") {
                for (key, label) in [
                    ("cf_serial_number", "Serial Number"),
                    ("cf_in_body_model", "Model"),
                    ("cf_company", "Company"),
                    ("cf_street", "Address"),
                    ("cf_city", "City"),
                    ("cf_country_1", "Country"),
                ] {
                    if let Some(s) = cf.get(key).and_then(|v| v.as_str()) {
                        if !s.is_empty() && s != "null" {
                            parts.push(format!("{label}: {s}"));
                        }
                    }
                }
            }
        }
    }

    // All threads for this ticket, ordered by creation date
    let threads: Vec<Value> = db
        .query("SELECT payload FROM document WHERE type = 'support_thread' AND ticket_id = $tid ORDER BY created_at ASC")
        .bind(("tid", ticket_id.clone()))
        .await?
        .take(0)?;

    for thread in &threads {
        if let Some(payload) = thread.get("payload") {
            if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
                let plain = strip_html(content);
                if !plain.is_empty() {
                    parts.push(plain);
                }
            }
        }
    }

    let full = parts.join("\n\n");
    // Truncate to ~28000 chars to stay within Gemini context limits
    Ok(if full.len() > 28000 {
        format!("{}\n\n[... truncated ...]", &full[..28000])
    } else {
        full
    })
}

/// Build raw text from an invoice document payload.
fn build_invoice_text(doc: &Value) -> String {
    let mut parts = Vec::new();

    if let Some(payload) = doc.get("payload") {
        // Try common invoice fields
        for (key, label) in [
            ("subject", "Betreff"),
            ("content", "Inhalt"),
            ("invoice_number", "Rechnungsnummer"),
            ("customer_name", "Kunde"),
            ("total_amount", "Betrag"),
        ] {
            if let Some(s) = payload.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    parts.push(format!("{label}: {s}"));
                }
            }
        }
        // If content is HTML, strip it
        if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
            let plain = strip_html(content);
            if !plain.is_empty() && !parts.iter().any(|p| p.starts_with("Inhalt:")) {
                parts.push(plain);
            }
        }
    }

    let full = parts.join("\n\n");
    if full.len() > 28000 {
        format!("{}\n\n[... truncated ...]", &full[..28000])
    } else {
        full
    }
}

/// Call Gemini via direct HTTP to summarize document text.
async fn summarize(
    http: &HttpClient,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    raw_text: &str,
) -> Result<String, anyhow::Error> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
    );

    let user_msg = format!(
        "Analyze the following raw support ticket data and produce the structured summary:\n\n{raw_text}"
    );

    let payload = serde_json::json!({
        "systemInstruction": { "parts": [{ "text": system_prompt }] },
        "contents": [{ "parts": [{ "text": user_msg }] }],
        "generationConfig": {
            "temperature": 0.2,
            "maxOutputTokens": 4096,
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

    let response_text = body["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No text in Gemini summarization response"))?;

    Ok(response_text.to_string())
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
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
