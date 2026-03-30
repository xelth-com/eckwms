use chrono::{Timelike, Utc};
use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use tracing::{info, warn, error};

use super::support;

const SCRAPER_BASE: &str = "http://127.0.0.1:3211";

/// Start all background cron jobs. Call once from main via `tokio::spawn`.
pub async fn start_cron_jobs(db: SurrealDb, instance_id: String) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("failed to create reqwest client");

    // Task 1: Hourly sync (OPAL, DHL, Zoho Desk)
    {
        let db = db.clone();
        let client = client.clone();
        let iid = instance_id.clone();
        tokio::spawn(async move {
            // Wait 30s after startup before first run
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                info!("[scheduler] Starting hourly sync cycle");
                sync_opal(&db, &client, &iid).await;
                sync_dhl(&db, &client, &iid).await;
                sync_zoho(&db, &client, &iid).await;
            }
        });
    }

    // Task 2: Daily morning sync (Excel & Exact Online) at 06:00
    {
        let db = db.clone();
        let client = client.clone();
        let iid = instance_id.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let hour = chrono::Local::now().hour();
                if hour == 6 {
                    info!("[scheduler] Starting daily morning sync (06:00)");
                    sync_exact_online(&db, &client, &iid).await;
                    sync_excel(&db, &client, &iid).await;
                    // Sleep 23h to prevent re-trigger within the same day
                    tokio::time::sleep(std::time::Duration::from_secs(23 * 3600)).await;
                }
            }
        });
    }
}

// ── Hourly providers ──────────────────────────────────────────────

async fn sync_opal(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let res = client
        .get(format!("{}/api/opal/fetch", SCRAPER_BASE))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let created = body.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
            let updated = body.get("updated").and_then(|v| v.as_i64()).unwrap_or(0);
            info!("[scheduler] OPAL sync: {} created, {} updated", created, updated);
            log_sync(db, instance_id, "opal", "ok", started, created, updated, 0, 0, "").await;
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!("[scheduler] OPAL sync failed ({}): {}", status, &text[..text.len().min(200)]);
            log_sync(db, instance_id, "opal", "error", started, 0, 0, 0, 1, &text[..text.len().min(500)]).await;
        }
        Err(e) => {
            warn!("[scheduler] OPAL sync: scraper unreachable: {}", e);
            log_sync(db, instance_id, "opal", "error", started, 0, 0, 0, 1, &e.to_string()).await;
        }
    }
}

async fn sync_dhl(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let res = client
        .get(format!("{}/api/dhl/fetch", SCRAPER_BASE))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let created = body.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
            let updated = body.get("updated").and_then(|v| v.as_i64()).unwrap_or(0);
            info!("[scheduler] DHL sync: {} created, {} updated", created, updated);
            log_sync(db, instance_id, "dhl", "ok", started, created, updated, 0, 0, "").await;
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!("[scheduler] DHL sync failed ({}): {}", status, &text[..text.len().min(200)]);
            log_sync(db, instance_id, "dhl", "error", started, 0, 0, 0, 1, &text[..text.len().min(500)]).await;
        }
        Err(e) => {
            warn!("[scheduler] DHL sync: scraper unreachable: {}", e);
            log_sync(db, instance_id, "dhl", "error", started, 0, 0, 0, 1, &e.to_string()).await;
        }
    }
}

async fn sync_zoho(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let created: i64 = 0;
    let mut updated: i64 = 0;
    let mut skipped: i64 = 0;
    let mut errors: i64 = 0;
    let mut error_detail = String::new();

    // 1. Fetch tickets from Zoho via scraper proxy
    let tickets_res = client
        .post(format!("{}/api/zoho/tickets", SCRAPER_BASE))
        .json(&json!({ "limit": 50, "_from_env": true }))
        .send()
        .await;

    let tickets: Vec<Value> = match tickets_res {
        Ok(resp) if resp.status().is_success() => {
            resp.json().await.unwrap_or_default()
        }
        Ok(resp) => {
            let text = resp.text().await.unwrap_or_default();
            warn!("[scheduler] Zoho tickets fetch failed: {}", &text[..text.len().min(200)]);
            log_sync(db, instance_id, "zoho_desk", "error", started, 0, 0, 0, 1, &text[..text.len().min(500)]).await;
            return;
        }
        Err(e) => {
            warn!("[scheduler] Zoho: scraper unreachable: {}", e);
            log_sync(db, instance_id, "zoho_desk", "error", started, 0, 0, 0, 1, &e.to_string()).await;
            return;
        }
    };

    info!("[scheduler] Zoho: fetched {} tickets", tickets.len());

    // 2. Import each ticket via the shared service
    for ticket in &tickets {
        let ticket_id = match ticket.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => { skipped += 1; continue; }
        };

        match support::import_ticket(db, ticket_id, ticket).await {
            Ok(r) if r.changed => { updated += 1; }
            Ok(_) => { skipped += 1; }
            Err(e) => {
                errors += 1;
                if error_detail.len() < 500 {
                    error_detail.push_str(&format!("ticket {}: {}; ", ticket_id, e));
                }
            }
        }

        // 3. Fetch and import threads for tickets that have them
        let thread_count = ticket
            .get("threadCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if thread_count > 0 {
            let threads_res = client
                .post(format!("{}/api/zoho/ticket-threads", SCRAPER_BASE))
                .json(&json!({ "ticketId": ticket_id, "_from_env": true }))
                .send()
                .await;

            if let Ok(resp) = threads_res {
                if resp.status().is_success() {
                    let threads: Vec<Value> = resp.json().await.unwrap_or_default();
                    for thread in &threads {
                        let thread_id = match thread.get("id").and_then(|v| v.as_str()) {
                            Some(id) => id,
                            None => continue,
                        };
                        if let Err(e) = support::import_thread(db, thread_id, ticket_id, thread).await {
                            errors += 1;
                            if error_detail.len() < 500 {
                                error_detail.push_str(&format!("thread {}: {}; ", thread_id, e));
                            }
                        }
                    }
                }
            }
        }
    }

    let status = if errors > 0 { "partial" } else { "ok" };
    info!("[scheduler] Zoho sync done: {} updated, {} skipped, {} errors", updated, skipped, errors);
    log_sync(db, instance_id, "zoho_desk", status, started, created, updated, skipped, errors, &error_detail).await;
}

// ── Daily providers ───────────────────────────────────────────────

async fn sync_exact_online(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();

    // Fetch products from Exact Online
    let products_res = client
        .get(format!("{}/api/exact/products", SCRAPER_BASE))
        .send()
        .await;

    let created: i64 = 0;
    let mut updated: i64 = 0;
    let errors: i64 = 0;
    let error_detail = String::new();

    if let Ok(resp) = products_res {
        if resp.status().is_success() {
            let items: Vec<Value> = resp.json().await.unwrap_or_default();
            for item in &items {
                let ext_id = item.get("ID").or(item.get("id")).and_then(|v| v.as_str()).unwrap_or_default();
                if ext_id.is_empty() { continue; }

                let _: Result<Option<Value>, _> = db
                    .upsert(("product", ext_id))
                    .merge(json!({
                        "source_system": "exact_online",
                        "external_id": ext_id,
                        "payload": item,
                        "updated_at": Utc::now().to_rfc3339(),
                    }))
                    .await;
                updated += 1;
            }
            info!("[scheduler] Exact Online products: {} synced", updated);
        }
    }

    // Fetch partners/contacts
    let partners_res = client
        .get(format!("{}/api/exact/partners", SCRAPER_BASE))
        .send()
        .await;

    let mut partner_count: i64 = 0;
    if let Ok(resp) = partners_res {
        if resp.status().is_success() {
            let items: Vec<Value> = resp.json().await.unwrap_or_default();
            for item in &items {
                let ext_id = item.get("ID").or(item.get("id")).and_then(|v| v.as_str()).unwrap_or_default();
                if ext_id.is_empty() { continue; }

                let _: Result<Option<Value>, _> = db
                    .upsert(("partner", ext_id))
                    .merge(json!({
                        "source_system": "exact_online",
                        "external_id": ext_id,
                        "payload": item,
                        "updated_at": Utc::now().to_rfc3339(),
                    }))
                    .await;
                partner_count += 1;
            }
            info!("[scheduler] Exact Online partners: {} synced", partner_count);
        }
    }

    log_sync(db, instance_id, "exact_online", "ok", started, created, updated + partner_count, 0, errors, &error_detail).await;
}

async fn sync_excel(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let (mut created, mut updated, mut skipped, mut errors) = (0i64, 0i64, 0i64, 0i64);
    let mut error_detail = String::new();

    let res = client
        .get(format!("{}/api/excel/read", SCRAPER_BASE))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            let rows: Vec<Value> = resp.json().await.unwrap_or_default();
            info!("[scheduler] Excel: fetched {} rows", rows.len());

            for row in &rows {
                let order_number = row
                    .get("order_number")
                    .or(row.get("Auftragsnummer"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if order_number.is_empty() { skipped += 1; continue; }

                // Safe Insert/Update: only fill empty fields, never overwrite existing data
                let existing: Option<Value> = db
                    .query("SELECT * FROM order WHERE order_number = $num LIMIT 1")
                    .bind(("num", order_number.clone()))
                    .await
                    .ok()
                    .and_then(|mut r| r.take(0).ok())
                    .flatten();

                if let Some(existing) = existing {
                    // Merge only empty/null fields from Excel into existing record
                    let existing_obj = existing.as_object();
                    let mut patch = serde_json::Map::new();
                    if let Some(row_obj) = row.as_object() {
                        for (k, v) in row_obj {
                            let existing_val = existing_obj.and_then(|o| o.get(k));
                            let is_empty = match existing_val {
                                None => true,
                                Some(Value::Null) => true,
                                Some(Value::String(s)) if s.is_empty() => true,
                                _ => false,
                            };
                            if is_empty && !v.is_null() {
                                patch.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    if !patch.is_empty() {
                        patch.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                        let id = existing.get("id").and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| order_number.clone());
                        let _: Result<Option<Value>, _> = db
                            .upsert(("order", id.as_str()))
                            .merge(Value::Object(patch))
                            .await;
                        updated += 1;
                    } else {
                        skipped += 1;
                    }
                } else {
                    // New row — insert
                    let mut new_record = row.clone();
                    if let Some(obj) = new_record.as_object_mut() {
                        obj.insert("source_system".to_string(), json!("excel"));
                        obj.insert("created_at".to_string(), json!(Utc::now().to_rfc3339()));
                        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
                    }
                    let _: Result<Option<Value>, _> = db
                        .upsert(("order", order_number.as_str()))
                        .content(new_record)
                        .await;
                    created += 1;
                }
            }
        }
        Ok(resp) => {
            let text = resp.text().await.unwrap_or_default();
            warn!("[scheduler] Excel sync failed: {}", &text[..text.len().min(200)]);
            errors += 1;
            error_detail = text[..text.len().min(500)].to_string();
        }
        Err(e) => {
            warn!("[scheduler] Excel: scraper unreachable: {}", e);
            errors += 1;
            error_detail = e.to_string();
        }
    }

    let status = if errors > 0 { "error" } else { "ok" };
    info!("[scheduler] Excel sync done: {} created, {} updated, {} skipped", created, updated, skipped);
    log_sync(db, instance_id, "excel", status, started, created, updated, skipped, errors, &error_detail).await;
}

// ── Sync history logging ──────────────────────────────────────────

async fn log_sync(
    db: &SurrealDb,
    instance_id: &str,
    provider: &str,
    status: &str,
    started_at: chrono::DateTime<Utc>,
    created: i64,
    updated: i64,
    skipped: i64,
    errors: i64,
    error_detail: &str,
) {
    let now = Utc::now();
    let duration = (now - started_at).num_milliseconds();

    let result: Result<Option<Value>, _> = db
        .create("sync_history")
        .content(json!({
            "instance_id": instance_id,
            "provider": provider,
            "status": status,
            "started_at": started_at.to_rfc3339(),
            "completed_at": now.to_rfc3339(),
            "duration": duration,
            "created": created,
            "updated": updated,
            "skipped": skipped,
            "errors": errors,
            "error_detail": error_detail,
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339(),
        }))
        .await;

    if let Err(e) = result {
        error!("[scheduler] Failed to log sync_history for {}: {}", provider, e);
    }
}
