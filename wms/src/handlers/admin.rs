use std::sync::Arc;

use axum::{extract::{State, Query}, http::StatusCode, Extension, Json};
use eck_core::auth::Claims;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::{services::scheduler, AppState};

#[derive(Deserialize)]
pub struct QueryRequest {
    pub query: String,
}

/// POST /api/admin/query — execute arbitrary SurrealQL for diagnostics.
/// Admin-only: this is a full read/write SQL surface (SELECT … FROM user, DELETE,
/// REMOVE TABLE …). The route is JWT-gated; this is the role check on top.
pub async fn query(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<QueryRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if claims.role != "admin" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "error": "admin only" })),
        ));
    }
    let result: Result<Vec<Value>, _> = state
        .db
        .query(&body.query)
        .await
        .and_then(|mut r| r.take(0));

    match result {
        Ok(rows) => Ok(Json(json!({ "ok": true, "result": rows }))),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )),
    }
}

/// POST /api/admin/force-sync — manually trigger every scraper provider in
/// sequence (OPAL, DHL, Zoho Desk, Exact Online, Excel). Bypasses the cron
/// schedule so the operator can drive a sync on demand and watch the logs.
/// Zoho runs in incremental mode (limit 100) to keep the run bounded —
/// switch to full_sync=true via the scheduler's daily path if a full
/// re-import is needed.
#[derive(Deserialize, Default)]
pub struct ForceSyncParams {
    /// `?full=true` → run a FULL Zoho re-import (all tickets, not just the
    /// recent 100). It can take a long time under Zoho's rate limit, so it is
    /// spawned in the background and this endpoint returns immediately.
    #[serde(default)]
    pub full: bool,
}

pub async fn force_sync(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ForceSyncParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    info!("[admin] force-sync triggered (full={})", params.full);

    // Full Zoho re-import: long-running (rate-limited), so spawn + return now.
    if params.full {
        let db = state.db.clone();
        let iid = state.instance_id.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("client build");
            info!("[admin] force-sync: Zoho Desk FULL re-import (background) started");
            scheduler::sync_zoho(&db, &client, &iid, true).await;
            info!("[admin] force-sync: Zoho Desk FULL re-import done");
        });
        return Ok(Json(json!({
            "ok": true,
            "mode": "zoho_full_background",
            "note": "Full Zoho re-import running detached. Watch wms.log / sync_history / observer.",
        })));
    }

    let started = std::time::Instant::now();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("client build: {}", e) })),
        ))?;

    let db = &state.db;
    let iid = state.instance_id.as_str();

    info!("[admin] force-sync: OPAL");
    scheduler::sync_opal(db, &client, iid).await;
    info!("[admin] force-sync: DHL");
    scheduler::sync_dhl(db, &client, iid).await;
    info!("[admin] force-sync: Zoho Desk (incremental)");
    scheduler::sync_zoho(db, &client, iid, false).await;
    info!("[admin] force-sync: Exact Online");
    scheduler::sync_exact_online(db, &client, iid).await;
    info!("[admin] force-sync: Excel");
    scheduler::sync_excel(db, &client, iid).await;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    info!("[admin] force-sync done in {} ms", elapsed_ms);

    Ok(Json(json!({
        "ok": true,
        "providers": ["opal", "dhl", "zoho_desk", "exact_online", "excel"],
        "elapsed_ms": elapsed_ms,
        "note": "See sync_history table and wms.log for per-provider details.",
    })))
}

/// POST /api/admin/mesh-replay/:entity_type — one-shot backfill: scan every
/// row of the given table on this node and queue a `push` task on the relay
/// for each other online peer. Useful after adding a new entity_type to
/// SYNC_ENTITY_TYPES (e.g. when a peer joins fresh and needs the existing
/// dataset, but cross-NAT direct merkle pull can't reach it).
///
/// Batches into chunks of `BATCH_SIZE` so a single 6k-row table doesn't post
/// a 50 MB envelope to the relay. The mesh_relay_poller on the receiving
/// side applies each batch via the same conflict-aware upsert as direct push.
///
/// Idempotent: running twice just re-asserts the same data; conflict
/// resolution treats matching VectorClock entries as no-ops.
pub async fn mesh_replay(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(entity_type): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    const BATCH_SIZE: usize = 50;

    // Cheap safety: allow-list against a hard-coded set of synced tables to
    // avoid an operator typo (`POST /mesh-replay/user`) dumping Zone 1 PII
    // into the relay queue. Mirrors SYNC_ENTITY_TYPES in core/engine.rs but
    // is intentionally re-declared so a future divergence on either side
    // doesn't silently widen this surface.
    const ALLOWED: &[&str] = &[
        "item", "order", "product", "partner", "file_resource", "location",
        "quant", "picking", "move_line", "rack", "action_proof",
        "delivery_carrier", "delivery_tracking", "device_intake",
        "inventory_discrepancy", "product_alias", "category", "menu_item",
        "ai_sop", "registered_device", "document",
    ];
    if !ALLOWED.contains(&entity_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("entity_type '{}' not in mesh-replay allow-list", entity_type) })),
        ));
    }

    info!("[admin] mesh-replay: scanning table '{}'", entity_type);

    let query = format!(
        "SELECT *, record::id(id) AS id FROM {}",
        entity_type
    );
    let rows: Vec<Value> = match state.db.query(&query).await.and_then(|mut r| r.take(0)) {
        Ok(rows) => rows,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": format!("scan failed: {}", e) })),
            ));
        }
    };
    let total_rows = rows.len();

    // Discover peers via the relay (skips ourselves + offline nodes).
    let peers = match state.sync_engine.relay().get_mesh_status().await {
        Ok(n) => n,
        Err(e) => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false, "error": format!("relay unreachable: {}", e) })),
            ));
        }
    };
    let target_uuids: Vec<String> = peers
        .into_iter()
        .filter(|n| n.instance_id != state.instance_id && n.status == "online")
        .filter(|n| n.node_role.as_deref() != Some("cache"))
        .map(|n| n.instance_id)
        .collect();

    if target_uuids.is_empty() {
        return Ok(Json(json!({
            "ok": true,
            "entity_type": entity_type,
            "rows": total_rows,
            "peers": 0,
            "note": "No online full peers — nothing to dispatch.",
        })));
    }

    let relay = state.sync_engine.relay();
    let mut tasks_queued = 0usize;
    let mut errors = 0usize;
    for chunk in rows.chunks(BATCH_SIZE) {
        for target in &target_uuids {
            let payload = json!({
                "entity_type": entity_type,
                "entities": chunk,
                "source_instance": state.instance_id,
            });
            match relay.mesh_dispatch(target, "push", payload).await {
                Ok(_) => tasks_queued += 1,
                Err(e) => {
                    errors += 1;
                    tracing::warn!(
                        "mesh-replay: dispatch to {} failed: {}",
                        target, e
                    );
                }
            }
        }
    }

    info!(
        "[admin] mesh-replay '{}': {} rows -> {} peers, {} tasks queued ({} errors)",
        entity_type, total_rows, target_uuids.len(), tasks_queued, errors
    );

    Ok(Json(json!({
        "ok": true,
        "entity_type": entity_type,
        "rows": total_rows,
        "peers": target_uuids.len(),
        "batch_size": BATCH_SIZE,
        "tasks_queued": tasks_queued,
        "errors": errors,
    })))
}
