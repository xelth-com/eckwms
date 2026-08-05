//! Admin API for the WASM plugin runtime (design `.eck/WASM_ARCHITECTURE.md`
//! §5: storage, registry, lifecycle). Install / list / enable / disable — all
//! admin-only, mounted under `admin_routes` in `main.rs` behind
//! `require_admin_middleware` (same layer as `handlers::admin::query`).
//!
//! Every handler here is a JSON 404 no-op when `ECK_PLUGIN_RUNTIME` is off
//! (`state.plugins` is `None`) — the runtime gate covers the whole admin
//! surface, not just hook execution.

use std::sync::Arc;

use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    Extension, Json,
};
use eck_core::auth::Claims;
use eck_core::plugin::{PluginHookRef, RegistryError, CAP_ANONYMIZE, CAP_LOG};
use serde_json::{json, Value};
use tracing::info;

use crate::AppState;

type ApiResult<T> = Result<T, (StatusCode, Json<Value>)>;

/// The only capability tokens the engine's host module knows how to link
/// (`core/src/plugin/host.rs::{CAP_LOG, CAP_ANONYMIZE}`, re-exported via
/// `eck_core::plugin`). Sourced from those constants rather than hardcoded
/// string literals so a new capability added to the engine doesn't silently
/// desync from what install accepts.
const KNOWN_CAPABILITIES: &[&str] = &[CAP_LOG, CAP_ANONYMIZE];

/// Plugin name grammar (design §5 / brief): lowercase alnum start, then
/// alnum/`_`/`-`, 2-64 chars total.
fn is_valid_plugin_name(name: &str) -> bool {
    regex::Regex::new(r"^[a-z0-9][a-z0-9_-]{1,63}$")
        .expect("static regex")
        .is_match(name)
}

/// Capability tokens in `caps` that aren't in [`KNOWN_CAPABILITIES`] — an
/// unrecognized claim would otherwise sit in the registry record forever
/// granting nothing (the engine only links host fns for tokens it knows), so
/// reject it loudly at install time instead of failing silently later.
fn unknown_capabilities(caps: &[String]) -> Vec<String> {
    caps.iter()
        .filter(|c| !KNOWN_CAPABILITIES.contains(&c.as_str()))
        .cloned()
        .collect()
}

fn runtime_off() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "ok": false,
            "error": "WASM plugin runtime is not enabled on this node (ECK_PLUGIN_RUNTIME)"
        })),
    )
}

/// Live `plugin:runtime` license re-check — defense in depth on top of the
/// boot gate in `main.rs`. `install` and `enable` are the verbs that ADD
/// executable code, so they re-verify per call: a runtime constructed before a
/// mid-run expiry/revocation can still list/disable, but not grow.
fn require_plugin_license(state: &AppState) -> Result<(), (StatusCode, Json<Value>)> {
    let token = std::env::var("ECK_LICENSE_TOKEN").ok();
    let verdict = eck_core::licensing::node_license_for_scope(
        token.as_deref(),
        &state.mesh_id,
        eck_core::licensing::SCOPE_PLUGIN_RUNTIME,
        chrono::Utc::now().timestamp(),
    );
    match verdict {
        v if v.allows() => Ok(()),
        eck_core::licensing::NodeLicense::Unlicensed { reason } => Err((
            StatusCode::PAYMENT_REQUIRED,
            Json(json!({
                "ok": false,
                "error": format!("plugin runtime is not licensed: {reason}")
            })),
        )),
        _ => unreachable!("allows() covers Licensed and Grace"),
    }
}

fn registry_err(e: RegistryError) -> (StatusCode, Json<Value>) {
    let status = match &e {
        RegistryError::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "ok": false, "error": e.to_string() })))
}

/// POST /api/admin/plugins — install (or upgrade) a plugin. Multipart fields:
/// - `name` (required): must match `^[a-z0-9][a-z0-9_-]{1,63}$`
/// - `version_label` (required): operator-facing label; sha256 is the real
///   version identity
/// - `capabilities` (optional): JSON array of capability tokens, e.g. `["log"]`
/// - `hooks` (required): JSON array of `{"name": "...", "abi": 1}`
/// - `wasm` (required): the `.wasm` binary
///
/// Lands DISABLED — the registry never auto-enables an install; an explicit
/// `enable` call is required.
///
pub async fn install(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Some(runtime) = state.plugins.as_ref() else {
        return Err(runtime_off());
    };
    require_plugin_license(&state)?;

    let mut name: Option<String> = None;
    let mut version_label: Option<String> = None;
    let mut capabilities_raw: Option<String> = None;
    let mut hooks_raw: Option<String> = None;
    let mut wasm_bytes: Option<Vec<u8>> = None;
    let mut signature: Option<String> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("multipart error: {e}") })),
        )
    })? {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "name" => name = Some(field.text().await.unwrap_or_default()),
            "version_label" => version_label = Some(field.text().await.unwrap_or_default()),
            "capabilities" => capabilities_raw = Some(field.text().await.unwrap_or_default()),
            "hooks" => hooks_raw = Some(field.text().await.unwrap_or_default()),
            "signature" => signature = Some(field.text().await.unwrap_or_default()),
            "wasm" | "file" => {
                wasm_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| {
                            (
                                StatusCode::BAD_REQUEST,
                                Json(json!({ "ok": false, "error": format!("read error: {e}") })),
                            )
                        })?
                        .to_vec(),
                );
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let name = name.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing field: name" })),
        )
    })?;
    if !is_valid_plugin_name(&name) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": format!(
                    "invalid plugin name '{name}' — must match ^[a-z0-9][a-z0-9_-]{{1,63}}$"
                )
            })),
        ));
    }
    let version_label = version_label.unwrap_or_default();
    let bytes = wasm_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing field: wasm" })),
        )
    })?;

    let capabilities: Vec<String> = match capabilities_raw {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("capabilities must be a JSON array of strings: {e}")
                })),
            )
        })?,
        _ => Vec::new(),
    };
    let unknown = unknown_capabilities(&capabilities);
    if !unknown.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": format!(
                    "unknown capability token(s): {} — valid capabilities: {}",
                    unknown.join(", "),
                    KNOWN_CAPABILITIES.join(", ")
                ),
                "invalid_capabilities": unknown,
                "valid_capabilities": KNOWN_CAPABILITIES,
            })),
        ));
    }
    let hooks: Vec<PluginHookRef> = match hooks_raw {
        Some(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("hooks must be a JSON array of {{name, abi}}: {e}")
                })),
            )
        })?,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "missing field: hooks" })),
            ))
        }
    };

    // Artifact-authenticity gate (WASM_ARCHITECTURE.md §9, layer 1). A
    // `signature` field, when present, MUST be the 9eck authority's Ed25519
    // signature over this artifact's sha256 (domain-prefixed) — a bad one is
    // rejected, never ignored. `ECK_PLUGIN_REQUIRE_SIGNED=1` makes the field
    // mandatory (marketplace posture); default keeps self-authored unsigned
    // plugins installable (they are the tenant's own code, and the admin JWT
    // + audit trail already own that risk).
    let sha256_hex = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(&bytes))
    };
    let require_signed = std::env::var("ECK_PLUGIN_REQUIRE_SIGNED")
        .map(|v| v == "1")
        .unwrap_or(false);
    let signature = signature.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let signed = match &signature {
        Some(sig) => {
            if !eck_core::licensing::verify_plugin_artifact(
                &eck_core::licensing::node_issuer_pubkey(),
                &sha256_hex,
                sig,
            ) {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "ok": false,
                        "error": "plugin signature does not verify against the 9eck authority for this artifact"
                    })),
                ));
            }
            true
        }
        None if require_signed => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "ok": false,
                    "error": "this node requires authority-signed plugins (ECK_PLUGIN_REQUIRE_SIGNED=1) — missing field: signature"
                })),
            ));
        }
        None => false,
    };

    let plugin = runtime
        .registry
        .install(
            &state.db,
            &bytes,
            &name,
            &version_label,
            hooks,
            capabilities,
            &state.instance_id,
        )
        .await
        .map_err(registry_err)?;

    eck_core::audit::append_soft(
        &state.db,
        &state.server_identity,
        &eck_core::audit::wms_chain(&state.instance_id),
        &claims.sub,
        "plugin.install",
        "plugin_lifecycle",
        &format!("Installed plugin '{}' ({})", plugin.name, plugin.sha256),
        json!({
            "name": plugin.name,
            "sha256": plugin.sha256,
            "capabilities": plugin.capabilities,
            "authority_signed": signed,
        }),
    )
    .await;

    info!(
        "[plugins] installed '{}' sha256={} authority_signed={} (lands disabled)",
        plugin.name, plugin.sha256, signed
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(&plugin).unwrap_or_default()),
    ))
}

/// GET /api/admin/plugins — list every registered plugin, each annotated with
/// this node's local failure-gate state (the kiosk-OTA-lesson auto-disable
/// latch — process-local, never meshed).
pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let Some(runtime) = state.plugins.as_ref() else {
        return Err(runtime_off());
    };

    let statuses = runtime.registry.list(&state.db).await.map_err(registry_err)?;
    let items: Vec<Value> = statuses
        .into_iter()
        .map(|s| {
            json!({
                "plugin": s.plugin,
                "locally_disabled": s.locally_disabled,
            })
        })
        .collect();

    Ok(Json(json!({ "ok": true, "plugins": items })))
}

/// POST /api/admin/plugins/:name/enable — flip the meshed `enabled` flag and
/// clear this node's failure-gate latch (an explicit re-enable is "give it
/// another chance").
pub async fn enable(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let Some(runtime) = state.plugins.as_ref() else {
        return Err(runtime_off());
    };
    require_plugin_license(&state)?;

    let plugin = runtime.registry.enable(&state.db, &name).await.map_err(registry_err)?;

    eck_core::audit::append_soft(
        &state.db,
        &state.server_identity,
        &eck_core::audit::wms_chain(&state.instance_id),
        &claims.sub,
        "plugin.enable",
        "plugin_lifecycle",
        &format!("Enabled plugin '{}'", plugin.name),
        json!({
            "name": plugin.name,
            "sha256": plugin.sha256,
            "capabilities": plugin.capabilities,
        }),
    )
    .await;

    info!("[plugins] enabled '{}'", plugin.name);
    Ok(Json(serde_json::to_value(&plugin).unwrap_or_default()))
}

/// POST /api/admin/plugins/:name/disable — flip the meshed `enabled` flag off.
/// Distinct from the node-local failure-gate latch.
pub async fn disable(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<Claims>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let Some(runtime) = state.plugins.as_ref() else {
        return Err(runtime_off());
    };

    let plugin = runtime.registry.disable(&state.db, &name).await.map_err(registry_err)?;

    eck_core::audit::append_soft(
        &state.db,
        &state.server_identity,
        &eck_core::audit::wms_chain(&state.instance_id),
        &claims.sub,
        "plugin.disable",
        "plugin_lifecycle",
        &format!("Disabled plugin '{}'", plugin.name),
        json!({
            "name": plugin.name,
            "sha256": plugin.sha256,
            "capabilities": plugin.capabilities,
        }),
    )
    .await;

    info!("[plugins] disabled '{}'", plugin.name);
    Ok(Json(serde_json::to_value(&plugin).unwrap_or_default()))
}
