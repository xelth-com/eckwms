//! Plugin registry (design §5): the `wasm_plugin` records plus the CAS blob and
//! the node-local failure gate.
//!
//! - **Blob storage**: the `.wasm` binary goes through the existing CAS
//!   [`FileStore`](crate::utils::filestore::FileStore); the sha256 is the
//!   version identity and the on-disk path is a pure function of it, so
//!   [`PluginRegistry::resolve`] reconstructs the path from the sha alone.
//! - **Records**: one [`WasmPlugin`] per plugin name; install lands it disabled,
//!   `enable`/`disable` flip the flag.
//! - **Failure gating** (the kiosk-OTA lesson — a bad plugin must never wedge a
//!   node): an in-process consecutive-failure counter per sha; N in a row →
//!   node-local auto-disable (a local latch, never meshed). Callers consult
//!   [`is_locally_disabled`](PluginRegistry::is_locally_disabled) and skip
//!   execution. The latch is process-local and DELIBERATELY does NOT persist
//!   across restarts (settled P5 decision, `.eck/WASM_ARCHITECTURE.md` §10): the
//!   meshed `enabled` flag is the durable fleet-wide truth (admin enables once,
//!   fleet-wide), while the latch is one node's transient "this build keeps
//!   trapping here" — a restart gives the plugin a fresh chance and a genuinely
//!   bad one re-trips within N fires; an admin `disable` (or `enable`, which
//!   also clears the latch) is the durable control. Persisting the latch would
//!   turn a transient/node-specific fault into a sticky one that survives the
//!   very restart that might fix it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use thiserror::Error;
use tracing::warn;

use super::manifest::{PluginHookRef, WasmPlugin, WASM_PLUGIN_TABLE};
use crate::db::SurrealDb;
use crate::sync::conflict::next_local_vclock;
use crate::sync::engine::SyncEngine;
use crate::utils::filestore::{sha256_hex, verify_sha256, FileStore};

/// Consecutive failures before a plugin auto-disables on this node.
const DEFAULT_MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Errors from registry operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("database error: {0}")]
    Db(#[from] surrealdb::Error),

    #[error("plugin blob storage error: {0}")]
    Storage(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("plugin `{0}` not found")]
    NotFound(String),
}

/// A registry record plus this node's live view of it (the failure-gate latch).
#[derive(Debug, Clone)]
pub struct PluginStatus {
    pub plugin: WasmPlugin,
    /// Node-local: auto-disabled after repeated failures on THIS process.
    pub locally_disabled: bool,
}

#[derive(Default)]
struct GateState {
    consecutive_failures: u32,
    locally_disabled: bool,
}

/// The registry. Holds the CAS handle and the node-local failure gate; DB access
/// is passed per call as `&SurrealDb`, matching the house handler convention.
pub struct PluginRegistry {
    filestore: Arc<FileStore>,
    max_consecutive_failures: u32,
    gate: Mutex<HashMap<String, GateState>>,
    /// This node's mesh instance id — the `_vclock` component advanced when THIS
    /// node authors a registry write (`enable`/`disable`; `install` takes the
    /// author id explicitly as `home_instance_id`). Empty when unconfigured
    /// (tests); set via [`with_instance_id`](Self::with_instance_id) from wms
    /// `main.rs` with `AppState::instance_id`.
    instance_id: String,
    /// Optional mesh handle for the LAZY binary fetch (design §6). On a local
    /// CAS miss, [`resolve`](Self::resolve) pulls the `.wasm` by sha256 from a
    /// peer via [`SyncEngine::fetch_file_from_peers`] — the SAME on-demand path
    /// attachments hydrate through (`MeshClient::fetch_file` → `GET
    /// /api/mesh/file/:hash`, direct then relay). `None` (tests / no mesh) makes
    /// a local miss a hard `resolve` error, which the engine surfaces as
    /// [`PluginError::BytesUnavailable`](super::PluginError::BytesUnavailable).
    /// Only ever consulted at hook time — NEVER at node startup.
    mesh: Option<Arc<SyncEngine>>,
}

impl PluginRegistry {
    /// Registry with the default failure threshold ([`DEFAULT_MAX_CONSECUTIVE_FAILURES`]).
    pub fn new(filestore: Arc<FileStore>) -> Self {
        Self::with_threshold(filestore, DEFAULT_MAX_CONSECUTIVE_FAILURES)
    }

    pub fn with_threshold(filestore: Arc<FileStore>, max_consecutive_failures: u32) -> Self {
        Self {
            filestore,
            max_consecutive_failures: max_consecutive_failures.max(1),
            gate: Mutex::new(HashMap::new()),
            instance_id: String::new(),
            mesh: None,
        }
    }

    /// Builder: set this node's mesh instance id (the `_vclock` component
    /// advanced on `enable`/`disable`). Call from wms `main.rs` with
    /// `state.instance_id`.
    pub fn with_instance_id(mut self, instance_id: &str) -> Self {
        self.instance_id = instance_id.to_string();
        self
    }

    /// Builder: wire the mesh handle [`resolve`](Self::resolve) uses to lazily
    /// fetch a missing `.wasm` blob by sha256 (design §6). Call from wms
    /// `main.rs` with the shared `Arc<SyncEngine>` — the same handle attachments
    /// hydrate through — so the registry reuses the existing mesh transport
    /// instead of standing up a parallel client.
    pub fn with_mesh(mut self, mesh: Arc<SyncEngine>) -> Self {
        self.mesh = Some(mesh);
        self
    }

    /// Install (or upgrade) a plugin: content-address the bytes into the CAS,
    /// then upsert the `wasm_plugin` record for `name`. Lands DISABLED — an
    /// explicit [`enable`](Self::enable) is required to run it. Re-installing the
    /// same name with new bytes is an upgrade (new sha on the same record).
    pub async fn install(
        &self,
        db: &SurrealDb,
        bytes: &[u8],
        name: &str,
        version_label: &str,
        hooks: Vec<PluginHookRef>,
        capabilities: Vec<String>,
        home_instance_id: &str,
    ) -> Result<WasmPlugin, RegistryError> {
        // CAS save. The filename carries the sha + `.wasm` so the on-disk path is
        // a pure function of the content — `resolve` reconstructs it from the sha.
        let sha = sha256_hex(bytes);
        let saved = self
            .filestore
            .save(bytes, &format!("{sha}.wasm"), None, None)
            .await
            .map_err(RegistryError::Storage)?;

        // Author `_vclock` (house convention `conflict::next_local_vclock`, the
        // same call `AppState::put_synced_entity` and `handlers::exact::
        // next_vclock` make): advance THIS author's component from whatever the
        // record currently holds. A fresh install lands `{home:1}`; an UPGRADE
        // (re-install the same name with new bytes) reads the existing clock and
        // strictly dominates the pre-upgrade copy on peers — otherwise the new
        // record is vclock-equal/less and a peer drops it as "local wins/equal"
        // (the partner-churn non-convergence class). `wasm_plugin` is in
        // `SYNC_ENTITY_TYPES`, and `_vclock` is in `merkle::IGNORED_FIELDS`, so
        // stamping it advances causality WITHOUT moving the content hash. The
        // installing node IS the creator/authority, so its component ==
        // `home_instance_id` here.
        let existing_vc: Vec<Value> = db
            .query("SELECT VALUE _vclock FROM type::record($tb, $id) LIMIT 1")
            .bind(("tb", WASM_PLUGIN_TABLE.to_string()))
            .bind(("id", name.to_string()))
            .await?
            .take(0)?;
        let vclock = next_local_vclock(existing_vc.first(), home_instance_id);

        // `created_at`/`updated_at` are stamped server-side via SurrealQL
        // `time::now()` rather than a client `Utc::now()` routed through
        // `serde_json::to_value(&record)` (the original approach here):
        // serde_json serializes `chrono::DateTime<Utc>` as a plain RFC3339
        // STRING, and the `WasmPlugin` `SurrealValue` derive then fails to
        // read that back out of `list()`/`get()` ("Expected datetime, got
        // string") — the same STRING-timestamp bug class documented on
        // `AppState::put_synced_entity`, here hitting a typed read instead of
        // backoff arithmetic. `type::record($tb, $id)` mirrors that helper's
        // bound-record pattern.
        let hooks_json = serde_json::to_value(&hooks)?;
        let mut resp = db
            .query(
                "UPSERT type::record($tb, $id) CONTENT { \
                     name: $name, \
                     version_label: $version_label, \
                     sha256: $sha256, \
                     hooks: $hooks, \
                     capabilities: $capabilities, \
                     enabled: false, \
                     home_instance_id: $home_id, \
                     _vclock: $vclock, \
                     created_at: time::now(), \
                     updated_at: time::now() \
                 } RETURN AFTER",
            )
            .bind(("tb", WASM_PLUGIN_TABLE.to_string()))
            .bind(("id", name.to_string()))
            .bind(("name", name.to_string()))
            .bind(("version_label", version_label.to_string()))
            .bind(("sha256", saved.sha256))
            .bind(("hooks", hooks_json))
            .bind(("capabilities", capabilities))
            .bind(("home_id", home_instance_id.to_string()))
            .bind(("vclock", vclock))
            .await?;
        let created: Vec<WasmPlugin> = resp.take(0)?;
        created
            .into_iter()
            .next()
            .ok_or_else(|| RegistryError::Storage("upsert returned no record".to_string()))
    }

    /// Fetch a single record by name.
    pub async fn get(&self, db: &SurrealDb, name: &str) -> Result<Option<WasmPlugin>, RegistryError> {
        let rec: Option<WasmPlugin> = db.select((WASM_PLUGIN_TABLE, name)).await?;
        Ok(rec)
    }

    /// List all records, each annotated with this node's failure-gate state.
    pub async fn list(&self, db: &SurrealDb) -> Result<Vec<PluginStatus>, RegistryError> {
        let recs: Vec<WasmPlugin> = db.select(WASM_PLUGIN_TABLE).await?;
        Ok(recs
            .into_iter()
            .map(|plugin| {
                let locally_disabled = self.is_locally_disabled(&plugin.sha256);
                PluginStatus {
                    plugin,
                    locally_disabled,
                }
            })
            .collect())
    }

    /// Enable a plugin. Also clears any node-local failure latch — an admin
    /// re-enable is an explicit "give it another chance".
    pub async fn enable(&self, db: &SurrealDb, name: &str) -> Result<WasmPlugin, RegistryError> {
        // `enabled` is a HASHED content field (NOT in `merkle::IGNORED_FIELDS`),
        // so flipping it is a real content change that MUST advance the author
        // `_vclock` — otherwise a peer sees the same clock over different content
        // and drops the enable as "local wins/equal" (it never propagates,
        // defeating "admin enables once, fleet-wide"). Read-advance this node's
        // component, same convention as `install`/`put_synced_entity`.
        let vclock = self.next_vclock(db, name).await?;
        // Raw query + `time::now()` — see the STRING-timestamp note on `install`.
        let mut resp = db
            .query("UPDATE type::record($tb, $id) MERGE { enabled: true, updated_at: time::now(), _vclock: $vclock } RETURN AFTER")
            .bind(("tb", WASM_PLUGIN_TABLE.to_string()))
            .bind(("id", name.to_string()))
            .bind(("vclock", vclock))
            .await?;
        let updated: Vec<WasmPlugin> = resp.take(0)?;
        let rec = updated
            .into_iter()
            .next()
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        self.clear_gate(&rec.sha256);
        Ok(rec)
    }

    /// Disable a plugin (the meshed enabled flag; distinct from the node-local latch).
    pub async fn disable(&self, db: &SurrealDb, name: &str) -> Result<WasmPlugin, RegistryError> {
        // Bumps `_vclock` for the same reason `enable` does — `enabled` is hashed.
        let vclock = self.next_vclock(db, name).await?;
        let mut resp = db
            .query("UPDATE type::record($tb, $id) MERGE { enabled: false, updated_at: time::now(), _vclock: $vclock } RETURN AFTER")
            .bind(("tb", WASM_PLUGIN_TABLE.to_string()))
            .bind(("id", name.to_string()))
            .bind(("vclock", vclock))
            .await?;
        let updated: Vec<WasmPlugin> = resp.take(0)?;
        updated
            .into_iter()
            .next()
            .ok_or_else(|| RegistryError::NotFound(name.to_string()))
    }

    /// Next author `_vclock` for a lifecycle write on `name`: read the record's
    /// current clock and advance THIS node's component ([`conflict::
    /// next_local_vclock`](crate::sync::conflict::next_local_vclock)). Shared by
    /// `enable`/`disable`. `self.instance_id` is the authoring node; empty when
    /// unconfigured (tests), which still produces a valid, advancing clock.
    async fn next_vclock(&self, db: &SurrealDb, name: &str) -> Result<Value, RegistryError> {
        let cur: Vec<Value> = db
            .query("SELECT VALUE _vclock FROM type::record($tb, $id) LIMIT 1")
            .bind(("tb", WASM_PLUGIN_TABLE.to_string()))
            .bind(("id", name.to_string()))
            .await?
            .take(0)?;
        Ok(next_local_vclock(cur.first(), &self.instance_id))
    }

    /// Read the wasm bytes for a sha, verifying integrity. This is the byte
    /// source the [`PluginEngine`](super::PluginEngine) pulls on a cache miss
    /// (the async fetch happens here; `run`'s provider closure is sync — the
    /// caller bridges the two).
    ///
    /// Local CAS first. On a local miss and with a mesh handle wired
    /// ([`with_mesh`](Self::with_mesh)), it LAZILY pulls the blob by sha256 from
    /// a peer (design §6) and lands it in the local CAS, so the next resolve is a
    /// plain disk hit. This runs ONLY at hook time (the ingest-transform seam
    /// calls `resolve` on a cache miss) — NEVER at node startup: a bad or absent
    /// plugin must never touch boot (the kiosk 101-rollback lesson). With no mesh
    /// handle, a local miss is a hard error that the engine maps to
    /// [`PluginError::BytesUnavailable`](super::PluginError::BytesUnavailable) —
    /// the seam already latches on it.
    pub async fn resolve(&self, sha256: &str) -> Result<Vec<u8>, RegistryError> {
        let path = storage_path(sha256)
            .ok_or_else(|| RegistryError::Storage(format!("malformed sha256: {sha256}")))?;

        // Fast path: already in the local CAS.
        if let Ok(bytes) = self.filestore.read(&path).await {
            if !verify_sha256(&bytes, sha256) {
                return Err(RegistryError::Storage(format!(
                    "resolved bytes for {sha256} failed sha verification"
                )));
            }
            return Ok(bytes);
        }

        // Local miss → lazy mesh fetch (design §6), if a mesh handle is wired.
        let Some(mesh) = self.mesh.as_ref() else {
            return Err(RegistryError::Storage(format!(
                "plugin blob {sha256} not in local CAS and no mesh handle configured"
            )));
        };
        // Same on-demand transport attachments use: direct peer HTTP then relay,
        // by sha256. `None` = no peer served it.
        let Some(bytes) = mesh.fetch_file_from_peers(sha256).await else {
            return Err(RegistryError::Storage(format!(
                "plugin blob {sha256} not resolvable locally or from any mesh peer"
            )));
        };
        // CAS poison guard: `fetch_file_from_peers`'s DIRECT-HTTP path does not
        // sha-verify (only its relay path does), so `write_verified` is what
        // rejects bytes a peer returned under the wrong content-address before
        // they land in our CAS. On mismatch nothing is written and we error.
        self.filestore
            .write_verified(&path, &bytes, sha256)
            .await
            .map_err(RegistryError::Storage)?;
        Ok(bytes)
    }

    // ── Node-local failure gate ─────────────────────────────────────────────

    /// Record a failed execution. On crossing the threshold the plugin
    /// auto-disables locally (logged once at the transition).
    pub fn record_failure(&self, sha256: &str) {
        let mut gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        let st = gate.entry(sha256.to_string()).or_default();
        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
        if !st.locally_disabled && st.consecutive_failures >= self.max_consecutive_failures {
            st.locally_disabled = true;
            warn!(
                target: "plugin::registry",
                "plugin {} auto-disabled on this node after {} consecutive failures",
                sha256, st.consecutive_failures
            );
        }
    }

    /// Record a successful execution — resets the consecutive-failure counter.
    /// (A locally-disabled plugin is skipped, so it cannot reach here to clear
    /// its own latch; only [`enable`](Self::enable) clears the latch.)
    pub fn record_success(&self, sha256: &str) {
        let mut gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(st) = gate.get_mut(sha256) {
            st.consecutive_failures = 0;
        }
    }

    /// Whether this node has auto-disabled the plugin — callers must skip execution.
    pub fn is_locally_disabled(&self, sha256: &str) -> bool {
        let gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        gate.get(sha256).is_some_and(|s| s.locally_disabled)
    }

    fn clear_gate(&self, sha256: &str) {
        let mut gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        gate.remove(sha256);
    }
}

/// Reconstruct the CAS storage path for a wasm sha — mirrors
/// [`FileStore::save`](crate::utils::filestore::FileStore) for a `"{sha}.wasm"`
/// filename. `None` if the sha is too short to be a real content address.
fn storage_path(sha256: &str) -> Option<String> {
    if sha256.len() < 4 {
        return None;
    }
    Some(format!(
        "data/filestore/{}/{}/{}.wasm",
        &sha256[0..2],
        &sha256[2..4],
        sha256
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Arc<FileStore>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("eck_plugin_reg_{}", uuid::Uuid::new_v4()));
        (Arc::new(FileStore::new(dir.to_str().unwrap())), dir)
    }

    /// Fresh embedded SurrealKv DB (the `kv-surrealkv` feature, mirroring
    /// `db::connect_with_db` embedded mode) — core lacks `kv-mem`, so the tests
    /// use a temp-dir SurrealKv instead. Caller removes the dir.
    async fn temp_db() -> (SurrealDb, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("eck_plugin_db_{}", uuid::Uuid::new_v4()));
        let db = surrealdb::engine::any::connect(format!("surrealkv://{}", dir.to_str().unwrap()))
            .await
            .expect("embedded surrealkv db");
        db.use_ns("eck").use_db("test").await.unwrap();
        db.query("DEFINE TABLE IF NOT EXISTS wasm_plugin SCHEMALESS;")
            .await
            .unwrap()
            .check()
            .unwrap();
        (db, dir)
    }

    fn sample_hooks() -> Vec<PluginHookRef> {
        vec![PluginHookRef {
            name: "ingest_transform".to_string(),
            abi: 1,
        }]
    }

    /// The house `_vclock` shape is a flat JSON object `{instance_id: counter}`
    /// (see `sync::vector_clock::VectorClock`); read the author's counter out.
    fn vclock_component(plugin: &WasmPlugin, instance: &str) -> Option<i64> {
        plugin
            ._vclock
            .as_ref()
            .and_then(|v| v.get(instance))
            .and_then(|v| v.as_i64())
    }

    #[tokio::test]
    async fn resolve_reads_and_verifies_cas_bytes() {
        let (fs, dir) = temp_store();
        // Content-agnostic: resolve just reads + sha-verifies whatever is at the
        // content-addressed path (a wasm magic prefix, arbitrary tail).
        let bytes: &[u8] = b"\0asm\x01\0\0\0registry-resolve-fixture";
        let sha = sha256_hex(bytes);
        fs.save(bytes, &format!("{sha}.wasm"), None, None)
            .await
            .unwrap();

        let reg = PluginRegistry::new(fs);
        let got = reg.resolve(&sha).await.expect("resolve should read CAS bytes");
        assert_eq!(got, bytes);

        // An unknown sha errors (not a panic).
        assert!(reg.resolve(&"b".repeat(64)).await.is_err());
        // A malformed (too short) sha errors cleanly.
        assert!(reg.resolve("ab").await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resolve_local_miss_without_mesh_errs_not_panics() {
        // A local CAS miss with NO mesh handle wired must be a clean `Err`
        // (which the engine maps to `PluginError::BytesUnavailable`), never a
        // panic — the failure the seam latches on.
        let (fs, dir) = temp_store();
        let reg = PluginRegistry::new(fs); // no `.with_mesh(...)`
        let unknown = "d".repeat(64);
        let got = reg.resolve(&unknown).await;
        assert!(got.is_err(), "local miss + no mesh handle must Err, not panic");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn install_and_lifecycle_stamp_and_bump_author_vclock() {
        let (db, ddir) = temp_db().await;
        let (fs, fdir) = temp_store();
        let reg = PluginRegistry::new(fs).with_instance_id("node-A");

        // Fresh install stamps the author (home) component to 1 — the house
        // shape `{author: 1}`.
        let installed = reg
            .install(
                &db,
                b"\0asm\x01\0\0\0vclock-fixture",
                "vc-plugin",
                "v1",
                sample_hooks(),
                vec![],
                "node-A",
            )
            .await
            .expect("install should succeed");
        assert!(!installed.enabled, "install lands disabled");
        assert_eq!(
            vclock_component(&installed, "node-A"),
            Some(1),
            "fresh install stamps {{author:1}}, got {:?}",
            installed._vclock
        );

        // Enabling flips a HASHED field → the author component MUST advance.
        let enabled = reg.enable(&db, "vc-plugin").await.expect("enable should succeed");
        assert!(enabled.enabled);
        assert_eq!(
            vclock_component(&enabled, "node-A"),
            Some(2),
            "enable bumps the author component to 2, got {:?}",
            enabled._vclock
        );

        // Disabling flips it back → advance again.
        let disabled = reg.disable(&db, "vc-plugin").await.expect("disable should succeed");
        assert!(!disabled.enabled);
        assert_eq!(
            vclock_component(&disabled, "node-A"),
            Some(3),
            "disable bumps the author component to 3, got {:?}",
            disabled._vclock
        );

        let _ = std::fs::remove_dir_all(&ddir);
        let _ = std::fs::remove_dir_all(&fdir);
    }

    #[tokio::test]
    async fn upgrade_reinstall_advances_vclock_not_resets() {
        let (db, ddir) = temp_db().await;
        let (fs, fdir) = temp_store();
        let reg = PluginRegistry::new(fs).with_instance_id("node-A");

        reg.install(&db, b"\0asm\x01\0\0\0one", "up-plugin", "v1", sample_hooks(), vec![], "node-A")
            .await
            .expect("install v1");
        // Re-install the SAME name with new bytes (an upgrade): the clock must
        // advance {node-A:1} → {node-A:2}, not reset to {node-A:1}, so peers
        // adopt the new sha instead of reading it as equal/concurrent.
        let up = reg
            .install(&db, b"\0asm\x01\0\0\0two", "up-plugin", "v2", sample_hooks(), vec![], "node-A")
            .await
            .expect("install v2 (upgrade)");
        assert_eq!(
            vclock_component(&up, "node-A"),
            Some(2),
            "upgrade advances the author component, got {:?}",
            up._vclock
        );
        assert!(!up.enabled, "upgrade lands disabled");

        let _ = std::fs::remove_dir_all(&ddir);
        let _ = std::fs::remove_dir_all(&fdir);
    }

    #[test]
    fn failure_gate_trips_at_threshold_and_success_resets() {
        let (fs, dir) = temp_store();
        let reg = PluginRegistry::with_threshold(fs, 3);
        let sha = "c".repeat(64);

        assert!(!reg.is_locally_disabled(&sha));
        reg.record_failure(&sha);
        reg.record_failure(&sha);
        // A success before the threshold resets the run of failures.
        reg.record_success(&sha);
        reg.record_failure(&sha);
        reg.record_failure(&sha);
        assert!(!reg.is_locally_disabled(&sha), "reset run must not have tripped");

        // Three in a row → trips.
        reg.record_failure(&sha);
        assert!(reg.is_locally_disabled(&sha), "threshold run must trip");

        // Re-enable (via the private latch clear enable() uses) restores it.
        reg.clear_gate(&sha);
        assert!(!reg.is_locally_disabled(&sha));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
