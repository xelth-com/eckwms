//! Pair-code onboarding rendezvous — a typed short code instead of a scanned QR.
//!
//! The code is a short-lived, single-use classified-ad on this discovery board
//! (the relay), NOT a stored QR and NOT a mesh-synced table:
//!   - a master publishes `code -> { uuid, key, mesh, invite_token, relay, paid }`
//!     via [`announce`] (TTL ~10 min);
//!   - a PDA types the code and [`resolve_code`] assembles the normal `ECK$…`
//!     pairing string ON THE FLY from that record and burns it.
//! The PDA then runs the existing relay-forwarded pairing (`/E/m/*`). Same QR
//! format the scan path uses, so the client needs no changes (variant A).
//!
//! Lives on the relay because nginx routes `9eck.com/E/*` here — the public
//! board the client resolves against (`9eck.com` -> `xelth.com`).

use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};

use crate::db::RelayDb;

const DEFAULT_TTL_SECS: i64 = 600; // 10 min
const MIN_TTL_SECS: i64 = 30;
const MAX_TTL_SECS: i64 = 1800; // 30 min cap

// ─── POST /E/pair/announce ─────────────────────────────────────────────────
// A master publishes a rendezvous entry keyed by the code. Open like /E/register:
// a bogus entry is harmless — pairing still needs a valid master-signed invite
// token, so spam can't actually onboard a device.
//
// Body: { code, uuid, key, mesh?, invite_token?, relay?, paid?, ttl_secs? }
//   uuid  — master instance_id
//   key   — master public key (hex, as in the scan-QR KEY field)
//   relay — public relay URL(s) to advertise in the QR (free: a NAT-traversal
//           relay the PDA can reach; paid: may be empty → PDA uses baked-in eckN)
pub async fn announce(
    State(db): State<RelayDb>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let s = |k: &str| body.get(k).and_then(|v| v.as_str()).map(|x| x.to_string());

    let code = match s("code") {
        Some(c) if !c.trim().is_empty() => c.trim().to_uppercase(),
        _ => return Err((StatusCode::BAD_REQUEST, "missing code".into())),
    };
    let uuid = s("uuid").filter(|v| !v.trim().is_empty())
        .ok_or((StatusCode::BAD_REQUEST, "missing uuid".into()))?;
    let key = s("key").filter(|v| !v.trim().is_empty())
        .ok_or((StatusCode::BAD_REQUEST, "missing key".into()))?;
    let mesh = s("mesh").unwrap_or_default();
    let invite_token = s("invite_token").unwrap_or_default();
    let relay = s("relay").unwrap_or_default();
    let paid = body.get("paid").and_then(|v| v.as_bool()).unwrap_or(false);
    let ttl = body
        .get("ttl_secs")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_TTL_SECS)
        .clamp(MIN_TTL_SECS, MAX_TTL_SECS);

    let now = chrono::Utc::now();
    let expires_at = (now + chrono::Duration::seconds(ttl)).to_rfc3339();

    // Re-mint of the same code overwrites (random codes ~never collide, but keep
    // it idempotent): delete any prior row, then create fresh.
    let res = db
        .query(
            "DELETE type::record('pair_rendezvous', $code); \
             CREATE type::record('pair_rendezvous', $code) SET \
                uuid = $uuid, key = $key, mesh = $mesh, \
                invite_token = $invite, relay = $relay, paid = $paid, \
                used = NONE, created_at = $now, expires_at = $exp;",
        )
        .bind(("code", code.clone()))
        .bind(("uuid", uuid))
        .bind(("key", key))
        .bind(("mesh", mesh))
        .bind(("invite", invite_token))
        .bind(("relay", relay))
        .bind(("paid", paid))
        .bind(("now", now.to_rfc3339()))
        .bind(("exp", expires_at.clone()))
        .await;

    if let Err(e) = res {
        tracing::error!("pair announce insert failed: {e}");
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }
    tracing::info!("pair-code announced: code={code} paid={paid} ttl={ttl}s");
    Ok(Json(json!({ "ok": true, "code": code, "expires_at": expires_at })))
}

// ─── POST /E/pair/code ─────────────────────────────────────────────────────
// PDA resolves a typed code to an `ECK$…` pairing string (variant A).
//   200 { "qr": "ECK$…" } — found, fresh, unused → built on the fly + burned
//   404                   — unknown / expired / already used (client: stop)
//   5xx                   — DB error (client: try the next board)
pub async fn resolve_code(
    State(db): State<RelayDb>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let code = match body.get("code").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => c.trim().to_uppercase(),
        _ => return Err(StatusCode::NOT_FOUND),
    };

    let rows: Vec<Value> = db
        .query(
            "SELECT uuid, key, mesh, invite_token, relay, paid, used, \
                    type::string(expires_at) AS expires_at \
             FROM type::record('pair_rendezvous', $code) LIMIT 1",
        )
        .bind(("code", code.clone()))
        .await
        .map_err(|e| {
            tracing::error!("pair resolve query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .map_err(|e| {
            tracing::error!("pair resolve take failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let rec = rows.into_iter().next().ok_or(StatusCode::NOT_FOUND)?;

    // Already burned?
    if rec.get("used").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(StatusCode::NOT_FOUND);
    }
    // Expired? (GC also sweeps these every 60s; this closes the in-between gap.)
    let expired = rec
        .get("expires_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|exp| exp < chrono::Utc::now())
        .unwrap_or(true);
    if expired {
        return Err(StatusCode::NOT_FOUND);
    }

    let qr = build_eck(&rec);

    // Burn on first successful resolve (best-effort; failure to mark used is not
    // fatal — TTL still expires it).
    let _ = db
        .query("UPDATE type::record('pair_rendezvous', $code) SET used = true, used_at = $now")
        .bind(("code", code.clone()))
        .bind(("now", chrono::Utc::now().to_rfc3339()))
        .await;

    tracing::info!("pair-code resolved + burned: code={code}");
    Ok(Json(json!({ "qr": qr })))
}

/// Assemble the `ECK$…` pairing string from a rendezvous record, in the exact
/// format the scan-QR uses (`device.rs::generate_pairing_qr`):
///   free → `ECK$2$UUID$KEY$URLS[$TOKEN]`
///   paid → `ECK$3$UUID$KEY$MESH$URLS[$TOKEN]`
/// UUID/KEY/MESH/URLS are compact uppercase; the invite-JWT token is appended
/// verbatim (case-sensitive base64url).
fn build_eck(rec: &Value) -> String {
    let s = |k: &str| rec.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let uuid = s("uuid").replace('-', "").to_uppercase();
    let key = s("key").to_uppercase();
    let urls = s("relay").to_uppercase();
    let invite = s("invite_token");
    let paid = rec.get("paid").and_then(|v| v.as_bool()).unwrap_or(false);
    let token_suffix = if invite.is_empty() {
        String::new()
    } else {
        format!("${}", invite)
    };
    if paid {
        let mesh = s("mesh").replace('-', "").to_uppercase();
        format!("ECK$3${uuid}${key}${mesh}${urls}{token_suffix}")
    } else {
        format!("ECK$2${uuid}${key}${urls}{token_suffix}")
    }
}
