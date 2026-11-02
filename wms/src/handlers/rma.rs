use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use eck_core::sync::hedera;
use rand::{distributions::Alphanumeric, Rng};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn db_err(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub r#type: Option<String>,
}

/// GET /api/rma — list orders, optionally filtered by `?type=`
pub async fn list_orders(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    // `rec_leaf` = the record-id leaf, projected so we can batch the icon lookup
    // (and match the `icons_for_orders` keys) without parsing the `id` Thing.
    let mut orders: Vec<Value> = if let Some(ref order_type) = q.r#type {
        state
            .db
            .query("SELECT *, record::id(id) AS rec_leaf FROM order WHERE order_type = $otype ORDER BY created_at DESC")
            .bind(("otype", order_type.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    } else {
        state
            .db
            .query("SELECT *, record::id(id) AS rec_leaf FROM order ORDER BY created_at DESC")
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    };

    // Representative photo per row in ONE batched query (no N+1 — §4).
    let ids: Vec<String> = orders
        .iter()
        .filter_map(|o| o.get("rec_leaf").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    let icons = crate::services::entity::icons_for_orders(&state.db, &ids).await;
    for o in orders.iter_mut() {
        let leaf = o.get("rec_leaf").and_then(|v| v.as_str()).map(|s| s.to_string());
        if let Some(obj) = o.as_object_mut() {
            let icon = leaf.as_deref().and_then(|l| icons.get(l)).cloned();
            obj.insert("icon_file_id".into(), icon.map(Value::String).unwrap_or(Value::Null));
            obj.remove("rec_leaf");
        }
    }

    Ok(Json(orders))
}

/// GET /api/repairs/board — the repairs board: repair orders (identical shape
/// to `GET /api/rma?type=repair`, including `icon_file_id`) PLUS an `awaiting`
/// column of inbound units that have no live order yet ("awaiting triage"). The
/// old list endpoint stays untouched; this one is additive.
pub async fn board(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    // ── orders: same query + batched icons as list_orders(type=repair) ──
    let mut orders: Vec<Value> = state
        .db
        .query("SELECT *, record::id(id) AS rec_leaf FROM order WHERE order_type = $otype ORDER BY created_at DESC")
        .bind(("otype", "repair".to_string()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let ids: Vec<String> = orders
        .iter()
        .filter_map(|o| o.get("rec_leaf").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    let icons = crate::services::entity::icons_for_orders(&state.db, &ids).await;
    // Clear-tier company + city per order, resolved from its support ticket
    // (metadata.ticket_id / zoho_ticket) in ONE batched ticket query. The
    // REQUESTER person is never added; company = ticket company (else null);
    // city = order.metadata.city, else the ticket's city. `company`/`city` are
    // NEW keys — the existing shape is untouched.
    use crate::services::order_enrich as oe;
    let mut order_refs: std::collections::HashMap<String, oe::TicketRef> = std::collections::HashMap::new();
    for o in &orders {
        if let (Some(leaf), Some(meta)) =
            (o.get("rec_leaf").and_then(|v| v.as_str()), o.get("metadata"))
        {
            let r = oe::ticket_ref_from_meta(meta);
            if !r.is_empty() {
                order_refs.insert(leaf.to_string(), r);
            }
        }
    }
    let order_contacts = oe::resolve_tickets(&state.db, &order_refs).await;
    // Receipt fallback (owner, 2026-11-01): company → order.customer_name →
    // receipt.client_name; city → receipt city. ONE batched receipt query.
    let order_receipt_uuids: Vec<String> = orders
        .iter()
        .filter_map(|o| {
            o.get("metadata")
                .and_then(|m| m.get("receipt_uuid"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let order_receipts = oe::resolve_receipts(&state.db, &order_receipt_uuids).await;
    for o in orders.iter_mut() {
        let leaf = o.get("rec_leaf").and_then(|v| v.as_str()).map(|s| s.to_string());
        let order_city = o
            .get("metadata")
            .and_then(|m| m.get("city"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let order_customer = o.get("customer_name").and_then(|v| v.as_str()).map(|s| s.to_string());
        let receipt_uuid = o
            .get("metadata")
            .and_then(|m| m.get("receipt_uuid"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let contact = leaf.as_deref().and_then(|l| order_contacts.get(l));
        let receipt_facts = receipt_uuid.as_deref().and_then(|r| order_receipts.get(r));
        let (company, city) = oe::company_city(
            contact,
            order_customer.as_deref(),
            order_city.as_deref(),
            receipt_facts,
        );
        if let Some(obj) = o.as_object_mut() {
            let icon = leaf.as_deref().and_then(|l| icons.get(l)).cloned();
            obj.insert("icon_file_id".into(), icon.map(Value::String).unwrap_or(Value::Null));
            obj.insert("company".into(), company.map(Value::String).unwrap_or(Value::Null));
            obj.insert("city".into(), city.map(Value::String).unwrap_or(Value::Null));
            obj.remove("rec_leaf");
        }
    }

    // ── awaiting: inbound items with no live order (UUID link or serial) ──
    let mut awaiting = crate::services::receiving::awaiting_units(&state.db, 200)
        .await
        .map_err(db_err)?;
    let unit_ids: Vec<String> = awaiting
        .iter()
        .filter_map(|u| u.get("unit_id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    let unit_icons = crate::services::entity::icons_for_units(&state.db, &unit_ids).await;
    // Awaiting cards gain the same clear-tier company + city, resolved from
    // THEIR receipt's ticket (receipt.ticket_id / zoho_ticket), falling back to
    // the receipt's own client_name / city. One batched receipt query + one
    // batched ticket query.
    let receipt_ids: Vec<String> = awaiting
        .iter()
        .filter_map(|u| u.get("receipt_id").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let receipt_rows: Vec<Value> = if receipt_ids.is_empty() {
        Vec::new()
    } else {
        state
            .db
            .query("SELECT record::id(id) AS id, ticket_id, zoho_ticket FROM receipt WHERE record::id(id) IN $ids")
            .bind(("ids", receipt_ids))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    };
    let mut receipt_refs: std::collections::HashMap<String, oe::TicketRef> = std::collections::HashMap::new();
    for row in &receipt_rows {
        if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
            let r = oe::ticket_ref_from_meta(row);
            if !r.is_empty() {
                receipt_refs.insert(id.to_string(), r);
            }
        }
    }
    let receipt_contacts = oe::resolve_tickets(&state.db, &receipt_refs).await;

    for u in awaiting.iter_mut() {
        let uid = u.get("unit_id").and_then(|v| v.as_str()).map(|s| s.to_string());
        let receipt_id = u.get("receipt_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let contact = receipt_contacts.get(&receipt_id);
        let client_name = u
            .get("client_name")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let existing_city = u
            .get("city")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let company = contact.and_then(|c| c.company.clone()).or(client_name);
        let city = existing_city.or_else(|| contact.and_then(|c| c.city.clone()));
        if let Some(obj) = u.as_object_mut() {
            let icon = uid.as_deref().and_then(|l| unit_icons.get(l)).cloned();
            obj.insert("icon_file_id".into(), icon.map(Value::String).unwrap_or(Value::Null));
            obj.insert("company".into(), company.map(Value::String).unwrap_or(Value::Null));
            obj.insert("city".into(), city.map(Value::String).unwrap_or(Value::Null));
        }
    }

    Ok(Json(json!({ "orders": orders, "awaiting": awaiting })))
}

/// GET /api/rma/:id — get a single order
///
/// Cache-mode aware: on a local miss, the cache pulls the row from a full
/// peer (transparent pull-through). Touch the cache row on every hit so the
/// LRU evictor sees activity. See [`crate::AppState::node_role`] for the
/// per-node role flag.
pub async fn get_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let order: Option<Value> = state
        .db
        .select(("order", &*id))
        .await
        .map_err(db_err)?;

    if let Some(mut v) = order {
        // Cache hit (or full-peer hit) — bump LRU access bit. Cheap UPSERT
        // on cache nodes; no-op on full peers since `is_cache=true` filter
        // never matches.
        if state.node_role == "cache" {
            state.sync_engine.touch_cache("order", &id).await;
        }
        // Representative photo (§4): full candidate set (order + units + receipt).
        let icon = crate::services::entity::icon_file_id(&state.db, "order", &id).await;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("icon_file_id".into(), icon.map(Value::String).unwrap_or(Value::Null));
        }
        return Ok(Json(v));
    }

    // Local miss. On a cache node, try pulling from a full peer.
    if state.node_role == "cache" {
        if let Some(v) = state.sync_engine.pull_entity_on_demand("order", &id).await {
            return Ok(Json(v));
        }
    }

    Err((StatusCode::NOT_FOUND, format!("Order '{id}' not found")))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CreateOrderRequest {
    pub order_type: String,
    pub order_number: String,
    pub customer_name: String,
    pub customer_email: String,
    pub customer_phone: String,
    pub item_id: Option<String>,
    pub product_sku: String,
    pub product_name: String,
    pub serial_number: String,
    pub purchase_date: Option<String>,
    pub issue_description: String,
    pub diagnosis_notes: String,
    pub assigned_to: Option<String>,
    pub status: String,
    pub priority: String,
    pub repair_notes: String,
    pub parts_used: Option<Value>,
    pub labor_hours: f64,
    pub total_cost: f64,
    pub resolution: String,
    pub notes: String,
    pub metadata: Option<Value>,
    pub rma_reason: String,
    pub is_refund_requested: bool,
}

/// POST /api/rma — create a new order with auto-generated order_number
pub async fn create_order(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateOrderRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let order_type = if payload.order_type.is_empty() {
        "rma".to_string()
    } else {
        payload.order_type
    };

    let order_number = if payload.order_number.is_empty() {
        let prefix = match order_type.as_str() {
            "rma" => "RMA",
            "repair" => "REP",
            _ => "ORD",
        };
        format!(
            "{}-{}-{:04}",
            prefix,
            Utc::now().format("%Y%m%d"),
            rand::random::<u16>() % 10000
        )
    } else {
        payload.order_number
    };

    let status = if payload.status.is_empty() {
        "pending"
    } else {
        &payload.status
    };
    let priority = if payload.priority.is_empty() {
        "normal"
    } else {
        &payload.priority
    };

    let now = Utc::now().to_rfc3339();
    let doc = json!({
        "uuid": Uuid::new_v4().to_string(),
        "order_number": order_number,
        "order_type": order_type,
        "customer_name": payload.customer_name,
        "customer_email": payload.customer_email,
        "customer_phone": payload.customer_phone,
        "item_id": payload.item_id,
        "product_sku": payload.product_sku,
        "product_name": payload.product_name,
        "serial_number": payload.serial_number,
        "purchase_date": payload.purchase_date,
        "issue_description": payload.issue_description,
        "diagnosis_notes": payload.diagnosis_notes,
        "assigned_to": payload.assigned_to,
        "status": status,
        "priority": priority,
        "repair_notes": payload.repair_notes,
        "parts_used": payload.parts_used.unwrap_or(json!([])),
        "labor_hours": payload.labor_hours,
        "total_cost": payload.total_cost,
        "resolution": payload.resolution,
        "notes": payload.notes,
        "metadata": payload.metadata.unwrap_or(json!({})),
        "rma_reason": payload.rma_reason,
        "is_refund_requested": payload.is_refund_requested,
        "created_at": now,
        "updated_at": now,
        "status_changed_at": now,
    });

    let created: Option<Value> = state
        .db
        .create("order")
        .content(doc)
        .await
        .map_err(db_err)?;

    match created {
        Some(v) => Ok((StatusCode::CREATED, Json(v))),
        None => Err((StatusCode::INTERNAL_SERVER_ERROR, "Create returned no record".into())),
    }
}

/// PUT /api/rma/:id — update an existing order
pub async fn update_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    // `order` is a synced table — route through put_synced_entity so the edit
    // advances `_vclock` (a raw .content() write left the clock null → peers
    // dropped the edit as local-wins/equal; same class as the importer churn).
    // Existence check keeps the update-only 404 (put_synced_entity upserts).
    let existing = state
        .get_synced_entity("order", &id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let Some(existing) = existing else {
        return Err((StatusCode::NOT_FOUND, format!("Order '{id}' not found")));
    };
    let old_status = existing
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let new_status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(ns) = &new_status {
        if old_status.as_deref() != Some(ns.as_str()) {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "status_changed_at".to_string(),
                    json!(Utc::now().to_rfc3339()),
                );
            }
        }
    }

    let updated = state
        .put_synced_entity("order", &id, payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Domain-event: order status change (fire-and-forget).
    if let Some(ns) = new_status {
        if old_status.as_deref() != Some(ns.as_str()) {
            crate::services::events::record(
                &state.db,
                &state.instance_id,
                crate::services::events::Event::new(
                    "status",
                    "order",
                    &id,
                    match &old_status {
                        Some(os) => format!("Status {os} → {ns}"),
                        None => format!("Status set to {ns}"),
                    },
                ),
            )
            .await;
        }
    }

    Ok(Json(updated))
}

/// DELETE /api/rma/:id — delete an order
pub async fn delete_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let deleted: Option<Value> = state
        .db
        .delete(("order", &*id))
        .await
        .map_err(db_err)?;

    match deleted {
        Some(_) => Ok(Json(json!({"message": "Order deleted successfully"}))),
        None => Err((StatusCode::NOT_FOUND, format!("Order '{id}' not found"))),
    }
}

// ─── Vector search ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
}

/// POST /api/rma/search — hybrid BM25 + vector search over repair orders
pub async fn search_orders(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SearchRequest>,
) -> ApiResult<Json<Vec<Value>>> {
    if payload.query.trim().is_empty() {
        return Ok(Json(vec![]));
    }

    // Get real embedding for the query via Gemini (embed_query self-resolves auth).
    let q_vector = match crate::ai::embeddings::embed_query(&payload.query).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Embedding query failed ({}), falling back to BM25-only", e);
            vec![]
        }
    };

    // SurrealDB v3 BM25: @N@ operator doesn't support bind variables,
    // and multi-word queries use AND semantics. We tokenize the query
    // into individual terms, each becoming a separate ranked list for RRF.
    let terms: Vec<String> = payload.query
        .split_whitespace()
        .filter(|t| t.len() > 2)
        .map(|t| t.replace('\'', "''").replace('\\', "\\\\"))
        .collect();

    let results: Vec<Value> = if q_vector.is_empty() {
        // BM25-only fallback: OR all term/field combinations with @@
        let bm25_where = if terms.is_empty() {
            let safe_q = payload.query.replace('\'', "''").replace('\\', "\\\\");
            format!("issue_description @@ '{safe_q}' OR order_number @@ '{safe_q}' OR customer_name @@ '{safe_q}'")
        } else {
            terms.iter()
                .flat_map(|term| [
                    format!("issue_description @@ '{term}'"),
                    format!("customer_name @@ '{term}'"),
                    format!("order_number @@ '{term}'"),
                ])
                .collect::<Vec<_>>()
                .join(" OR ")
        };
        state
            .db
            .query(&format!(
                "SELECT * FROM order WHERE {bm25_where} LIMIT 10"
            ))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    } else {
        // Hybrid search: per-term BM25 + Vector via native SurrealDB RRF
        let mut let_stmts = Vec::new();
        let mut rrf_vars = vec!["$vec_results".to_string()];

        let_stmts.push(
            "LET $vec_results = SELECT id, vector::distance::knn() AS distance FROM order WHERE embedding <|10,100|> $query_emb".to_string()
        );

        for (i, term) in terms.iter().enumerate() {
            let r1 = i * 3 + 1;
            let r2 = i * 3 + 2;
            let r3 = i * 3 + 3;
            let vi = format!("$bm25_i{i}");
            let vn = format!("$bm25_n{i}");
            let vo = format!("$bm25_o{i}");
            let_stmts.push(format!(
                "LET {vi} = SELECT id, search::score({r1}) AS s FROM order WHERE issue_description @{r1}@ '{term}' ORDER BY s DESC"
            ));
            let_stmts.push(format!(
                "LET {vn} = SELECT id, search::score({r2}) AS s FROM order WHERE customer_name @{r2}@ '{term}' ORDER BY s DESC"
            ));
            let_stmts.push(format!(
                "LET {vo} = SELECT id, search::score({r3}) AS s FROM order WHERE order_number @{r3}@ '{term}' ORDER BY s DESC"
            ));
            rrf_vars.push(vi);
            rrf_vars.push(vn);
            rrf_vars.push(vo);
        }

        let rrf_array = rrf_vars.join(", ");
        let total_stmts = let_stmts.len() + 1 + 1; // + RRF LET + final SELECT
        let final_idx = total_stmts - 1;

        let sql = format!(
            "{stmts};\
             LET $hybrid = search::rrf([{rrf_array}], 10, 60);\
             SELECT * FROM $hybrid.id;",
            stmts = let_stmts.join(";\n")
        );

        let mut response = state
            .db
            .query(&sql)
            .bind(("query_emb", q_vector))
            .await
            .map_err(db_err)?;

        response.take(final_idx).map_err(db_err)?
    };

    // Strip embedding from response to reduce payload size
    let results: Vec<Value> = results
        .into_iter()
        .map(|mut val| {
            if let Some(obj) = val.as_object_mut() {
                obj.remove("embedding");
            }
            val
        })
        .collect();

    Ok(Json(results))
}

// ─── Clickwrap Agreement (repairs) ─────────────────────────────────

/// POST /api/rma/:id/generate-link
pub async fn generate_agreement_link(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let token: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let updated: Option<Value> = state
        .db
        .query("UPDATE order SET agreement_token = $token, agreement_status = 'sent' WHERE record::id(id) = $id")
        .bind(("id", id.clone()))
        .bind(("token", token.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    match updated {
        Some(_) => {
            let base_url = std::env::var("BASE_URL")
                .unwrap_or_else(|_| format!("http://localhost:{}", state.port));
            Ok(Json(json!({
                "success": true,
                "token": token,
                "url": format!("{}/E/sign/{}", base_url, token)
            })))
        }
        None => Err((StatusCode::NOT_FOUND, "Order not found".into())),
    }
}

/// GET /api/public/agreement/:token
pub async fn get_agreement_by_token(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> ApiResult<Json<Value>> {
    let order: Option<Value> = state
        .db
        .query("SELECT record::id(id) AS id, order_number, product_name, serial_number, customer_name, agreement_status FROM order WHERE agreement_token = $token LIMIT 1")
        .bind(("token", token))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    match order {
        Some(o) => Ok(Json(o)),
        None => Err((StatusCode::NOT_FOUND, "Invalid or expired link".into())),
    }
}

#[derive(Deserialize)]
pub struct SignAgreementRequest {
    pub agreed_to_agb: bool,
    pub agreed_to_avv: bool,
}

/// POST /api/public/agreement/:token/sign
pub async fn sign_agreement(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<SignAgreementRequest>,
) -> ApiResult<Json<Value>> {
    if !payload.agreed_to_agb || !payload.agreed_to_avv {
        return Err((
            StatusCode::BAD_REQUEST,
            "Both agreements must be accepted".into(),
        ));
    }

    let order: Option<Value> = state
        .db
        .query("SELECT record::id(id) AS id FROM order WHERE agreement_token = $token LIMIT 1")
        .bind(("token", token.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    let order = order.ok_or((StatusCode::NOT_FOUND, "Invalid or expired link".into()))?;
    let order_id = order
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let ip = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    let user_agent = headers
        .get("User-Agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    let timestamp = Utc::now().to_rfc3339();

    let audit_log = json!({
        "action": "clickwrap_signed",
        "order_id": order_id,
        "ip_address": ip,
        "user_agent": user_agent,
        "timestamp": timestamp,
        "agreed_to_agb": true,
        "agreed_to_avv": true,
    });

    let content_hash = hex::encode(Sha256::digest(
        serde_json::to_string(&audit_log).unwrap().as_bytes(),
    ));
    let hcs_receipt =
        hedera::submit_hash_if_configured(state.hedera.as_ref(), &content_hash).await;

    let final_log = json!({
        "audit_log": audit_log,
        "content_hash": content_hash,
        "hedera_sequence": hcs_receipt.as_ref().map(|r| r.sequence_number),
        "hedera_timestamp": hcs_receipt.as_ref().map(|r| &r.consensus_timestamp),
    });

    let _: Option<Value> = state
        .db
        .query("UPDATE order SET agreement_status = 'signed', agreement_token = NONE, agreement_log = $log WHERE record::id(id) = $id")
        .bind(("id", order_id))
        .bind(("log", final_log))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;

    Ok(Json(
        json!({"success": true, "message": "Contract legally bound"}),
    ))
}

