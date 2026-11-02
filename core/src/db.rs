use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::{Namespace, Root};
use surrealdb::Surreal;
use tracing::info;

/// Backend-agnostic SurrealDB handle. Resolves at runtime to either the
/// embedded SurrealKv engine (edge nodes / kiosks) or a remote SurrealDB server
/// over WebSocket (shared `:8000` for the paid eck1/eck2/eck3 engines) — see B1
/// in `.eck/ENTERPRISE_CLUSTER.md`. Using the `any` engine keeps every handler
/// (`&SurrealDb`) identical regardless of where the data physically lives.
pub type SurrealDb = Surreal<Any>;

/// Initialize a SurrealDB connection (db `main`). See [`connect_with_db`].
pub async fn connect(path: &str) -> Result<SurrealDb, surrealdb::Error> {
    connect_with_db(path, "main").await
}

/// Connect to SurrealDB, picking the database within the namespace.
///
/// **Remote mode** (when `SURREAL_REMOTE_URL` is set, e.g. `ws://127.0.0.1:8000`):
/// connect to the shared server and select namespace `SURREAL_NS` (default
/// `eck`) + database `db_name`. Multiple engines share one server by each
/// using a distinct namespace (`SURREAL_NS=eck1|eck2|eck3`); the Zone-1 /
/// Zone-2 split stays as the `db_name` (`main` / `users`) within that namespace.
///
/// Auth (in order of preference):
/// 1. `SURREAL_NS_USER` / `SURREAL_NS_PASS` — a namespace-scoped user
///    (`DEFINE USER ... ON NAMESPACE`), which cannot touch other namespaces
///    on the shared server. Preferred for isolation.
/// 2. `SURREAL_ROOT_USER` / `SURREAL_ROOT_PASS` (falling back to
///    `SURREAL_USER` / `SURREAL_PASS`, the names used by the host-shared
///    `/etc/surrealdb/root.env`) — root signin.
///
/// **Embedded mode** (default): SurrealKv at `path`, namespace `eck`, database
/// `db_name` — the original behavior for edge nodes / kiosks.
pub async fn connect_with_db(path: &str, db_name: &str) -> Result<SurrealDb, surrealdb::Error> {
    if let Ok(remote) = std::env::var("SURREAL_REMOTE_URL") {
        let remote = remote.trim();
        if !remote.is_empty() {
            let db = any::connect(remote).await?;
            let ns = std::env::var("SURREAL_NS").unwrap_or_else(|_| "eck".to_string());
            let ns_user = std::env::var("SURREAL_NS_USER").unwrap_or_default();
            if !ns_user.is_empty() {
                db.signin(Namespace {
                    namespace: ns.clone(),
                    username: ns_user,
                    password: std::env::var("SURREAL_NS_PASS").unwrap_or_default(),
                })
                .await?;
            } else {
                let user = std::env::var("SURREAL_ROOT_USER")
                    .or_else(|_| std::env::var("SURREAL_USER"))
                    .unwrap_or_default();
                if !user.is_empty() {
                    let pass = std::env::var("SURREAL_ROOT_PASS")
                        .or_else(|_| std::env::var("SURREAL_PASS"))
                        .unwrap_or_default();
                    db.signin(Root {
                        username: user,
                        password: pass,
                    })
                    .await?;
                }
            }
            db.use_ns(ns.clone()).use_db(db_name).await?;
            info!(
                "SurrealDB connected (remote {}) ns={} db={}",
                remote, ns, db_name
            );
            return Ok(db);
        }
    }

    // Embedded SurrealKv via the `any` engine (scheme `surrealkv://`).
    //
    // Memory tuning for small embedded nodes. These knobs are honored in-process
    // via the vendored `surrealdb` API-crate patch (see the module notes below and
    // vendor/README.md): the endpoint query params below are re-injected UNPREFIXED
    // onto the datastore builder's ConfigMap so SurrealKV's `block_cache_capacity`
    // / `max_memtable_size` and the engine's `hnsw_cache_size` parsers actually
    // pick them up. Sizes are per-store (each `connect()` carries its own query).
    let total_ram = total_system_memory_bytes();
    // The users store (db_name "users") is tiny (auth rows only) → default 16 MiB.
    let is_users = db_name == "users";
    let block_cache_mb = if is_users {
        users_block_cache_mb(std::env::var("ECK_KV_USERS_BLOCK_CACHE_MB").ok().as_deref())
    } else {
        main_block_cache_mb(total_ram, std::env::var("ECK_KV_BLOCK_CACHE_MB").ok().as_deref())
    };
    let memtable_mb_v = if is_users {
        users_memtable_mb(std::env::var("ECK_KV_USERS_MEMTABLE_MB").ok().as_deref())
    } else {
        memtable_mb(total_ram, std::env::var("ECK_KV_MEMTABLE_MB").ok().as_deref())
    };
    let hnsw_mb = hnsw_cache_mb(total_ram, std::env::var("ECK_HNSW_CACHE_MB").ok().as_deref());

    // Append the SurrealKV + HNSW tuning params. All three keys are read UNPREFIXED
    // by their parsers (`surrealkv_block_cache_capacity`, `surrealkv_max_memtable_size`
    // in `kvs/surrealkv/cnf.rs`; `hnsw_cache_size` in `cnf/mod.rs` CommonConfig). The
    // vendored API-crate patch re-injects the query onto the builder ConfigMap so
    // these land; values are in BYTES.
    let endpoint = format!(
        "surrealkv://{path}?surrealkv_block_cache_capacity={}&surrealkv_max_memtable_size={}&hnsw_cache_size={}",
        block_cache_mb * MIB,
        memtable_mb_v * MIB,
        hnsw_mb * MIB,
    );
    let db = any::connect(endpoint).await?;
    db.use_ns("eck").use_db(db_name).await?;
    info!(
        "SurrealDB connected (SurrealKv) at: {} (db={}); kv-tuning applied block_cache={}MiB memtable={}MiB hnsw_cache={}MiB (total_ram={:?}MiB) via vendored surrealdb patch.",
        path,
        db_name,
        block_cache_mb,
        memtable_mb_v,
        hnsw_mb,
        total_ram.map(|b| b / MIB),
    );
    Ok(db)
}

// ─── Embedded SurrealKV memory tuning (small edge nodes) ──────────────────────
//
// On a small edge kiosk (3.3 GiB RAM, 4 cores) SurrealKV's server-sized defaults
// dominate RSS: the block cache defaults to `mem/2 - 1 GiB` (~655 MiB) and the
// store is opened TWICE (wms.db + wms_users.db) → ~1.3 GiB of cache cap alone,
// the bulk of the 1.6 GiB boot RSS. These pure fns compute conservative edge
// defaults, overridable via env:
//   ECK_KV_BLOCK_CACHE_MB        main store block cache (MiB)
//   ECK_KV_USERS_BLOCK_CACHE_MB  users store block cache (MiB, default 16)
//   ECK_KV_MEMTABLE_MB           max memtable size (MiB)
//   ECK_KV_USERS_MEMTABLE_MB     users store max memtable size (MiB, default 16)
//   ECK_HNSW_CACHE_MB            HNSW ANN cache size (MiB)
//
// HOW THESE TAKE EFFECT (was previously inert; fixed 2026-09-03 via a vendored
// surrealdb API-crate patch — see vendor/README.md):
//   Upstream surrealdb-core 3.2.1 parses the embedded endpoint query string into
//   a ConfigMap and force-prefixes every key with `datastore_`
//   (`kvs/ds.rs` `new_transaction_builder` → `map_keys(|x| format!("datastore_{x}"))`),
//   so only `versioned`/`retention`/`sync` (read as `datastore_*`) land;
//   `surrealkv_block_cache_capacity`/`surrealkv_max_memtable_size` (read UNPREFIXED
//   by `kvs/surrealkv/cnf.rs`) and `hnsw_cache_size` (read UNPREFIXED by
//   `cnf/mod.rs` CommonConfig, and loaded only from the BUILDER config — never the
//   query join) never match. `ConfigMap::from_env()` is only called by the
//   `surreal` server binary, never on the embedded library path.
//   The vendored patch adds ~7 lines to `engine/local/native.rs::run_router` that
//   re-inject the endpoint's own query params UNPREFIXED onto the datastore
//   builder's ConfigMap (`builder.with_config(ConfigMap::from_config_string(query))`).
//   `build_with_path` then merges them (`join`) so both the SurrealKV parser and
//   CommonConfig pick them up — per-store, since each `connect()` carries its own
//   query. Remote (`ws://`) engines never reach `run_router`, so they are
//   unaffected and the shared `Surreal<Any>` handle type is preserved (no
//   `Surreal<Db>` split, no blast radius across the ~44 handler call sites).
//   `TOTAL_SYSTEM_MEMORY` (host RAM, cgroup-blind) still drives the crate's
//   *fallback* defaults, so systemd `MemoryMax=` remains a useful belt-and-braces
//   cap on edge nodes.

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Total physical RAM in bytes, best-effort and `sysinfo`-free. Linux: parse
/// `/proc/meminfo` `MemTotal` (kB). Other platforms or an unreadable
/// `/proc/meminfo` (e.g. systemd `ProcSubset=pid`): `None` — callers fall back
/// to a conservative default.
pub fn total_system_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let txt = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in txt.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// SurrealKV's own default block cache size in MiB: `mem/2 - 1 GiB`, min 16 MiB.
fn surreal_default_block_cache_mb(total: u64) -> u64 {
    (total / 2).saturating_sub(GIB).max(16 * MIB) / MIB
}

/// Effective MAIN-store block cache size (MiB). `env` (ECK_KV_BLOCK_CACHE_MB)
/// wins; otherwise `min(surreal default, 20% of total RAM, 512)`, or 256 MiB
/// when total RAM is unknown.
pub fn main_block_cache_mb(total: Option<u64>, env: Option<&str>) -> u64 {
    if let Some(v) = env.and_then(|s| s.trim().parse::<u64>().ok()) {
        return v;
    }
    match total {
        Some(t) => {
            let twenty_pct_mb = (t / 5) / MIB;
            surreal_default_block_cache_mb(t).min(twenty_pct_mb).min(512)
        }
        None => 256,
    }
}

/// Effective USERS-store block cache size (MiB). `env`
/// (ECK_KV_USERS_BLOCK_CACHE_MB) wins; otherwise 16 MiB — the store holds only
/// the node-local setup account, so it never needs a large cache.
pub fn users_block_cache_mb(env: Option<&str>) -> u64 {
    env.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(16)
}

/// Effective USERS-store max memtable size (MiB). `env`
/// (ECK_KV_USERS_MEMTABLE_MB) wins; otherwise 16 MiB — the users store holds
/// only the node-local setup account, and its only steady writer is
/// surrealdb-core's 3-second node heartbeat (a `replace` of the `/!nd` key).
/// SurrealKV flushes the active memtable only when its arena (pre-allocated
/// at `max_memtable_size`) fills, so a small arena here bounds RAM and keeps
/// WAL replay at boot short — a large arena (e.g. the main store's 64+ MiB)
/// would let heartbeat versions accumulate for a very long time before ever
/// flushing.
pub fn users_memtable_mb(env: Option<&str>) -> u64 {
    env.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(16)
}

/// Effective max memtable size (MiB). `env` (ECK_KV_MEMTABLE_MB) wins; otherwise
/// 64 MiB on `<4 GiB` boxes (and when RAM is unknown), else SurrealKV's own
/// size-based default (256/1024/4096 MiB for `<16`/`<64`/`≥64` GiB).
pub fn memtable_mb(total: Option<u64>, env: Option<&str>) -> u64 {
    if let Some(v) = env.and_then(|s| s.trim().parse::<u64>().ok()) {
        return v;
    }
    match total {
        Some(t) if t >= 4 * GIB => {
            if t < 16 * GIB {
                256
            } else if t < 64 * GIB {
                1024
            } else {
                4096
            }
        }
        _ => 64,
    }
}

/// Effective HNSW ANN cache size (MiB). `env` (ECK_HNSW_CACHE_MB) wins; otherwise
/// 64 MiB on `<4 GiB` boxes (and when RAM is unknown) — well under SurrealDB's own
/// fixed 256 MiB default — else that 256 MiB default. The users store holds no
/// vector indexes, so its HNSW cache is never populated regardless.
pub fn hnsw_cache_mb(total: Option<u64>, env: Option<&str>) -> u64 {
    if let Some(v) = env.and_then(|s| s.trim().parse::<u64>().ok()) {
        return v;
    }
    match total {
        Some(t) if t >= 4 * GIB => 256,
        _ => 64,
    }
}

#[cfg(test)]
mod mem_tuning_tests {
    use super::*;

    const KIOSK: u64 = 3458310144; // ~3.22 GiB, a small edge kiosk

    #[test]
    fn main_cache_env_override_wins() {
        assert_eq!(main_block_cache_mb(Some(16 * GIB), Some("128")), 128);
        assert_eq!(main_block_cache_mb(None, Some("300")), 300);
        // Whitespace / junk env falls through to the computed default.
        assert_eq!(main_block_cache_mb(None, Some("  ")), 256);
        assert_eq!(main_block_cache_mb(None, Some("xyz")), 256);
    }

    #[test]
    fn main_cache_unknown_ram_is_256() {
        assert_eq!(main_block_cache_mb(None, None), 256);
    }

    #[test]
    fn main_cache_kiosk_is_capped_by_20pct() {
        // surreal default here ≈ 1729-1024 = 705 MiB; 20% of 3.22 GiB ≈ 659 MiB;
        // 512 cap wins → 512.
        assert_eq!(main_block_cache_mb(Some(KIOSK), None), 512);
    }

    #[test]
    fn main_cache_tiny_box_tracks_surreal_default_floor() {
        // 2 GiB: surreal default = 1GiB-1GiB→floor 16 MiB; 20% = 409 MiB → min 16.
        assert_eq!(main_block_cache_mb(Some(2 * GIB), None), 16);
    }

    #[test]
    fn main_cache_big_box_capped_at_512() {
        assert_eq!(main_block_cache_mb(Some(64 * GIB), None), 512);
    }

    #[test]
    fn users_cache_defaults_16_env_overrides() {
        assert_eq!(users_block_cache_mb(None), 16);
        assert_eq!(users_block_cache_mb(Some("8")), 8);
        assert_eq!(users_block_cache_mb(Some("bad")), 16);
    }

    #[test]
    fn users_memtable_defaults_16_env_overrides() {
        assert_eq!(users_memtable_mb(None), 16);
        assert_eq!(users_memtable_mb(Some("8")), 8);
        assert_eq!(users_memtable_mb(Some("bad")), 16);
    }

    #[test]
    fn memtable_small_box_and_unknown_are_64() {
        assert_eq!(memtable_mb(Some(KIOSK), None), 64);
        assert_eq!(memtable_mb(Some(2 * GIB), None), 64);
        assert_eq!(memtable_mb(None, None), 64);
    }

    #[test]
    fn memtable_larger_boxes_use_crate_default() {
        assert_eq!(memtable_mb(Some(8 * GIB), None), 256);
        assert_eq!(memtable_mb(Some(32 * GIB), None), 1024);
        assert_eq!(memtable_mb(Some(128 * GIB), None), 4096);
    }

    #[test]
    fn memtable_env_override_wins() {
        assert_eq!(memtable_mb(Some(8 * GIB), Some("32")), 32);
    }

    #[test]
    fn hnsw_small_box_and_unknown_are_64() {
        assert_eq!(hnsw_cache_mb(Some(KIOSK), None), 64);
        assert_eq!(hnsw_cache_mb(Some(2 * GIB), None), 64);
        assert_eq!(hnsw_cache_mb(None, None), 64);
    }

    #[test]
    fn hnsw_larger_boxes_use_crate_default() {
        assert_eq!(hnsw_cache_mb(Some(8 * GIB), None), 256);
        assert_eq!(hnsw_cache_mb(Some(64 * GIB), None), 256);
    }

    #[test]
    fn hnsw_env_override_wins() {
        assert_eq!(hnsw_cache_mb(Some(8 * GIB), Some("32")), 32);
        assert_eq!(hnsw_cache_mb(None, Some("bad")), 64);
    }
}
