use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use extism::{CompiledPlugin, Manifest, Plugin, PluginBuilder, Wasm};
use tracing::debug;

use super::{host, HostIdentity, PluginCallContext, PluginError};
use crate::utils::filestore::sha256_hex;

/// Size of one wasm linear-memory page (64 KiB) — the granularity the memory
/// cap is expressed in to wasmtime.
const WASM_PAGE_BYTES: u64 = 64 * 1024;

/// Default wall-clock budget per call. Generous for the first hook surface
/// (`INGEST_TRANSFORM`, a background flow); hot-path hooks tune this down.
const DEFAULT_TIMEOUT_MS: u64 = 100;

/// Default linear-memory ceiling per call (32 MiB).
const DEFAULT_MAX_MEMORY_BYTES: u64 = 32 * 1024 * 1024;

/// Default deterministic instruction cap. A backstop for reproducibility and
/// runaway loops that is orthogonal to the wall-clock timer; whichever trips
/// first wins. Per-hook tuning is a later concern.
const DEFAULT_FUEL: u64 = 1_000_000_000;

/// Per-call resource limits, all three enforced inside [`PluginEngine::run`].
///
/// The limits are baked into the compiled module (memory + timeout live on the
/// extism manifest, fuel on the builder), so they are fixed at first compile of
/// a given `(sha, capabilities)` within one engine. Different limit profiles →
/// different engines.
#[derive(Debug, Clone, Copy)]
pub struct PluginLimits {
    /// Wall-clock budget in milliseconds, enforced via extism epoch interruption.
    pub timeout_ms: u64,
    /// Deterministic instruction cap (wasmtime fuel). `None` disables the fuel
    /// meter and leaves only the wall-clock + memory guards.
    pub fuel: Option<u64>,
    /// Linear-memory ceiling in bytes, rounded down to whole 64 KiB pages.
    pub max_memory_bytes: u64,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            fuel: Some(DEFAULT_FUEL),
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
        }
    }
}

/// Runs sandboxed WASM plugins with a compiled-module cache.
///
/// Compilation (cranelift → native code) is the expensive step, so a
/// [`CompiledPlugin`] is compiled once and cached by `(sha256, capability-set)`.
/// Instantiation is cheap and every call gets a **fresh instance**: no
/// cross-call state, no state bleed between tenants' data, deterministic replay.
///
/// The cache key folds in the granted capability set (not just the sha) so that
/// narrowing an existing plugin's capabilities can never be served a stale
/// module compiled with the wider host-fn set — a different capability set is a
/// different key, a fresh compile, and a fresh capability pre-check.
///
/// The engine is `Send + Sync`; share one behind an `Arc`.
pub struct PluginEngine {
    limits: PluginLimits,
    /// Node identity baked into every plugin instance's host functions.
    identity: HostIdentity,
    /// `(sha256, cap-fingerprint)` → compiled module. `Arc` so a call clones a
    /// cheap handle and drops the lock before the (uncontended) instantiate.
    cache: RwLock<HashMap<String, Arc<CompiledPlugin>>>,
}

impl PluginEngine {
    /// Create an engine with the given per-call limits and node identity.
    pub fn new(limits: PluginLimits, identity: HostIdentity) -> Self {
        Self {
            limits,
            identity,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create an engine with the default limits (100 ms / 1e9 fuel / 32 MiB).
    pub fn with_default_limits(identity: HostIdentity) -> Self {
        Self::new(PluginLimits::default(), identity)
    }

    /// The limits this engine enforces on every call.
    pub fn limits(&self) -> PluginLimits {
        self.limits
    }

    /// Whether a module with this sha256 + capability set is already compiled.
    pub fn is_cached(&self, sha256: &str, capabilities: &[String]) -> bool {
        let caps: BTreeSet<String> = capabilities.iter().cloned().collect();
        let key = cache_key(&sha256.to_ascii_lowercase(), &caps);
        self.read_cache().contains_key(&key)
    }

    /// Run one plugin hook and return its output bytes.
    ///
    /// - `sha256` — hex content-address of the wasm; part of the cache key.
    /// - `capabilities` — the host-function capabilities this plugin is granted;
    ///   also part of the cache key. A host-fn import outside this set is rejected
    ///   as [`PluginError::CapabilityDenied`] before instantiation.
    /// - `provide_bytes` — called ONLY on a cache miss to supply the wasm; the
    ///   engine verifies it hashes to `sha256` before compiling or caching it.
    /// - `hook_fn_name` — the exported function to invoke.
    /// - `input` — the input envelope passed to the plugin verbatim.
    /// - `ctx` — per-invocation context (audit correlation, host-fn guards);
    ///   surfaced to host functions via `call_with_host_context`.
    ///
    /// Execution is synchronous and CPU-bound — async callers should wrap this in
    /// `spawn_blocking`. A trap, timeout, OOM, fuel exhaustion, or capability
    /// violation returns the matching [`PluginError`] rather than unwinding.
    pub fn run<F, E>(
        &self,
        sha256: &str,
        capabilities: &[String],
        provide_bytes: F,
        hook_fn_name: &str,
        input: &[u8],
        ctx: PluginCallContext,
    ) -> Result<Vec<u8>, PluginError>
    where
        F: FnOnce() -> Result<Vec<u8>, E>,
        E: std::fmt::Display,
    {
        let sha_lower = sha256.to_ascii_lowercase();
        let caps: BTreeSet<String> = capabilities.iter().cloned().collect();
        let compiled = self.compiled(&sha_lower, &caps, provide_bytes)?;

        // Fresh instance per invocation — the whole point of caching the
        // *compiled* module rather than a live instance.
        let mut plugin = Plugin::new_from_compiled(&compiled)
            .map_err(|e| PluginError::InvalidModule(format!("instantiation failed: {e:#}")))?;

        // call_with_host_context threads the per-call context to host functions
        // (audit correlation + the host_log rate guard) without recompiling.
        plugin
            .call_with_host_context::<&[u8], Vec<u8>, PluginCallContext>(hook_fn_name, input, ctx)
            .map_err(|e| self.classify_call_error(e, hook_fn_name))
    }

    /// Resolve the compiled module for `(sha256, caps)`, compiling + caching on a miss.
    fn compiled<F, E>(
        &self,
        sha_lower: &str,
        caps: &BTreeSet<String>,
        provide_bytes: F,
    ) -> Result<Arc<CompiledPlugin>, PluginError>
    where
        F: FnOnce() -> Result<Vec<u8>, E>,
        E: std::fmt::Display,
    {
        let key = cache_key(sha_lower, caps);

        // Fast path: already compiled for this sha + capability set.
        if let Some(compiled) = self.read_cache().get(&key).cloned() {
            return Ok(compiled);
        }

        // Miss: pull the bytes from the caller and verify them before we trust
        // (compile / cache) anything — a wrong-address blob is rejected here.
        let bytes = provide_bytes().map_err(|e| PluginError::BytesUnavailable(e.to_string()))?;
        let computed = sha256_hex(&bytes);
        if computed != sha_lower {
            return Err(PluginError::HashMismatch {
                expected: sha_lower.to_string(),
                computed,
            });
        }

        debug!(
            "PluginEngine: compiling module {} ({} bytes, caps={:?})",
            sha_lower,
            bytes.len(),
            caps
        );
        let compiled = Arc::new(self.compile(&bytes, caps)?);

        // Insert, tolerating a racing compile of the same key: the bytes are
        // content-addressed, so whichever Arc wins is byte-identical.
        let stored = self
            .write_cache()
            .entry(key)
            .or_insert_with(|| compiled.clone())
            .clone();
        Ok(stored)
    }

    /// Compile wasm into a cached module with this engine's limits + the host
    /// functions the granted capabilities allow. The capability pre-check runs
    /// FIRST so a denied import costs nothing to reject.
    fn compile(&self, wasm: &[u8], caps: &BTreeSet<String>) -> Result<CompiledPlugin, PluginError> {
        host::check_capabilities(wasm, caps)?;

        // wasmtime wants the memory cap as a page count; clamp to at least one
        // page and to the u32 range (a > 256 TiB cap would be absurd anyway).
        let max_pages = (self.limits.max_memory_bytes / WASM_PAGE_BYTES).max(1);
        let max_pages = u32::try_from(max_pages).unwrap_or(u32::MAX);

        let manifest = Manifest::new([Wasm::data(wasm.to_vec())])
            .with_memory_max(max_pages)
            .with_timeout(Duration::from_millis(self.limits.timeout_ms))
            // Belt-and-suspenders: no HTTP host feature is compiled in, but deny
            // all egress on the manifest too so an enabled feature can't leak.
            .disallow_all_hosts();

        let host_fns = host::host_functions(caps, &self.identity);
        let mut builder = PluginBuilder::new(manifest)
            .with_wasi(false)
            .with_functions(host_fns);
        if let Some(fuel) = self.limits.fuel {
            builder = builder.with_fuel_limit(fuel);
        }

        builder
            .compile()
            .map_err(|e| PluginError::InvalidModule(format!("{e:#}")))
    }

    /// Map an extism call error onto a typed [`PluginError`]. Extism collapses
    /// each enforced limit onto a stable root-cause string (see the extism
    /// runtime): `"timeout"`, `"oom"`, `"plugin ran out of fuel"`, and
    /// `"Function not found: …"`; anything else is a genuine trap. (Capability
    /// violations are caught earlier, at compile, never here.)
    fn classify_call_error(&self, err: extism::Error, hook_fn_name: &str) -> PluginError {
        let root = err.root_cause().to_string();
        match root.as_str() {
            "timeout" => PluginError::Timeout(self.limits.timeout_ms),
            "oom" => PluginError::MemoryExceeded(self.limits.max_memory_bytes),
            "plugin ran out of fuel" => PluginError::FuelExhausted,
            other if other.starts_with("Function not found") => {
                PluginError::MissingExport(hook_fn_name.to_string())
            }
            _ => PluginError::Trap(format!("{err:#}")),
        }
    }

    /// Read guard that recovers from a poisoned lock — the map is only ever
    /// mutated with simple, panic-free operations, so poison is benign and must
    /// not wedge the engine (or violate the no-panic contract).
    fn read_cache(&self) -> RwLockReadGuard<'_, HashMap<String, Arc<CompiledPlugin>>> {
        self.cache.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write_cache(&self) -> RwLockWriteGuard<'_, HashMap<String, Arc<CompiledPlugin>>> {
        self.cache.write().unwrap_or_else(|p| p.into_inner())
    }
}

/// Cache key = sha256 hex + a hash of the sorted capability set. Sorting makes
/// the fingerprint order-independent; hashing keeps the key bounded.
fn cache_key(sha_lower: &str, caps: &BTreeSet<String>) -> String {
    let joined = caps.iter().map(String::as_str).collect::<Vec<_>>().join(",");
    format!("{sha_lower}:{}", sha256_hex(joined.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> HostIdentity {
        HostIdentity::new("test-node", "dev")
    }

    fn ctx(hook: &str) -> PluginCallContext {
        PluginCallContext::new(hook, "test-correlation", None)
    }

    /// Compile hand-written WAT to a wasm binary at test time (hermetic — no
    /// network, no checked-in fixtures). Fixtures wrap the extism ABI by hand:
    /// they import kernel functions from `extism:host/env` (and, for host-fn
    /// tests, our functions from `extism:host/user`) and drive I/O through them.
    fn wasm(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("test WAT should compile")
    }

    /// Happy path: echoes the input envelope back through the extism ABI.
    const ECHO_WAT: &str = r#"
        (module
          (import "extism:host/env" "input_length" (func $input_length (result i64)))
          (import "extism:host/env" "input_load_u8" (func $input_load_u8 (param i64) (result i32)))
          (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
          (import "extism:host/env" "store_u8" (func $store_u8 (param i64 i32)))
          (import "extism:host/env" "output_set" (func $output_set (param i64 i64)))
          (func (export "transform") (result i32)
            (local $len i64) (local $out i64) (local $i i64)
            (local.set $len (call $input_length))
            (local.set $out (call $alloc (local.get $len)))
            (block $done
              (loop $loop
                (br_if $done (i64.ge_u (local.get $i) (local.get $len)))
                (call $store_u8
                  (i64.add (local.get $out) (local.get $i))
                  (call $input_load_u8 (local.get $i)))
                (local.set $i (i64.add (local.get $i) (i64.const 1)))
                (br $loop)))
            (call $output_set (local.get $out) (local.get $len))
            (i32.const 0)))
    "#;

    /// A tight infinite loop — used for both the fuel and timeout tests.
    const LOOP_WAT: &str = r#"
        (module
          (func (export "run") (result i32)
            (loop $l (br $l))
            (i32.const 0)))
    "#;

    /// Asks the kernel allocator for 512 MiB in one shot — far past any sane cap.
    const MEM_HOG_WAT: &str = r#"
        (module
          (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
          (func (export "run") (result i32)
            (drop (call $alloc (i64.const 536870912)))
            (i32.const 0)))
    "#;

    /// Calls `host_anonymize(input)` and returns its masked output. Imports our
    /// host function from `extism:host/user` (needs cap "anonymize").
    const ANON_WAT: &str = r#"
        (module
          (import "extism:host/env" "input_offset" (func $input_offset (result i64)))
          (import "extism:host/env" "length" (func $length (param i64) (result i64)))
          (import "extism:host/env" "output_set" (func $output_set (param i64 i64)))
          (import "extism:host/user" "host_anonymize" (func $host_anonymize (param i64) (result i64)))
          (func (export "mask") (result i32)
            (local $out i64)
            (local.set $out (call $host_anonymize (call $input_offset)))
            (call $output_set (local.get $out) (call $length (local.get $out)))
            (i32.const 0)))
    "#;

    /// Calls `host_log` 150 times (needs cap "log"). Exercises the rate guard.
    const LOG_SPAM_WAT: &str = r#"
        (module
          (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
          (import "extism:host/env" "store_u8" (func $store_u8 (param i64 i32)))
          (import "extism:host/user" "host_log" (func $host_log (param i64 i64)))
          (func (export "spam") (result i32)
            (local $msg i64) (local $i i64)
            (local.set $msg (call $alloc (i64.const 2)))
            (call $store_u8 (local.get $msg) (i32.const 104))
            (call $store_u8 (i64.add (local.get $msg) (i64.const 1)) (i32.const 105))
            (block $done
              (loop $l
                (br_if $done (i64.ge_u (local.get $i) (i64.const 150)))
                (call $host_log (i64.const 1) (local.get $msg))
                (local.set $i (i64.add (local.get $i) (i64.const 1)))
                (br $l)))
            (i32.const 0)))
    "#;

    #[test]
    fn happy_path_echo_roundtrip_and_cache_hit() {
        let bytes = wasm(ECHO_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::with_default_limits(identity());
        let input = br#"{"source":"zoho","raw_payload":{"x":1}}"#;

        let out = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "transform", input, ctx("transform"))
            .expect("echo plugin should run");
        assert_eq!(out, input);
        assert!(engine.is_cached(&sha, &[]));

        // Second call is a cache hit: the provider must NOT be consulted.
        let out2 = engine
            .run(
                &sha,
                &[],
                || Err::<Vec<u8>, String>("provider must not run on a cache hit".into()),
                "transform",
                input,
                ctx("transform"),
            )
            .expect("cached echo plugin should run");
        assert_eq!(out2, input);
    }

    #[test]
    fn infinite_loop_trips_fuel() {
        let bytes = wasm(LOOP_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::new(
            PluginLimits {
                fuel: Some(5_000_000),
                timeout_ms: 60_000,
                ..PluginLimits::default()
            },
            identity(),
        );

        let err = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "run", b"", ctx("run"))
            .expect_err("an infinite loop must not return Ok");
        assert!(matches!(err, PluginError::FuelExhausted), "got {err:?}");
    }

    #[test]
    fn infinite_loop_trips_timeout() {
        let bytes = wasm(LOOP_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::new(
            PluginLimits {
                fuel: None,
                timeout_ms: 100,
                ..PluginLimits::default()
            },
            identity(),
        );

        let err = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "run", b"", ctx("run"))
            .expect_err("an infinite loop must not return Ok");
        assert!(matches!(err, PluginError::Timeout(100)), "got {err:?}");
    }

    #[test]
    fn oversized_alloc_trips_memory_cap() {
        let bytes = wasm(MEM_HOG_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::new(
            PluginLimits {
                fuel: None,
                timeout_ms: 5_000,
                max_memory_bytes: 32 * 1024 * 1024,
            },
            identity(),
        );

        let err = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "run", b"", ctx("run"))
            .expect_err("a 512 MiB alloc must not succeed under a 32 MiB cap");
        assert!(matches!(err, PluginError::MemoryExceeded(_)), "got {err:?}");
    }

    #[test]
    fn wrong_sha256_is_rejected_without_caching() {
        let bytes = wasm(ECHO_WAT);
        let wrong_sha = sha256_hex(b"these are not the plugin bytes");
        let engine = PluginEngine::with_default_limits(identity());

        let err = engine
            .run(&wrong_sha, &[], || Ok::<_, String>(bytes.clone()), "transform", b"x", ctx("transform"))
            .expect_err("bytes that don't match the claimed sha must be rejected");
        assert!(matches!(err, PluginError::HashMismatch { .. }), "got {err:?}");
        assert!(!engine.is_cached(&wrong_sha, &[]));
    }

    #[test]
    fn missing_export_is_reported() {
        let bytes = wasm(ECHO_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::with_default_limits(identity());

        let err = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "does_not_exist", b"x", ctx("does_not_exist"))
            .expect_err("calling an absent export must error");
        assert!(matches!(err, PluginError::MissingExport(_)), "got {err:?}");
    }

    #[test]
    fn host_anonymize_masks_pii_through_the_abi() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let bytes = wasm(ANON_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::with_default_limits(identity());
        let caps = vec!["anonymize".to_string()];
        let input = "Katharina Niedermayer schreibt an a.b@example.de".as_bytes();

        let out = engine
            .run(&sha, &caps, || Ok::<_, String>(bytes.clone()), "mask", input, ctx("mask"))
            .expect("granted host_anonymize plugin should run");
        let out = String::from_utf8(out).expect("masked output is utf8");
        assert!(!out.contains("Katharina"), "person leaked: {out}");
        assert!(!out.contains("a.b@example.de"), "email leaked: {out}");
        assert!(out.contains("Name_"), "no Name token: {out}");
        assert!(out.contains("Email_"), "no Email token: {out}");
    }

    #[test]
    fn ungranted_host_import_is_capability_denied() {
        let bytes = wasm(ANON_WAT); // imports extism:host/user::host_anonymize
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::with_default_limits(identity());

        // No capabilities granted → the host_anonymize import is rejected before
        // instantiation, as CapabilityDenied (not InvalidModule).
        let err = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "mask", b"x", ctx("mask"))
            .expect_err("importing an ungranted host fn must be denied");
        match err {
            PluginError::CapabilityDenied { import } => {
                assert!(import.contains("host_anonymize"), "unexpected import: {import}");
            }
            other => panic!("expected CapabilityDenied, got {other:?}"),
        }
        assert!(!engine.is_cached(&sha, &[]));
    }

    #[test]
    fn host_log_rate_guard_does_not_break_execution() {
        let bytes = wasm(LOG_SPAM_WAT);
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::with_default_limits(identity());
        let caps = vec!["log".to_string()];

        // 150 host_log calls: the guard drops everything past the budget but the
        // call itself must complete normally.
        engine
            .run(&sha, &caps, || Ok::<_, String>(bytes.clone()), "spam", b"", ctx("spam"))
            .expect("spamming host_log must not break the call");
    }

    /// End-to-end against the REAL demo plugin (design §3/§8:
    /// `examples/plugins/ingest_transform_demo`), not a hand-written WAT
    /// fixture — proves an actual extism-pdk (Rust) build round-trips through
    /// this engine, not just our own kernel-ABI-by-hand fixtures.
    ///
    /// Not run by default: it needs a real compiled `.wasm`, which needs the
    /// wasm32 rustup target. Build it with
    /// `scripts/build-demo-plugin.ps1`, then run:
    ///   `$env:ECK_DEMO_PLUGIN_WASM = "<path from the script's output>"`
    ///   `cargo test -p eck_core --lib -- --ignored demo_ingest_transform_plugin`
    #[test]
    #[ignore = "requires ECK_DEMO_PLUGIN_WASM (built via scripts/build-demo-plugin.ps1)"]
    fn demo_ingest_transform_plugin_maps_custom_field_to_allowlisted_meta() {
        let path = std::env::var("ECK_DEMO_PLUGIN_WASM")
            .expect("set ECK_DEMO_PLUGIN_WASM to the built demo plugin's .wasm path (see scripts/build-demo-plugin.ps1)");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let sha = sha256_hex(&bytes);
        let engine = PluginEngine::with_default_limits(identity());

        // The exact envelope `encode_ingest_input` produces: abi-tagged,
        // fake vendor custom field the demo plugin looks for.
        let input = br#"{"abi":1,"source":"zoho","raw_payload":{"cf":{"cf_acme_model":"770"}},"existing_meta":{}}"#;
        let out = engine
            .run(&sha, &[], || Ok::<_, String>(bytes.clone()), "ingest_transform", input, ctx("ingest_transform"))
            .expect("demo plugin should run");
        let out: serde_json::Value =
            serde_json::from_slice(&out).expect("demo plugin output should be JSON");
        assert_eq!(out["meta_patch"]["model"], serde_json::json!("770"));
        assert_eq!(out["meta_patch"]["category"], serde_json::json!("hardware"));
        assert!(!out["notes"].as_array().unwrap().is_empty());
    }
}
