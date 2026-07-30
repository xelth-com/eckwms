use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::Response,
    body::Body,
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use std::sync::Arc;

use eck_core::utils::identity;

use crate::AppState;

// ============================================================
// Request / Response types
// ============================================================

#[derive(Deserialize)]
pub struct DeviceRegisterRequest {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceName")]
    pub device_name: Option<String>,
    #[serde(rename = "devicePublicKey")]
    pub device_public_key: String,
    pub signature: String,
    #[serde(rename = "inviteToken")]
    pub invite_token: Option<String>,
    /// Single-use server-issued challenge (from GET /api/auth/device-challenge)
    /// that the client folded into the signed message to make registration
    /// replay-proof. Absent for legacy clients that sign the constant message —
    /// accepted unless `DEVICE_AUTH_REQUIRE_NONCE=true` (staged rollout).
    #[serde(default)]
    pub nonce: Option<String>,
}

#[derive(Serialize)]
pub struct DeviceRegisterResponse {
    pub success: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enc_key: Option<String>,
    /// Canonical server-minted device UUID. The app stores this and uses it as
    /// its `deviceId` from here on (signing future registrations over it); the
    /// raw ANDROID_ID is only the bootstrap handle for the first handshake.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_uuid: Option<String>,
}

#[derive(Deserialize)]
pub struct PairingQrQuery {
    #[serde(rename = "type")]
    pub qr_type: Option<String>,
}

#[derive(Deserialize)]
pub struct ListDevicesQuery {
    pub include_deleted: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateStatusRequest {
    pub status: String,
}

/// Stored in SurrealDB as `registered_device:<device_id>`, where `device_id` is
/// a server-minted UUID. The stable identity anchor is `public_key`; `android_id`
/// is a secondary lookup hint for the bootstrap pairing handshake and for
/// migrating legacy ANDROID_ID-keyed rows.
#[derive(Clone, Debug, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct DeviceRecord {
    pub device_id: String,
    #[serde(default)]
    pub android_id: Option<String>,
    pub device_name: Option<String>,
    pub public_key: String,
    pub status: String,
    pub home_instance_id: Option<String>,
    pub last_seen_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    // ─── Xelixir C2 control plane (replicated via mesh sync) ───
    // Cloud admin writes `xelixir_command` ("start" | "stop"); edge node's
    // AgentController catches the propagated update via LIVE SELECT and
    // reacts. The edge writes back `xelixir_status` and `xelixir_token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xelixir_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xelixir_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xelixir_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xelixir_session_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xelixir_updated_at: Option<String>,
}

/// Tolerant row→DeviceRecord conversion. Since the SurrealDB 3.2.x bump the
/// SDK's typed `take::<Option<DeviceRecord>>` fails on live rows (extra
/// `_vclock`/`id` fields, absent options, explicit NULLs) and every caller
/// that swallowed the error with `.ok()` saw "no such device" — 2026-07-23
/// every PDA got 403 "Device not registered" and the day's trips piled up on
/// the relay. Untyped JSON + serde keeps the pre-3.x semantics: unknown
/// fields ignored, missing/NULL options → None. Callers filter tombstones
/// in code (`deleted_at.is_none()`) because a `deleted_at = NULL` poison row
/// also breaks `IS NONE` in a WHERE clause.
pub(crate) fn devices_from_rows(rows: Vec<serde_json::Value>) -> Vec<DeviceRecord> {
    rows.into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

// ============================================================
// Device-auth challenge (replay-proof registration)
// ============================================================

/// TTL for a device-auth challenge nonce. Long enough for a phone on a slow link
/// to fetch it, sign, and POST register-device; short enough that a leaked nonce
/// is useless almost immediately (and it's single-use anyway).
pub const DEVICE_CHALLENGE_TTL_SECS: u64 = 120;

/// Mint + store a single-use challenge nonce and return it. Shared by the direct
/// HTTP handler (`GET /api/auth/device-challenge`) and the relay-forwarded
/// pairing path (`device_challenge` mesh-task), so a NAT'd master issues the
/// SAME kind of nonce it later validates — the stateful single-use guarantee
/// holds regardless of transport. Opportunistically prunes expired rows.
pub async fn issue_device_challenge(db: &eck_core::db::SurrealDb) -> Result<String, String> {
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    let _ = db
        .query("DELETE device_auth_challenge WHERE expires_at < time::now()")
        .await;

    db.query(
        "INSERT INTO device_auth_challenge { \
            nonce: $n, \
            created_at: time::now(), \
            expires_at: time::now() + type::duration($ttl) \
         }",
    )
    .bind(("n", nonce.clone()))
    .bind(("ttl", format!("{}s", DEVICE_CHALLENGE_TTL_SECS)))
    .await
    .map_err(|e| e.to_string())?;

    Ok(nonce)
}

// ============================================================
// POST /api/public/devices/register (no JWT)
// ============================================================

pub async fn register_device(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeviceRegisterRequest>,
) -> Result<Json<DeviceRegisterResponse>, (StatusCode, String)> {
    register_device_core(&state, body).await.map(Json)
}

/// Core device-registration logic, shared by the HTTP handler
/// (`POST /api/internal/register-device`) and the relay reverse-fetch poller
/// (the `device_register` mesh-task). The mesh-task path lets a NAT'd master
/// pair a phone through a blind relay — the phone never needs a directly
/// reachable full WMS, so the eckN service nodes can stay pure relays.
pub async fn register_device_core(
    state: &Arc<AppState>,
    body: DeviceRegisterRequest,
) -> Result<DeviceRegisterResponse, (StatusCode, String)> {
    // 1. Validate required fields
    if body.device_id.is_empty() || body.device_public_key.is_empty() || body.signature.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing required fields".into()));
    }

    // 2. Verify Ed25519 signature over a SERVER-ISSUED single-use nonce.
    //
    // The client first fetches a nonce from GET /api/auth/device-challenge and
    // folds it into the signed message, so a captured register-device POST can't
    // be replayed (the nonce is consumed in 2b). The signed-message shape MUST
    // match the client byte-for-byte:
    //   {"deviceId":"..","devicePublicKey":"..","nonce":".."}
    // The nonce is mandatory — a nonce-less signature is a static, replayable
    // credential, so we reject it outright (no legacy path).
    let nonce = match body.nonce.as_deref().filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => {
            return Err((
                StatusCode::FORBIDDEN,
                "Missing challenge nonce — GET /api/auth/device-challenge first".into(),
            ));
        }
    };

    let message = format!(
        "{{\"deviceId\":\"{}\",\"devicePublicKey\":\"{}\",\"nonce\":\"{}\"}}",
        body.device_id, body.device_public_key, nonce
    );

    let valid = identity::verify_signature(&body.device_public_key, &message, &body.signature)
        .map_err(|e| (StatusCode::FORBIDDEN, format!("Signature verification failed: {}", e)))?;

    if !valid {
        return Err((StatusCode::FORBIDDEN, "Invalid signature".into()));
    }

    // 2b. Consume the nonce AFTER the signature checks out. Atomic single-use:
    // `DELETE … RETURN BEFORE` returns the row iff it existed and was unexpired;
    // an empty result means the nonce was never issued, already used (replay),
    // or expired → reject. Done post-signature so a bad signature can't burn a
    // victim's outstanding nonce.
    let consumed: Vec<serde_json::Value> = state
        .db
        .query("DELETE device_auth_challenge WHERE nonce = $n AND expires_at > time::now() RETURN BEFORE")
        .bind(("n", nonce.to_string()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if consumed.is_empty() {
        return Err((
            StatusCode::FORBIDDEN,
            "Challenge nonce invalid, expired, or already used".into(),
        ));
    }

    // 3. Determine initial status
    let mut final_status = "pending".to_string();

    if let Some(ref invite_token) = body.invite_token {
        if !invite_token.is_empty() {
            // Validate invite token as a JWT
            if eck_core::auth::validate_token(invite_token, &state.jwt_secret).is_ok() {
                final_status = "active".to_string();
            }
        }
    }

    let now = Utc::now().to_rfc3339();

    // 4. Resolve the device to its canonical UUID.
    //
    // Identity anchor is the Ed25519 `public_key`: two registrations are the same
    // device iff they present the same key (this survives a factory reset / an
    // ANDROID_ID change, since the key lives in the Android keystore). Resolution
    // order, all ignoring soft-deleted tombstones:
    //   (a) by public_key   — the anchor (covers re-pairing after ANDROID_ID change)
    //   (b) by record key   — UUID re-registration, or a not-yet-migrated legacy row
    //   (c) by android_id   — a migrated legacy device that still sends its ANDROID_ID
    // No match → a brand-new device: mint a fresh UUID.
    let looks_like_uuid = uuid::Uuid::parse_str(&body.device_id).is_ok();

    let db_err = |e: surrealdb::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());

    // 4a. Self-heal poisoned rows FIRST: mesh-synced or hand-edited rows can
    // carry native SurrealDB datetimes (or NULLs) in the timestamp fields, and
    // the typed DeviceRecord reads below then 500 with "Expected string, got
    // datetime" — which blocked EVERY re-auth of an existing device (field day
    // 2026-07-13: trips piled up behind token-rejected relay acks forever).
    // Idempotent, tiny table; errors are swallowed (worst case we fail exactly
    // as before).
    let _ = state
        .db
        .query(
            "UPDATE registered_device SET \
                created_at = type::string(created_at), \
                updated_at = type::string(updated_at), \
                last_seen_at = IF last_seen_at != NONE AND last_seen_at != NULL \
                    THEN type::string(last_seen_at) ELSE NONE END, \
                deleted_at = IF deleted_at != NONE AND deleted_at != NULL \
                    THEN type::string(deleted_at) ELSE NONE END, \
                xelixir_updated_at = IF xelixir_updated_at != NONE AND xelixir_updated_at != NULL \
                    THEN type::string(xelixir_updated_at) ELSE NONE END \
             WHERE type::is_datetime(created_at) OR type::is_datetime(updated_at) \
                OR type::is_datetime(last_seen_at) OR type::is_datetime(deleted_at) \
                OR type::is_datetime(xelixir_updated_at) \
                OR deleted_at = NULL OR last_seen_at = NULL OR xelixir_updated_at = NULL",
        )
        .await;

    // (a) public_key anchor — `ORDER BY created_at ASC` makes the canonical pick
    // DETERMINISTIC (oldest registration wins) so every node independently collapses
    // duplicates of the same phone to the SAME row, instead of an arbitrary `LIMIT 1`.
    // All three lookups read untyped rows and convert via devices_from_rows —
    // the 3.2.x typed take fails on live rows (see that helper). Tombstones are
    // skipped in code, NOT via `deleted_at IS NONE` (NULL-poisoned rows defeat it).
    let rows: Vec<serde_json::Value> = state
        .db
        .query("SELECT * FROM registered_device WHERE public_key = $pk ORDER BY created_at ASC")
        .bind(("pk", body.device_public_key.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    let mut existing = devices_from_rows(rows)
        .into_iter()
        .find(|d| d.deleted_at.is_none());

    // (b) the presented id as the canonical device_id — IGNORING soft-deleted
    // rows, so a re-pair of a previously-deleted device gets a FRESH UUID
    // instead of resurrecting the old tombstone (which, for a legacy device,
    // would keep the ANDROID_ID as the key).
    if existing.is_none() {
        let rows: Vec<serde_json::Value> = state
            .db
            .query("SELECT * FROM registered_device WHERE device_id = $id LIMIT 3")
            .bind(("id", body.device_id.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?;
        existing = devices_from_rows(rows)
            .into_iter()
            .find(|d| d.deleted_at.is_none());
    }

    // (c) android_id field (legacy device re-pairing after migration)
    if existing.is_none() {
        let rows: Vec<serde_json::Value> = state
            .db
            .query("SELECT * FROM registered_device WHERE android_id = $aid LIMIT 3")
            .bind(("aid", body.device_id.clone()))
            .await
            .map_err(db_err)?
            .take(0)
            .map_err(db_err)?;
        existing = devices_from_rows(rows)
            .into_iter()
            .find(|d| d.deleted_at.is_none());
    }

    // Canonical record key for this device: reuse the matched row's, else mint one.
    let device_uuid = match &existing {
        Some(dev) => dev.device_id.clone(),
        None => uuid::Uuid::new_v4().to_string(),
    };

    // android_id to persist: keep any existing one, else the presented id when it
    // is a raw ANDROID_ID (i.e. the app hasn't been told its UUID yet).
    let android_id = existing
        .as_ref()
        .and_then(|d| d.android_id.clone())
        .or_else(|| if looks_like_uuid { None } else { Some(body.device_id.clone()) });

    // 5. Upsert keyed by the canonical UUID.
    if let Some(existing_device) = existing {
        let was_deleted = existing_device.deleted_at.is_some();
        let current_status = existing_device.status.clone();

        let new_status = if was_deleted {
            final_status.clone()
        } else if current_status == "pending" && final_status == "active" {
            "active".to_string()
        } else {
            final_status = current_status.clone();
            current_status
        };

        let updated = DeviceRecord {
            device_id: device_uuid.clone(),
            android_id: android_id.clone(),
            device_name: body.device_name.clone().or(existing_device.device_name),
            public_key: body.device_public_key.clone(),
            status: new_status,
            home_instance_id: Some(state.instance_id.clone()),
            last_seen_at: Some(now.clone()),
            created_at: existing_device.created_at,
            updated_at: now,
            deleted_at: if was_deleted { None } else { existing_device.deleted_at },
            // Re-registration preserves any in-flight xelixir state.
            xelixir_command: existing_device.xelixir_command,
            xelixir_status: existing_device.xelixir_status,
            xelixir_token: existing_device.xelixir_token,
            xelixir_session_url: existing_device.xelixir_session_url,
            xelixir_updated_at: existing_device.xelixir_updated_at,
        };

        // Untyped return: the write itself succeeds — a typed echo that fails
        // to convert must not 500 a registration that already happened.
        let _: Option<serde_json::Value> = state
            .db
            .update(("registered_device", &*device_uuid))
            .content(updated)
            .await
            .map_err(db_err)?;
    } else {
        let new_device = DeviceRecord {
            device_id: device_uuid.clone(),
            android_id: android_id.clone(),
            device_name: body.device_name.clone(),
            public_key: body.device_public_key.clone(),
            status: final_status.clone(),
            home_instance_id: Some(state.instance_id.clone()),
            last_seen_at: Some(now.clone()),
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
            xelixir_command: None,
            xelixir_status: None,
            xelixir_token: None,
            xelixir_session_url: None,
            xelixir_updated_at: None,
        };

        let _: Option<serde_json::Value> = state
            .db
            .create(("registered_device", &*device_uuid))
            .content(new_device)
            .await
            .map_err(db_err)?;
    }

    // Advance this node's vclock on the (UUID-keyed) record so the registration /
    // status change propagates and STICKS across the mesh — registered_device is
    // not auto-bumped, so without this a peer's stale copy can resurrect/override.
    let rid = format!("registered_device:`{}`", device_uuid);
    let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &rid, &state.instance_id).await;

    // 5b. Collapse duplicates: the same phone (public_key) may have spawned other
    // UUID rows on other nodes before this one converged (registered_device doesn't
    // fully merge across the mesh). Soft-delete every OTHER non-deleted row with this
    // pubkey, vclock-bumped so the tombstone propagates instead of being resurrected.
    // Combined with the deterministic canonical pick in (a), every node converges to
    // one row per device.
    let dup_ids: Vec<String> = state
        .db
        .query("SELECT VALUE record::id(id) FROM registered_device WHERE public_key = $pk AND deleted_at IS NONE AND record::id(id) != $canon")
        .bind(("pk", body.device_public_key.clone()))
        .bind(("canon", device_uuid.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .unwrap_or_default();
    let tomb_now = Utc::now().to_rfc3339();
    for dup in &dup_ids {
        let drid = format!("registered_device:`{}`", dup);
        let _ = state
            .db
            .query("UPDATE type::record($rid) SET deleted_at = $now, updated_at = $now")
            .bind(("rid", drid.clone()))
            .bind(("now", tomb_now.clone()))
            .await;
        let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &drid, &state.instance_id).await;
    }
    if !dup_ids.is_empty() {
        tracing::info!(
            "Device registration: collapsed {} duplicate row(s) for pubkey into {}",
            dup_ids.len(),
            device_uuid
        );
    }

    // 6. Generate JWT if active — subject is the canonical UUID.
    let access_token = if final_status == "active" {
        eck_core::auth::create_token(&device_uuid, "device", "ed25519_signature", &state.jwt_secret).ok()
    } else {
        None
    };

    // 7. Include enc_key for active devices
    let enc_key = if final_status == "active" {
        std::env::var("ENC_KEY").ok().filter(|k| !k.is_empty())
    } else {
        None
    };

    tracing::info!(
        "Device registration: {} (android_id={:?}, {}) -> status={}",
        device_uuid,
        android_id,
        body.device_name.as_deref().unwrap_or("unnamed"),
        final_status
    );

    Ok(DeviceRegisterResponse {
        success: true,
        status: final_status,
        token: access_token,
        message: "Device handshake complete".into(),
        enc_key,
        device_uuid: Some(device_uuid),
    })
}

/// True for Docker/bridge/VPN virtual interface names whose IPs must not be
/// advertised off-host (they only make remote clients waste connection probes).
fn is_virtual_iface_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("docker")
        || n.starts_with("br-")
        || n.starts_with("veth")
        || n.starts_with("virbr")
        || n.starts_with("tun")
        || n.starts_with("tap")
        || n.starts_with("cni")
        || n.starts_with("flannel")
}

/// True for the Docker default bridge address pool (172.17.0.0–172.31.255.255).
/// Real LAN ranges (192.168/16, 10/8) are kept — they're valid for LAN pairing.
fn is_virtual_ip(ip: &std::net::IpAddr) -> bool {
    if let std::net::IpAddr::V4(v4) = ip {
        let o = v4.octets();
        o[0] == 172 && (16..=31).contains(&o[1])
    } else {
        false
    }
}

/// True for IPv4 link-local / APIPA (169.254.0.0/16) — a self-assigned address
/// an interface gives itself only when DHCP never answered (i.e. there is no
/// router). It's usable for direct ad-hoc/same-cable pairing, but must NEVER be
/// advertised when a real routable LAN IP exists, or clients waste probes on an
/// address that only reaches the host's own link.
fn is_link_local(ip: &std::net::IpAddr) -> bool {
    matches!(ip, std::net::IpAddr::V4(v4) if v4.is_link_local())
}

// ============================================================
// GET /api/internal/pairing-qr (JWT protected)
// ============================================================

pub async fn generate_pairing_qr(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PairingQrQuery>,
) -> Result<Response, (StatusCode, String)> {
    let identity = &state.server_identity;

    // 1. Compact UUID (remove dashes, uppercase)
    let compact_uuid = identity
        .instance_id
        .replace('-', "")
        .to_uppercase();

    // 2. Public key hex (uppercase)
    let pub_key_hex = identity
        .public_key_hex()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // 3. Build connection candidates
    let mut candidates = Vec::new();
    // 169.254.x link-local (APIPA) addresses are collected separately and only
    // folded in as a last resort: they appear when DHCP failed (no router), so
    // they're valid for ad-hoc/same-link pairing but useless — and noise — once
    // a real routable LAN IP exists.
    let mut linklocal = Vec::new();
    let port = state.port;

    // Add local IPs — but skip Docker/bridge/VPN virtual interfaces so their
    // off-host-unreachable addresses (e.g. 172.17.0.1 docker0) never leak into
    // the pairing QR or a device's saved candidate list, where they only make
    // clients waste connection probes. Real LAN NICs (192.168.x / 10.x on
    // eth0/wlan0) are kept — they're valid for local pairing.
    if let Ok(local_ip) = local_ip_address::local_ip() {
        if !is_virtual_ip(&local_ip) {
            let url = format!("http://{}:{}/E", local_ip, port);
            if is_link_local(&local_ip) {
                linklocal.push(url);
            } else {
                candidates.push(url);
            }
        }
    }

    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (name, ip) in &ifaces {
            if ip.is_ipv4()
                && !ip.is_loopback()
                && !is_virtual_iface_name(name)
                && !is_virtual_ip(ip)
            {
                let url = format!("http://{}:{}/E", ip, port);
                if is_link_local(ip) {
                    if !linklocal.contains(&url) {
                        linklocal.push(url);
                    }
                } else if !candidates.contains(&url) {
                    candidates.push(url);
                }
            }
        }
    }

    // No real LAN address at all (DHCP never answered / no router) → fall back
    // to the self-assigned link-local addresses so direct same-link pairing
    // still has something to dial. Checked BEFORE BASE_URL is appended so the
    // decision reflects only what the NICs actually report.
    if candidates.is_empty() {
        candidates.append(&mut linklocal);
    }

    // Add global URL if configured
    if let Ok(base_url) = std::env::var("BASE_URL") {
        if !base_url.is_empty() {
            let mut global = base_url;
            if !global.ends_with('/') {
                global.push('/');
            }
            candidates.push(global);
        }
    }

    // 4. Handle VIP/invite token
    let invite_suffix = if params.qr_type.as_deref() == Some("vip") {
        match eck_core::auth::create_token("invite", "invite", "system", &state.jwt_secret) {
            Ok(token) => format!("${}", token),
            Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    } else {
        String::new()
    };

    // 5. Build the QR string.
    //   Paid (has a license) → v3 with mesh_id (for the app's mod3 ordering).
    //   The eckN service nodes are baked-in app defaults, so a QR generated BY
    //   an eckN node omits them (short). But a paid CUSTOMER's OWN server (e.g.
    //   a LAN node) is NOT a default — its URL MUST be embedded, or the device
    //   would be sent to the eckN (a different mesh) and land in quarantine.
    //     ECK$3$UUID$KEY$MESH$OWN_URLS[$TOKEN]
    //   Free → v2 with this node's own URLs embedded (no app defaults exist):
    //     ECK$2$UUID$KEY$URLS[$TOKEN]
    let is_paid = std::env::var("ECK_LICENSE_TOKEN").ok().filter(|t| !t.is_empty()).is_some();
    let eckn_hosts = ["eck1.com", "eck2.com", "eck3.com"];
    let qr_string = if is_paid {
        let mesh_compact = state.mesh_id.replace('-', "").to_uppercase();
        let base_url = std::env::var("BASE_URL").unwrap_or_default();
        let this_is_eckn = eckn_hosts.iter().any(|h| base_url.contains(h));
        // This node's own reachable URLs (BASE_URL + local IPs), MINUS any eckN
        // default host. Omitted entirely only when THIS node is itself an eckN.
        let mut own: Vec<String> = if this_is_eckn {
            Vec::new()
        } else {
            candidates
                .iter()
                .filter(|u| !eckn_hosts.iter().any(|h| u.contains(h)))
                .cloned()
                .collect()
        };
        for s in std::env::var("MESH_DEVICE_URLS").unwrap_or_default().split(',') {
            let s = s.trim();
            if !s.is_empty() && !own.iter().any(|u| u == s) {
                own.push(s.to_string());
            }
        }
        let own_string = own.join(",").to_uppercase();
        format!(
            "ECK$3${}${}${}${}{}",
            compact_uuid, pub_key_hex, mesh_compact, own_string, invite_suffix
        )
    } else {
        let connection_string = candidates.join(",").to_uppercase();
        format!(
            "ECK$2${}${}${}{}",
            compact_uuid, pub_key_hex, connection_string, invite_suffix
        )
    };

    // 6. Generate QR code PNG
    let qr = qrcode::QrCode::new(qr_string.as_bytes())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("QR generation failed: {}", e)))?;

    let image = qr
        .render::<image::Luma<u8>>()
        .quiet_zone(true)
        .max_dimensions(512, 512)
        .build();

    let mut png_data = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
    image::ImageEncoder::write_image(
        encoder,
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::L8,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("PNG encoding failed: {}", e)))?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "image/png")
        .body(Body::from(png_data))
        .unwrap())
}

// ============================================================
// POST /api/admin/pair-code  (admin) — mint a typed onboarding code
// ============================================================

#[derive(Deserialize, Default)]
pub struct MintPairCodeRequest {
    /// Optional TTL override (seconds); relay clamps to [30, 1800]. Default 600.
    pub ttl_secs: Option<i64>,
}

/// 6-char Crockford base32 code (no I/L/O/U → unambiguous to read out / type).
fn gen_pair_code() -> String {
    use rand::Rng;
    const ALPHA: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut rng = rand::thread_rng();
    (0..6).map(|_| ALPHA[rng.gen_range(0..ALPHA.len())] as char).collect()
}

/// Mint a short-lived, single-use onboarding code and publish it as a rendezvous
/// entry on the public discovery board(s). A PDA later types the code → the board
/// resolves it to an `ECK$…` pairing string (see relay `pair.rs`) → normal
/// relay-forwarded pairing. No printed QR needed.
///
/// The board is reached over the network (`PAIR_BOARD_URLS`, default the public
/// `https://9eck.com`); the PDA's default resolver list is the same public board,
/// so we publish there regardless of tier. `paid` (license present) picks the QR
/// version the board assembles: free `ECK$2` with a public relay, paid `ECK$3`
/// with mesh + the eckN polygon.
pub async fn mint_pair_code(
    State(state): State<Arc<AppState>>,
    body: Option<Json<MintPairCodeRequest>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let ttl = body.and_then(|Json(b)| b.ttl_secs).unwrap_or(600);
    let code = gen_pair_code();

    let invite = eck_core::auth::create_token("invite", "invite", "system", &state.jwt_secret)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("invite token: {e}")))?;
    let key = state
        .server_identity
        .public_key_hex()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("pubkey: {e}")))?;
    let paid = std::env::var("ECK_LICENSE_TOKEN").ok().filter(|t| !t.is_empty()).is_some();

    // QR `URLS` field the board will embed: paid → the eckN relay polygon
    // (RELAY_URLS); free → a public relay the NAT'd master is reachable through.
    let relay_adv = if paid {
        std::env::var("RELAY_URLS").unwrap_or_default()
    } else {
        std::env::var("PAIR_FREE_RELAY").unwrap_or_else(|_| "https://9eck.com".to_string())
    };

    let boards: Vec<String> = std::env::var("PAIR_BOARD_URLS")
        .unwrap_or_else(|_| "https://9eck.com".to_string())
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let announce = serde_json::json!({
        "code": code,
        "uuid": state.instance_id,
        "key": key,
        "mesh": state.mesh_id,
        "invite_token": invite,
        "relay": relay_adv,
        "paid": paid,
        "ttl_secs": ttl,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut published = 0usize;
    let mut expires_at = String::new();
    let mut last_err = String::new();
    for board in &boards {
        let url = format!("{board}/E/pair/announce");
        match client.post(&url).json(&announce).send().await {
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    if let Some(e) = v.get("expires_at").and_then(|x| x.as_str()) {
                        expires_at = e.to_string();
                    }
                }
                published += 1;
            }
            Ok(r) => last_err = format!("{url} -> HTTP {}", r.status()),
            Err(e) => last_err = format!("{url} -> {e}"),
        }
    }

    if published == 0 {
        return Err((StatusCode::BAD_GATEWAY, format!("no board accepted the code: {last_err}")));
    }

    Ok(Json(serde_json::json!({
        "code": code,
        "expires_at": expires_at,
        "boards_published": published,
        "paid": paid,
    })))
}

// ============================================================
// GET /api/admin/devices
// ============================================================

pub async fn list_devices(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListDevicesQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let include_deleted = params.include_deleted.as_deref() == Some("true");

    // Read into untyped JSON, NOT Vec<DeviceRecord>: a stray `deleted_at: NULL`
    // (SurrealDB NULL != NONE — e.g. a row a migration wrote with an explicit
    // null) makes the SurrealValue derive fail ("expected string, got null") and
    // 500s the whole dashboard. Untyped JSON tolerates it; the frontend reads the
    // same fields (device_id/status/device_name/last_seen_at/…) either way.
    //
    // Exclude node-self rows (`device_id == home_instance_id`): a mesh node
    // "homes itself" with a registered_device row, but those are SERVERS, not
    // PDAs — they belong in the Mesh tab (/api/mesh/nodes), and leaking them into
    // the Scanners list is what made servers show up there (and the local node
    // appear as a "device" homed to itself).
    let sql = if include_deleted {
        "SELECT * FROM registered_device WHERE device_id != home_instance_id ORDER BY status ASC"
    } else {
        "SELECT * FROM registered_device WHERE deleted_at IS NONE AND device_id != home_instance_id ORDER BY status ASC"
    };

    let devices: Vec<serde_json::Value> = state
        .db
        .query(sql)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(devices))
}

// ============================================================
// PUT /api/admin/devices/:id/status
// ============================================================

pub async fn update_device_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateStatusRequest>,
) -> Result<Json<DeviceRecord>, (StatusCode, String)> {
    let valid_statuses = ["active", "pending", "blocked"];
    if !valid_statuses.contains(&body.status.as_str()) {
        return Err((StatusCode::BAD_REQUEST, "Invalid status. Must be: active, pending, or blocked".into()));
    }

    // Untyped read + code-side tombstone filter — the 3.2.x typed take fails
    // on live rows (see devices_from_rows).
    let rows: Vec<serde_json::Value> = state
        .db
        .query("SELECT * FROM registered_device WHERE device_id = $id LIMIT 3")
        .bind(("id", id.clone()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .take(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let device = devices_from_rows(rows)
        .into_iter()
        .find(|d| d.deleted_at.is_none())
        .ok_or((StatusCode::NOT_FOUND, "Device not found".into()))?;

    let updated = DeviceRecord {
        status: body.status,
        updated_at: Utc::now().to_rfc3339(),
        ..device
    };

    let result: Option<serde_json::Value> = state
        .db
        .update(("registered_device", &*id))
        .content(updated.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // status is a synced/hashed field → advance this node's vclock so the change
    // propagates instead of being dropped as local-wins (registered_device is not
    // auto-bumped). Same pattern as the ticket handlers.
    let rid = format!("registered_device:`{}`", id);
    let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &rid, &state.instance_id).await;

    result
        .map(|_| Json(updated))
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Update returned no record".into()))
}

// ============================================================
// DELETE /api/admin/devices/:id (soft delete)
// ============================================================

pub async fn delete_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db_err = |e: surrealdb::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());

    // Existence check via untyped JSON (NOT DeviceRecord) so a malformed row
    // (e.g. `deleted_at: NULL`) can still be deleted rather than 500ing.
    let exists: Vec<serde_json::Value> = state
        .db
        .query("SELECT record::id(id) AS rid FROM registered_device WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    if exists.is_empty() {
        return Err((StatusCode::NOT_FOUND, "Device not found".into()));
    }

    // Soft-delete via a raw UPDATE (tolerant of malformed fields) + advance this
    // node's _vclock so the tombstone wins mesh conflict-resolution and actually
    // propagates — without the bump a plain deleted_at write is causally dominated
    // by peers and the row gets resurrected (registered_device is NOT auto-bumped).
    let now = Utc::now().to_rfc3339();
    let rid = format!("registered_device:`{}`", id);
    state
        .db
        .query("UPDATE type::record($rid) SET deleted_at = $now, updated_at = $now")
        .bind(("rid", rid.clone()))
        .bind(("now", now))
        .await
        .map_err(db_err)?;
    let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &rid, &state.instance_id).await;

    Ok(Json(serde_json::json!({
        "message": "Device deleted successfully (soft deleted for sync)",
        "id": id
    })))
}

#[derive(Deserialize)]
pub struct UpdateHomeRequest {
    #[serde(rename = "homeInstanceId")]
    pub home_instance_id: String,
}

// ============================================================
// PUT /api/admin/devices/:id/home — transfer a device's home/authority node
// ============================================================
//
// Per the ownership rule (creator is authority, transferable — see
// support::claim_home), this re-homes ONE device to a chosen node. Like the
// delete/status paths it advances THIS node's `_vclock` component so the change
// wins mesh conflict-resolution and propagates (registered_device is not
// auto-bumped). Tolerant raw UPDATE so a malformed row can still be re-homed.
pub async fn update_device_home(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateHomeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db_err = |e: surrealdb::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    let home = body.home_instance_id.trim().to_string();
    if home.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "homeInstanceId required".into()));
    }

    let exists: Vec<serde_json::Value> = state
        .db
        .query("SELECT record::id(id) AS rid FROM registered_device WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    if exists.is_empty() {
        return Err((StatusCode::NOT_FOUND, "Device not found".into()));
    }

    let now = Utc::now().to_rfc3339();
    let rid = format!("registered_device:`{}`", id);
    state
        .db
        .query("UPDATE type::record($rid) SET home_instance_id = $home, updated_at = $now")
        .bind(("rid", rid.clone()))
        .bind(("home", home.clone()))
        .bind(("now", now))
        .await
        .map_err(db_err)?;
    let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &rid, &state.instance_id).await;

    Ok(Json(serde_json::json!({ "id": id, "home_instance_id": home })))
}

// ============================================================
// POST /api/admin/devices/:id/restore — un-delete (clear deleted_at)
// ============================================================
pub async fn restore_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let db_err = |e: surrealdb::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());

    let exists: Vec<serde_json::Value> = state
        .db
        .query("SELECT record::id(id) AS rid FROM registered_device WHERE record::id(id) = $id LIMIT 1")
        .bind(("id", id.clone()))
        .await
        .map_err(db_err)?
        .take(0)
        .map_err(db_err)?;
    if exists.is_empty() {
        return Err((StatusCode::NOT_FOUND, "Device not found".into()));
    }

    // Clear to NONE (unset), never NULL — and bump vclock so the un-delete sticks.
    let now = Utc::now().to_rfc3339();
    let rid = format!("registered_device:`{}`", id);
    state
        .db
        .query("UPDATE type::record($rid) SET deleted_at = NONE, updated_at = $now")
        .bind(("rid", rid.clone()))
        .bind(("now", now))
        .await
        .map_err(db_err)?;
    let _ = eck_core::sync::conflict::bump_local_vclock(&state.db, &rid, &state.instance_id).await;

    Ok(Json(serde_json::json!({ "id": id, "restored": true })))
}
