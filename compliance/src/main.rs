// ============================================================================
// COMPLIANCE WORKER — Isolated ELSTER (ERiC) & VIES Process
// ============================================================================
//
// This crate exists as a SEPARATE BINARY specifically to sandbox external
// compliance and tax authority APIs.
//
// 1. ERiC (ELSTER): A closed-source C library for electronic tax filing.
//    Running it here contains potential segfaults/crashes.
// 2. VIES (VAT ID Validation): External EU REST API that can experience
//    timeouts. Running it here prevents it from blocking the main Axum
//    runtimes in WMS and POS.
//
// COMMUNICATION:
// - The POS/WMS servers call this worker via local HTTP (127.0.0.1:3230).
//
// This crate does NOT depend on eck_core to minimize the blast radius.
// ============================================================================

use axum::{routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    service: String,
    version: String,
    eric_loaded: bool,
}

#[derive(Deserialize)]
struct ViesValidationRequest {
    target_country_code: String,
    target_vat_number: String,
    requester_country_code: Option<String>,
    requester_vat_number: Option<String>,
}

#[derive(Serialize)]
struct ViesValidationResponse {
    valid: bool,
    request_date: Option<String>,
    consultation_number: Option<String>,
    name: Option<String>,
    address: Option<String>,
    error: Option<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    info!("Starting compliance worker (ELSTER/ERiC & VIES sandbox)");

    let port: u16 = std::env::var("COMPLIANCE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3230);

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/vies/validate", post(validate_vat));
        // TODO: POST /ustva — submit Umsatzsteuervoranmeldung
        // TODO: POST /euer — generate Einnahmenüberschussrechnung

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind compliance worker");
    info!("Compliance worker listening on http://{}", addr);

    axum::serve(listener, app).await.unwrap();
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: "compliance".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        eric_loaded: false, // TODO: true once ERiC FFI bindings are linked
    })
}

async fn validate_vat(Json(payload): Json<ViesValidationRequest>) -> Json<ViesValidationResponse> {
    info!("Validating VAT ID: {}{}", payload.target_country_code, payload.target_vat_number);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let url = "https://ec.europa.eu/taxation_customs/vies/rest-api/check-vat-number";

    let body = serde_json::json!({
        "countryCode": payload.target_country_code,
        "vatNumber": payload.target_vat_number,
        "requesterMemberStateCode": payload.requester_country_code,
        "requesterNumber": payload.requester_vat_number
    });

    match client.post(url).json(&body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                if let Ok(data) = resp.json::<serde_json::Value>().await {
                    return Json(ViesValidationResponse {
                        valid: data["isValid"].as_bool().unwrap_or(false),
                        request_date: data["requestDate"].as_str().map(|s| s.to_string()),
                        consultation_number: data["requestIdentifier"].as_str().map(|s| s.to_string()),
                        name: data["name"].as_str().map(|s| s.to_string()),
                        address: data["address"].as_str().map(|s| s.to_string()),
                        error: None,
                    });
                }
            }
            warn!("VIES API returned non-success status: {}", status);
            Json(ViesValidationResponse {
                valid: false,
                request_date: None,
                consultation_number: None,
                name: None,
                address: None,
                error: Some(format!("VIES API error: {}", status)),
            })
        }
        Err(e) => {
            warn!("VIES API request failed: {}", e);
            Json(ViesValidationResponse {
                valid: false,
                request_date: None,
                consultation_number: None,
                name: None,
                address: None,
                error: Some(e.to_string()),
            })
        }
    }
}
