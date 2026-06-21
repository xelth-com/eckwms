use axum::{extract::State, http::StatusCode, Json};

use eck_core::models::relay::{RegisterRequest, RegisterResponse};
use crate::db::RelayDb;

pub async fn register(
    State(db): State<RelayDb>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, StatusCode> {
    let status = req.status.unwrap_or_else(|| "online".to_string());

    let instance_id = req.instance_id.clone();
    let mesh_id = req.mesh_id.clone();
    let external_ip = req.external_ip.clone();
    let port = req.port;
    let status_clone = status.clone();
    let base_url = req.base_url.clone().unwrap_or_default();
    let lan_url = req.lan_url.clone().unwrap_or_default();
    let node_role = req.node_role.clone().unwrap_or_else(|| "full".to_string());

    // 9eck product license: verify offline against the issuer pubkey (if this
    // relay is configured to recognize licenses) and tag the registration. The
    // payload-relay gate (mesh_relay::dispatch) reads `paid` from here.
    let (paid, tier, license_exp) = evaluate_license(req.license.as_deref(), &mesh_id);

    // IMPORTANT: SurrealDB embedded mode doesn't share transaction state across
    // multi-statement queries. Each .query() call must be separate.

    // Step 1: Delete old registration
    db.query("DELETE FROM registration WHERE instance_id = $iid")
        .bind(("iid", instance_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Register DELETE failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Step 2: Insert new registration (separate query for tx isolation)
    db.query("INSERT INTO registration {
            instance_id: $iid,
            mesh_id: $mid,
            external_ip: $eip,
            port: $pt,
            status: $st,
            base_url: $burl,
            lan_url: $lurl,
            node_role: $role,
            paid: $paid,
            tier: $tier,
            license_exp: $lexp,
            last_seen: time::now()
        }")
    .bind(("iid", instance_id.clone()))
    .bind(("mid", mesh_id.clone()))
    .bind(("eip", external_ip.clone()))
    .bind(("pt", port as i64))
    .bind(("st", status_clone))
    .bind(("burl", base_url))
    .bind(("lurl", lan_url))
    .bind(("role", node_role))
    .bind(("paid", paid))
    .bind(("tier", tier))
    .bind(("lexp", license_exp))
    .await
    .map_err(|e| {
        tracing::error!("Register INSERT failed: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tracing::info!(
        "Heartbeat: {} ({}) at {}:{} [{}]",
        instance_id, mesh_id, external_ip, port, status
    );

    Ok(Json(RegisterResponse {
        ok: true,
        instance_id,
        mesh_id,
        status,
    }))
}

/// Verify a presented 9eck license offline and derive `(paid, tier, exp)` for
/// the registration row. Returns `(false, "free", 0)` when no license is
/// presented, this relay isn't configured with `ECK_LICENSE_PUBKEY`, or the
/// token is invalid / for a different mesh. Grace window from
/// `ECK_LICENSE_GRACE_SECS` (default 7 days).
fn evaluate_license(token: Option<&str>, mesh_id: &str) -> (bool, String, i64) {
    let free = || (false, "free".to_string(), 0i64);

    let token = match token {
        Some(t) if !t.trim().is_empty() => t,
        _ => return free(),
    };
    let pubkey = match std::env::var("ECK_LICENSE_PUBKEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => return free(), // this relay doesn't recognize licenses
    };
    let grace = std::env::var("ECK_LICENSE_GRACE_SECS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(eck_core::licensing::DEFAULT_GRACE_SECS);
    let now = chrono::Utc::now().timestamp();

    match eck_core::licensing::verify(&pubkey, token, now, grace) {
        Ok(c) if c.sub == mesh_id => (c.is_paid(), c.tier, c.exp),
        Ok(c) => {
            tracing::warn!(
                "License sub mismatch: token bound to '{}', heartbeat mesh '{}'",
                c.sub,
                mesh_id
            );
            free()
        }
        Err(e) => {
            tracing::warn!("License verify failed: {e}");
            free()
        }
    }
}
