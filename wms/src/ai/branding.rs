//! Tenant branding for AI prompts — the pilot customer's product brand and
//! market vertical are deployment config, not baked-in strings, so the public
//! open-core build carries neutral wording.
//!
//! Two env vars, both EMPTY by default (→ neutral phrasing):
//!   * `ECK_TENANT_BRAND`    — the operating company's device/product brand
//!                             (e.g. "Acme").
//!   * `ECK_TENANT_VERTICAL` — a short market phrase (e.g. "medical devices").
//!
//! The fleet sets these to restore the branded wording; an unset build reads as
//! a generic device-service operation. Values are read once (OnceLock) at first
//! prompt build — the env is fixed for the process lifetime.

use std::sync::OnceLock;

fn brand() -> &'static str {
    static B: OnceLock<String> = OnceLock::new();
    B.get_or_init(|| {
        std::env::var("ECK_TENANT_BRAND")
            .unwrap_or_default()
            .trim()
            .to_string()
    })
}

fn vertical() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        std::env::var("ECK_TENANT_VERTICAL")
            .unwrap_or_default()
            .trim()
            .to_string()
    })
}

/// "<brand> devices" when a brand is configured, else "the company's devices".
pub(crate) fn devices_phrase() -> String {
    let b = brand();
    if b.is_empty() {
        "the company's devices".to_string()
    } else {
        format!("{b} devices")
    }
}

/// "<brand> <vertical>" (e.g. "Acme medical devices") when both are set;
/// "<brand> devices" when only a brand is set; else "a device-service operation".
pub(crate) fn product_domain_phrase() -> String {
    let (b, v) = (brand(), vertical());
    match (b.is_empty(), v.is_empty()) {
        (false, false) => format!("{b} {v}"),
        (false, true) => format!("{b} devices"),
        _ => "a device-service operation".to_string(),
    }
}

/// "<brand> device support" when a brand is configured, else "device support".
pub(crate) fn device_support_phrase() -> String {
    let b = brand();
    if b.is_empty() {
        "device support".to_string()
    } else {
        format!("{b} device support")
    }
}

/// Repair-distiller subject: "an <brand> medical-device service operation" when
/// a brand is set, else "a device-service operation".
pub(crate) fn service_operation_phrase() -> String {
    let b = brand();
    if b.is_empty() {
        "a device-service operation".to_string()
    } else {
        format!("an {b} medical-device service operation")
    }
}
