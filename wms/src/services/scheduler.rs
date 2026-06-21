use chrono::{Timelike, Utc};
use eck_core::db::SurrealDb;
use serde_json::{json, Value};
use tracing::{debug, info, warn, error};

use super::support;
use crate::handlers::scraper_proxy::scraper_base;

/// Start all background cron jobs. Call once from main via `tokio::spawn`.
pub async fn start_cron_jobs(db: SurrealDb, instance_id: String) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
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
                sync_zoho(&db, &client, &iid, false).await;
            }
        });
    }

    // Task 2: Daily morning sync (Excel & Exact Online) at 06:00, with catch-up on startup
    {
        let db = db.clone();
        let client = client.clone();
        let iid = instance_id.clone();
        tokio::spawn(async move {
            // Check if daily sync was missed (e.g. PC was off) — run once on startup if >24h ago
            tokio::time::sleep(std::time::Duration::from_secs(45)).await;
            if should_run_daily_catchup(&db).await {
                info!("[scheduler] Daily sync catch-up: last run >24h ago, running now");
                run_daily_sync(&db, &client, &iid).await;
            }

            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let hour = chrono::Local::now().hour();
                if hour == 6 {
                    info!("[scheduler] Starting daily morning sync (06:00)");
                    run_daily_sync(&db, &client, &iid).await;
                    // Sleep 23h to prevent re-trigger within the same day
                    tokio::time::sleep(std::time::Duration::from_secs(23 * 3600)).await;
                }
            }
        });
    }
}

// ── Hourly providers ──────────────────────────────────────────────

pub async fn sync_opal(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let res = client
        .post(format!("{}/api/opal/fetch", scraper_base()))
        .json(&json!({"_from_env": true}))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let orders = body.get("orders").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            let mut updated: i64 = 0;

            for order in &orders {
                let tracking = order.get("ocu_number")
                    .or(order.get("tracking_number"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if tracking.is_empty() { continue; }

                let _: Result<Option<Value>, _> = db.upsert(("shipment", tracking))
                    .merge(json!({
                        "tracking_number": tracking,
                        "status": order.get("status").unwrap_or(&json!("processing")),
                        "raw_response": serde_json::to_string(order).unwrap_or_default(),
                        "provider": "opal",
                        "updated_at": Utc::now().to_rfc3339()
                    })).await;
                updated += 1;
            }

            info!("[scheduler] OPAL sync: {} upserted from {} orders", updated, orders.len());
            log_sync(db, instance_id, "opal", "ok", started, 0, updated, 0, 0, "").await;
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

pub async fn sync_dhl(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let res = client
        .post(format!("{}/api/dhl/fetch", scraper_base()))
        .json(&json!({"_from_env": true}))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let shipments = body.get("shipments").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            let mut updated: i64 = 0;

            for shipment in &shipments {
                let tracking = shipment.get("tracking_number")
                    .or(shipment.get("sendungsnummer"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if tracking.is_empty() { continue; }

                let _: Result<Option<Value>, _> = db.upsert(("shipment", tracking))
                    .merge(json!({
                        "tracking_number": tracking,
                        "status": shipment.get("status").unwrap_or(&json!("processing")),
                        "raw_response": serde_json::to_string(shipment).unwrap_or_default(),
                        "provider": "dhl",
                        "updated_at": Utc::now().to_rfc3339()
                    })).await;
                updated += 1;
            }

            info!("[scheduler] DHL sync: {} upserted from {} shipments", updated, shipments.len());
            log_sync(db, instance_id, "dhl", "ok", started, 0, updated, 0, 0, "").await;
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

pub async fn sync_zoho(db: &SurrealDb, client: &reqwest::Client, instance_id: &str, full_sync: bool) {
    let started = Utc::now();
    let created: i64 = 0;
    let mut updated: i64 = 0;
    let mut skipped: i64 = 0;
    let mut errors: i64 = 0;
    let mut error_detail = String::new();

    // 1. Fetch tickets from Zoho via scraper proxy
    let tickets_res = client
        .post(format!("{}/api/zoho/tickets", scraper_base()))
        .json(&json!({ "limit": if full_sync { 0 } else { 100 }, "_from_env": true }))
        .send()
        .await;

    let tickets: Vec<Value> = match tickets_res {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            body.get("tickets").and_then(|v| v.as_array()).cloned()
                .or_else(|| body.as_array().cloned())
                .unwrap_or_default()
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

    info!("[scheduler] Zoho {}: fetched {} tickets", if full_sync { "full" } else { "incremental" }, tickets.len());

    // 2. Import each ticket via the shared service
    for ticket in &tickets {
        let ticket_id_owned = match ticket.get("id") {
            Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
            Some(v) if v.is_number() => v.to_string(),
            _ => { skipped += 1; continue; }
        };
        let ticket_id = ticket_id_owned.as_str();

        match support::import_ticket(db, ticket_id, ticket, instance_id).await {
            Ok(r) if r.changed => { updated += 1; }
            Ok(_) => { skipped += 1; }
            Err(e) => {
                errors += 1;
                if error_detail.len() < 500 {
                    error_detail.push_str(&format!("ticket {}: {}; ", ticket_id, e));
                }
            }
        }

        // 3. Always fetch the ticket detail + threads via ticket-threads.
        // The list endpoint (/tickets) does NOT return customFields; only the
        // detail endpoint (/tickets/{id}) does. We need customFields to promote
        // address/city/zip into meta for the map. Running this for every ticket
        // is fine on incremental syncs (few modified per run) and necessary on
        // full syncs / backfills.
        // On full syncs pull attachment binaries too — incremental runs every
        // hour would be too noisy to re-download everything each time.
        let threads_res = client
            .post(format!("{}/api/zoho/ticket-threads", scraper_base()))
            .json(&json!({
                "ticketId": ticket_id,
                "_from_env": true,
                "includeAttachmentContent": full_sync,
            }))
            .send()
            .await;

        if let Ok(resp) = threads_res {
            if resp.status().is_success() {
                let body: Value = resp.json().await.unwrap_or(json!({}));

                // Re-import ticket with enriched payload (includes cf.* custom fields like InBody Model, Serial Number, Address, City, Zip)
                if let Some(enriched_ticket) = body.get("ticket") {
                    if enriched_ticket.get("cf").is_some() || enriched_ticket.get("customFields").is_some() {
                        match support::import_ticket(db, ticket_id, enriched_ticket, instance_id).await {
                            Ok(r) if r.changed => { debug!("[scheduler] Zoho ticket {} re-imported with cf fields", ticket_id); }
                            _ => {}
                        }
                    }
                }

                let threads = body.get("threads").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                for thread in &threads {
                    let thread_id_owned = match thread.get("id") {
                        Some(v) if v.is_string() => v.as_str().unwrap().to_string(),
                        Some(v) if v.is_number() => v.to_string(),
                        _ => continue,
                    };
                    let thread_id = thread_id_owned.as_str();
                    if let Err(e) = support::import_thread(db, thread_id, ticket_id, thread, instance_id).await {
                        errors += 1;
                        if error_detail.len() < 500 {
                            error_detail.push_str(&format!("thread {}: {}; ", thread_id, e));
                        }
                    }

                    // Persist attachment binaries to file_resource + has_attachment.
                    // Attachments live at the thread level in Zoho, but they're
                    // logically owned by the parent ticket (QC reports, photos, etc.),
                    // so we RELATE them to document:$ticket_id — that's what the
                    // orchestrator's list_ticket_attachments tool queries.
                    if let Some(atts) = thread.get("attachments").and_then(|v| v.as_array()) {
                        for att in atts {
                            if let Err(e) = support::import_attachment(db, ticket_id, att).await {
                                debug!("[scheduler] attachment import failed for thread {}: {}", thread_id, e);
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

/// Get the `updated` count from the last successful sync for a given provider+sub_key.
/// Returns None if no previous sync or if the last sync was an error.
async fn last_sync_count(db: &SurrealDb, provider: &str) -> Option<i64> {
    let result: Option<Value> = db
        .query("SELECT updated FROM sync_history WHERE provider = $p AND status = 'ok' ORDER BY completed_at DESC LIMIT 1")
        .bind(("p", provider.to_string()))
        .await
        .ok()
        .and_then(|mut r| r.take(0).ok())
        .flatten();
    result.and_then(|v| v.get("updated").and_then(|u| u.as_i64()))
}

/// Fetch rows from an Exact Online scraper endpoint. Returns (count_from_api, rows).
/// If `last_count` matches the API count, returns empty rows (skip).
async fn fetch_exact_rows(
    client: &reqwest::Client,
    endpoint: &str,
    last_count: Option<i64>,
) -> Result<(i64, Vec<Value>), String> {
    let res = client
        .post(format!("{}{}", scraper_base(), endpoint))
        .json(&json!({ "_from_env": true }))
        .send()
        .await
        .map_err(|e| format!("scraper unreachable: {}", e))?;

    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("HTTP {}", &text[..text.len().min(200)]));
    }

    let body: Value = res.json().await.unwrap_or(json!({}));
    let api_count = body.get("count").and_then(|v| v.as_i64()).unwrap_or(-1);

    // Skip full download if count matches previous sync
    if let Some(prev) = last_count {
        if api_count == prev && api_count > 0 {
            return Ok((api_count, vec![])); // empty = skipped
        }
    }

    let rows = body.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    Ok((api_count, rows))
}

pub async fn sync_exact_online(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let mut total_updated: i64 = 0;
    let mut total_skipped: i64 = 0;
    let mut total_errors: i64 = 0;
    let mut error_detail = String::new();

    // ── Products (items) ──
    let prev_products = last_sync_count(db, "exact_products").await;
    match fetch_exact_rows(client, "/api/exact/items/fetch", prev_products).await {
        Ok((count, items)) if items.is_empty() && count > 0 => {
            info!("[scheduler] Exact Online products: skipped (count unchanged: {})", count);
            total_skipped += count;
        }
        Ok((_, items)) => {
            let mut n = 0i64;
            for item in &items {
                let ext_id = item.get("code").and_then(|v| v.as_str()).unwrap_or_default();
                if ext_id.is_empty() { continue; }
                let name = item.get("description").or(item.get("name"))
                    .and_then(|v| v.as_str()).unwrap_or_default();
                let barcode = item.get("barcode").and_then(|v| v.as_str()).unwrap_or_default();
                let _: Result<Option<Value>, _> = db
                    .upsert(("product", ext_id))
                    .merge(json!({
                        "source_system": "exact_online",
                        "external_id": ext_id,
                        "name": name,
                        "default_code": ext_id,
                        "barcode": barcode,
                        "payload": item,
                        "updated_at": Utc::now().to_rfc3339(),
                    }))
                    .await;
                n += 1;
            }
            info!("[scheduler] Exact Online products: {} synced", n);
            total_updated += n;
            log_sync(db, instance_id, "exact_products", "ok", started, 0, n, 0, 0, "").await;
        }
        Err(e) => {
            warn!("[scheduler] Exact Online products failed: {}", e);
            total_errors += 1;
            error_detail.push_str(&format!("products: {}; ", e));
        }
    }

    // ── Partners (customers) ──
    let prev_partners = last_sync_count(db, "exact_partners").await;
    match fetch_exact_rows(client, "/api/exact/customers/fetch", prev_partners).await {
        Ok((count, items)) if items.is_empty() && count > 0 => {
            info!("[scheduler] Exact Online partners: skipped (count unchanged: {})", count);
            total_skipped += count;
        }
        Ok((_, items)) => {
            let mut n = 0i64;
            for item in &items {
                let ext_id = item.get("code").and_then(|v| v.as_str()).unwrap_or_default();
                if ext_id.is_empty() { continue; }
                let name = item.get("name").or(item.get("accountName"))
                    .and_then(|v| v.as_str()).unwrap_or_default();
                let email = item.get("email").and_then(|v| v.as_str()).unwrap_or_default();
                let phone = item.get("phone").and_then(|v| v.as_str()).unwrap_or_default();
                let city = item.get("city").and_then(|v| v.as_str()).unwrap_or_default();
                let country = item.get("country").and_then(|v| v.as_str()).unwrap_or_default();
                let _: Result<Option<Value>, _> = db
                    .upsert(("partner", ext_id))
                    .merge(json!({
                        "source_system": "exact_online",
                        "external_id": ext_id,
                        "name": name,
                        "email": email,
                        "phone": phone,
                        "city": city,
                        "country": country,
                        "payload": item,
                        "updated_at": Utc::now().to_rfc3339(),
                    }))
                    .await;
                n += 1;
            }
            info!("[scheduler] Exact Online partners: {} synced", n);
            total_updated += n;
            log_sync(db, instance_id, "exact_partners", "ok", started, 0, n, 0, 0, "").await;
        }
        Err(e) => {
            warn!("[scheduler] Exact Online partners failed: {}", e);
            total_errors += 1;
            error_detail.push_str(&format!("partners: {}; ", e));
        }
    }

    // ── Stock Positions (always fetch — quantities change even if count doesn't) ──
    match fetch_exact_rows(client, "/api/exact/stock-positions/fetch", None).await {
        Ok((_, items)) => {
            let mut n = 0i64;
            for item in &items {
                let item_code = item.get("item_code").and_then(|v| v.as_str()).unwrap_or_default();
                let wh_code = item.get("warehouse_code").and_then(|v| v.as_str()).unwrap_or_default();
                if item_code.is_empty() { continue; }
                // Composite key: item_code + warehouse_code
                let ext_id = format!("{}_{}", item_code, wh_code);
                let _: Result<Option<Value>, _> = db
                    .upsert(("stock_position", ext_id.as_str()))
                    .merge(json!({
                        "source_system": "exact_online",
                        "item_code": item_code,
                        "warehouse_code": wh_code,
                        "in_stock": item.get("in_stock"),
                        "planned_in": item.get("planned_in"),
                        "planned_out": item.get("planned_out"),
                        "projected_stock": item.get("projected_stock"),
                        "reorder_point": item.get("reorder_point"),
                        "payload": item,
                        "updated_at": Utc::now().to_rfc3339(),
                    }))
                    .await;
                n += 1;
            }
            info!("[scheduler] Exact Online stock positions: {} synced", n);
            total_updated += n;
            log_sync(db, instance_id, "exact_stock", "ok", started, 0, n, 0, 0, "").await;
        }
        Err(e) => {
            warn!("[scheduler] Exact Online stock positions failed: {}", e);
            total_errors += 1;
            error_detail.push_str(&format!("stock: {}; ", e));
        }
    }

    // ── Quotations (always fetch — new ones added frequently) ──
    match fetch_exact_rows(client, "/api/exact/quotations/fetch", None).await {
        Ok((_, items)) => {
            let mut n = 0i64;
            for item in &items {
                let number = item.get("number_version").and_then(|v| v.as_str()).unwrap_or_default();
                if number.is_empty() { continue; }
                // Use number_version as ID (e.g. "548 / 1"), sanitize for SurrealDB key
                let ext_id = number.replace(['/', ' '], "_");
                let _: Result<Option<Value>, _> = db
                    .upsert(("quotation", ext_id.as_str()))
                    .merge(json!({
                        "source_system": "exact_online",
                        "number_version": number,
                        "ordered_by_code": item.get("ordered_by_code"),
                        "ordered_by_name": item.get("ordered_by_name"),
                        "amount": item.get("amount"),
                        "currency": item.get("currency"),
                        "status": item.get("status"),
                        "quotation_date": item.get("quotation_date"),
                        "description": item.get("description"),
                        "payload": item,
                        "updated_at": Utc::now().to_rfc3339(),
                    }))
                    .await;
                n += 1;
            }
            info!("[scheduler] Exact Online quotations: {} synced", n);
            total_updated += n;
            log_sync(db, instance_id, "exact_quotations", "ok", started, 0, n, 0, 0, "").await;
        }
        Err(e) => {
            warn!("[scheduler] Exact Online quotations failed: {}", e);
            total_errors += 1;
            error_detail.push_str(&format!("quotations: {}; ", e));
        }
    }

    let status = if total_errors > 0 { "partial" } else { "ok" };
    info!("[scheduler] Exact Online total: {} updated, {} skipped, {} errors", total_updated, total_skipped, total_errors);
    log_sync(db, instance_id, "exact_online", status, started, 0, total_updated, total_skipped, total_errors, &error_detail).await;
}

pub async fn sync_excel(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    let started = Utc::now();
    let (mut created, mut updated, mut skipped, mut errors) = (0i64, 0i64, 0i64, 0i64);
    let mut error_detail = String::new();

    let res = client
        .post(format!("{}/api/excel/read", scraper_base()))
        .json(&json!({ "limit": 99999 }))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            let body: Value = resp.json().await.unwrap_or(json!({}));
            let rows: Vec<Value> = body.get("repairs").and_then(|v| v.as_array()).cloned()
                .unwrap_or_default();
            info!("[scheduler] Excel: fetched {} rows", rows.len());

            for row in &rows {
                let order_number = row
                    .get("repairNumber")
                    .or(row.get("order_number"))
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
                        obj.insert("order_number".to_string(), json!(order_number));
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

                // ── Build Graph Relations ──
                let serial = row.get("serialNumber").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let model = row.get("model").and_then(|v| v.as_str()).unwrap_or("Unknown Device").to_string();
                let ticket_number = match row.get("ticketNumber") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                let customer_name = row.get("customerName").and_then(|v| v.as_str()).unwrap_or("").to_string();

                let graph_query = "
                    BEGIN TRANSACTION;
                    LET $order_record = type::record('order', $order_id);

                    -- 1. Link to NAS folder (if exists)
                    LET $nas_folder = type::record('document', $order_id);
                    IF (SELECT id FROM $nas_folder)[0].id != NONE {
                        RELATE $order_record -> has_folder -> $nas_folder SET updated_at = time::now();
                    };

                    -- 2. Link to Item (Device)
                    IF $serial != '' {
                        LET $item_record = type::record('item', $serial);
                        UPSERT $item_record MERGE { primary_barcode: $serial, name: $model, updated_at: time::now() };
                        RELATE $order_record -> repairs -> $item_record SET updated_at = time::now();
                    };

                    -- 3. Link to Ticket
                    IF $ticket_number != '' {
                        LET $ticket = (SELECT id FROM document WHERE type = 'support_ticket' AND meta.ticket_number = $ticket_number LIMIT 1)[0].id;
                        IF $ticket != NONE {
                            RELATE $ticket -> initiated -> $order_record SET updated_at = time::now();
                        };
                    };

                    -- 4. Link to Partner
                    IF $customer_name != '' {
                        LET $partner = (SELECT id FROM partner WHERE string::lowercase(name) = string::lowercase($customer_name) LIMIT 1)[0].id;
                        IF $partner != NONE {
                            RELATE $order_record -> belongs_to -> $partner SET updated_at = time::now();
                        };
                    };
                    COMMIT TRANSACTION;
                ";

                if let Err(e) = db.query(graph_query)
                    .bind(("order_id", order_number.clone()))
                    .bind(("serial", serial))
                    .bind(("model", model))
                    .bind(("ticket_number", ticket_number))
                    .bind(("customer_name", customer_name))
                    .await
                {
                    warn!("[scheduler] Excel graph linking failed for {}: {}", order_number, e);
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

async fn run_daily_sync(db: &SurrealDb, client: &reqwest::Client, instance_id: &str) {
    sync_zoho(db, client, instance_id, true).await;
    sync_exact_online(db, client, instance_id).await;
    sync_excel(db, client, instance_id).await;
}

async fn should_run_daily_catchup(db: &SurrealDb) -> bool {
    let result: Result<Option<Value>, _> = db
        .query("SELECT completed_at FROM sync_history WHERE provider IN ['exact_products', 'exact_partners', 'zoho_desk'] AND status IN ['ok', 'partial'] ORDER BY completed_at DESC LIMIT 1")
        .await
        .and_then(|mut r| r.take(0));

    match result {
        Ok(Some(row)) => {
            let last = row.get("completed_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
            match last {
                Some(dt) => Utc::now().signed_duration_since(dt.with_timezone(&Utc)).num_hours() >= 24,
                None => true,
            }
        }
        _ => true, // No history = never ran
    }
}
