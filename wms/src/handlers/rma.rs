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
    let orders: Vec<Value> = if let Some(ref order_type) = q.r#type {
        state
            .db
            .query("SELECT * FROM order WHERE order_type = $otype ORDER BY created_at DESC")
            .bind(("otype", order_type.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    } else {
        state
            .db
            .query("SELECT * FROM order ORDER BY created_at DESC")
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    };

    Ok(Json(orders))
}

/// GET /api/rma/:id — get a single order
pub async fn get_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let order: Option<Value> = state
        .db
        .select(("order", &*id))
        .await
        .map_err(db_err)?;

    match order {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Order '{id}' not found"))),
    }
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
    Json(payload): Json<Value>,
) -> ApiResult<Json<Value>> {
    let updated: Option<Value> = state
        .db
        .update(("order", &*id))
        .content(payload)
        .await
        .map_err(db_err)?;

    match updated {
        Some(v) => Ok(Json(v)),
        None => Err((StatusCode::NOT_FOUND, format!("Order '{id}' not found"))),
    }
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

    let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();

    // Get real embedding for the query via Gemini
    let q_vector = match crate::ai::embeddings::embed_query(&api_key, &payload.query).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Embedding query failed ({}), falling back to BM25-only", e);
            vec![]
        }
    };

    let results: Vec<Value> = if q_vector.is_empty() {
        // BM25-only fallback when embedding is unavailable
        state
            .db
            .query(
                "SELECT *,
                    search::score(1) AS score
                 FROM order
                 WHERE issue_description @1@ $q_text
                    OR order_number @1@ $q_text
                    OR customer_name @1@ $q_text
                 ORDER BY score DESC LIMIT 10",
            )
            .bind(("q_text", payload.query.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?
    } else {
        // Hybrid: separate BM25 + cosine queries, merged with RRF in Rust.
        // SurrealDB 3.x doesn't support KNN mixed with BM25 in one WHERE,
        // and search::rrf() is not available in v3.0.4.
        let mut response = state
            .db
            .query(
                "SELECT *, search::score(1) AS _bm25
                 FROM order
                 WHERE issue_description @1@ $q_text
                    OR order_number @1@ $q_text
                    OR customer_name @1@ $q_text
                 ORDER BY _bm25 DESC LIMIT 10;

                 SELECT *, vector::similarity::cosine(embedding, $q_vector) AS _vec
                 FROM order
                 WHERE embedding IS NOT NONE
                 ORDER BY _vec DESC LIMIT 10;",
            )
            .bind(("q_text", payload.query.clone()))
            .bind(("q_vector", q_vector))
            .await
            .map_err(db_err)?;

        let bm25_hits: Vec<Value> = response.take(0).map_err(db_err)?;
        let vec_hits: Vec<Value> = response.take(1).map_err(db_err)?;

        // Reciprocal Rank Fusion: score = sum(1 / (k + rank)) across both lists
        const K: f64 = 60.0;
        let mut scores: std::collections::HashMap<String, (Value, f64)> =
            std::collections::HashMap::new();

        for (rank, mut val) in bm25_hits.into_iter().enumerate() {
            let id = val.as_object().and_then(|o| o.get("id"))
                .map(|v| v.to_string()).unwrap_or_default();
            if let Some(obj) = val.as_object_mut() { obj.remove("_bm25"); }
            let entry = scores.entry(id).or_insert((val, 0.0));
            entry.1 += 1.0 / (K + rank as f64 + 1.0);
        }

        for (rank, mut val) in vec_hits.into_iter().enumerate() {
            let id = val.as_object().and_then(|o| o.get("id"))
                .map(|v| v.to_string()).unwrap_or_default();
            if let Some(obj) = val.as_object_mut() { obj.remove("_vec"); }
            let entry = scores.entry(id).or_insert((val, 0.0));
            entry.1 += 1.0 / (K + rank as f64 + 1.0);
        }

        let mut merged: Vec<(Value, f64)> = scores.into_values().collect();
        merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        merged.truncate(10);

        merged.into_iter().map(|(mut val, score)| {
            if let Some(obj) = val.as_object_mut() {
                obj.insert("score".to_string(), serde_json::Value::from(score));
            }
            val
        }).collect()
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

// ─── Clickwrap Agreement (InBody / Repairs) ─────────────────────────────────

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

