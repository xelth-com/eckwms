<!-- machine-first: generated from source audit 2026-07-29; audience=agents -->

# Mesh Sync Protocol

## SCOPE

This document specifies the multi-node data synchronization protocol used between
WMS nodes. It is descriptive of the shipped code in `core/src/sync/*` (content
hashing, Merkle exchange, conflict resolution, the sync engine, the relay client)
and its counterparts in `wms/src/services/mesh_relay_poller.rs` and `relay/src/*`.
Anchors are given as `file::symbol`; line numbers are intentionally omitted.

The mesh is eventually-consistent, peer-to-peer, and network-topology-agnostic.
A central relay exists only as a **tracker** (peer discovery via heartbeat) and,
when two peers cannot dial each other directly, as a **blind store-and-forward
queue**. The relay never routes or reads business data payloads on the direct
path and holds nothing readable at rest on the fallback path.

Every persisted business row carries two pieces of sync metadata:

- `_vclock` — a vector clock (`{instance_id: logical_counter}`), the causality record.
- an `entity_checksum` leaf — a content hash stored in a side table, the Merkle input.

---

## 1. Content-hash model

Identity of a row is defined by its **content hash**, computed by
`core/src/sync/merkle.rs::compute_content_hash`. This is what two nodes compare;
it must be byte-identical across builds and platforms for equal business data.

### 1.1 Canonical serialization

`compute_content_hash` and its recursive helper `write_canonical_value`:

- Take a `serde_json::Value` object; non-objects hash to `None`.
- Sort keys (`BTreeMap`); recurse into nested objects sorting **every** level's
  keys (`sort_unstable`), so field order is irrelevant at any depth.
- JSON-quote and escape every string key and value, so a value containing
  structural characters (`,`, `:`, `{`) cannot collide with real structure.
- Normalize any RFC3339-parseable string to UTC before hashing, so the same
  instant written in different timezones hashes identically.
- **Float canonicalization:** every `f64` number is rounded through `f32` at any
  depth, then formatted via its shortest `Display`. Cross-node ULP-level residue
  (e.g. an embedding component or a nested classifier confidence arriving as
  `0.9200000166893005` vs `…04`) collapses to identical `f32` bits. Integers
  (`i64`/`u64`) are kept **exact** — never cast through `f32` (only 24 mantissa
  bits), because they carry identities and counters.
- Digest is SHA-256, hex-encoded.

### 1.2 IGNORED_FIELDS — node-local status is excluded from identity

`merkle.rs::IGNORED_FIELDS` is a compile-time set of field names stripped before
hashing. Semantically: **a field is ignored iff its value is legitimately
per-node and must not drive cross-node convergence.** Categories present:

- Record identity and timestamps: `id`, `created_at`, `updated_at`,
  `last_synced_at`, `synced_by` (and camel/Pascal variants). The row's identity
  is its Merkle **leaf key**, not its content; different rows are already
  different leaves. Hashing `id` was actively harmful because different query
  paths present it in different textual forms.
- Causality metadata: `vector_clock`, `_vclock` (compared separately, not hashed).
- Per-node worker bookkeeping: `summary_status`/`summary_retries`/`summary_error`,
  `embedding_status`/`embedding_retries`/`embedding_error`, and similar. A node
  with its AI worker disabled legitimately holds `pending` forever while the
  authored result arrives over the mesh; hashing the status would block
  convergence between the producing and consuming node.
- Derived indexes: `pii_fingerprints` (a deterministic token scan re-derived
  locally by every node, so it always agrees) and per-node operational fields
  (login counters, device heartbeat, node-local optimization timestamps).

Content-bearing fields written by exactly one authoritative node (`ai_summary`,
`embedding`, `embedding_model`) are **deliberately NOT ignored** — they are
content, and the author's write bumps `_vclock` so its version dominates a peer's
result-less copy regardless of timestamps.

### 1.3 Mixed-build guard (two layers)

Two nodes on different builds can disagree about what bytes to hash. Two
mechanisms make that safe:

- **Algorithm revision:** `merkle.rs::HASH_ALGO_REV` (a manually bumped `u32`).
  Bump it whenever the canonicalization algorithm changes (string construction,
  sort order, timestamp normalization, digest function).
- **Schema digest:** `merkle.rs::hash_schema_version` → `compute_hash_schema_digest`
  returns a 16-hex SHA-256 over `HASH_ALGO_REV` plus the sorted, de-duplicated
  `IGNORED_FIELDS` set. Adding or removing an ignored field changes this digest
  with **zero** human action — a field-set change is a fleet-visible protocol
  change and announces itself.

The digest rides additively in the Merkle exchange. Two nodes with different
digests can never agree on a root even on byte-identical data, so
`engine.rs::sync_entity_with_peer` (via `schema_allows_repair`) **skips**
tree-repair between them rather than re-pulling forever. A peer that sends no
digest (older build) is grandfathered as compatible.

### 1.4 Frozen tie-break comparator

`merkle.rs::tiebreak_hash` is a **separate, permanently frozen** canonical hash
used only for the equal-timestamp LWW tie-break (see §3). It excludes only
`_vclock`/`vector_clock`, applies no timestamp normalization, and is deliberately
**independent of `IGNORED_FIELDS`** (a merkle-ignored field still participates).
It must produce identical output in every past and future build; that is what
stops two peers from each concluding "I win" and ping-ponging a clock.

---

## 2. Merkle tree exchange

Trees are built on demand from the `entity_checksum` table by
`merkle.rs::MerkleService`. The tree has **two levels**:

- **Level 0 (root):** `get_root`. Children are `bucket_key → bucket_hash`.
  Root hash = `compute_root_hash` over the sorted bucket map.
- **Level 1 (bucket):** `get_bucket`. Children are `entity_id → content_hash`.
  Bucket hash = `compute_bucket_hash` over the sorted `(entity_id, hash)` pairs.

Bucketing (`get_bucket_index`) is the first character of the entity id,
lowercased — a flat 1-level fan-out, not a deep tree.

**Diff walk** (`engine.rs::sync_entity_with_peer`):

1. Compare local vs remote root children with `merkle::compare_trees` →
   `(need_from_remote, need_to_push)` bucket keys.
2. For each differing bucket, fetch both sides' level-1 nodes and `compare_trees`
   again → per-entity `pull_ids` / `push_ids`.
3. Pull missing/divergent entities in bounded chunks (a large divergent set never
   exceeds the client timeout in one request), resolve each (§3), record the
   resulting checksum.

**Root cache.** `merkle.rs` holds a process-wide `ROOT_CACHE` keyed by
`(advertise_cache_only, entity_type)`. An idle node answers every peer's root and
bucket request from memory. Every `entity_checksum` write path calls
`invalidate_root_cache`; a generation counter prevents a write that lands
mid-build from being shadowed by a stale entry.

**Checksum sweep.** The primary hash-maintenance path is the live-watch bridge
(`LIVE SELECT` on every synced table, funneled through `upsert_checksum` /
`record_tombstone`). The sweep (`engine.rs::refresh_checksums`, cadence
`ECK_CHECKSUM_SWEEP_SECS`, hourly by default) is a **low-frequency integrity
audit** for events the live watch missed, writes from outside the process, or bit
rot — not a per-cycle chore. `bootstrap_checksums` does one full pass at startup
(batched under a single transaction via `upsert_checksums_batch`).

**Empty-root backoff.** A cache node advertises an empty (or authoritative-only)
root as its steady state. `CACHE_EMPTY_ROOT_RECHECK_CYCLES` (10) + the
`cache_empty_backoff` map (`consume_empty_root_skip`) makes a full node skip that
`(cache-peer, table)` pair for that many cycles after seeing an empty root,
instead of re-fetching it every cycle.

**Futility backoff.** A version-agnostic circuit breaker: a repair pass that
pulls rows but writes none is *futile* (real convergence would make the next
cycle pull zero). Three futile passes against one `(peer, entity_type)` pair park
it for an escalating window (5min → 30min → 1h cap). In-memory only, keyed
`"<peer_url>|<entity_type>"`; a restart resets it.

**Tombstones.** A delete records a leaf with the constant `TOMBSTONE_HASH`,
`deleted = true`, and the deleting node's advanced `_vclock`
(`MerkleService::record_tombstone`). Conflict resolution reads it via
`tombstone_vclock` to distinguish "never had it" from "we deleted it, at this
clock", so a stale re-create cannot silently resurrect a delete.

---

## 3. Conflict resolution

Entry point: `core/src/sync/conflict.rs::resolve_and_upsert`. It returns a
`ResolveOutcome` (`Wrote` / `AlreadyEqual` / `LocalNewer` / `Tombstoned`) so the
caller records the checksum of the **actually-stored** content on every branch,
including the no-write branches (recording the wrong side masks real divergence
and causes perpetual re-pulls).

### 3.1 Vector clocks

`core/src/sync/vector_clock.rs::VectorClock` is `{instance_id: i64}`.
`compare` returns `ClockRelation::{Before, After, Equal, Concurrent}` by the
standard componentwise rule (missing component = 0). `merge` takes the
componentwise max.

Decision order in `resolve_and_upsert`:

- Incoming **tombstone**: a delete is intent — it wins on `Before`/`Equal`/
  `Concurrent`; only a strictly `After` local version (an intervening re-create)
  survives it.
- Local row **absent**: a held tombstone blocks resurrection unless the incoming
  create strictly dominates it (`tomb Before remote`); otherwise adopt.
- Both live: `Before` → adopt remote (adopt its clock **as-is**, no local
  increment — incrementing on adopt is what previously degraded everything to LWW
  with unbounded clock growth). `After` → keep local. `Equal` → if content hashes
  match, converged; if they differ (legacy null-clock rows), fall to LWW.
  `Concurrent` → LWW.

### 3.2 LWW tiebreak

`resolve_lww_conflict` merges the two clocks componentwise (max) **without** a
local increment — for a `Concurrent` pair the merge alone strictly dominates the
remote clock, so the kept side becomes causally `After` while the clock reaches a
**fixpoint** instead of growing on every keep-local. Winner selection:

1. **Ownership rule** (§3.3) if it applies;
2. else higher `updated_at` wins;
3. else (timestamp tie, incl. both-missing) the **frozen** `tiebreak_hash` (§1.4)
   decides — the side whose hash sorts lower adopts the other's. Both builds must
   reach the same verdict, so the comparator must be version-independent.

The merged clock is written back only if it actually advanced past what is stored
(re-writing an identical `_vclock` is pure churn).

### 3.3 Home-node ownership rule

`conflict.rs::ownership_verdict`. A row may carry `home_instance_id` — the node
that **authored** it (creator = authority). The rule is **transferable** via a
home-claim (the field changes). When both copies agree on the home node, the
copy that has seen **more of the home node's history** — the higher `_vclock`
component for `home_instance_id` — wins outright; LWW only breaks a tie. If the
copies disagree on the home (an in-flight transfer), the rule abstains and LWW
decides; the new home's later writes dominate naturally.

**Why vclock richness beats timestamps:** a peer's incidental touch (a UI edit
with a "newer" wall-clock time) must not override the authority's version of its
own record. Causal history (how much of the authority's edit stream a copy has
absorbed) is a stronger signal than a clock that can skew, tie, or run backwards
across machines.

---

## 4. Data tiering

Not all data replicates the same way. Three tiers.

**Merkle-synced entity whitelist.** `engine.rs::SYNC_ENTITY_TYPES` is the
explicit allowlist of tables that participate in Merkle sync (business master
data, processed document metadata + AI summary, graph-edge relation tables, etc).
`engine.rs::is_mesh_entity_type` validates any peer-supplied `entity_type`
against it (plus one point-to-point extra) **before** it is interpolated into
SurrealQL — the table position cannot be a bind parameter, so an unvalidated
value is both a SQL-injection vector and a write-anything surface. Relation
(`TYPE RELATION`) tables are flagged by `RELATION_ENTITY_TYPES` and take the
`write_adopted_relation` adopt path (a plain `UPSERT … CONTENT` is refused by the
DB on a relation table).

**"Summary out, blob maybe."** Heavy raw payloads and binary blobs are **not**
Merkle-synced. A processed `document` row (metadata + masked AI summary +
anonymized embedding) syncs; its bulky raw source (`document_raw`) stays on the
origin node and is served only by explicit point-to-point reverse-fetch. Binary
file bytes never ride the sync at all (§5). This is the origin-side data
classification: derived/summarized data propagates, fat originals stay put and
are fetched on demand.

**Relay-synced subset.** `engine.rs::RELAY_SYNC_TYPES` is the **bounded** subset
of `SYNC_ENTITY_TYPES` that also converges over the relay queue when a peer is
unreachable directly (different LAN / NAT). It is intentionally small — a full
Merkle walk over a poll-based queue is slow — and is ordered so that small,
genuinely-converging types finish first and a large slow type is placed last so
it cannot starve the others. Fat inline data (e.g. rows carrying an inline
thumbnail) is kept off this list even when a thin edge referencing it is on it.

**On-demand blob fetch.** `engine.rs::fetch_file_from_peers(hash)` back-fetches a
content-addressed (SHA-256) blob only when something actually opens the file. It
tries direct HTTP to each full peer first (`MeshClient::fetch_file` →
`/api/mesh/file/:hash`), then the relay blind-conduit (§5). The caller writes the
verified bytes into its own local store, so blobs converge **lazily** — only what
is opened, never a bulk push of every binary to every node. Cache peers are
skipped (a blind cache refuses to serve file content).

---

## 5. Blind-conduit file delivery

Cross-NAT blob delivery when no full peer is directly dialable. The relay only
ever shuttles ciphertext; it stores nothing readable at rest.

**Envelope.** `core/src/utils/crypto.rs::encrypt_bytes` / `decrypt_bytes`:
`{ "v": 1, "n": "<base64 24-byte XNonce>", "ct": "<base64 ciphertext>" }`.
Cipher is **XChaCha20-Poly1305** under a 32-byte shared mesh data key
(`crypto.rs::data_key`, from env `MESH_DATA_KEY`, 64 hex). The 24-byte nonce is
generated randomly per message (safe with XChaCha20's extended nonce). AEAD
authenticates the ciphertext: any tamper (bad version, flipped bytes, wrong key)
surfaces as an error, never a mis-decrypt. `BYTES_ENC_VERSION` is refused if it
does not match — no silent cross-version mis-parse.

(The entity-sync path uses a sibling envelope, `encrypt_json`/`decrypt_json`,
shape `{ "__enc": "<base64(24-byte nonce || ciphertext)>" }`, same cipher/key —
used to keep a **blind cache** node holding only ciphertext.)

**Round trip.** `RelayClient::fetch_file_via_relay(target, hash, timeout)`
dispatches a `file_fetch` mesh-task `{hash}`. The responder
(`wms/src/services/mesh_relay_poller.rs::handle_file_fetch` →
`build_file_fetch_ack`) looks the blob up by SHA-256 and returns one of:

- `{found:false, reason:"cache"}` — this node is a blind cache and refuses.
- `{found:false}` — not held / not on disk.
- `{found:false, reason:"too_large", size}` — over the cap.
- `{found:true, size, enc:<envelope>}` — encrypted (holder has the key).
- `{found:true, size, bytes:<base64>}` — plaintext fallback (a keyless full node;
  logged as a warning).

**Size cap.** `mesh_relay_poller.rs::FILE_FETCH_MAX_BYTES = 20 * 1024 * 1024`
(20 MiB). Chunking is a v1 non-goal; a single blob is either under the cap or
refused. Kept under the relay's 32 MiB body limit with headroom.

**CAS verification.** The requester rejects any returned bytes whose
`sha256(bytes) != hash` (`fetch_file_from_peers`, via
`utils::filestore::verify_sha256`) before trusting them — the bytes crossed an
untrusted relay and an untrusted peer.

**Direct-first.** The direct HTTP path is tried before the relay fallback; a
diagnostic env `ECK_FILE_FETCH_FORCE_RELAY=1` skips direct and exercises only the
relay path.

---

## 6. Relay roles (queue mechanics)

The relay is a **process-local notify + DB queue**: the client dispatches to the
same relay instance the target node polls, so an in-process `Notify` can wake a
held poll instantly while the durable queue lives in SurrealDB. Two decoupled
queues exist so control and data never collide: `mesh_task`
(`relay/src/handlers/mesh_relay.rs`, data-sync) and `client_mcp_task`
(`relay/src/handlers/client_mcp.rs`, gated MCP).

**Endpoints** (mesh queue): `dispatch/:target` (enqueue), `poll/:self`
(target pulls pending), `ack/:task_id` (target stores the result body),
`result/:task_id` (dispatcher reads it). The relay does not inspect payloads on
the dumb-pipe mesh queue; it only enforces that `envelope.target_uuid` matches
the path so a task cannot be misrouted by editing the URL.

**Poll cadence** is adaptive: `POLL_INTERVAL_BUSY_SECS = 3` when tasks are
waiting, `POLL_INTERVAL_IDLE_SECS = 30` when idle (returned to the poller as
`next_poll_in_seconds`).

**Ack byte budget.** Relay bodies are bounded. The responder trims an entity list
to fit `relay_ack_byte_budget()` (env `RELAY_ACK_MAX_BYTES`) via
`mesh_relay_poller.rs::fit_ack_budget`: it packs entities until the budget is
reached, defers the rest to a later cycle, and withholds any single entity larger
than the budget (that entity can only converge via the direct path). A missing
ack must be a `404`, not a success — an `UPDATE` on a task a given relay never
held still "succeeds" in the DB, so an unconditional ok let acks land on the
wrong relay while the right one redelivered forever.

**Delivered-flag + redelivery lease** (MCP queue, `client_mcp.rs`): a polled task
is stamped `delivered_at` and becomes **invisible** to further polls for
`REDELIVER_AFTER_SECS = 60`. That window — not the node's transient in-flight set
— is what prevents double execution; if the node crashes mid-execution the task
becomes pollable again after the lease. Acks are **first-ack-wins** (a
re-delivered second run fails a nonce check).

**Payload policy gate.** `mesh_relay.rs::enforce_payload_policy` reads
`RELAY_PAYLOAD_MODE`: `open` (passthrough), `disabled` (discovery-only board —
data relay refused), `paid` (allowed only when at least one party is paid). Only
`dispatch` is gated; `poll`/`ack`/`result` inherit — a task that exists was
already authorized.

---

## 7. Node roles

`engine.rs::SyncEngine.node_role` is `"full"` (default) or `"cache"`.

- **Full node:** holds authoritative data, advertises its content via Merkle,
  serves pulls, participates in conflict resolution, holds `MESH_DATA_KEY` (can
  encrypt/decrypt), and answers `file_fetch`.
- **Cache node:** skips the periodic Merkle sync entirely (`sync_cycle`
  short-circuits when `is_cache_node()`); it pulls entities on demand
  (`pull_entity_on_demand`, stored `is_cache = true`) and serves peers only its
  own **authoritative** subset (`MerkleService::new_cache_filtered` filters out
  `is_cache=true` rows). A cache node holds **no** `MESH_DATA_KEY`: it can neither
  read nor produce plaintext, only shuttle ciphertext (`crypto::prepare_outbound`
  withholds any plaintext row on a keyless cache). Its cached set is LRU-bounded
  (`evict_cache_lru`, `touch_cache`). Pushing canonical data *into* a cache is
  forbidden (`sync_entity_with_peer` clears `push_ids` for cache peers).

---

## PROTOCOL INVARIANTS

1. Two rows with identical business fields but differing node-local status
   fields (any `IGNORED_FIELDS` member) produce the **same** content hash.
2. Field order, nesting order, and equivalent timezone spellings of the same
   instant never change the content hash; canonicalization sorts keys at every
   depth and normalizes RFC3339 to UTC.
3. Two `f64` values that round to the same `f32` bits hash identically; two
   distinct integers never collide via float coercion.
4. Two nodes whose `hash_schema_version` digests differ never attempt
   tree-repair against each other (they would hash different field-sets); a peer
   that advertises no digest is treated as compatible.
5. `tiebreak_hash` output is invariant across all builds and independent of
   `IGNORED_FIELDS`; both peers in an equal-timestamp conflict derive the same
   winner.
6. Adopting a strictly-newer remote version copies its vector clock verbatim and
   never increments the local component.
7. A `Concurrent` conflict resolved by keep-local advances the stored clock at
   most once (to the componentwise merge) and reaches a fixpoint; repeated
   resolution of an already-merged pair writes nothing.
8. When both copies of a row agree on `home_instance_id`, the copy with the
   higher vector-clock component for that home node wins regardless of
   `updated_at`; timestamps decide only on a tie of that component.
9. A delete (tombstone) is overridden only by a strictly causally-newer
   re-create; equal or concurrent creates stay deleted.
10. Binary blob bytes never traverse Merkle sync or the relay sync queue; they
    are fetched on demand by SHA-256 and every fetched blob is CAS-verified
    (`sha256(bytes) == hash`) before it is trusted.
11. Any bytes a full node serves over the relay leave encrypted under
    `MESH_DATA_KEY` when the node holds it; the relay stores only the ciphertext
    envelope and can read neither the blob nor the entity payloads.
12. A cache node never emits plaintext it holds and never receives pushed
    canonical data; it advertises only its own authoritative rows.
13. A peer-supplied `entity_type` that is not in the mesh whitelist
    (`is_mesh_entity_type`) is rejected before any SurrealQL is built.
14. A relay `ack` that matches zero rows returns `404`, so a dispatcher walking
    multiple relays never treats a wrong-relay ack as delivered.
15. A relay task delivered to a node is invisible to re-poll for
    `REDELIVER_AFTER_SECS`; a crash before ack makes it pollable again, and only
    the first ack is accepted.
