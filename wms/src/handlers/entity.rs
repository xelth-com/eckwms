//! `GET /api/entity/:kind/:id` — the EntityView read model behind the object
//! page and the scan tray (`.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md` §2).
//!
//! Auth: mounted in the JWT-protected router group, so any logged-in role —
//! `observer` included — may read. Unknown kind → 400; missing record → 404,
//! both as JSON (`feedback_axum_routing`: Axum 0.7 `:id`, JSON error bodies).

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use eck_core::auth::Claims;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

type JsonErr = (StatusCode, Json<Value>);

/// GET /api/entity/:kind/:id
pub async fn get_entity(
    Extension(claims): Extension<Claims>,
    State(state): State<Arc<AppState>>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<Value>, JsonErr> {
    if crate::services::entity::table_for(&kind).is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown entity kind '{kind}'") })),
        ));
    }

    match crate::services::entity::load(&state.db, &kind, &id).await {
        Ok(Some(mut view)) => {
            // The read-only `observer` tier is the kiosk screen in the warehouse:
            // it may see the business facts of a partner, never the contact PII.
            if claims.role == "observer" && kind == "partner" {
                for k in ["email", "phone", "mobile", "street", "zip", "vat"] {
                    view.fields.remove(k);
                }
            }
            Ok(Json(serde_json::to_value(view).unwrap_or(Value::Null)))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("{kind} '{id}' not found") })),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}
