use std::env;

use chrono::{DateTime, Utc};
use crc32fast::Hasher;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::FromRow;
use tracing::info;
use uuid::Uuid;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use eck_core::db;
use eck_core::utils::filestore::FileStore;

// ─── Legacy PG row types ────────────────────────────────────────────────────

#[derive(FromRow)]
struct PgLocation {
    id: Uuid,
    name: Option<String>,
    complete_name: Option<String>,
    barcode: Option<String>,
    usage: Option<String>,
    location_id: Option<Uuid>,
    active: Option<bool>,
}

#[derive(FromRow)]
struct PgRack {
    id: Uuid,
    name: Option<String>,
    prefix: Option<String>,
    columns: Option<i64>,
    rows: Option<i64>,
    start_index: Option<i64>,
    sort_order: Option<i64>,
    warehouse_id: Option<Uuid>,
}

#[derive(FromRow)]
struct PgOrder {
    id: Uuid,
    order_number: Option<String>,
    order_type: Option<String>,
    customer_name: Option<String>,
    customer_email: Option<String>,
    customer_phone: Option<String>,
    product_sku: Option<String>,
    product_name: Option<String>,
    serial_number: Option<String>,
    issue_description: Option<String>,
    diagnosis_notes: Option<String>,
    repair_notes: Option<String>,
    resolution: Option<String>,
    notes: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    parts_used: Option<serde_json::Value>,
    metadata: Option<serde_json::Value>,
    labor_hours: Option<f64>,
    total_cost: Option<f64>,
    rma_reason: Option<String>,
    is_refund_requested: Option<bool>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(FromRow)]
struct PgUser {
    id: Uuid,
    username: Option<String>,
    email: Option<String>,
    name: Option<String>,
    role: Option<String>,
    pin: Option<String>,
    is_active: Option<bool>,
    preferred_language: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

#[derive(FromRow)]
struct PgProduct {
    id: Uuid,
    default_code: Option<String>,
    barcode: Option<String>,
    name: Option<String>,
    active: Option<bool>,
    #[sqlx(rename = "type")]
    product_type: Option<String>,
    list_price: Option<f64>,
    standard_price: Option<f64>,
    weight: Option<f64>,
    volume: Option<f64>,
}

#[derive(FromRow)]
struct PgPartner {
    id: Uuid,
    name: Option<String>,
    street: Option<String>,
    zip: Option<String>,
    city: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    vat: Option<String>,
    company_type: Option<String>,
    is_company: Option<bool>,
}

// ─── Main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    info!("Starting 9eck legacy migrator...");

    let pg_url = env::var("LEGACY_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/eckwms".into());

    info!("Connecting to legacy PostgreSQL: {}", pg_url);
    let pg = PgPoolOptions::new()
        .max_connections(3)
        .connect(&pg_url)
        .await?;

    let db_path = env::var("SURREAL_DB_PATH").unwrap_or_else(|_| "data/wms.db".into());
    info!("Connecting to SurrealDB: {}", db_path);
    let sdb = db::connect(&db_path).await?;

    // ── Schema ──────────────────────────────────────────────────────────────
    info!("Applying SurrealDB schema...");
    sdb.query(
        "-- Vector index for RAG on orders
         DEFINE TABLE IF NOT EXISTS order SCHEMALESS;
         REMOVE FIELD IF EXISTS embedding ON order;
         DEFINE FIELD embedding ON order TYPE option<array<float>>;
         DEFINE INDEX IF NOT EXISTS repair_embedding ON order FIELDS embedding
             HNSW DIMENSION 768 DIST COSINE TYPE F32;

         -- BM25 full-text indexes for hybrid search
         DEFINE ANALYZER IF NOT EXISTS custom_analyzer TOKENIZERS blank,class,camel,punct FILTERS lowercase,ascii;
         DEFINE INDEX IF NOT EXISTS issue_bm25 ON order FIELDS issue_description SEARCH ANALYZER custom_analyzer BM25;
         DEFINE INDEX IF NOT EXISTS order_number_bm25 ON order FIELDS order_number SEARCH ANALYZER custom_analyzer BM25;
         DEFINE INDEX IF NOT EXISTS customer_name_bm25 ON order FIELDS customer_name SEARCH ANALYZER custom_analyzer BM25;

         -- Graph: location ─contains→ rack
         DEFINE TABLE IF NOT EXISTS contains TYPE RELATION IN location OUT rack;

         -- Vector index for RAG on documents
         DEFINE TABLE IF NOT EXISTS document SCHEMALESS;
         REMOVE FIELD IF EXISTS embedding ON document;
         DEFINE FIELD embedding ON document TYPE option<array<float>>;
         DEFINE INDEX IF NOT EXISTS document_embedding ON document FIELDS embedding
             HNSW DIMENSION 768 DIST COSINE TYPE F32;
         DEFINE INDEX IF NOT EXISTS doc_content_bm25 ON document FIELDS payload.content SEARCH ANALYZER custom_analyzer BM25;

         -- Vector + BM25 indexes for partner (360-degree view)
         DEFINE FIELD IF NOT EXISTS embedding ON partner TYPE option<array<float>>;
         DEFINE INDEX IF NOT EXISTS partner_embedding ON partner FIELDS embedding
             HNSW DIMENSION 768 DIST COSINE TYPE F32;
         DEFINE INDEX IF NOT EXISTS partner_name_bm25 ON partner FIELDS name SEARCH ANALYZER custom_analyzer BM25;
         DEFINE INDEX IF NOT EXISTS partner_email_bm25 ON partner FIELDS email SEARCH ANALYZER custom_analyzer BM25;

         -- Vector + BM25 indexes for product (360-degree view)
         DEFINE FIELD IF NOT EXISTS embedding ON product TYPE option<array<float>>;
         DEFINE INDEX IF NOT EXISTS product_embedding ON product FIELDS embedding
             HNSW DIMENSION 768 DIST COSINE TYPE F32;
         DEFINE INDEX IF NOT EXISTS product_name_bm25 ON product FIELDS name SEARCH ANALYZER custom_analyzer BM25;
         DEFINE INDEX IF NOT EXISTS product_code_bm25 ON product FIELDS default_code SEARCH ANALYZER custom_analyzer BM25;
         DEFINE INDEX IF NOT EXISTS product_barcode_bm25 ON product FIELDS barcode SEARCH ANALYZER custom_analyzer BM25;

         -- Vector + BM25 indexes for picking / delivery (360-degree view)
         DEFINE FIELD IF NOT EXISTS embedding ON picking TYPE option<array<float>>;
         DEFINE INDEX IF NOT EXISTS picking_embedding ON picking FIELDS embedding
             HNSW DIMENSION 768 DIST COSINE TYPE F32;
         DEFINE INDEX IF NOT EXISTS picking_tracking_bm25 ON picking FIELDS tracking_number SEARCH ANALYZER custom_analyzer BM25;
         DEFINE INDEX IF NOT EXISTS picking_recipient_bm25 ON picking FIELDS recipient_name SEARCH ANALYZER custom_analyzer BM25;",
    )
    .await?;

    // ── CLI flags ────────────────────────────────────────────────────────────
    let args: Vec<String> = env::args().collect();
    let only_files = args.iter().any(|a| a == "--only-files");
    let verify = args.iter().any(|a| a == "--verify");

    if verify {
        verify_counts(&pg, &sdb).await?;
        return Ok(());
    }

    // ── Migrate ─────────────────────────────────────────────────────────────
    if !only_files {
        migrate_locations(&pg, &sdb).await?;
        migrate_racks(&pg, &sdb).await?;
        migrate_orders(&pg, &sdb).await?;
        migrate_users(&pg, &sdb).await?;
        migrate_products(&pg, &sdb).await?;
        migrate_partners(&pg, &sdb).await?;
        migrate_documents(&pg, &sdb).await?;
    } else {
        info!("Skipping base migrations (--only-files mode)");
    }

    let legacy_base = env::var("LEGACY_FILESTORE_BASE")
        .unwrap_or_else(|_| "../eckwmsr".into());
    let filestore = FileStore::new(".");
    migrate_files(&pg, &sdb, &filestore, &legacy_base).await?;
    migrate_attachments(&pg, &sdb).await?;

    info!("Migration complete!");
    Ok(())
}

// ─── Locations (stock_location) ─────────────────────────────────────────────

async fn migrate_locations(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating stock_location...");
    let rows: Vec<PgLocation> = sqlx::query_as(
        "SELECT id, name, complete_name, barcode, usage, location_id, active
         FROM stock_location ORDER BY complete_name",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} locations", rows.len());
    for r in &rows {
        let _: Option<serde_json::Value> = sdb
            .upsert(("location", r.id.to_string().as_str()))
            .content(json!({
                "name": r.name,
                "complete_name": r.complete_name,
                "barcode": r.barcode,
                "usage": r.usage,
                "parent_id": r.location_id.map(|id| id.to_string()),
                "active": r.active.unwrap_or(true),
            }))
            .await?;
    }
    info!("  Locations migrated.");
    Ok(())
}

// ─── Racks (warehouse_racks) + graph edges ──────────────────────────────────

async fn migrate_racks(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating warehouse_racks...");
    let rows: Vec<PgRack> = sqlx::query_as(
        "SELECT id, name, prefix, columns, rows, start_index, sort_order, warehouse_id
         FROM warehouse_racks WHERE deleted_at IS NULL ORDER BY sort_order",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} racks", rows.len());
    for r in &rows {
        let _: Option<serde_json::Value> = sdb
            .upsert(("rack", r.id.to_string().as_str()))
            .content(json!({
                "name": r.name,
                "prefix": r.prefix,
                "columns": r.columns.unwrap_or(1),
                "rows": r.rows.unwrap_or(1),
                "start_index": r.start_index.unwrap_or(0),
                "sort_order": r.sort_order.unwrap_or(0),
                "warehouse_id": r.warehouse_id.map(|id| id.to_string()),
            }))
            .await?;

        // Graph edge: location → contains → rack
        if let Some(wid) = r.warehouse_id {
            sdb.query(
                "RELATE (type::record('location', $wid)) -> contains -> (type::record('rack', $rid))",
            )
            .bind(("wid", wid.to_string()))
            .bind(("rid", r.id.to_string()))
            .await?;
        }
    }
    info!("  Racks migrated with graph edges.");
    Ok(())
}

// ─── Orders (orders) + dummy embeddings ─────────────────────────────────────

async fn migrate_orders(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating orders...");
    let rows: Vec<PgOrder> = sqlx::query_as(
        "SELECT id, order_number, order_type, customer_name, customer_email, customer_phone,
                product_sku, product_name, serial_number,
                issue_description, diagnosis_notes, repair_notes, resolution, notes,
                status, priority, parts_used, metadata,
                labor_hours, total_cost, rma_reason, is_refund_requested,
                started_at, completed_at, created_at, updated_at
         FROM orders WHERE deleted_at IS NULL ORDER BY created_at",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} orders", rows.len());
    for r in &rows {
        let combined_text = format!(
            "Issue: {} Resolution: {}",
            r.issue_description.as_deref().unwrap_or(""),
            r.resolution.as_deref().unwrap_or("")
        );
        let embedding = homebrew_embedding(&combined_text);

        let _: Option<serde_json::Value> = sdb
            .upsert(("order", r.id.to_string().as_str()))
            .content(json!({
                "order_number": r.order_number,
                "order_type": r.order_type,
                "customer_name": r.customer_name,
                "customer_email": r.customer_email,
                "customer_phone": r.customer_phone,
                "product_sku": r.product_sku,
                "product_name": r.product_name,
                "serial_number": r.serial_number,
                "issue_description": r.issue_description,
                "diagnosis_notes": r.diagnosis_notes,
                "repair_notes": r.repair_notes,
                "resolution": r.resolution,
                "notes": r.notes,
                "status": r.status,
                "priority": r.priority,
                "parts_used": r.parts_used,
                "metadata": r.metadata,
                "labor_hours": r.labor_hours,
                "total_cost": r.total_cost,
                "rma_reason": r.rma_reason,
                "is_refund_requested": r.is_refund_requested.unwrap_or(false),
                "embedding": embedding,
                "embedding_status": "pending",
                "started_at": r.started_at.map(|t| t.to_rfc3339()),
                "completed_at": r.completed_at.map(|t| t.to_rfc3339()),
                "created_at": r.created_at.map(|t| t.to_rfc3339()),
                "updated_at": r.updated_at.map(|t| t.to_rfc3339()),
            }))
            .await?;
    }
    info!("  Orders migrated with trigram embeddings.");
    Ok(())
}

// ─── Users (user_auths) ─────────────────────────────────────────────────────

async fn migrate_users(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating user_auths...");
    let rows: Vec<PgUser> = sqlx::query_as(
        "SELECT id, username, email, name, role, pin, is_active, preferred_language,
                created_at, updated_at
         FROM user_auths WHERE deleted_at IS NULL",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} users", rows.len());
    for r in &rows {
        let _: Option<serde_json::Value> = sdb
            .upsert(("user", r.id.to_string().as_str()))
            .content(json!({
                "username": r.username,
                "email": r.email,
                "name": r.name,
                "role": r.role,
                "pin": r.pin.as_deref().unwrap_or(""),
                "isActive": r.is_active.unwrap_or(true),
                "preferredLanguage": r.preferred_language,
                "password": "",
                "createdAt": r.created_at.map(|t| t.to_rfc3339()),
                "updatedAt": r.updated_at.map(|t| t.to_rfc3339()),
            }))
            .await?;
    }
    info!("  Users migrated (passwords NOT transferred — users must reset).");
    Ok(())
}

// ─── Products (product_product) ─────────────────────────────────────────────

async fn migrate_products(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating product_product...");
    let rows: Vec<PgProduct> = sqlx::query_as(
        r#"SELECT id, default_code, barcode, name, active, "type", list_price, standard_price, weight, volume
         FROM product_product"#,
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} products", rows.len());
    for r in &rows {
        let _: Option<serde_json::Value> = sdb
            .upsert(("product", r.id.to_string().as_str()))
            .content(json!({
                "default_code": r.default_code,
                "barcode": r.barcode,
                "name": r.name,
                "active": r.active.unwrap_or(true),
                "product_type": r.product_type,
                "list_price": r.list_price,
                "standard_price": r.standard_price,
                "weight": r.weight,
                "volume": r.volume,
                "embedding_status": "pending",
            }))
            .await?;
    }
    info!("  Products migrated.");
    Ok(())
}

// ─── Partners (res_partner) ─────────────────────────────────────────────────

async fn migrate_partners(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating res_partner...");
    let rows: Vec<PgPartner> = sqlx::query_as(
        "SELECT id, name, street, zip, city, phone, email, vat, company_type, is_company
         FROM res_partner",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} partners", rows.len());
    for r in &rows {
        let _: Option<serde_json::Value> = sdb
            .upsert(("partner", r.id.to_string().as_str()))
            .content(json!({
                "name": r.name,
                "street": r.street,
                "zip": r.zip,
                "city": r.city,
                "phone": r.phone,
                "email": r.email,
                "vat": r.vat,
                "company_type": r.company_type,
                "is_company": r.is_company.unwrap_or(false),
                "embedding_status": "pending",
            }))
            .await?;
    }
    info!("  Partners migrated.");
    Ok(())
}

// ─── Documents (documents, 2026+ only) ─────────────────────────────────────

#[derive(FromRow)]
struct PgDocument {
    #[sqlx(rename = "document_id")]
    id: Uuid,
    #[sqlx(rename = "type")]
    doc_type: Option<String>,
    status: Option<String>,
    payload: Option<serde_json::Value>,
    device_id: Option<String>,
    user_id: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

async fn migrate_documents(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Migrating documents (2026+)...");
    let rows: Vec<PgDocument> = sqlx::query_as(
        "SELECT document_id, type, status, payload, device_id, user_id, created_at, updated_at
         FROM documents
         WHERE deleted_at IS NULL AND created_at >= '2026-01-01'
         ORDER BY created_at",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} documents (2026+)", rows.len());
    for r in &rows {
        let _: Option<serde_json::Value> = sdb
            .upsert(("document", r.id.to_string().as_str()))
            .content(json!({
                "type": r.doc_type,
                "status": r.status,
                "payload": r.payload,
                "device_id": r.device_id,
                "user_id": r.user_id,
                "embedding_status": "pending",
                "created_at": r.created_at.map(|t| t.to_rfc3339()),
                "updated_at": r.updated_at.map(|t| t.to_rfc3339()),
            }))
            .await?;
    }
    info!("  Documents migrated with embedding_status=pending.");
    Ok(())
}

// ─── File Resources (file_resources) ────────────────────────────────────────

#[derive(FromRow)]
struct PgFileResource {
    id: Uuid,                          // CAS UUID (MurmurHash3)
    hash: Option<String>,              // SHA-256 hex
    file_name: Option<String>,         // original_name
    mime_type: Option<String>,
    size: Option<i64>,                 // size_bytes
    width: Option<i32>,
    height: Option<i32>,
    avatar_data: Option<Vec<u8>>,      // binary thumbnail (≤50KB)
    file_path: Option<String>,         // CAS path: data/filestore/aa/bb/hash.ext
    source_instance: Option<String>,   // device_id
    context: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

async fn migrate_files(
    pg: &sqlx::PgPool,
    sdb: &db::SurrealDb,
    filestore: &FileStore,
    legacy_base: &str,
) -> Result<(), anyhow::Error> {
    info!("Migrating file_resources...");
    let rows: Vec<PgFileResource> = sqlx::query_as(
        "SELECT id, hash, file_name, mime_type, size, width, height,
                avatar_data, file_path, source_instance, context,
                created_at, updated_at
         FROM file_resources WHERE deleted_at IS NULL",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} file resources", rows.len());
    let mut copied = 0u32;
    let mut skipped = 0u32;

    for r in &rows {
        // Try to copy the physical file via CAS FileStore
        let mut storage_path: Option<String> = None;
        let mut avatar_b64: Option<String> = None;

        if let Some(ref legacy_path) = r.file_path {
            let src = std::path::PathBuf::from(legacy_base).join(legacy_path);
            match tokio::fs::read(&src).await {
                Ok(bytes) => {
                    let filename = r.file_name.as_deref().unwrap_or("unknown");
                    match filestore.save(&bytes, filename, None, None).await {
                        Ok(saved) => {
                            storage_path = Some(saved.storage_path);
                            // Avatar: prefer legacy thumbnail, fall back to inline if ≤50KB
                            if let Some(ref av) = r.avatar_data {
                                avatar_b64 = Some(BASE64.encode(av));
                            } else if let Some(ref av) = saved.avatar_data {
                                avatar_b64 = Some(BASE64.encode(av));
                            }
                            copied += 1;
                        }
                        Err(e) => {
                            tracing::warn!("  FileStore save failed for {}: {}", r.id, e);
                        }
                    }
                }
                Err(_) => {
                    skipped += 1;
                    // File missing on disk — still create DB record without storage_path
                    if let Some(ref av) = r.avatar_data {
                        avatar_b64 = Some(BASE64.encode(av));
                    }
                }
            }
        }

        let _: Option<serde_json::Value> = sdb
            .upsert(("file_resource", r.id.to_string().as_str()))
            .content(json!({
                "cas_uuid": r.id.to_string(),
                "hash": r.hash,
                "original_name": r.file_name,
                "mime_type": r.mime_type,
                "size_bytes": r.size,
                "width": r.width.unwrap_or(0),
                "height": r.height.unwrap_or(0),
                "avatar_b64": avatar_b64,
                "storage_path": storage_path,
                "source_instance": r.source_instance,
                "context": r.context,
                "created_at": r.created_at.map(|t| t.to_rfc3339()),
                "updated_at": r.updated_at.map(|t| t.to_rfc3339()),
            }))
            .await?;
    }
    info!("  Files migrated: {} copied, {} missing on disk (metadata only).", copied, skipped);
    Ok(())
}

// ─── Attachments (entity_attachments → has_attachment graph) ────────────────

#[derive(FromRow)]
struct PgAttachment {
    #[allow(dead_code)]
    id: Uuid,
    file_resource_id: Uuid,
    res_model: Option<String>,  // e.g. "order", "document"
    res_id: Option<String>,     // entity UUID
    is_main: Option<bool>,
    tags: Option<String>,
    comment: Option<String>,
    created_at: Option<DateTime<Utc>>,
}

async fn migrate_attachments(
    pg: &sqlx::PgPool,
    sdb: &db::SurrealDb,
) -> Result<(), anyhow::Error> {
    info!("Migrating entity_attachments → has_attachment graph edges...");
    // Clear existing edges to make re-runs idempotent
    sdb.query("DELETE has_attachment").await?;

    let rows: Vec<PgAttachment> = sqlx::query_as(
        "SELECT id, file_resource_id, res_model, res_id, is_main, tags, comment, created_at
         FROM entity_attachments WHERE deleted_at IS NULL",
    )
    .fetch_all(pg)
    .await?;

    info!("  Found {} attachments", rows.len());
    let mut created = 0u32;

    for r in &rows {
        let res_model = r.res_model.clone().unwrap_or_else(|| "unknown".into());
        let res_id = r.res_id.clone().unwrap_or_else(|| "unknown".into());
        let file_id = r.file_resource_id.to_string();
        let label = r.tags.clone().or_else(|| r.comment.clone()).unwrap_or_default();
        let is_main = r.is_main.unwrap_or(false);
        let cat = r.created_at.map(|t| t.to_rfc3339()).unwrap_or_default();

        sdb.query(
            "RELATE (type::record($model, $eid)) -> has_attachment -> (type::record('file_resource', $fid))
                SET label = $label,
                    is_main = $is_main,
                    created_at = $cat"
        )
        .bind(("model", res_model))
        .bind(("eid", res_id))
        .bind(("fid", file_id))
        .bind(("label", label))
        .bind(("is_main", is_main))
        .bind(("cat", cat))
        .await?;

        created += 1;
    }
    info!("  Attachments migrated: {} graph edges created.", created);
    Ok(())
}

// ─── Verify counts ─────────────────────────────────────────────────────────

async fn verify_counts(pg: &sqlx::PgPool, sdb: &db::SurrealDb) -> Result<(), anyhow::Error> {
    info!("Verifying migration counts...\n");

    let checks: &[(&str, &str, &str)] = &[
        ("location",        "SELECT COUNT(*) as count FROM stock_location",
                            "SELECT count() AS c FROM location GROUP ALL"),
        ("rack",            "SELECT COUNT(*) as count FROM warehouse_racks WHERE deleted_at IS NULL",
                            "SELECT count() AS c FROM rack GROUP ALL"),
        ("order",           "SELECT COUNT(*) as count FROM orders WHERE deleted_at IS NULL",
                            "SELECT count() AS c FROM order GROUP ALL"),
        ("user",            "SELECT COUNT(*) as count FROM user_auths WHERE deleted_at IS NULL",
                            "SELECT count() AS c FROM user GROUP ALL"),
        ("product",         "SELECT COUNT(*) as count FROM product_product",
                            "SELECT count() AS c FROM product GROUP ALL"),
        ("partner",         "SELECT COUNT(*) as count FROM res_partner",
                            "SELECT count() AS c FROM partner GROUP ALL"),
        ("document",        "SELECT COUNT(*) as count FROM documents WHERE deleted_at IS NULL AND created_at >= '2026-01-01'",
                            "SELECT count() AS c FROM document GROUP ALL"),
        ("file_resource",   "SELECT COUNT(*) as count FROM file_resources WHERE deleted_at IS NULL",
                            "SELECT count() AS c FROM file_resource GROUP ALL"),
        ("has_attachment",  "SELECT COUNT(*) as count FROM entity_attachments WHERE deleted_at IS NULL",
                            "SELECT count() AS c FROM has_attachment GROUP ALL"),
    ];

    println!("{:<20} {:>10} {:>10} {:>10}", "Entity", "PG", "SDB", "Diff");
    println!("{}", "-".repeat(54));

    for (name, pg_sql, sdb_sql) in checks {
        let pg_count: (i64,) = sqlx::query_as(pg_sql).fetch_one(pg).await?;
        let pg_n = pg_count.0;

        let sdb_n = match sdb.query(*sdb_sql).await {
            Ok(mut res) => {
                let vals: Vec<serde_json::Value> = res.take(0).unwrap_or_default();
                vals.first()
                    .and_then(|v| v.get("c"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
            }
            Err(_) => 0, // table doesn't exist yet
        };

        let diff = sdb_n - pg_n;
        let marker = if diff == 0 { "✓" } else { "✗" };
        println!("{:<20} {:>10} {:>10} {:>9} {}", name, pg_n, sdb_n, diff, marker);
    }

    println!();
    Ok(())
}

// ─── Trigram feature-hashing embedding ──────────────────────────────────────

fn homebrew_embedding(text: &str) -> Vec<f32> {
    let mut vec = vec![0.0_f32; 768];
    let text = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();

    if chars.len() < 3 {
        vec[0] = 1.0;
        return vec;
    }

    for window in chars.windows(3) {
        let trigram: String = window.iter().collect();
        let mut hasher = Hasher::new();
        hasher.update(trigram.as_bytes());
        let hash = hasher.finalize();
        let idx = (hash % 768) as usize;
        vec[idx] += 1.0;
    }

    // L2 normalize to unit length (prevents NaN in cosine similarity)
    let magnitude: f32 = vec.iter().map(|&v| v * v).sum::<f32>().sqrt();
    if magnitude > 0.0 {
        for v in vec.iter_mut() {
            *v /= magnitude;
        }
    } else {
        vec[0] = 1.0;
    }

    vec
}
