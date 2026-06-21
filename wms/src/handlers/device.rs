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

/// Stored in SurrealDB as `registered_device:<device_id>`
#[derive(Clone, Debug, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct DeviceRecord {
    pub device_id: String,
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

    // 2. Verify Ed25519 signature
    // Android client signs: {"deviceId":"...","devicePublicKey":"..."}
    let message = format!(
        "{{\"deviceId\":\"{}\",\"devicePublicKey\":\"{}\"}}",
        body.device_id, body.device_public_key
    );

    let valid = identity::verify_signature(&body.device_public_key, &message, &body.signature)
        .map_err(|e| (StatusCode::FORBIDDEN, format!("Signature verification failed: {}", e)))?;

    if !valid {
        return Err((StatusCode::FORBIDDEN, "Invalid signature".into()));
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

    // 4. Upsert device in SurrealDB
    let existing: Option<DeviceRecord> = state
        .db
        .select(("registered_device", &*body.device_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
            device_id: body.device_id.clone(),
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

        let _: Option<DeviceRecord> = state
            .db
            .update(("registered_device", &*body.device_id))
            .content(updated)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    } else {
        let new_device = DeviceRecord {
            device_id: body.device_id.clone(),
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

        let _: Option<DeviceRecord> = state
            .db
            .create(("registered_device", &*body.device_id))
            .content(new_device)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // 5. Generate JWT if active
    let access_token = if final_status == "active" {
        eck_core::auth::create_token(&body.device_id, "device", "ed25519_signature", &state.jwt_secret).ok()
    } else {
        None
    };

    // 6. Include enc_key for active devices
    let enc_key = if final_status == "active" {
        std::env::var("ENC_KEY").ok().filter(|k| !k.is_empty())
    } else {
        None
    };

    tracing::info!(
        "Device registration: {} ({}) -> status={}",
        body.device_id,
        body.device_name.as_deref().unwrap_or("unnamed"),
        final_status
    );

    Ok(DeviceRegisterResponse {
        success: true,
        status: final_status,
        token: access_token,
        message: "Device handshake complete".into(),
        enc_key,
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
// GET /api/admin/devices
// ============================================================

pub async fn list_devices(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListDevicesQuery>,
) -> Result<Json<Vec<DeviceRecord>>, (StatusCode, String)> {
    let include_deleted = params.include_deleted.as_deref() == Some("true");

    let devices: Vec<DeviceRecord> = if include_deleted {
        state
            .db
            .select("registered_device")
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        state
            .db
            .query("SELECT * FROM registered_device WHERE deleted_at IS NONE ORDER BY status ASC")
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .take(0)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

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

    let existing: Option<DeviceRecord> = state
        .db
        .select(("registered_device", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let device = existing.ok_or((StatusCode::NOT_FOUND, "Device not found".into()))?;

    let updated = DeviceRecord {
        status: body.status,
        updated_at: Utc::now().to_rfc3339(),
        ..device
    };

    let result: Option<DeviceRecord> = state
        .db
        .update(("registered_device", &*id))
        .content(updated)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    result
        .map(Json)
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Update returned no record".into()))
}

// ============================================================
// DELETE /api/admin/devices/:id (soft delete)
// ============================================================

pub async fn delete_device(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let existing: Option<DeviceRecord> = state
        .db
        .select(("registered_device", &*id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let device = existing.ok_or((StatusCode::NOT_FOUND, "Device not found".into()))?;

    let now = Utc::now().to_rfc3339();
    let updated = DeviceRecord {
        deleted_at: Some(now.clone()),
        updated_at: now,
        ..device
    };

    let _: Option<DeviceRecord> = state
        .db
        .update(("registered_device", &*id))
        .content(updated)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "message": "Device deleted successfully (soft deleted for sync)",
        "id": id
    })))
}
