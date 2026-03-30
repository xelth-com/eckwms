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
    device_id: String,
    device_name: Option<String>,
    public_key: String,
    status: String,
    home_instance_id: Option<String>,
    last_seen_at: Option<String>,
    created_at: String,
    updated_at: String,
    deleted_at: Option<String>,
}

// ============================================================
// POST /api/public/devices/register (no JWT)
// ============================================================

pub async fn register_device(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DeviceRegisterRequest>,
) -> Result<Json<DeviceRegisterResponse>, (StatusCode, String)> {
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
        eck_core::auth::create_token(&body.device_id, "device", &state.jwt_secret).ok()
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

    Ok(Json(DeviceRegisterResponse {
        success: true,
        status: final_status,
        token: access_token,
        message: "Device handshake complete".into(),
        enc_key,
    }))
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
    let port = state.port;

    // Add local IPs
    if let Ok(local_ip) = local_ip_address::local_ip() {
        candidates.push(format!("http://{}:{}/E", local_ip, port));
    }

    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in &ifaces {
            if ip.is_ipv4() && !ip.is_loopback() {
                let url = format!("http://{}:{}/E", ip, port);
                if !candidates.contains(&url) {
                    candidates.push(url);
                }
            }
        }
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

    let connection_string = candidates.join(",").to_uppercase();

    // 4. Handle VIP/invite token
    let invite_suffix = if params.qr_type.as_deref() == Some("vip") {
        match eck_core::auth::create_token("invite", "invite", &state.jwt_secret) {
            Ok(token) => format!("${}", token),
            Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    } else {
        String::new()
    };

    // 5. Build QR string: ECK$2$UUID$KEY$URLS[$TOKEN]
    let qr_string = format!(
        "ECK$2${}${}${}{}",
        compact_uuid, pub_key_hex, connection_string, invite_suffix
    );

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
