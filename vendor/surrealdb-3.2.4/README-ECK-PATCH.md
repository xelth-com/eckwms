# Vendored `surrealdb` (API crate) — eck patch

**Upstream:** `surrealdb` v3.2.4, verbatim copy of the crates.io source, EXCEPT for the
single patch documented below. `surrealdb-core` is **not** vendored — the vendored
API crate still pulls `surrealdb-core = "3.2.4"` from crates.io.

Wired in via the workspace root `Cargo.toml`:

```toml
[patch.crates-io]
surrealdb = { path = "vendor/surrealdb-3.2.4" }
```

## Why

The embedded SurrealKV memory knobs we set on the connection endpoint
(`surrealkv_block_cache_capacity`, `surrealkv_max_memtable_size`, `hnsw_cache_size`)
were inert. On the `any::connect("surrealkv://…?…")` (embedded) path,
`surrealdb-core`'s `kvs/ds.rs::new_transaction_builder` parses the endpoint query
string and **force-prefixes every key with `datastore_`**
(`map_keys(|x| format!("datastore_{x}"))`). But:

- `kvs/surrealkv/cnf.rs` reads `surrealkv_block_cache_capacity` /
  `surrealkv_max_memtable_size` **unprefixed**, so the prefixed keys never match.
- `cnf/mod.rs` `CommonConfig` reads `hnsw_cache_size` **unprefixed**, and only from
  the datastore **builder** ConfigMap — never from the query join at all.
- `ConfigMap::from_env()` is only called by the `surreal` server binary, never on
  the embedded library path (the builder starts from `ConfigMap::empty()`).

Result upstream: only host-RAM/cgroup-derived defaults applied; the knobs did nothing.

Routes A (public `Surreal::<Db>::unstable_from_datastore`) and B (a newer minor) were
rejected: A yields `Surreal<Db>`, incompatible with the `Surreal<Any>` alias used by
~44 handler call sites (the fluent builder API can't be enum-wrapped cheaply); no
newer 3.x on crates.io changes the embedded path. This patch (route C) keeps
`Surreal<Any>` intact with zero blast radius.

## The patch

Single hunk in **`src/engine/local/native.rs`**, in `run_router`, immediately after
the `Datastore::builder()` chain (`.with_auth(...)`). It re-injects the endpoint's
own query params **unprefixed** onto the builder's ConfigMap. `build_with_path` then
`join`s them so both the SurrealKV parser and `CommonConfig` pick them up. Per-store,
since each `connect()` carries its own query string. Remote (`ws://`) engines never
reach `run_router`, so they are unaffected.

```rust
let builder = match address.path.split_once('?') {
    Some((_, query)) if !query.is_empty() => {
        builder.with_config(surrealdb_core::cnf::ConfigMap::from_config_string(query))
    }
    _ => builder,
};
```

(The committed source carries an `// ── eck patch (vendored) ──` … `// ── end eck patch ──`
comment banner around this hunk. `with_config` replaces the builder's config, which
starts as `ConfigMap::empty()` here, so the injection is purely additive. All API
surfaces used are `pub`: `Datastore::builder`, `Builder::with_config`,
`surrealdb_core::cnf::ConfigMap::from_config_string`.)

## On upgrade

Drop this vendored copy and the `[patch.crates-io]` stanza IF a future `surrealdb`
release honors these keys on the embedded path (e.g. calls `ConfigMap::from_env()`
in the local engine, or stops force-prefixing engine-specific query keys). Otherwise
re-apply the one hunk above to the new version's `run_router`. The consumer side
(`core/src/db.rs`) needs no change on upgrade — it only sets query params.

Verify after any bump: `core/src/db.rs` emits, with the tuning env vars set,
`Setting block cache capacity: <bytes>` and `Setting max memtable size: <bytes>` from
`kvs/surrealkv/mod.rs` at the requested sizes.

## Upgrade history

- **3.2.1 → 3.2.4 (2026-09-05).** Re-vendored from the pristine crates.io tarball.
  Upstream 3.2.4 differs from 3.2.1 in only four files — `Cargo.toml`, `Cargo.lock`,
  `.cargo_vcs_info.json` and `src/engine/tasks.rs`; **`src/engine/local/native.rs` is
  byte-identical between the two releases**, so the single hunk applied cleanly at the
  same location with `patch -p0`. Verified by diffing the patched 3.2.4
  `src/engine/local/native.rs` against the previously vendored 3.2.1 copy: identical.
  Upstream still force-prefixes the endpoint query keys with `datastore_`, so the patch
  is still required. `Cargo.lock` / `Cargo.toml.orig` / `.cargo_vcs_info.json` are
  stripped from the vendored copy, as in the 3.2.1 vendoring.
