//! WASM plugin runtime.
//!
//! This is the executable form of the standing product rule *client-specific
//! behavior lives in config, not in code*: integrators extend the system with
//! sandboxed WebAssembly plugins instead of forking. The approved design lives
//! in `.eck/WASM_ARCHITECTURE.md` — §1 (runtime + limits), §2 (placement),
//! §3 (hook surfaces), §4 (host functions), §5 (storage/registry/lifecycle).
//!
//! Runtime: [`extism`](https://extism.org) on wasmtime/cranelift, built with
//! `default-features = false` so no HTTP/filesystem/TLS host surface is linked
//! (`cargo tree -i openssl-sys` must stay empty — see `core/Cargo.toml`).
//!
//! Module map (P1 + P2):
//! - [`engine`] — `PluginEngine`: compiled-module cache keyed by
//!   `(sha256, capability-set)`, fresh instance per call, hard limits.
//! - [`host`] — the v1 host-fn set (`host_log`, `host_anonymize`), linked per
//!   capability claim; the pre-instantiation capability check.
//! - [`hooks`] — typed hook ABI contracts (`INGEST_TRANSFORM`) + abi versioning.
//! - [`manifest`] — the `wasm_plugin` registry record type.
//! - [`registry`] — install / enable / disable / resolve over the CAS
//!   [`FileStore`](crate::utils::filestore::FileStore) + node-local failure gating.
//!
//! Runtime gate for the wider system: `ECK_PLUGIN_RUNTIME=1` (wired at the wms
//! call sites in a later phase; the engine itself is inert until invoked).

mod engine;
mod host;
mod hooks;
mod manifest;
mod registry;

pub use engine::{PluginEngine, PluginLimits};
pub use hooks::{
    decode_ingest_input, decode_ingest_output, encode_ingest_input, HookError, HookKind,
    IngestTransformInput, IngestTransformOutput, HOOK_ABI_VERSION,
};
// Capability tokens ONLY — the rest of `host` (host-fn linking, the
// pre-instantiation import check) stays internal to the engine. Re-exported so
// callers validating a manifest's `capabilities` claim (e.g. the wms admin
// install handler) have one source of truth instead of hardcoding "log"/
// "anonymize" string literals.
pub use host::{CAP_ANONYMIZE, CAP_LOG};
pub use manifest::{PluginHookRef, WasmPlugin, WASM_PLUGIN_TABLE};
pub use registry::{PluginRegistry, PluginStatus, RegistryError};

use thiserror::Error;

/// Immutable host identity baked into every plugin instance at compile time
/// (via extism `UserData`). A plugin never passes identity as an argument, so
/// it cannot impersonate a node — host functions read it from here. Cloned into
/// each instance; keep it small.
#[derive(Debug, Clone)]
pub struct HostIdentity {
    /// This node's mesh instance id (canonical UUID).
    pub instance_id: String,
    /// This node's mesh role (e.g. "home", "cache", "kiosk").
    pub node_role: String,
}

impl HostIdentity {
    pub fn new(instance_id: impl Into<String>, node_role: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            node_role: node_role.into(),
        }
    }
}

/// Per-invocation context, passed through `call_with_host_context` so host
/// functions can see *this call's* metadata (for audit correlation and log
/// attribution) and enforce per-invocation guards — without recompiling the
/// module. Distinct from [`HostIdentity`], which is constant per node.
///
/// A fresh one is constructed for every [`PluginEngine::run`] call, so the
/// internal guard counters reset per invocation.
#[derive(Debug)]
pub struct PluginCallContext {
    /// Human-facing hook label for logs/audit (e.g. "ingest_transform").
    pub hook: String,
    /// Correlation id tying this execution to an audit/trace record.
    pub correlation_id: String,
    /// Optional source tag (e.g. the ingest source "zoho"/"exact").
    pub source_tag: Option<String>,

    /// `host_log` calls made so far this invocation (guard counter).
    pub(crate) log_calls: u32,
    /// Whether the one-time "log budget exceeded" warning has been emitted.
    pub(crate) log_warned: bool,
}

impl PluginCallContext {
    pub fn new(
        hook: impl Into<String>,
        correlation_id: impl Into<String>,
        source_tag: Option<String>,
    ) -> Self {
        Self {
            hook: hook.into(),
            correlation_id: correlation_id.into(),
            source_tag,
            log_calls: 0,
            log_warned: false,
        }
    }
}

/// Every way a plugin call can fail.
///
/// A plugin trap, timeout, out-of-memory, or fuel exhaustion is surfaced here
/// as an ordinary value — it must NEVER unwind into the host. There are no
/// `unwrap`/`panic` calls on any engine or host-fn path: a hostile or buggy
/// plugin can degrade its own hook, never the process running it.
#[derive(Debug, Error)]
pub enum PluginError {
    /// The supplied bytes did not hash to the sha256 the caller claimed. The
    /// module is rejected and NOT cached (CAS poison guard).
    #[error("plugin bytes do not match expected sha256 (expected {expected}, computed {computed})")]
    HashMismatch { expected: String, computed: String },

    /// The caller's byte provider failed on a cache miss (e.g. the CAS/mesh
    /// blob could not be resolved).
    #[error("could not obtain plugin bytes on cache miss: {0}")]
    BytesUnavailable(String),

    /// The wasm failed to compile, or an instance could not be linked.
    #[error("wasm module is invalid or failed to instantiate: {0}")]
    InvalidModule(String),

    /// The module imports a host function it was not granted the capability for
    /// (or an unknown host symbol). Checked BEFORE instantiation.
    #[error("plugin imports `{import}` without the required capability")]
    CapabilityDenied { import: String },

    /// The plugin has no exported function with the requested hook name.
    #[error("plugin has no exported function named `{0}`")]
    MissingExport(String),

    /// The call exceeded its wall-clock budget (enforced via epoch interruption).
    #[error("plugin call exceeded its {0} ms wall-clock budget")]
    Timeout(u64),

    /// The call exhausted its deterministic instruction (fuel) budget.
    #[error("plugin call exhausted its instruction (fuel) budget")]
    FuelExhausted,

    /// The call tried to grow linear memory past its cap.
    #[error("plugin call exceeded its linear-memory cap ({0} bytes)")]
    MemoryExceeded(u64),

    /// The plugin trapped (unreachable, out-of-bounds, non-zero exit, an error
    /// the plugin set itself, …). The message carries the full wasmtime chain.
    #[error("plugin trapped during execution: {0}")]
    Trap(String),
}
