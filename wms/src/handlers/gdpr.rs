//! GDPR Art. 17 (right to erasure) for AI-derived data.
//!
//! The embedding pipeline replaces customer PII with keyed-SimHash tokens
//! (`eck_core::utils::anonymizer`) and stores those tokens on each embedded row
//! as `pii_fingerprints`. Because the tokens are **deterministic** (same input +
//! same `SYNC_SECRET` → same token), they double as a per-subject index: given a
//! subject's identifiers we recompute their tokens and match rows by
//! `pii_fingerprints CONTAINSANY`, then erase the derived vector.
//!
//! Scope: this erases the **AI-derived** data — the embedding vector and the
//! fingerprint list — which are pseudonymised personal data under GDPR (the
//! controller holds the pepper, so they're re-identifiable; see
//! `PPRL_ARCHITECTURE.md`). Deleting the underlying raw business record (and any
//! tax-relevant copy retained under §147 AO / GoBD per Art. 17(3)(b)) is a
//! separate operator decision and is intentionally NOT done here.
//!
//! Token matching is exact: a typo variant stored as a different string yields a
//! different token. Pass every known spelling/format via the arrays below, or a
//! `raw_samples` blob that is run through the same regex backstop the pipeline
//! uses, to derive format-identical email/phone/IBAN tokens.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use eck_core::utils::anonymizer::{obfuscate_pii, scrub_pii_regex};
use crate::AppState;

/// Tables that carry an `embedding` + `pii_fingerprints` (kept in sync with the
/// embedding worker's table list in `wms/src/ai/embeddings.rs`).
const EMBEDDED_TABLES: &[&str] = &["document", "order", "partner", "product", "picking"];

#[derive(Deserialize, Default)]
pub struct EraseRequest {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub ibans: Vec<String>,
    /// Free-text samples (e.g. a stored ticket body) run through the regex
    /// backstop to derive the exact email/phone/IBAN/card/VAT-Id tokens the
    /// embedding pipeline produced — avoids format-mismatch misses.
    #[serde(default)]
    pub raw_samples: Vec<String>,
    /// When true, report what WOULD be erased without mutating anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// POST /api/admin/gdpr/erase — erase AI-derived vectors for a data subject.
pub async fn erase_subject(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EraseRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 1. Recompute the subject's deterministic tokens from every identifier.
    let mut tokens: Vec<String> = Vec::new();
    let push = |v: &str, ty: &str, out: &mut Vec<String>| {
        let v = v.trim();
        if !v.is_empty() {
            out.push(obfuscate_pii(v, ty));
        }
    };
    for v in &req.names { push(v, "Name", &mut tokens); }
    for v in &req.emails { push(v, "Email", &mut tokens); }
    for v in &req.phones { push(v, "Phone", &mut tokens); }
    for v in &req.addresses { push(v, "Address", &mut tokens); }
    for v in &req.ibans { push(v, "Iban", &mut tokens); }
    for sample in &req.raw_samples {
        let (_scrubbed, fps) = scrub_pii_regex(sample);
        tokens.extend(fps);
    }
    tokens.sort();
    tokens.dedup();

    if tokens.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": "no identifiers supplied — provide names/emails/phones/addresses/ibans/raw_samples"
            })),
        ));
    }

    // 2. Per table: find (dry-run) or erase the derived vectors.
    let mut erased: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut total: usize = 0;

    for table in EMBEDDED_TABLES {
        let sql = if req.dry_run {
            format!(
                "SELECT record::id(id) AS id FROM {table} \
                 WHERE pii_fingerprints CONTAINSANY $tokens"
            )
        } else {
            // Null the vector, drop the fingerprints, and park the row in a
            // terminal 'erased' state so the embedding worker (which only picks
            // up 'pending') will not re-embed it.
            format!(
                "UPDATE {table} SET embedding = NONE, pii_fingerprints = [], \
                     embedding_status = 'erased', embedding_error = NONE, erased_at = time::now() \
                 WHERE pii_fingerprints CONTAINSANY $tokens \
                 RETURN record::id(id) AS id"
            )
        };

        let rows: Vec<Value> = match state
            .db
            .query(&sql)
            .bind(("tokens", tokens.clone()))
            .await
            .and_then(|mut r| r.take(0))
        {
            Ok(v) => v,
            Err(e) => {
                warn!("[gdpr] erase query failed on {table}: {e}");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "ok": false, "table": table, "error": e.to_string() })),
                ));
            }
        };

        let ids: Vec<Value> = rows
            .into_iter()
            .filter_map(|r| r.get("id").cloned())
            .collect();
        total += ids.len();
        erased.insert((*table).to_string(), Value::Array(ids));
    }

    // 3. Record the erasure itself in the tamper-evident audit chain (GoBD
    //    accountability). Only counts go in — never the tokens or raw PII.
    if !req.dry_run && total > 0 {
        let per_table: Value = erased
            .iter()
            .map(|(t, v)| (t.clone(), json!(v.as_array().map(|a| a.len()).unwrap_or(0))))
            .collect::<serde_json::Map<_, _>>()
            .into();
        eck_core::audit::append_soft(
            &state.db,
            &state.server_identity,
            &eck_core::audit::wms_chain(&state.instance_id),
            "gdpr",
            "gdpr-erase",
            eck_core::audit::class::MUTATE,
            &format!("GDPR Art.17 erasure of {total} AI-derived vector(s)"),
            json!({ "token_count": tokens.len(), "total": total, "tables": per_table }),
        )
        .await;
        info!("[gdpr] erased {total} derived vector(s) across {} table(s)", EMBEDDED_TABLES.len());
    }

    Ok(Json(json!({
        "ok": true,
        "dry_run": req.dry_run,
        "token_count": tokens.len(),
        "total": total,
        "erased": Value::Object(erased),
    })))
}
