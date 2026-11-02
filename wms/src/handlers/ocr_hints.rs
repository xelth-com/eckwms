//! `POST /api/ai/ocr-hints` — the PDA's "OCR review" (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md`
//! §10): the worker sees the camera still with word boxes and taps the words
//! that form a code; the PDA asks which text parts matter. Mounted in the
//! same device-JWT group as `/scan` and `/receiving/*` — `require_active_device`
//! is enforced here exactly like `handlers::capture` does.
//!
//! Always 200 (never 404 — "no fields found" is just an empty array). v1 is
//! heuristic-only (`services::ocr_hints`): no LLM call, no network egress, and
//! nothing but the request's LINE COUNT is logged, because a label line can
//! carry a customer's shipping address.

use axum::{extract::State, http::StatusCode, Extension, Json};
use eck_core::auth::Claims;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::services::ocr_hints::{extract_fields, Field, OcrLine};
use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, String)>;

const MAX_LINES: usize = 200;
const MAX_LINE_LEN: usize = 300;

#[derive(Debug, Deserialize)]
pub struct OcrHintsRequest {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub lines: Vec<OcrLine>,
    /// Accepted and IGNORED in v1 — no image fetch, no LLM call.
    #[serde(default)]
    #[allow(dead_code)]
    pub image_file_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OcrHintsResponse {
    pub fields: Vec<Field>,
    pub source: &'static str,
}

/// `POST /api/ai/ocr-hints`
pub async fn ocr_hints(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<OcrHintsRequest>,
) -> ApiResult<Json<OcrHintsResponse>> {
    let device_id = req.device_id.clone().unwrap_or_else(|| claims.sub.clone());
    crate::handlers::pda::require_active_device(&state, &device_id).await?;
    build_response(&req)
}

/// The validate-and-extract core, free of `AppState`/HTTP auth so the 400
/// paths and the heuristics are testable without a DB.
pub(crate) fn build_response(req: &OcrHintsRequest) -> ApiResult<Json<OcrHintsResponse>> {
    if req.lines.len() > MAX_LINES {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("too many lines ({} > {})", req.lines.len(), MAX_LINES),
        ));
    }
    if let Some(bad) = req.lines.iter().find(|l| l.text.len() > MAX_LINE_LEN) {
        let _ = bad; // never log the offending text — it may carry PII.
        return Err((
            StatusCode::BAD_REQUEST,
            format!("a line exceeds {MAX_LINE_LEN} chars"),
        ));
    }

    let context = req.context.as_deref().unwrap_or("warehouse");
    // Counts only — the line text itself (possibly a customer address) must
    // never hit a log line.
    tracing::debug!(
        "[ocr-hints] context={} lines={} device={:?}",
        context,
        req.lines.len(),
        req.device_id
    );

    let fields = extract_fields(context, &req.lines);
    Ok(Json(OcrHintsResponse { fields, source: "heuristic" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_lines(lines: Vec<OcrLine>) -> OcrHintsRequest {
        OcrHintsRequest { context: Some("warehouse".into()), lines, image_file_id: None, device_id: None }
    }

    #[test]
    fn empty_lines_is_200_with_empty_fields() {
        let out = build_response(&req_with_lines(Vec::new())).unwrap();
        assert!(out.0.fields.is_empty());
        assert_eq!(out.0.source, "heuristic");
    }

    #[test]
    fn too_many_lines_is_400() {
        let lines: Vec<OcrLine> = (0..=MAX_LINES)
            .map(|_| OcrLine { text: "x".into(), box_: [0.0, 0.0, 0.1, 0.1] })
            .collect();
        let err = build_response(&req_with_lines(lines)).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn oversize_line_text_is_400() {
        let lines = vec![OcrLine { text: "x".repeat(MAX_LINE_LEN + 1), box_: [0.0, 0.0, 0.1, 0.1] }];
        let err = build_response(&req_with_lines(lines)).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn image_file_id_is_accepted_and_ignored() {
        let mut req = req_with_lines(vec![OcrLine { text: "S/N: PS123456789".into(), box_: [0.0, 0.0, 1.0, 0.1] }]);
        req.image_file_id = Some("f1".into());
        let out = build_response(&req).unwrap();
        assert!(out.0.fields.iter().any(|f| f.kind == "serial"));
    }
}
