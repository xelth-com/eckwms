# Vendored `surrealdb-engine-local` (embedded engine crate) — eck patch

**Upstream:** `surrealdb-engine-local` v3.3.5, verbatim copy of the crates.io source,
EXCEPT for the single patch documented below. Neither `surrealdb` (the API crate) nor
`surrealdb-core` is vendored — they still come from crates.io. The vendored engine crate
still pulls `surrealdb-core = "3.3.5"`, `surrealdb-cnf = "3.3.5"`, etc. from crates.io.

Wired in via the workspace root `Cargo.toml`:

```toml
[patch.crates-io]
surrealdb-engine-local = { path = "vendor/surrealdb-engine-local-3.3.5" }
```

The `surrealdb` API crate (3.3.5) depends on `surrealdb-engine-local = 3.3.5`; the patch
stanza substitutes this path copy, which satisfies that requirement.

## Why

The embedded SurrealKV memory knobs we set on the connection endpoint
(`surrealkv_block_cache_capacity`, `surrealkv_max_memtable_size`, `hnsw_cache_size`)
are inert upstream. On the `any::connect("surrealkv://…?…")` (embedded) path, the query
string is split off the path and **force-prefixed with `datastore_`** before it reaches
the storage backend:

- `surrealdb-kvs-any/src/registry.rs::new_transaction_builder` (~line 70):
  `config.join(ConfigMap::from_config_string(q).map_keys(|x| format!("datastore_{x}")))`.

But the consumers read the keys **unprefixed**:

- `surrealdb-kvs-surrealkv/src/cnf.rs` parses `surrealkv_block_cache_capacity` /
  `surrealkv_max_memtable_size` **unprefixed** (its `lib.rs` logs
  "Setting block cache capacity" / "Setting max memtable size").
- `surrealdb-core` `RuntimeConfig`/`IdxConfig` (`src/config.rs`, loaded by
  `RuntimeConfig::load(&self.config)` in `kvs/ds/builder.rs::build_with_tx_builder_buckets`)
  reads `hnsw_cache_size` **unprefixed**, and only from the datastore **builder**
  ConfigMap — never from the query join at all.
- `ConfigMap::from_env()` is only called by the `surreal` server binary, never on the
  embedded library path (the builder starts from `ConfigMap::empty()`).

So the prefixed query keys never match any parser; only host-RAM/cgroup-derived defaults
apply and the knobs do nothing. The upstream bug persists in 3.3.5.

## Where the code moved (3.2.4 → 3.3.5)

In 3.2.4 the embedded connection path lived in the **API crate** `surrealdb`, in
`src/engine/local/native.rs::run_router`, and the patch went there. In 3.3.5 the SDK was
split into many crates: the local engine is now its own crate `surrealdb-engine-local`,
and the connection logic is `src/lib.rs::connect`, which builds
`Datastore::builder()…` and calls `build_with_path(&config.path)`. That is where the
patch now lives. (This is why we vendor `surrealdb-engine-local` now instead of
`surrealdb`.)

## The patch

Single hunk in **`src/lib.rs`**, in `connect`, immediately after the builder's
`.with_capabilities(config.capabilities)` line and before `build_with_path`. It
re-injects the endpoint's own query params **unprefixed** onto the builder's ConfigMap.
`build_with_path` then both passes this map to the backend registry (so the SurrealKV
parser sees the unprefixed keys) and runs `RuntimeConfig::load` over it (so `IdxConfig`
sees `hnsw_cache_size`). Per-store, since each `connect()` carries its own query string.
Remote (`ws://`) engines never reach this crate, so they are unaffected.

```rust
let builder = match config.path.split_once('?') {
    Some((_, q)) if !q.is_empty() => {
        builder.with_config(surrealdb_cnf::ConfigMap::from_config_string(q))
    }
    _ => builder,
};
```

(The committed source carries an `// ── eck patch (vendored) ──` … `// ── end eck patch ──`
comment banner around this hunk. `with_config` *replaces* the builder's config
(`self.config = config`), which starts as `ConfigMap::empty()` here, so the injection is
purely additive. `surrealdb_cnf` is already a direct dependency of this crate. All API
surfaces used are `pub`: `Builder::with_config` (`surrealdb-core/src/kvs/ds/builder.rs:109`),
`surrealdb_cnf::ConfigMap::from_config_string` (`surrealdb-cnf/src/config/mod.rs:71`).)

## On upgrade

Drop this vendored copy and the `[patch.crates-io]` stanza IF a future `surrealdb`
release honors these keys on the embedded path (e.g. stops force-prefixing
engine-specific query keys, or calls `ConfigMap::from_env()` in the local engine).
Otherwise re-apply the one hunk above to the new version's `connect` (or wherever the
embedded `Datastore::builder()…build_with_path` chain has moved). The consumer side
(`core/src/db.rs`) needs no change on upgrade — it only sets query params.

Verify after any bump: `core/src/db.rs` emits, with the tuning env vars set,
`Setting block cache capacity: <bytes>` and `Setting max memtable size: <bytes>` from
`surrealdb-kvs-surrealkv/src/lib.rs` at the requested sizes.

## Upgrade history

- **3.2.1 → 3.2.4 (2026-09-05).** Vendored the `surrealdb` API crate; patch was a
  ~7-line hunk in `src/engine/local/native.rs::run_router`. `native.rs` was
  byte-identical between 3.2.1 and 3.2.4, so the hunk applied cleanly at the same
  location.
- **3.2.4 → 3.3.5 (2026-10-31).** Major SDK re-crate: the 3.3.x releases split the SDK
  into `surrealdb-engine-local`, `surrealdb-kvs-any`, `surrealdb-kvs-surrealkv`,
  `surrealdb-cnf`, `surrealdb-datastore`, etc. The embedded connection path left the API
  crate entirely, so vendoring moved from `surrealdb` to `surrealdb-engine-local` and the
  patch moved from `run_router` (`engine/local/native.rs`) to `connect` (`lib.rs`). The
  upstream `datastore_`-prefix bug is unchanged (verified in `surrealdb-kvs-any`
  `registry.rs` and `surrealdb-kvs-surrealkv` `cnf.rs`), so the patch is still required.
  `with_config` now takes a `surrealdb_cnf::ConfigMap` (was `surrealdb_core::cnf::ConfigMap`)
  and `from_config_string` takes `&str`. `Cargo.lock` / `Cargo.toml.orig` /
  `.cargo_vcs_info.json` are stripped from the vendored copy, as before.
