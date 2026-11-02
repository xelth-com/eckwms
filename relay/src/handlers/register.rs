use axum::{extract::State, http::StatusCode, Json};

use eck_core::models::relay::{RegisterRequest, RegisterResponse};
use eck_core::sync::relay_client::register_canonical;
use eck_core::utils::identity::verify_signature;
use crate::db::RelayDb;

/// How far a registration signature timestamp may drift from the relay clock
/// before it's rejected as stale (replay window bound). Matches the client-MCP
/// node-auth freshness window.
const REGISTER_MAX_AGE_SECS: i64 = 300;

/// Pure: is the node's registration signature fresh (within `max_age` of `now`)
/// and cryptographically valid over `eck-register:{instance_id}:{ts}`?
fn register_sig_valid(
    instance_id: &str,
    pubkey: &str,
    ts: i64,
    sig: &str,
    now: i64,
    max_age: i64,
) -> bool {
    if (now - ts).abs() > max_age {
        return false;
    }
    let canonical = register_canonical(instance_id, ts);
    verify_signature(pubkey, &canonical, sig).unwrap_or(false)
}

/// The rotation policy applied when a node presents a verified pubkey and the
/// registry already holds one for that instance_id. Pure so it's unit-testable.
#[derive(Debug, PartialEq, Eq)]
enum RotationDecision {
    /// No stored key, or the presented key equals the stored one → store/refresh.
    Accept,
    /// A DIFFERENT key arrived and rotation isn't allowed → reject (409).
    RejectConflict,
}

/// Decide whether a newly-verified `incoming` pubkey may be written over the
/// `existing` stored one. A first key (`existing == None`/empty) or an unchanged
/// key is always accepted; a changed key is rejected unless `allow_rotation`.
fn rotation_decision(
    existing: Option<&str>,
    incoming: &str,
    allow_rotation: bool,
) -> RotationDecision {
    match existing {
        Some(prev) if !prev.is_empty() && prev != incoming && !allow_rotation => {
            RotationDecision::RejectConflict
        }
        _ => RotationDecision::Accept,
    }
}

fn allow_key_rotation() -> bool {
    std::env::var("ECK_RELAY_ALLOW_KEY_ROTATION")
        .ok()
        .as_deref()
        .map(str::trim)
        == Some("1")
}

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

    // F3 STRONG BINDING: if the node signed `eck-register:{instance_id}:{ts}`
    // with its server-identity key, verify it and carry the pubkey onto the
    // registry row. The client-MCP channel (client_mcp::check_node_binding)
    // trusts this over trust-on-first-use. Reject a DIFFERENT key for an
    // instance that already has one (possible impersonation) unless
    // ECK_RELAY_ALLOW_KEY_ROTATION=1.
    //
    // Read the currently-bound key (if any) BEFORE we DELETE the row below, so a
    // heartbeat's own delete/insert cycle can't erase the history we
    // rotation-check against, and so an unsigned/invalid heartbeat carries the
    // existing binding forward instead of clearing it.
    let existing_row: Option<serde_json::Value> = db
        .query("SELECT pubkey FROM registration WHERE instance_id = $iid LIMIT 1")
        .bind(("iid", instance_id.clone()))
        .await
        .map_err(|e| {
            tracing::error!("Register pubkey lookup failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .take(0)
        .unwrap_or_default();
    let existing_key: Option<String> = existing_row
        .as_ref()
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // (stored_pubkey, freshly_verified). Default: carry the existing binding
    // forward untouched.
    let (stored_pubkey, verified_now): (Option<String>, bool) =
        match (&req.node_pubkey, req.node_ts, &req.node_sig) {
            (Some(pk), Some(ts), Some(sig)) if !pk.is_empty() && !sig.is_empty() => {
                let now = chrono::Utc::now().timestamp();
                if register_sig_valid(&instance_id, pk, ts, sig, now, REGISTER_MAX_AGE_SECS) {
                    match rotation_decision(existing_key.as_deref(), pk, allow_key_rotation()) {
                        RotationDecision::RejectConflict => {
                            let had = existing_key.as_deref().unwrap_or("");
                            tracing::warn!(
                                "Register REJECTED for instance '{}' (mesh '{}'): a DIFFERENT \
                                 server-identity pubkey arrived (had {}… now {}…) — POSSIBLE \
                                 IMPERSONATION. Set ECK_RELAY_ALLOW_KEY_ROTATION=1 only if this \
                                 node legitimately re-keyed.",
                                instance_id,
                                mesh_id,
                                &had[..had.len().min(12)],
                                &pk[..pk.len().min(12)],
                            );
                            return Err(StatusCode::CONFLICT);
                        }
                        RotationDecision::Accept => (Some(pk.clone()), true),
                    }
                } else {
                    // Bad/stale signature: don't record a new key and don't fail
                    // the heartbeat — carry the existing binding forward; the node
                    // stays discoverable.
                    tracing::warn!(
                        "Register: instance '{}' presented an invalid/stale registration \
                         signature — keeping prior binding, node still registered",
                        instance_id
                    );
                    (existing_key.clone(), false)
                }
            }
            // Unsigned heartbeat (old node): preserve any existing binding.
            _ => (existing_key.clone(), false),
        };

    // Once a server-identity key is bound to an instance_id, only a request
    // that just proved possession of it may rewrite the row. Until 2026-09-30
    // an unsigned request "preserved the binding" but still replaced
    // external_ip/port/base_url/lan_url/mesh_id — anyone could re-point a
    // node's advertised endpoint and have its peers dial the impostor with the
    // mesh secret in the Authorization header (review F02-1/F03-1). First
    // registration stays TOFU: a node that has never bound a key is accepted
    // as before, and every current node signs its heartbeat.
    if existing_key.is_some() && !verified_now {
        tracing::warn!(
            "Register REJECTED for instance '{}' (mesh '{}'): row has a bound pubkey but the \
             request carried no valid registration signature",
            instance_id,
            mesh_id
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

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
            pubkey: $pubkey,
            pubkey_verified_at: $pk_verified,
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
    .bind(("pubkey", stored_pubkey.clone().unwrap_or_default()))
    .bind((
        "pk_verified",
        if verified_now {
            Some(chrono::Utc::now().to_rfc3339())
        } else {
            None
        },
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::SigningKey;
    use eck_core::utils::identity::sign_message;

    fn keypair(seed: u8) -> (String, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        (
            STANDARD.encode(sk.to_bytes()),
            STANDARD.encode(sk.verifying_key().to_bytes()),
        )
    }

    #[test]
    fn register_sig_roundtrip_and_freshness() {
        let (priv_b64, pub_b64) = keypair(9);
        let ts = 2_000_000i64;
        let canonical = register_canonical("node-1", ts);
        let sig = sign_message(&priv_b64, &canonical).unwrap();

        // Fresh + correct → valid.
        assert!(register_sig_valid("node-1", &pub_b64, ts, &sig, ts + 10, REGISTER_MAX_AGE_SECS));
        // Stale timestamp (beyond 300 s window) → rejected.
        assert!(!register_sig_valid(
            "node-1",
            &pub_b64,
            ts,
            &sig,
            ts + REGISTER_MAX_AGE_SECS + 1,
            REGISTER_MAX_AGE_SECS
        ));
        // Future skew beyond the window → rejected.
        assert!(!register_sig_valid(
            "node-1",
            &pub_b64,
            ts,
            &sig,
            ts - REGISTER_MAX_AGE_SECS - 1,
            REGISTER_MAX_AGE_SECS
        ));
    }

    #[test]
    fn register_sig_rejects_wrong_instance_or_key() {
        let (priv_b64, pub_b64) = keypair(3);
        let ts = 500_000i64;
        let sig = sign_message(&priv_b64, &register_canonical("node-A", ts)).unwrap();
        // Signature was over node-A; verifying against node-B's canonical fails
        // (a replayed registration for a different instance_id).
        assert!(!register_sig_valid("node-B", &pub_b64, ts, &sig, ts, REGISTER_MAX_AGE_SECS));

        // Attacker signs the right canonical with the wrong key but presents the
        // legit pubkey → verification fails.
        let (attacker_priv, _) = keypair(4);
        let forged = sign_message(&attacker_priv, &register_canonical("node-A", ts)).unwrap();
        assert!(!register_sig_valid("node-A", &pub_b64, ts, &forged, ts, REGISTER_MAX_AGE_SECS));
    }

    #[test]
    fn rotation_accepts_first_and_unchanged_key() {
        assert_eq!(rotation_decision(None, "KEY1", false), RotationDecision::Accept);
        assert_eq!(rotation_decision(Some(""), "KEY1", false), RotationDecision::Accept);
        assert_eq!(rotation_decision(Some("KEY1"), "KEY1", false), RotationDecision::Accept);
    }

    #[test]
    fn rotation_rejects_changed_key_unless_allowed() {
        // A different key for an instance that already has one → conflict.
        assert_eq!(
            rotation_decision(Some("KEY1"), "KEY2", false),
            RotationDecision::RejectConflict
        );
        // …unless rotation is explicitly allowed.
        assert_eq!(
            rotation_decision(Some("KEY1"), "KEY2", true),
            RotationDecision::Accept
        );
    }
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
        Ok(c) if c.sub == mesh_id => {
            // Mid-period kill switch: a CRL entry beats a valid signature. The
            // `paid` flag decays at this node's next heartbeat, so revocation
            // takes effect within the heartbeat cadence, no restart needed.
            if let Some(crl) = eck_core::licensing::load_revocations(&pubkey) {
                if crl.revokes_license_sub(&c.sub) {
                    tracing::warn!(
                        "License for mesh '{}' (tenant '{}') is REVOKED (crl updated {}) — treating as free",
                        c.sub,
                        c.tenant,
                        crl.updated
                    );
                    return free();
                }
            }
            (c.is_paid(), c.tier, c.exp)
        }
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
