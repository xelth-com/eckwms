use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use surrealdb::types::SurrealValue;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};


use crate::db::SurrealDb;
use crate::sync::{
    merkle::{self, extract_entity_leaf_id, MerkleRequest, MerkleService},
    mesh_client::{self, MeshClient},
    relay_client::RelayClient,
};

/// Entity types to include in Merkle sync sweeps.
/// Add new SurrealDB tables here as they become syncable.
pub const SYNC_ENTITY_TYPES: &[&str] = &[
    "item",
    "order",
    "product",
    "partner",
    // Real staff accounts replicate across the customer's own mesh so one
    // password/PIN works on every node (owner decision 2026-07-09; record id
    // is the USERNAME so independently-seeded nodes converge per-account via
    // LWW instead of duplicating). Hashes only — never plaintext — and the
    // ops control plane still can't read the table (ZONE_1_TABLES denylist).
    // The install-time `setup-admin` bootstrap account deliberately does NOT
    // mesh: it lives in the separate node-local `users_db`, which this engine
    // never touches.
    "user",
    "file_resource",
    "location",
    "quant",
    "picking",
    "move_line",
    "rack",
    "action_proof",
    "delivery_carrier",
    "delivery_tracking",
    // The shipment record itself (carrier ref, tracking#, recipient, dims). Its
    // recipient_* fields are PII → also in ops.rs ZONE_1_TABLES. The raw scraper
    // payload stays in the local-only `shipment_raw` table (not listed here).
    "stock_picking_delivery",
    "device_intake",
    "inventory_discrepancy",
    "product_alias",
    "category",
    "menu_item",
    // POS fiscal records (ecKasse, paid tier). A closed/aborted/storno'd sale
    // and its storno decisions replicate across the customer's own mesh (the
    // register that created the sale is its HOME node — `home_instance_id` +
    // `_vclock` are stamped on every write in `pos::services::transaction`, so
    // the creator stays the fiscal authority and a peer's incidental touch can
    // never overwrite the `tse_*`/`status`/`resolution_status`/`content_hash`
    // fields via the ownership rule in `conflict::ownership_verdict`). The
    // CONTENT of a sale meshes; the NODE-LOCAL fiscal state tables
    // (`pending_fiscal_operations`, `tse_state`) are deliberately NOT here —
    // the TSE/WAL is per-register and must never replicate. On a non-POS node
    // these tables simply stay empty (and are skipped by the sweep).
    "transaction",
    "storno_log",
    "ai_sop",
    // WASM plugin registry (design .eck/WASM_ARCHITECTURE.md §6). The RECORD
    // meshes — it is tiny (name, sha256, hooks, capabilities, enabled,
    // home_instance_id, _vclock) — so an admin installs/enables a plugin once
    // and the whole customer mesh converges on it. The `.wasm` BINARY does NOT
    // merkle-sync: each node lazily pulls it by sha256 on the first hook fire
    // (`PluginRegistry::resolve` → `MeshClient::fetch_file` → `GET
    // /api/mesh/file/:hash`), NEVER at startup (kiosk-OTA lesson). No per-table
    // sweep registration is needed beyond this entry: the record has no
    // dedicated id column, so `bootstrap_checksums` keys it via the implicit
    // `id` leaf (= plugin name) through `extract_entity_leaf_id`, exactly like
    // `ai_sop`. `enabled` is a HASHED content field (not in
    // `merkle::IGNORED_FIELDS`), so an enable/disable bumps the author `_vclock`
    // and its version dominates peers' — registry writes stamp `_vclock` the
    // house way (`conflict::next_local_vclock`).
    "wasm_plugin",
    // Carries the Xelixir C2 control plane (xelixir_command/status/token fields).
    // The edge node's `LIVE SELECT` watcher reacts to remote `xelixir_command` writes.
    "registered_device",
    // Processed support-ticket metadata + AI summary + PPRL-anonymized embedding.
    // The raw Zoho payload lives in `document_raw` (intentionally NOT synced — stays
    // on the scraper node). When a peer needs the full raw body, it requests it via
    // the `mesh_task` reverse-fetch queue (see `engine.rs::process_tasks`).
    "document",
    // Fahrtenbuch — replicate across a customer's own PAID mesh (HA/backup).
    // The blind relay can't read the payload (encrypted), and raw track points
    // are pruned at TRIP_RAW_RETENTION_DAYS; what survives is the sealed
    // aggregate. cell_tower is a PII-free mast cache (shared = fewer lookups).
    "trip",
    "visit_task",
    "cell_tower",
    // Fahrtenbuch vehicle registry (plate / Kennzeichen + plate-photo CAS ref).
    // PII-free reference data — replicated across the customer's own mesh.
    "vehicle",
    // Odoo mirror — the tenant's warehouse master data pulled via the external
    // JSON-RPC connector (wms `services::odoo`). Authored only on the connector
    // node; replicated read-only across the paid mesh so peers (e.g. the kiosk)
    // hold the catalogue/stock without each hitting Odoo. Rows carry an
    // `updated_at` stamp so a changed re-pull wins LWW on peers.
    "odoo_warehouse",
    "odoo_location",
    "odoo_product",
    "odoo_quant",
    // Exact Online scraped stock positions — the "soll" (should-be) qty per
    // (item_code, warehouse_code), authored only on the scraper node. Replicated
    // read-only so peers (the kiosk the PDA talks to) can run the soll/ist
    // reconcile locally without the Exact data being scraper-local.
    "stock_position",
    // On-demand machine translations of user-facing content (support-ticket AI
    // summaries → the viewer's language). Deterministic id from (source,field,
    // lang) so it keys on the implicit leaf `id` (no dedicated id column, like
    // `location`/`product_alias`); content-hashed over its business fields, so a
    // translation produced on one node converges to peers instead of each node
    // re-calling Gemini. The `has_translation` provenance edge stays authored
    // node-locally at store time (it is not in RELATION_ENTITY_TYPES, unlike
    // `has_attachment`); serving reads the row by deterministic id, not by graph
    // traversal, so a peer serves it without the edge.
    "translation",
    // Attachment graph edge (document/order/user -> file_resource). A RELATION
    // table, so it takes the `write_adopted_relation` path in conflict.rs —
    // a plain `UPSERT … CONTENT` is REFUSED by SurrealDB on a TYPE RELATION
    // table ("which is not a relation, but expected a RELATION"), and `in`/`out`
    // cross the wire as Thing STRINGS (`document:\`53451000033373254\``) that
    // must be coerced back with `type::record()`.
    //
    // Why it has to sync (2026-07-25): `file_resource` replicated but the edges
    // did not, so a peer held all 1961 attachment rows without knowing WHICH
    // ticket each one hangs on — every peer's ticket view showed zero
    // attachments. Edge ids are random ULIDs from RELATE, unique per author, so
    // there is no id collision to resolve; the per-author `$edge_exists` guard
    // plus convergence keeps a synced edge from being re-created as a duplicate.
    "has_attachment",
    // Gemini-vision journal: one row per paid vision answer on an attachment page
    // (metadata tier, like `has_attachment`/`file_resource` metadata — NOT a blob).
    // The stored `answer`/`question` are PPRL-MASKED before write (the raw scan
    // itself never meshes), so replicating the journal leaks no PII while making
    // an already-paid answer reusable fleet-wide. The row id is deterministic —
    // `sha256(file|page|region|question_norm)[..20]` — so the same analysis from
    // two nodes converges instead of duplicating; it has no dedicated id column,
    // so `bootstrap_checksums` keys it via the implicit `id` leaf through
    // `extract_entity_leaf_id`, exactly like `ai_sop`/`translation`. The author's
    // write bumps `_vclock` so its version dominates a peer's copy. The
    // per-node-derived `question_embedding` is in `merkle::IGNORED_FIELDS`.
    "vision_analysis",
    // Warehouse receiving (2026-09-07): the parcel header and the units it was
    // broken into. Both carry client_name/client_address -> also on the ops.rs
    // ZONE_1_TABLES denylist. They mesh because receiving happens on whichever
    // node the PDA can reach (kiosk on-LAN, master off-LAN) while the units are
    // looked up from anywhere; ids are client-minted UUIDs, so two nodes never
    // collide and LWW/vclock converges a re-edit.
    "receipt",
    "receipt_unit",
    // Domain-event log (DOMAIN_MODEL Layer 5, `.eck/ENTITY_PAGES_AND_SCAN_MIRROR.md`
    // §3): one append-only row per meaningful act (move / status / photo / scan),
    // written fire-and-forget by `services::events::record`. Rows carry
    // `home_instance_id` + `_vclock` the house way and are never updated, so —
    // like `has_attachment` edges — ids (random UUIDs) never collide and the
    // merkle bootstrap keys each row by its implicit leaf id. Meshes so a
    // PDA-side act converges to the office master's timeline.
    "event",
    // Examination-bundle write path (wms mcp/bundles.rs, 2026-10-28): the unit
    // registry and its STK/MTK/repair events. Deterministic ids (murmur of the
    // normalized serial / serial|kind|protocol_no), so the same record from two
    // nodes converges. Staging, protocol numbering and the write audit stay
    // node-local (one authoritative protocol writer).
    "device",
    "device_event",
];

/// Sync class **`vault`** (`.eck/PII_VAULT.md` §3.1, P1 item 1) — the tables
/// that carry decryptable customer PII and therefore replicate ONLY between
/// reveal-capable full nodes (`home` ↔ `operator`). They are a tier *next to*
/// [`SYNC_ENTITY_TYPES`], not a member of it, so every existing sweep, merkle
/// bootstrap and live-watch stays byte-identical on a node that has no vault
/// key.
///
/// **How the role is decided: by the key, not by config.** A node participates
/// in this tier iff [`vault_sync_enabled`] — i.e. `keyring::vault_key()` is
/// `Some`. That is only true for a node whose safe blob (or keyfile) carries a
/// VK, which only the `home`/`operator` roles get (§2). A cache node (eck1/2/3)
/// or a relay can neither hold nor be handed a VK, so it can neither send nor
/// receive these rows — and `MESH_NODE_ROLE` in a node's own env cannot
/// escalate it into the tier, because the env never yields a VK. Belt and
/// braces on top of that: [`sync_entity_with_peer`] refuses a vault table
/// against any `peer.is_cache()` peer, and `pii_vault` is deliberately NOT in
/// `RELAY_SYNC_TYPES` — vault rows never touch the relay queue.
pub const VAULT_ENTITY_TYPES: &[&str] = &["pii_vault"];

/// True when this node may hold and replicate vault rows (it has a VK).
pub fn vault_sync_enabled() -> bool {
    crate::pii_vault::enabled()
}

/// True when `entity_type` belongs to the `vault` sync class.
pub fn is_vault_entity_type(entity_type: &str) -> bool {
    VAULT_ENTITY_TYPES.contains(&entity_type)
}

/// The tables THIS node sweeps: the normal set, plus the `vault` tier when the
/// node holds a vault key. Call this instead of iterating `SYNC_ENTITY_TYPES`
/// directly in any sweep that should follow the node's role.
pub fn sync_entity_types() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = SYNC_ENTITY_TYPES.to_vec();
    if vault_sync_enabled() {
        v.extend_from_slice(VAULT_ENTITY_TYPES);
    }
    v
}

/// Which `SYNC_ENTITY_TYPES` entries are graph-edge (`TYPE RELATION`) tables.
/// These need `INSERT RELATION` + `type::record()` coercion on the adopt path;
/// see `conflict::write_adopted_relation`. `contains` (location->rack) is
/// deliberately NOT here — it is derived node-locally from synced rows.
pub const RELATION_ENTITY_TYPES: &[&str] = &["has_attachment"];

/// True when `entity_type` names a `TYPE RELATION` table in the mesh.
pub fn is_relation_entity_type(entity_type: &str) -> bool {
    RELATION_ENTITY_TYPES.contains(&entity_type)
}

/// Validate a peer-supplied entity_type against the tables the mesh may touch.
/// The P2P pull/push handlers interpolate `entity_type` into SurrealQL (table
/// position can't be a bind parameter), so an unvalidated value is both a
/// SurrealQL injection vector ("product; DELETE partner; SELECT …") and a
/// write-anything surface (system_config, mesh_task, …). `document_raw` is
/// allowed on top of SYNC_ENTITY_TYPES: it's not merkle-synced but IS pushed
/// point-to-point by the `request_raw_docs` reverse-fetch.
///
/// `pii_vault` is accepted only while this node is in the `vault` tier: a node
/// without a VK answers ENTITY_UNSUPPORTED for it, which is exactly the
/// "version skew" path the caller already tolerates — so a cache node silently
/// refuses vault rows instead of storing ciphertext it can never open.
pub fn is_mesh_entity_type(entity_type: &str) -> bool {
    if is_vault_entity_type(entity_type) {
        return vault_sync_enabled();
    }
    entity_type == "document_raw" || SYNC_ENTITY_TYPES.contains(&entity_type)
}

/// Entity types converged over the RELAY when a peer is unreachable directly
/// (different LAN / NAT). Bounded to the operational tables that must reach every
/// node regardless of network (a full merkle walk over the poll-based relay queue
/// is slow, so we don't run all ~25 types this way — the big Odoo/Exact mirrors
/// are authored on one node and converge on the LAN). This is what makes trips
/// (and devices/visits/docs) part of the mesh across networks, not just on-LAN.
const RELAY_SYNC_TYPES: &[&str] = &[
    // POS fiscal records (2026-10-06, eckasse Stage 0): a register in a shop and
    // its back office are usually NOT on one LAN; a closed sale or storno that
    // missed the outbox push must still converge over the relay.
    "transaction",
    "storno_log",
    "trip",
    "visit_task",
    "vehicle",
    "registered_device",
    "cell_tower",
    // Translation claims + results are tiny text rows that MUST cross NATs:
    // otherwise cross-node work-dedup (one node claims "I'm translating X", the
    // result "X is translated") only works on-LAN, and off-LAN nodes each re-call
    // Gemini for the same (source,field,lang).
    "translation",
    // Vision journal (2026-09-07): one masked Gemini answer per (file, page,
    // region, question), deterministic id, a few hundred bytes. Same argument as
    // `translation`: a paid answer must reach off-LAN nodes or each of them
    // pays Gemini again for the same page.
    "vision_analysis",
    // Attachment edges ride the relay even though `file_resource` does not
    // (2026-07-25). Asymmetric on purpose: a file_resource row carries the
    // inline `avatar_b64` thumbnail (up to 50 KB of base64) and there are ~2000
    // of them — too fat for the poll-based queue — while an edge is four short
    // fields, so the whole set is a few hundred KB. And `document` IS on this
    // list, so without the edges an off-LAN node receives tickets that claim
    // attachments and can name none of them. A peer that has not yet met
    // `file_resource` on-LAN just holds dangling edges, which SurrealDB permits
    // and `list_attachments` renders as empty fields.
    "has_attachment",
    // Receiving rides the relay for the same reason `trip` does: the PDA that
    // took the receipt is frequently off-LAN (LTE), and a parcel booked into
    // ZONE-INBOUND from the road must still show up on the office master's zone
    // list. Small rows (a header + a handful of units per parcel).
    "receipt",
    "receipt_unit",
    // ORDER MATTERS, and `document` goes LAST — the loop below is sequential
    // and every relay round-trip is capped at 20 s, so a type that never
    // converges starves everything after it. `document` is exactly that: ~10 k
    // rows, the peer asks for 100 ids per chunk and the ack byte budget only
    // fits ~60-70, so it re-requests the same chunk every cycle forever.
    // Measured 2026-07-25 with it listed 6th: the two types after it got one
    // late, already-busy slot per cycle and 408'd out — has_attachment stalled
    // at 315/1942 while document ground on. Small, genuinely-converging types
    // first: they finish in a few cycles and then cost one root-hash compare.
    "document",
];

/// Coordinates mesh synchronization for this node.
///
/// Architecture: the central relay at 9eck.com is **strictly a tracker** (service
/// discovery via heartbeat + mesh status). It never routes actual data payloads.
/// All entity sync happens directly P2P between nodes using Merkle tree diffing.
///
/// Flow:
/// 1. Relay heartbeat (every 5 min) — register our base_url so peers can find us
/// 2. Relay discovery — ask relay for list of online peers
/// 3. For each peer: compare Merkle roots → drill into differing buckets → exchange entities
/// Per-peer adaptive backoff state. Cross-NAT meshes have peers that are
/// permanently unreachable from this side (the other peer dials in instead),
/// so a peer that fails repeatedly should not be retried every minute.
///
/// Stored as wall-clock (`chrono::DateTime<Utc>`) rather than monotonic
/// `Instant` so the state can be persisted to SurrealDB and restored across
/// WMS restarts — otherwise a chronically-unreachable peer ramps through
/// 0→2→3→…→7+ from scratch on every launch.
#[derive(Debug, Default, Clone)]
struct PeerHealth {
    consecutive_failures: u32,
    skip_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl PeerHealth {
    /// Backoff schedule based on consecutive failures.
    /// 0–2 fails: no skip (transient hiccups). From the 3rd fail on it doubles:
    /// 30s, 60s, 120s, 240s, 480s, 960s, then capped at 30min. A peer that
    /// recovers resets the counter, so a brief outage doesn't poison the
    /// long-term cadence.
    fn next_skip(&self) -> Option<chrono::Duration> {
        if self.consecutive_failures <= 2 {
            return None;
        }
        let shift = (self.consecutive_failures - 3).min(7);
        let secs = (30u32 << shift).min(1800);
        Some(chrono::Duration::seconds(secs as i64))
    }
}

/// How many sync cycles a full node skips a (cache peer, table) pair after
/// seeing the cache advertise an EMPTY merkle root. Empty root = the steady
/// state for caches (they hold only what API traffic asked them to hold and
/// advertise only their authoritative subset), so per-cycle re-fetching is
/// pure churn. Kept short: a cache's own authoritative rows appear at most
/// this many cycles late (~10 min at the ~60 s cycle period).
const CACHE_EMPTY_ROOT_RECHECK_CYCLES: u8 = 10;

/// Consume one skip credit for `key`. `true` = skip this cycle. When the
/// credits run out the entry is removed so the next call does a real check
/// (and re-arms if the root is still empty).
fn consume_empty_root_skip(map: &mut HashMap<String, u8>, key: &str) -> bool {
    match map.get_mut(key) {
        Some(left) if *left > 0 => {
            *left -= 1;
            true
        }
        Some(_) => {
            map.remove(key);
            false
        }
        None => false,
    }
}

// ─── Futility backoff (mixed-build mesh churn, layer 1) ──────────────────────
//
// Version-AGNOSTIC circuit breaker for merkle tree-repair. A repair pass that
// pulls rows but writes NONE is FUTILE: had convergence actually happened, the
// next cycle would pull 0. Three futile passes in a row against the same
// (peer, entity_type) pair park it for an escalating window (5min → 30min → 1h
// cap), so the mesh stays quiet under ANY useless-churn pathology — including a
// hash-field-set skew between builds, which layer 2 cannot damp while one side
// is too old to announce its schema (the grandfather case). IN-MEMORY ONLY,
// keyed "<peer_url>|<entity_type>": a process restart resets it, approximating
// "reset on peer restart". The live outbox/push path is untouched.
#[derive(Debug, Default, Clone, PartialEq)]
struct FutilityState {
    consecutive_futile: u32,
    skip_until: Option<chrono::DateTime<chrono::Utc>>,
}

/// Outcome of one merkle repair pass, fed to the futility state machine.
#[derive(Debug, Clone, Copy)]
struct PassResult {
    pulled_n: usize,
    writes_n: usize,
}

/// Escalating skip window once the futility threshold (3) is reached:
/// 3 → 5min, 4 → 30min, 5+ → 1h (cap). Below the threshold: no skip.
fn futility_skip_delay(consecutive_futile: u32) -> Option<chrono::Duration> {
    match consecutive_futile {
        0..=2 => None,
        3 => Some(chrono::Duration::minutes(5)),
        4 => Some(chrono::Duration::minutes(30)),
        _ => Some(chrono::Duration::hours(1)),
    }
}

/// Pure transition of the futility state machine. A pass is FUTILE iff it pulled
/// > 0 and wrote 0 (`converged_n` does NOT excuse it — if convergence worked the
/// next cycle would pull 0). On the 3rd+ consecutive futile pass, arm
/// `skip_until`. Any write (`writes_n > 0`) or any pass that pulled 0 (trees
/// agree) resets to the default.
fn futility_next(
    prev: &FutilityState,
    pass: PassResult,
    now: chrono::DateTime<chrono::Utc>,
) -> FutilityState {
    if pass.writes_n > 0 || pass.pulled_n == 0 {
        return FutilityState::default();
    }
    let consecutive_futile = prev.consecutive_futile + 1;
    let skip_until = match futility_skip_delay(consecutive_futile) {
        Some(d) => Some(now + d),
        None => prev.skip_until,
    };
    FutilityState {
        consecutive_futile,
        skip_until,
    }
}

// ─── Boot-time checksum bootstrap mode ───────────────────────────────────────
//
// Decide whether the boot `bootstrap_checksums` should WIPE `entity_checksum`
// before rebuilding, or take the cheap idempotent compare-and-skip refresh.
//
// The historical wipe existed to purge structurally-invalid rows — legacy
// Thing-repr keys ("registered_device:de19…" instead of the bare leaf) that
// landed records in the wrong merkle bucket. But wiping + re-UPSERTing ~35k rows
// on EVERY restart costs ~80 s at ~77% CPU and races itself into "Transaction
// conflict" on the kiosk. The compare-and-skip refresh already corrects every
// still-existing row whose hash drifted, so a wipe is only actually needed when
// the stored rows may be *structurally* wrong for this build — i.e. the content
// hash schema changed (`merkle::hash_schema_version`: algorithm rev or
// IGNORED_FIELDS set). We persist that marker and auto-wipe only when it changes
// (or is absent — first boot after this ships), and honor an explicit
// `ECK_CHECKSUM_BOOT_WIPE=1` override. Warm restarts on an unchanged schema then
// take the no-wipe path and report "N unchanged".
fn boot_should_wipe(
    force_env: Option<&str>,
    stored_version: Option<&str>,
    current_version: &str,
) -> bool {
    if matches!(force_env, Some(v) if v == "1" || v.eq_ignore_ascii_case("true")) {
        return true;
    }
    match stored_version {
        Some(v) => v != current_version, // schema/hash-version changed → rebuild clean
        None => true,                    // pre-marker DB (first boot after upgrade) → wipe once
    }
}

// ─── Peer hash-schema tracking (mixed-build mesh churn, layer 2) ─────────────
//
// Remembers the last content-hash schema digest each peer advertised (see
// merkle::hash_schema_version) plus a WARN throttle, so a schema mismatch is
// logged at most once per peer per hour and a mismatch→match transition (peer
// upgraded to our field-set) can clear the peer's futility backoff.
#[derive(Debug, Default, Clone)]
struct PeerSchemaState {
    /// Last schema the peer advertised. `None` = an older build that doesn't
    /// send the field (grandfathered as compatible).
    last_schema: Option<String>,
    /// Last time a schema mismatch was WARN-logged for this peer (throttle).
    last_mismatch_warn: Option<std::time::Instant>,
}

pub struct SyncEngine {
    instance_id: String,
    mesh_id: String,
    relay: RelayClient,
    db: SurrealDb,
    sync_secret: Option<String>,
    /// `"full"` (default) or `"cache"`. Cache nodes skip the periodic merkle
    /// sync — they only pull entities on demand from full peers.
    node_role: String,
    /// Tracks per-peer reachability. Keyed by base_url so it survives peer
    /// rediscovery from the relay (instance_id might change after re-registration
    /// but the URL is stable). Cleared if a peer recovers.
    peer_health: Arc<Mutex<HashMap<String, PeerHealth>>>,
    /// Unix seconds of the last full checksum sweep. Live watchers are the
    /// primary hash-maintenance path; the sweep is a low-frequency integrity
    /// audit (missed live events, writes from outside the process, bit rot),
    /// not a per-cycle chore — running it every 60 s re-hashed the whole DB
    /// continuously and pegged ~2 cores.
    last_sweep: std::sync::atomic::AtomicI64,
    /// Per `(cache peer, entity_type)` skip credits, keyed `"<peer_url>|<table>"`.
    /// A cache advertises only its authoritative subset — usually an EMPTY
    /// merkle root — and pushing into a cache is forbidden, so the whole
    /// exchange is a no-op; without this backoff every full node re-fetched
    /// every cache's root for every table every cycle (~80 TLS GETs/min on a
    /// 4-cache mesh). See [`CACHE_EMPTY_ROOT_RECHECK_CYCLES`].
    cache_empty_backoff: Mutex<HashMap<String, u8>>,
    /// Per-`(peer_url, entity_type)` merkle tree-repair futility backoff
    /// (layer 1). In-memory only, keyed `"<peer_url>|<entity_type>"`. See
    /// [`FutilityState`].
    futility: Mutex<HashMap<String, FutilityState>>,
    /// Per-peer last-seen content-hash schema + WARN throttle (layer 2), keyed
    /// by peer base_url. See [`PeerSchemaState`].
    peer_schema: Mutex<HashMap<String, PeerSchemaState>>,
}

impl SyncEngine {
    pub fn new(
        instance_id: String,
        mesh_id: String,
        relay: RelayClient,
        db: SurrealDb,
        sync_secret: Option<String>,
        node_role: String,
    ) -> Self {
        Self {
            instance_id,
            mesh_id,
            relay,
            db,
            sync_secret,
            node_role,
            peer_health: Arc::new(Mutex::new(HashMap::new())),
            // Start the sweep clock at boot: bootstrap_checksums already does
            // the full pass on startup, the first audit is due one interval later.
            last_sweep: std::sync::atomic::AtomicI64::new(chrono::Utc::now().timestamp()),
            cache_empty_backoff: Mutex::new(HashMap::new()),
            futility: Mutex::new(HashMap::new()),
            peer_schema: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_cache_node(&self) -> bool {
        self.node_role == "cache"
    }

    // ─── Peer health persistence ─────────────────────────────────────────────

    /// Restore peer_health from the `peer_health_state` table. Called once
    /// at startup so a chronically-unreachable peer (cross-NAT) doesn't ramp
    /// through 0→2→3→…→7+ again on every WMS restart.
    pub async fn load_peer_health(&self) -> anyhow::Result<usize> {
        let rows: Vec<Value> = self
            .db
            .query(
                "SELECT base_url, consecutive_failures, \
                        type::string(skip_until) AS skip_until \
                 FROM peer_health_state",
            )
            .await?
            .take(0)
            .map_err(|e| anyhow::anyhow!(e))?;

        let mut health = self.peer_health.lock().await;
        let mut loaded = 0usize;
        for row in &rows {
            let Some(url) = row.get("base_url").and_then(|v| v.as_str()) else {
                continue;
            };
            let cf = row
                .get("consecutive_failures")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let skip_until = row
                .get("skip_until")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
            health.insert(
                url.to_string(),
                PeerHealth {
                    consecutive_failures: cf,
                    skip_until,
                },
            );
            loaded += 1;
        }
        if loaded > 0 {
            info!("Restored peer_health for {} peer(s) from previous run", loaded);
        }
        Ok(loaded)
    }

    /// Persist a single peer's health to the DB. Called from sync_cycle when
    /// the entry transitions (failure count increment or recovery). UPSERT
    /// keyed by base_url so it's idempotent across re-discoveries.
    async fn persist_peer_health(&self, url: &str, health: &PeerHealth) {
        let skip = health.skip_until.map(|d| d.to_rfc3339()).unwrap_or_default();
        let cf = health.consecutive_failures as i64;
        let _ = self
            .db
            .query(
                "UPSERT peer_health_state SET \
                    base_url = $url, \
                    consecutive_failures = $cf, \
                    skip_until = $skip, \
                    updated_at = time::now() \
                 WHERE base_url = $url",
            )
            .bind(("url", url.to_string()))
            .bind(("cf", cf))
            .bind(("skip", skip))
            .await;
    }

    // ─── Cache mode — pull-on-demand ─────────────────────────────────────────

    /// Pull a single entity from any reachable full peer and store it locally
    /// marked as `is_cache=true`. Intended for cache nodes — when an API hit
    /// on a synced table doesn't find a row, the handler calls this to lazily
    /// hydrate the entity from a full peer instead of returning 404.
    ///
    /// Returns `Some(entity_json)` on first successful pull, `None` if no peer
    /// has it. Idempotent: re-pulling an already-cached entity just refreshes
    /// `last_accessed_at`.
    pub async fn pull_entity_on_demand(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Option<Value> {
        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        // Skip cache peers — they only have what they themselves cached,
        // and asking the cache for what we don't have just bounces.
        for peer in peers.into_iter().filter(|p| !p.is_cache()) {
            // Direct HTTP first (fast on a reachable peer). On failure fall back
            // to the relay reverse-fetch queue: dispatch a `pull_request` that
            // the peer's mesh_relay_poller fulfills and acks. This is the same
            // cross-NAT path the periodic sync uses — without it a cache node
            // could never hydrate from a NAT'd full peer (e.g. the kiosk master
            // registered with a LAN-only base_url, unreachable from the public
            // cache node).
            let entities: Vec<Value> = match peer
                .pull_entities(entity_type, &[entity_id.to_string()])
                .await
            {
                Ok(e) if !e.is_empty() => e,
                Ok(_) => continue, // peer reachable but lacks this id — next peer
                Err(err) => {
                    debug!(
                        "pull_entity_on_demand: direct {} failed ({}) — relay reverse-fetch from {}",
                        peer.peer_url(),
                        err,
                        peer.target_instance_id()
                    );
                    match self
                        .relay
                        .pull_entities_via_relay(
                            peer.target_instance_id(),
                            entity_type,
                            &[entity_id.to_string()],
                            15,
                        )
                        .await
                    {
                        Ok(e) if !e.is_empty() => e,
                        Ok(_) => continue,
                        Err(e) => {
                            debug!(
                                "pull_entity_on_demand: relay reverse-fetch from {} failed: {}",
                                peer.target_instance_id(),
                                e
                            );
                            continue;
                        }
                    }
                }
            };

            let entity = entities.into_iter().next().unwrap();

            // Upsert into the local table — same path conflict::resolve_and_upsert
            // would take on a sync push from this peer.
            if let Err(e) = crate::sync::conflict::resolve_and_upsert(
                &self.db,
                entity_type,
                entity_id,
                entity.clone(),
                &self.instance_id,
            )
            .await
            {
                warn!(
                    "pull_entity_on_demand: upsert {}:{} failed: {}",
                    entity_type, entity_id, e
                );
                continue;
            }

            // Record the checksum, flagged as cache + freshly accessed.
            let _ = self
                .db
                .query(
                    "UPSERT entity_checksum SET \
                        entity_type = $et, \
                        entity_id = $eid, \
                        content_hash = $ch, \
                        full_hash = $ch, \
                        source_instance = $src, \
                        is_cache = true, \
                        last_accessed_at = time::now(), \
                        last_updated = time::now(), \
                        updated_at = time::now() \
                     WHERE entity_type = $et AND entity_id = $eid",
                )
                .bind(("et", entity_type.to_string()))
                .bind(("eid", entity_id.to_string()))
                .bind((
                    "ch",
                    merkle::compute_content_hash(&entity).unwrap_or_default(),
                ))
                .bind(("src", peer.peer_url().to_string()))
                .await;
            merkle::invalidate_root_cache(entity_type);

            info!(
                "Cache pull: {}:{} from {} ({} bytes)",
                entity_type,
                entity_id,
                peer.peer_url(),
                entity.to_string().len()
            );
            return Some(entity);
        }
        None
    }

    /// Fetch a CAS blob's BYTES from whichever full peer still has them, by
    /// SHA-256 hash. `file_resource` metadata (incl. the inline `avatar_b64`
    /// thumbnail) merkle-syncs to every node, but the blob itself never did —
    /// `MeshClient::fetch_file` existed with no caller, so a peer that received
    /// an attachment row could show its thumbnail and nothing else. This is the
    /// caller: the wms `/api/files/:id` handler invokes it on a local CAS miss
    /// and writes the bytes into its own filestore, so the blob converges LAZILY
    /// (only what someone actually opens) instead of replicating ~2600 binaries
    /// to every node.
    ///
    /// On-demand only, per `.eck/SYNC_CLASSIFICATION.md` ("summary out, blob
    /// maybe"): the bytes STAY on the origin node and are back-fetched by hash
    /// when someone actually opens the file. Nothing is ever eagerly pushed —
    /// eager replication is a per-item opt-in the spec reserves for the future
    /// `blob_policy: replicate`, not a default. (2026-07-25: an owner-push
    /// stager + sealed store on the eckN nodes was briefly built here, which
    /// made the blind transit nodes HOLD data; reverted the same day.)
    ///
    /// Cache peers are skipped — a blind cache refuses to serve file content by
    /// design (see wms `serve_mesh_file`), so asking one just bounces.
    ///
    /// Two paths, tried in order:
    /// 1. **Direct HTTP** — dial each full peer's `/api/mesh/file/:hash`
    ///    (`MeshClient::fetch_file`). Fast when a peer is reachable.
    /// 2. **Relay blind-conduit** (cross-NAT) — when no full peer is directly
    ///    dialable (both sides NAT'd), ask each full peer over the relay
    ///    `file_fetch` mesh-task. The owner encrypts the blob under
    ///    `MESH_DATA_KEY`, so the relay only shuttles ciphertext transiently
    ///    (nothing readable at rest); this node decrypts on arrival. This is the
    ///    spec's blind-conduit design — previously "not built yet", now built.
    ///    Every relay-served blob is CAS-verified (`sha256(bytes) == hash`)
    ///    before it's trusted, since it crossed an untrusted relay + peer.
    ///
    /// Diagnostic flag: `ECK_FILE_FETCH_FORCE_RELAY=1` (read per-call) SKIPS the
    /// direct loop and exercises only the relay path — for testing cross-NAT
    /// blob serving from a box that could otherwise reach the peer directly.
    ///
    /// The caller writes the returned bytes into its own local filestore (via
    /// `FileStore::write_verified`), so the blob converges LAZILY. When every
    /// path misses, this returns `None` and the caller 404s.
    pub async fn fetch_file_from_peers(&self, hash: &str) -> Option<Vec<u8>> {
        // Longer than the direct path's 8 s client timeout: the target only
        // answers a relay task on its idle poll (~15 s cadence, POLL_INTERVAL_
        // IDLE_SECS), so a direct-style short timeout would false-fail before the
        // target ever picks the task up.
        const FILE_FETCH_RELAY_TIMEOUT: u64 = 90;

        let force_relay = std::env::var("ECK_FILE_FETCH_FORCE_RELAY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        // ── Path 1: direct HTTP (skipped under ECK_FILE_FETCH_FORCE_RELAY) ──
        if !force_relay {
            for peer in peers.iter().filter(|p| !p.is_cache()) {
                match peer.fetch_file(hash).await {
                    Ok(bytes) if !bytes.is_empty() => {
                        info!(
                            "Mesh file fetch: {} ({} bytes) from {}",
                            hash,
                            bytes.len(),
                            peer.peer_url()
                        );
                        return Some(bytes);
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        debug!("Mesh file fetch: {} from {} failed: {}", hash, peer.peer_url(), e);
                        continue;
                    }
                }
            }
        }

        // ── Path 2: relay blind-conduit (cross-NAT fallback) ──
        for peer in peers.iter().filter(|p| !p.is_cache()) {
            let target = peer.target_instance_id();
            if target.is_empty() {
                continue;
            }
            match self
                .relay
                .fetch_file_via_relay(target, hash, FILE_FETCH_RELAY_TIMEOUT)
                .await
            {
                Ok(Some(bytes)) if !bytes.is_empty() => {
                    // CAS integrity: the bytes transited a blind relay and an
                    // untrusted peer. Reject anything that doesn't hash to what
                    // we asked for (poison guard) before returning it to be
                    // written under this content-addressed name.
                    if !crate::utils::filestore::verify_sha256(&bytes, hash) {
                        warn!(
                            "Mesh file fetch (relay): {} from {} FAILED sha256 verify ({} bytes) — rejecting",
                            hash,
                            target,
                            bytes.len()
                        );
                        continue;
                    }
                    info!(
                        "Mesh file fetch (relay): {} ({} bytes) from {}",
                        hash,
                        bytes.len(),
                        target
                    );
                    return Some(bytes);
                }
                Ok(_) => continue,
                Err(e) => {
                    debug!(
                        "Mesh file fetch (relay): {} from {} failed: {}",
                        hash, target, e
                    );
                    continue;
                }
            }
        }
        None
    }

    /// Bump `last_accessed_at` on an existing cached row. Called by API
    /// handlers when a cached entity is read so the LRU evictor knows it's
    /// still hot. No-op if no checksum row exists (e.g. row was never
    /// cache-tagged because it's locally owned).
    pub async fn touch_cache(&self, entity_type: &str, entity_id: &str) {
        let _ = self
            .db
            .query(
                "UPDATE entity_checksum SET last_accessed_at = time::now() \
                 WHERE entity_type = $et AND entity_id = $eid AND is_cache = true",
            )
            .bind(("et", entity_type.to_string()))
            .bind(("eid", entity_id.to_string()))
            .await;
    }

    /// Evict cached rows beyond the configured budget, LRU-style. Called
    /// periodically on cache nodes. `budget_rows` is the total row count
    /// across all is_cache=true entries; over-budget removes oldest by
    /// `last_accessed_at` until under budget.
    pub async fn evict_cache_lru(&self, budget_rows: u64) -> anyhow::Result<usize> {
        if !self.is_cache_node() {
            return Ok(0);
        }

        // Count.
        let count: Option<i64> = self
            .db
            .query(
                "SELECT count() AS n FROM entity_checksum WHERE is_cache = true GROUP ALL",
            )
            .await?
            .take::<Vec<Value>>(0)
            .map_err(|e| anyhow::anyhow!(e))?
            .into_iter()
            .next()
            .and_then(|v| v.get("n")?.as_i64());

        let count = count.unwrap_or(0) as u64;
        if count <= budget_rows {
            return Ok(0);
        }
        let to_evict = (count - budget_rows) as i64;

        let victims: Vec<Value> = self
            .db
            .query(
                "SELECT entity_type, entity_id FROM entity_checksum \
                 WHERE is_cache = true \
                 ORDER BY last_accessed_at ASC \
                 LIMIT $n",
            )
            .bind(("n", to_evict))
            .await?
            .take(0)
            .map_err(|e| anyhow::anyhow!(e))?;

        let mut evicted = 0usize;
        for v in &victims {
            let Some(et) = v.get("entity_type").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(eid) = v.get("entity_id").and_then(|x| x.as_str()) else {
                continue;
            };

            // Drop the checksum FIRST, then the row. Ordering matters for tombstones:
            // the live-watch, seeing the row-delete, tombstones a real (authoritative)
            // delete — but an EVICTION must not tombstone. By removing the checksum
            // before the row, the live-watch finds no checksum and skips (a cache
            // eviction is local-only; the owner still holds the data).
            if let Err(e) = self
                .db
                .query(
                    "DELETE entity_checksum WHERE entity_type = $et AND entity_id = $eid",
                )
                .bind(("et", et.to_string()))
                .bind(("eid", eid.to_string()))
                .await
            {
                warn!("evict_cache: drop checksum {}:{} failed: {}", et, eid, e);
            }
            merkle::invalidate_root_cache(et);
            // Drop source row. WHERE-by-record::id — a `table:id` literal breaks
            // on backtick-quoted string ids (all-digit codes) and UUID leaves.
            let q1 = format!("DELETE {} WHERE record::id(id) = $eid", et);
            if let Err(e) = self
                .db
                .query(&q1)
                .bind(("eid", eid.to_string()))
                .await
            {
                warn!("evict_cache: DELETE {}:{} failed: {}", et, eid, e);
                continue;
            }
            evicted += 1;
        }

        if evicted > 0 {
            info!(
                "Cache LRU eviction: removed {} entries (now ≤{})",
                evicted, budget_rows
            );
        }
        Ok(evicted)
    }

    // ─── Bootstrap ───────────────────────────────────────────────────────────

    /// Backfill `entity_checksum` for every existing row across all
    /// SYNC_ENTITY_TYPES tables. Call once at startup. Without this the merkle
    /// tree shows an empty root and `sync_entity_with_peer` short-circuits with
    /// "roots match, nothing to sync" — even though both peers have legitimate
    /// records they have never exchanged. Idempotent: re-records the same hash
    /// on every boot, which costs only a few UPSERTs per table.
    pub async fn bootstrap_checksums(&self) -> anyhow::Result<usize> {
        // Cache nodes don't advertise content via merkle — they pull on demand
        // and store with is_cache=true. A full scan + UPSERT of every checksum
        // row defeats the lightweight design.
        if self.is_cache_node() {
            info!("bootstrap_checksums: skipped (node_role=cache)");
            return Ok(0);
        }
        // Pick the boot mode: cheap no-wipe refresh on a warm restart with an
        // unchanged content-hash schema; full wipe only on a schema change,
        // first boot after this ships (no marker yet), or ECK_CHECKSUM_BOOT_WIPE=1.
        let current_version = merkle::hash_schema_version();
        let stored_version = self.load_checksum_schema_version().await;
        let force_env = std::env::var("ECK_CHECKSUM_BOOT_WIPE").ok();
        let wipe = boot_should_wipe(force_env.as_deref(), stored_version.as_deref(), current_version);
        if wipe {
            info!(
                "bootstrap_checksums: WIPE mode (stored_schema={:?} current_schema={} force_env={:?})",
                stored_version, current_version, force_env
            );
        } else {
            info!(
                "bootstrap_checksums: no-wipe refresh (content-hash schema {} unchanged since last boot)",
                current_version
            );
        }
        let n = self.bootstrap_checksums_inner(wipe).await?;
        // Persist the marker so the next warm restart takes the no-wipe path.
        self.store_checksum_schema_version(current_version).await;
        Ok(n)
    }

    /// Read the content-hash schema marker stored by the last successful boot
    /// bootstrap. `None` on a pre-marker DB (older build / first boot) or any
    /// read error — both treated as "wipe once" by [`boot_should_wipe`]. The
    /// marker row is node-local (`checksum_bootstrap_meta` is NOT in
    /// `SYNC_ENTITY_TYPES`, so it never meshes).
    async fn load_checksum_schema_version(&self) -> Option<String> {
        let rows: Vec<Value> = self
            .db
            .query("SELECT schema_version FROM checksum_bootstrap_meta:version")
            .await
            .ok()?
            .take(0)
            .ok()?;
        rows.into_iter()
            .next()?
            .get("schema_version")?
            .as_str()
            .map(String::from)
    }

    /// Persist the current content-hash schema marker (fixed record id) after a
    /// successful boot bootstrap.
    async fn store_checksum_schema_version(&self, version: &str) {
        let _ = self
            .db
            .query(
                "UPSERT checksum_bootstrap_meta:version SET \
                    schema_version = $v, updated_at = time::now()",
            )
            .bind(("v", version.to_string()))
            .await;
    }

    /// Idempotent variant — no pre-wipe. Called from `sync_cycle` as the
    /// low-frequency integrity sweep (hourly by default): live watchers keep
    /// checksums current in real time, this pass only audits for missed live
    /// events, out-of-process writes, and bit rot.
    pub async fn refresh_checksums(&self) -> anyhow::Result<usize> {
        crate::metrics::tick(crate::metrics::M::RefreshChecksums);
        if self.is_cache_node() {
            return Ok(0);
        }
        self.bootstrap_checksums_inner(false).await
    }

    async fn bootstrap_checksums_inner(&self, wipe: bool) -> anyhow::Result<usize> {
        let merkle_svc = merkle::MerkleService::new(self.db.clone(), self.instance_id.clone());
        let mut total = 0usize;
        let mut total_unchanged = 0usize;
        // Profiling: total wall time across all tables — surface this at INFO so
        // a single grep in the journal exposes runaway bootstrap cost without
        // needing to bump the whole sync namespace to debug.
        let bootstrap_started = std::time::Instant::now();

        if wipe {
            // Wipe and rebuild — earlier versions of this code keyed checksums by
            // SurrealDB's Thing repr (e.g. "registered_device:de1911de-…") instead
            // of the bare leaf, which placed every record into the wrong merkle
            // bucket. Clearing on each boot lets the re-record overwrite that
            // legacy state cleanly. Cheap: O(rows) per node, runs once per launch.
            if let Err(e) = self
                .db
                .query("DELETE entity_checksum")
                .await
                .and_then(|mut r| r.take::<Vec<serde_json::Value>>(0))
            {
                warn!("bootstrap_checksums: pre-wipe of entity_checksum failed: {}", e);
            }
            merkle::invalidate_root_cache_all();
        }

        // `sync_entity_types()` = the normal set plus the `vault` tier when
        // this node holds a VK. On a node without one the list is identical to
        // SYNC_ENTITY_TYPES, so nothing about this sweep changes.
        for entity_type in &sync_entity_types() {
            let table_started = std::time::Instant::now();
            let mut hash_us_total: u64 = 0;
            let mut upsert_ms_total: u64 = 0;
            // Phase 1: snapshot existing checksums for this entity_type so we
            // can compare and skip UPSERT calls when the row is unchanged.
            // Without this, on the very first refresh after a cold start with
            // a 5000-row partner table we'd issue 5000 fsync'd UPSERTs every
            // 60 s — sustained 100% disk write, with sync_cycle effectively
            // never finishing.
            let existing: std::collections::HashMap<String, String> = if !wipe {
                let q = "SELECT entity_id, content_hash FROM entity_checksum WHERE entity_type = $et";
                let rows: Vec<serde_json::Value> = self
                    .db
                    .query(q)
                    .bind(("et", entity_type.to_string()))
                    .await
                    .and_then(|mut r| r.take(0))
                    .unwrap_or_default();
                rows.into_iter()
                    .filter_map(|v| {
                        let eid = v.get("entity_id")?.as_str()?.to_string();
                        let ch = v.get("content_hash")?.as_str()?.to_string();
                        Some((eid, ch))
                    })
                    .collect()
            } else {
                Default::default()
            };

            let query = format!("SELECT * FROM {}", entity_type);
            let rows: Vec<serde_json::Value> = match self
                .db
                .query(&query)
                .await
                .and_then(|mut r| r.take(0))
            {
                Ok(rows) => rows,
                Err(e) => {
                    // Tables in SYNC_ENTITY_TYPES that don't exist yet are
                    // normal on a freshly bootstrapped node — don't logspam
                    // them on every cycle. Demote to debug.
                    debug!(
                        "bootstrap_checksums: SELECT * FROM {} failed: {}",
                        entity_type, e
                    );
                    continue;
                }
            };

            let mut count = 0usize;
            let mut skipped = 0usize;

            // Pre-compute hashes and collect only the (eid, new_hash) rows that
            // actually need writing. The batch path below pays one fsync per
            // BATCH_SIZE rows instead of one fsync per row — on SurrealKV that's
            // the difference between minutes and seconds on big tables.
            const BATCH_SIZE: usize = 100;
            let mut dirty: Vec<(String, String)> = Vec::with_capacity(BATCH_SIZE);

            for entity in &rows {
                // Prefer the canonical foo_id column (a bare UUID) when the table
                // has one — that's what the API and the conflict resolver use.
                // Fall back to extracting the leaf from SurrealDB's implicit `id`
                // Thing for tables without a dedicated id column. Skip rows we
                // can't key at all.
                let id_field = match *entity_type {
                    "registered_device" => Some("device_id"),
                    "order" => Some("order_id"),
                    _ => None,
                };
                let eid_opt = id_field
                    .and_then(|f| entity.get(f).and_then(|v| v.as_str()).map(String::from))
                    .or_else(|| entity.get("id").and_then(extract_entity_leaf_id));

                let Some(eid) = eid_opt else {
                    continue;
                };

                // Compute the would-be content hash once. If we already have
                // that exact hash recorded, skip the UPSERT (and its fsync).
                let hash_started = std::time::Instant::now();
                let new_hash = match merkle::compute_content_hash(entity) {
                    Some(h) => h,
                    None => continue,
                };
                hash_us_total += hash_started.elapsed().as_micros() as u64;

                if let Some(existing_hash) = existing.get(&eid) {
                    if *existing_hash == new_hash {
                        skipped += 1;
                        continue;
                    }
                }

                dirty.push((eid, new_hash));
                if dirty.len() >= BATCH_SIZE {
                    let upsert_started = std::time::Instant::now();
                    let n = dirty.len();
                    match merkle_svc.upsert_checksums_batch(entity_type, &dirty).await {
                        Ok(()) => { count += n; }
                        Err(e) => warn!(
                            "bootstrap_checksums: batch UPSERT ({}, n={}) failed: {}",
                            entity_type, n, e
                        ),
                    }
                    upsert_ms_total += upsert_started.elapsed().as_millis() as u64;
                    dirty.clear();
                }
            }
            // Flush the remainder.
            if !dirty.is_empty() {
                let upsert_started = std::time::Instant::now();
                let n = dirty.len();
                match merkle_svc.upsert_checksums_batch(entity_type, &dirty).await {
                    Ok(()) => { count += n; }
                    Err(e) => warn!(
                        "bootstrap_checksums: batch UPSERT ({}, n={}) failed: {}",
                        entity_type, n, e
                    ),
                }
                upsert_ms_total += upsert_started.elapsed().as_millis() as u64;
                dirty.clear();
            }

            let table_ms = table_started.elapsed().as_millis() as u64;
            let nrows = rows.len();
            if count > 0 && !wipe {
                // In sweep mode every update is a drift finding: the live
                // watchers should have recorded it the moment the row
                // changed. Either a live event was missed, something wrote
                // to the DB from outside this process, or the data rotted.
                warn!(
                    "checksum sweep: {} -> {} DRIFTED checksums re-recorded ({} unchanged, {} rows scanned in {} ms) — live-watch missed events or data changed outside the process",
                    entity_type, count, skipped, nrows, table_ms
                );
            } else if count > 0 {
                // Per-table profile. `hash_us` and `upsert_ms` are sums across
                // every row we actually processed (skipped rows excluded).
                // Divide `upsert_ms / count` to see avg per-row write latency,
                // which is what fsync-per-commit dominates.
                info!(
                    "bootstrap_checksums: {} -> {} updated ({} unchanged, {} rows scanned in {} ms; hash_us_sum={} upsert_ms_sum={})",
                    entity_type, count, skipped, nrows, table_ms, hash_us_total, upsert_ms_total
                );
            } else if skipped > 0 {
                debug!(
                    "bootstrap_checksums: {} -> 0 changes ({} unchanged, {} rows in {} ms)",
                    entity_type, skipped, nrows, table_ms
                );
            }
            total += count;
            total_unchanged += skipped;
        }

        info!(
            "bootstrap_checksums: total {} entity checksums updated, {} unchanged (wipe={}) in {} ms",
            total,
            total_unchanged,
            wipe,
            bootstrap_started.elapsed().as_millis()
        );
        Ok(total)
    }

    // ─── P2P Merkle Sync ─────────────────────────────────────────────────────

    /// Run a full sync cycle: discover peers via relay, then Merkle-diff with each.
    /// Returns total entities exchanged (pulled + pushed) across all peers.
    pub async fn sync_cycle(&self) -> anyhow::Result<usize> {
        crate::metrics::tick(crate::metrics::M::SyncCycle);
        // Cache nodes don't advertise data; they pull on demand. Skip the
        // periodic merkle dance entirely — heartbeat in main.rs keeps the
        // node visible to discovery.
        if self.is_cache_node() {
            debug!("sync_cycle skipped (node_role=cache)");
            return Ok(0);
        }

        // Integrity sweep, NOT the primary hash-maintenance path — the live
        // watchers already recompute a row's checksum the moment it changes.
        // The sweep only catches what they can't: missed live events, writes
        // from outside the process, disk rot. Hourly by default
        // (ECK_CHECKSUM_SWEEP_SECS to tune); running it every cycle meant a
        // full re-hash of every synced table each 60 s.
        let sweep_secs: i64 = std::env::var("ECK_CHECKSUM_SWEEP_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);
        let now_ts = chrono::Utc::now().timestamp();
        let last = self.last_sweep.load(std::sync::atomic::Ordering::Relaxed);
        if now_ts - last >= sweep_secs {
            self.last_sweep.store(now_ts, std::sync::atomic::Ordering::Relaxed);
            if let Err(e) = self.refresh_checksums().await {
                warn!("refresh_checksums (integrity sweep) failed: {}", e);
            }
        }

        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        if peers.is_empty() {
            debug!("No online peers found, skipping sync cycle");
            return Ok(0);
        }

        // Apply per-peer backoff for unreachable peers. Cache peers are NOT
        // skipped at this stage anymore — they advertise their own
        // authoritative subset (the cache-aware merkle view filters out
        // is_cache=true rows), so the pull direction is still useful. The
        // push half is gated later inside sync_entity_with_peer.
        let now = chrono::Utc::now();
        let mut active_peers = Vec::with_capacity(peers.len());
        // Peers that are direct-unreachable (in backoff) but relay-addressable:
        // we still converge the operational tables with them over the relay queue,
        // so a node on a different LAN/NAT (e.g. the kiosk from an off-site dev box)
        // isn't just dropped — trips/devices/visits must be part of the mesh
        // regardless of network.
        let mut relay_peers: Vec<MeshClient> = Vec::new();
        {
            let health = self.peer_health.lock().await;
            for peer in &peers {
                let url = peer.peer_url().to_string();
                if let Some(state) = health.get(&url) {
                    if let Some(skip_until) = state.skip_until {
                        if now < skip_until {
                            let remaining = (skip_until - now).num_seconds();
                            debug!(
                                "Backoff: skipping {} for {}s more ({} consecutive failures)",
                                url, remaining, state.consecutive_failures
                            );
                            if !peer.target_instance_id().is_empty() && !peer.is_cache() {
                                relay_peers.push(peer.clone());
                            }
                            continue;
                        }
                    }
                }
                active_peers.push(peer.clone());
            }
        }

        if active_peers.is_empty() && relay_peers.is_empty() {
            debug!(
                "All {} discovered peer(s) are in backoff, skipping cycle body",
                peers.len()
            );
            return Ok(0);
        }

        info!(
            "Sync cycle: {} active peer(s) out of {} discovered",
            active_peers.len(),
            peers.len()
        );
        let mut total = 0usize;

        for peer in &active_peers {
            let url = peer.peer_url().to_string();
            let mut peer_failures = 0u32;
            let mut peer_attempts = 0u32;

            // Fan out per-entity sync across all worker threads. Each
            // sync_entity_with_peer is an independent merkle walk + HTTP
            // exchange, so they share no state except the SurrealKV handle
            // (which serialises writes internally anyway). join_all lets
            // Tokio schedule them across cores; on a quiet machine the cycle
            // finishes ~N× faster, on a busy one OS nice/priority still
            // throttles us down.
            let results = futures_util::future::join_all(
                sync_entity_types()
                    .into_iter()
                    .map(|et| async move { (et, self.sync_entity_with_peer(peer, et).await) }),
            )
            .await;

            for (entity_type, res) in results {
                match res {
                    Ok(n) => {
                        peer_attempts += 1;
                        total += n;
                    }
                    // Version skew: the peer runs an older binary that doesn't
                    // know this entity type (added to SYNC_ENTITY_TYPES more
                    // recently). Not a failure and not a real attempt — skip it
                    // transparently so a freshly-added synced type never spams
                    // warnings or trips peer-health backoff until the fleet
                    // finishes rolling forward. The type still converges from
                    // peers that DO know it.
                    Err(e) if e.contains("ENTITY_UNSUPPORTED") => {
                        debug!(
                            "Skipping {} with {}: peer does not support this entity type yet",
                            entity_type, url
                        );
                    }
                    Err(e) => {
                        peer_attempts += 1;
                        peer_failures += 1;
                        if peer_failures == 1 {
                            warn!(
                                "Sync first entity ({}) with {} failed: {} (further failures suppressed this cycle)",
                                entity_type,
                                url,
                                e
                            );
                        } else {
                            debug!(
                                "Sync {} with {} failed: {}",
                                entity_type,
                                url,
                                e
                            );
                        }
                    }
                }
            }

            // Update peer health for next cycle.
            let mut persist_pair: Option<(String, PeerHealth)> = None;
            {
                let mut health = self.peer_health.lock().await;
                let entry = health.entry(url.clone()).or_default();
                let mut changed = false;
                if peer_failures == 0 {
                    if entry.consecutive_failures > 0 {
                        info!(
                            "Peer {} recovered after {} failures",
                            url, entry.consecutive_failures
                        );
                        changed = true;
                    }
                    entry.consecutive_failures = 0;
                    entry.skip_until = None;
                } else if peer_failures == peer_attempts {
                    // Every entity type failed → almost certainly the peer itself is
                    // unreachable, not a per-table issue. Apply backoff.
                    entry.consecutive_failures += 1;
                    if let Some(skip) = entry.next_skip() {
                        entry.skip_until = Some(chrono::Utc::now() + skip);
                        if entry.consecutive_failures == 3 {
                            info!(
                                "Peer {} unreachable, backing off for {}s",
                                url,
                                skip.num_seconds()
                            );
                        }
                    }
                    changed = true;
                }
                if changed {
                    persist_pair = Some((url.clone(), entry.clone()));
                }
            }
            // UPSERT outside the lock to keep the contention window short.
            if let Some((u, h)) = persist_pair {
                self.persist_peer_health(&u, &h).await;
            }
        }

        // Relay convergence for direct-unreachable peers: diff + pull the
        // operational tables over the relay queue (the merkle_state + pull_request
        // mesh-tasks), so cross-network nodes still converge. Pull-only — the peer
        // pulls our side on its own cycle, so both directions cover each other.
        for peer in &relay_peers {
            let target = peer.target_instance_id().to_string();
            for et in RELAY_SYNC_TYPES {
                match self.sync_entity_via_relay(peer, et).await {
                    Ok(n) => total += n,
                    Err(e) => debug!("relay-sync {} with {} failed: {}", et, target, e),
                }
            }
        }

        // Process reverse-fetch task queue (NAT traversal)
        match self.process_tasks().await {
            Ok(n) => total += n,
            Err(e) => warn!("Task queue processing failed: {}", e),
        }

        if total > 0 {
            info!("Sync cycle complete: {} entities exchanged", total);
        }
        Ok(total)
    }

    // ─── Mixed-build mesh churn: futility + hash-schema helpers ──────────────

    /// If the `(peer, entity_type)` pair is currently parked by futility backoff,
    /// return the remaining seconds; else `None`. Read-only.
    async fn futility_parked_secs(
        &self,
        key: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<i64> {
        let map = self.futility.lock().await;
        let until = map.get(key)?.skip_until?;
        (now < until).then(|| (until - now).num_seconds())
    }

    /// Fold a completed repair pass into the `(peer, entity_type)` futility
    /// state. A futile pass (pulled > 0, wrote 0) advances the counter and may
    /// park the pair; a write or a pulled-0 pass resets it.
    async fn futility_record(&self, key: &str, pass: PassResult) {
        let now = chrono::Utc::now();
        let mut map = self.futility.lock().await;
        let prev = map.get(key).cloned().unwrap_or_default();
        let next = futility_next(&prev, pass, now);
        if next == FutilityState::default() {
            map.remove(key);
            return;
        }
        // Log the moment a pair first parks (mirrors PeerHealth's 3rd-failure info).
        if prev.skip_until.is_none() {
            if let Some(until) = next.skip_until {
                info!(
                    "Futility backoff: {} parked for {}s after {} futile repair passes (pulled>0, wrote 0)",
                    key,
                    (until - now).num_seconds(),
                    next.consecutive_futile
                );
            }
        }
        map.insert(key.to_string(), next);
    }

    /// Clear every futility entry for a peer (all entity_types). Called when the
    /// peer's hash schema transitions mismatch→match so the next cycle runs a
    /// full repair pass to reconcile anything live-sync missed while paused.
    async fn futility_clear_peer(&self, peer_url: &str) {
        let prefix = format!("{}|", peer_url);
        let mut map = self.futility.lock().await;
        map.retain(|k, _| !k.starts_with(&prefix));
    }

    /// Reconcile a peer's advertised hash-schema with ours (layer 2). Returns
    /// `true` if tree-repair should PROCEED, `false` if it must be SKIPPED (roots
    /// can never agree across different schemas). Records the peer's last-seen
    /// schema and, on a mismatch→match transition, clears the peer's futility
    /// backoff. A peer that sends no schema is grandfathered as compatible.
    async fn schema_allows_repair(&self, peer_url: &str, peer_schema: Option<&str>) -> bool {
        let ours = merkle::hash_schema_version();
        let compatible = match peer_schema {
            None => true,          // old build, grandfathered
            Some(s) => s == ours,  // same field-set + algorithm
        };

        let mut transition_to_match = false;
        let mut warn_now = false;
        {
            let mut map = self.peer_schema.lock().await;
            let st = map.entry(peer_url.to_string()).or_default();
            let prev_mismatch = matches!(&st.last_schema, Some(s) if s != ours);
            if compatible && prev_mismatch {
                transition_to_match = true;
            }
            if !compatible {
                // WARN at most once per peer per hour; debug otherwise.
                let due = st
                    .last_mismatch_warn
                    .map(|t| t.elapsed() >= std::time::Duration::from_secs(3600))
                    .unwrap_or(true);
                if due {
                    st.last_mismatch_warn = Some(std::time::Instant::now());
                    warn_now = true;
                }
            }
            st.last_schema = peer_schema.map(|s| s.to_string());
        }

        if transition_to_match {
            self.futility_clear_peer(peer_url).await;
            debug!(
                "Peer {} hash-schema now matches ours ({}) — cleared futility backoff, running full repair",
                peer_url, ours
            );
        }

        if compatible {
            if peer_schema.is_none() {
                debug!(
                    "Peer {} sent no hash_schema (old build) — grandfathered as compatible",
                    peer_url
                );
            }
            true
        } else {
            let peer_s = peer_schema.unwrap_or("<none>");
            if warn_now {
                warn!(
                    "Peer {} hash-schema {} != ours {} — mixed build, skipping merkle tree-repair for this pair (roots can never agree); further mismatches this hour at debug",
                    peer_url, peer_s, ours
                );
            } else {
                debug!(
                    "Peer {} hash-schema {} != ours {} — skipping tree-repair (mixed build)",
                    peer_url, peer_s, ours
                );
            }
            false
        }
    }

    /// Merkle-diff a single entity type with a single peer.
    ///
    /// 1. Compare roots — if identical, nothing to do.
    /// 2. Find differing buckets.
    /// 3. Drill into each differing bucket to find specific entity IDs.
    /// 4. Pull missing entities from peer, push our missing entities to peer.
    async fn sync_entity_with_peer(
        &self,
        peer: &MeshClient,
        entity_type: &str,
    ) -> Result<usize, String> {
        // Max rows per pull/push HTTP exchange. A large divergent set (e.g.
        // embedding-bloated `document` rows) serialized in one response blows
        // MeshClient's 8s timeout → the body never decodes → the pair never
        // converges. Chunking keeps every request small enough to complete.
        const SYNC_BATCH: usize = 100;

        // Sync class `vault` (`.eck/PII_VAULT.md` §3.1): decryptable customer
        // PII replicates ONLY between reveal-capable full nodes. Two gates,
        // both cheap and both fail-closed:
        //   * this node must be in the tier at all (it holds a VK) — a node
        //     that lost its key mid-run must stop pushing vault rows, not keep
        //     shipping ciphertext around;
        //   * the PEER must not be a cache (eck1/2/3). A cache could not
        //     decrypt the rows anyway, so replicating them there would only
        //     widen the ciphertext surface for nothing — the same reasoning
        //     that keeps `document_raw` off the merkle sweep entirely.
        // The relay path is covered by `pii_vault` simply not being in
        // RELAY_SYNC_TYPES.
        if is_vault_entity_type(entity_type) && (!vault_sync_enabled() || peer.is_cache()) {
            return Ok(0);
        }

        // Empty-root backoff: while a (cache peer, table) pair holds skip
        // credits, don't even fetch the remote root this cycle.
        let empty_backoff_key = format!("{}|{}", peer.peer_url(), entity_type);
        if peer.is_cache()
            && consume_empty_root_skip(
                &mut *self.cache_empty_backoff.lock().await,
                &empty_backoff_key,
            )
        {
            return Ok(0);
        }

        // Futility backoff (layer 1): if this (peer, table) pair has been futile
        // (pulled rows, wrote nothing) for 3+ passes in a row, it's parked — skip
        // the whole merkle exchange this cycle WITHOUT even fetching the root.
        // Non-cache only; caches have their own empty-root backoff above.
        let futility_key = format!("{}|{}", peer.peer_url(), entity_type);
        if !peer.is_cache() {
            if let Some(remaining) = self
                .futility_parked_secs(&futility_key, chrono::Utc::now())
                .await
            {
                debug!(
                    "{}: futility backoff parks {} for {}s more — skipping tree-repair",
                    entity_type,
                    peer.peer_url(),
                    remaining
                );
                return Ok(0);
            }
        }

        let merkle_svc = MerkleService::new(self.db.clone(), self.instance_id.clone());

        // Step 1: Compare roots
        let local_root = merkle_svc
            .get_state(&MerkleRequest {
                entity_type: entity_type.to_string(),
                level: 0,
                bucket: None,
            })
            .await?;

        let remote_root = peer
            .get_merkle_state(&MerkleRequest {
                entity_type: entity_type.to_string(),
                level: 0,
                bucket: None,
            })
            .await?;

        // A cache advertises only its authoritative subset — usually NOTHING.
        // Empty remote root ⇒ nothing to pull, and pushing into a cache is
        // forbidden (asymmetric rule below), so the whole exchange is a no-op
        // whether the roots happen to match (both empty) or differ (we have
        // data): arm the skip credits instead of re-fetching this root every
        // cycle. Lag ceiling for a cache's rare authoritative rows (its own
        // registered_device / system_config) = credits × cycle period (~10 min).
        if peer.is_cache() && remote_root.children.is_empty() {
            debug!(
                "{}: cache {} advertises empty root — nothing to exchange, re-check in {} cycles",
                entity_type,
                peer.peer_url(),
                CACHE_EMPTY_ROOT_RECHECK_CYCLES
            );
            self.cache_empty_backoff
                .lock()
                .await
                .insert(empty_backoff_key, CACHE_EMPTY_ROOT_RECHECK_CYCLES);
            return Ok(0);
        }

        // Hash-schema guard (layer 2): a peer on a DIFFERENT content-hash schema
        // can never share our merkle root even on byte-identical data — skip
        // tree-repair for this pair (throttled WARN inside schema_allows_repair).
        // A peer that sends no schema is grandfathered as compatible (today's
        // fleet shares the identical field-set; keeps repair alive during THIS
        // feature's own rollout). Also records the peer's schema and clears
        // futility on a mismatch→match transition. Non-cache only.
        if !peer.is_cache()
            && !self
                .schema_allows_repair(peer.peer_url(), remote_root.hash_schema.as_deref())
                .await
        {
            return Ok(0);
        }

        if local_root.hash == remote_root.hash {
            debug!("{}: roots match, nothing to sync", entity_type);
            // Trees agree = a pulled-0 pass; clear any futility for this pair.
            if !peer.is_cache() {
                self.futility_record(&futility_key, PassResult { pulled_n: 0, writes_n: 0 })
                    .await;
            }
            return Ok(0);
        }

        debug!(
            "{}: roots differ (local={}, remote={}), drilling down",
            entity_type,
            &local_root.hash[..8],
            &remote_root.hash[..8]
        );

        // Step 2: Find differing buckets.
        // Asymmetric rule: a cache peer holds only what API hits asked it to
        // hold, so "we have rows the cache doesn't" is the steady state and
        // we MUST NOT interpret that as "push canonical data into the
        // cache". Drop push_ids entirely when peer.is_cache(). pull_ids is
        // kept — the cache may own authoritative records (its own
        // registered_device, system_config) that full peers should mirror.
        let (buckets_to_pull, mut buckets_to_push) =
            merkle::compare_trees(&local_root.children, &remote_root.children);
        if peer.is_cache() {
            buckets_to_push.clear();
        }

        let mut pull_ids: Vec<String> = Vec::new();
        let mut push_ids: Vec<String> = Vec::new();

        // Step 3: Drill into each differing bucket
        let all_buckets: Vec<String> = buckets_to_pull
            .iter()
            .chain(buckets_to_push.iter())
            .cloned()
            .collect::<std::collections::HashSet<String>>()
            .into_iter()
            .collect();

        for bucket in &all_buckets {
            let local_bucket = merkle_svc
                .get_state(&MerkleRequest {
                    entity_type: entity_type.to_string(),
                    level: 1,
                    bucket: Some(bucket.clone()),
                })
                .await?;

            let remote_bucket = peer
                .get_merkle_state(&MerkleRequest {
                    entity_type: entity_type.to_string(),
                    level: 1,
                    bucket: Some(bucket.clone()),
                })
                .await?;

            let (need_pull, need_push) =
                merkle::compare_trees(&local_bucket.children, &remote_bucket.children);

            pull_ids.extend(need_pull);
            // Same asymmetric rule at entity level: don't push into a cache.
            if !peer.is_cache() {
                push_ids.extend(need_push);
            }
        }

        let mut exchanged = 0usize;
        // Pull-phase counters, hoisted to function scope so the futility state
        // machine (layer 1) can classify this pass at the end: FUTILE iff
        // pulled_n > 0 && writes_n == 0.
        let mut pulled_n = 0usize;
        let mut writes_n = 0usize;

        // Step 4: Pull missing entities from peer — in SYNC_BATCH-sized chunks so
        // a large divergent set never exceeds the MeshClient 8s timeout in one
        // request (which would truncate the body, fail to decode, and stall
        // convergence forever).
        if !pull_ids.is_empty() {
            debug!(
                "{}: pulling {} entities from {} (chunks of {})",
                entity_type,
                pull_ids.len(),
                peer.peer_url(),
                SYNC_BATCH
            );

            let pull_started = std::time::Instant::now();
            // Checksums (re)recorded on the NO-WRITE paths (Equal / local-wins).
            // Recording these converges a stale leaf so it stops being re-pulled —
            // the fix for the "pulled N wrote 0 never-converges" churn.
            let mut converged_n = 0usize;
            for chunk in pull_ids.chunks(SYNC_BATCH) {
                let entities = match peer.pull_entities(entity_type, chunk).await {
                    Ok(e) => e,
                    Err(e) => {
                        // Keep progress from earlier chunks; the next cycle's
                        // merkle pass retries whatever is still divergent.
                        warn!(
                            "{}: pull chunk ({} ids) from {} failed: {} — keeping partial progress",
                            entity_type, chunk.len(), peer.peer_url(), e
                        );
                        break;
                    }
                };
                pulled_n += entities.len();
                for entity in &entities {
                    // Match the bootstrap policy: prefer canonical foo_id, fall
                    // back to the implicit Thing id. Both sides MUST use the same
                    // key or upsert routes the record to the wrong record-id.
                    let id_field = match entity_type {
                        "registered_device" => Some("device_id"),
                        "order" => Some("order_id"),
                        _ => None,
                    };
                    let eid_opt = id_field
                        .and_then(|f| entity.get(f).and_then(|v| v.as_str()).map(String::from))
                        .or_else(|| entity.get("id").and_then(extract_entity_leaf_id));
                    if let Some(eid_owned) = eid_opt {
                        match crate::sync::conflict::resolve_and_upsert(
                            &self.db,
                            entity_type,
                            &eid_owned,
                            entity.clone(),
                            &self.instance_id,
                        )
                        .await
                        {
                            Ok(outcome) => {
                                use crate::sync::conflict::ResolveOutcome;
                                // Record the checksum of whatever content is
                                // ACTUALLY stored (remote on write/equal, local on
                                // local-wins) so the leaf converges — recording the
                                // wrong side would MASK divergence in a GoBD store.
                                let checksum_of: Option<&serde_json::Value> = match &outcome {
                                    ResolveOutcome::Wrote => { exchanged += 1; writes_n += 1; Some(entity) }
                                    ResolveOutcome::AlreadyEqual(local) => { converged_n += 1; Some(local) }
                                    ResolveOutcome::LocalNewer(local) => { converged_n += 1; Some(local) }
                                    // resolve already wrote the tombstone checksum.
                                    ResolveOutcome::Tombstoned => { converged_n += 1; None }
                                };
                                if let Some(v) = checksum_of {
                                    if let Err(e) = merkle_svc
                                        .record_checksum(entity_type, &eid_owned, v)
                                        .await
                                    {
                                        warn!("Checksum update failed for {}:{}: {}", entity_type, eid_owned, e);
                                    }
                                }
                            }
                            Err(e) => warn!("Conflict resolve {}:{} failed: {}", entity_type, eid_owned, e),
                        }
                    }
                }
            }
            let elapsed_ms = pull_started.elapsed().as_millis() as u64;
            // Loud signal when a pull returns rows but none get written — the
            // cache-stale-checksum / version-skew "merkle never converges"
            // pattern (we pull, resolve says "local wins/equal" on all, write
            // zero, next cycle sees the same divergent root, repeats).
            if pulled_n > 0 && writes_n == 0 && converged_n == 0 {
                // Pulled rows but neither wrote NOR recorded a converging checksum
                // — every resolve errored, the genuine non-convergence signal.
                warn!(
                    "{}: pulled {} entities from {} but wrote 0 and converged 0 — possible merkle non-convergence (peer version skew, hash determinism bug, resolve errors). elapsed_ms={}",
                    entity_type, pulled_n, peer.peer_url(), elapsed_ms
                );
            } else {
                debug!(
                    "{}: pulled {} from {}, wrote {}, converged {} (elapsed_ms={})",
                    entity_type, pulled_n, peer.peer_url(), writes_n, converged_n, elapsed_ms
                );
            }
        }

        // Step 5: Push our entities to peer — chunked for the same 8s-timeout
        // reason (a single large POST body fails with "error sending request").
        if !push_ids.is_empty() {
            debug!(
                "{}: pushing {} entities to {} (chunks of {})",
                entity_type,
                push_ids.len(),
                peer.peer_url(),
                SYNC_BATCH
            );
            for chunk in push_ids.chunks(SYNC_BATCH) {
                // Fetch this chunk's local entities by IDs
                let query = format!(
                    "SELECT *, record::id(id) AS id FROM {} WHERE record::id(id) IN $ids",
                    entity_type
                );
                let mut local_entities: Vec<Value> = match self
                    .db
                    .query(&query)
                    .bind(("ids", chunk.to_vec()))
                    .await
                    .and_then(|mut r| r.take(0))
                {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("{}: push-chunk select failed: {}", entity_type, e);
                        break;
                    }
                };
                // TOMBSTONES over PUSH: a push_id with no live row but a `deleted`
                // checksum is a delete this peer must learn — carry it as a
                // {id,_deleted,_vclock} marker (same as sync_pull). Without this a
                // NAT'd/push-only source's deletes never reach peers (pull can't
                // reach it) → the delete silently fails to converge.
                let missing: Vec<String> = chunk
                    .iter()
                    .filter(|id| {
                        !local_entities
                            .iter()
                            .any(|e| e.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
                    })
                    .cloned()
                    .collect();
                if !missing.is_empty() {
                    let tombs: Vec<Value> = self
                        .db
                        .query(
                            "SELECT entity_id, vclock FROM entity_checksum \
                             WHERE entity_type = $et AND deleted = true AND entity_id IN $ids",
                        )
                        .bind(("et", entity_type.to_string()))
                        .bind(("ids", missing))
                        .await
                        .and_then(|mut r| r.take(0))
                        .unwrap_or_default();
                    for t in tombs {
                        if let Some(eid) = t.get("entity_id").and_then(|v| v.as_str()) {
                            local_entities.push(serde_json::json!({
                                "id": eid,
                                "_deleted": true,
                                "_vclock": t.get("vclock").cloned().unwrap_or(Value::Null),
                            }));
                        }
                    }
                }
                if local_entities.is_empty() {
                    continue;
                }
                match peer
                    .push_entities(entity_type, &local_entities, &self.instance_id)
                    .await
                {
                    Ok(n) => exchanged += n,
                    Err(e) => {
                        warn!(
                            "{}: push chunk ({} rows) to {} failed: {} — keeping partial progress",
                            entity_type, local_entities.len(), peer.peer_url(), e
                        );
                        break;
                    }
                }
            }
        }

        if exchanged > 0 {
            info!(
                "{}: exchanged {} entities with {}",
                entity_type,
                exchanged,
                peer.peer_url()
            );
        }

        // Fold this completed pass into the futility backoff (layer 1): a pass
        // that pulled rows but wrote none is futile; a write or a pulled-0 pass
        // resets. Non-cache only (caches use the empty-root backoff above).
        if !peer.is_cache() {
            self.futility_record(&futility_key, PassResult { pulled_n, writes_n })
                .await;
        }

        Ok(exchanged)
    }

    /// Merkle-diff a single entity type with a peer we can't reach directly,
    /// entirely over the RELAY queue (merkle_state + pull_request mesh-tasks).
    /// PULL-only: we fetch what the peer has and we lack; the peer symmetrically
    /// pulls our side on its own cycle, so both converge without a push half.
    /// Slower than the direct path (each round-trip rides the relay poll), so it's
    /// gated to RELAY_SYNC_TYPES + backed-off peers by the caller.
    async fn sync_entity_via_relay(
        &self,
        peer: &MeshClient,
        entity_type: &str,
    ) -> Result<usize, String> {
        const SYNC_BATCH: usize = 100;
        const RELAY_TIMEOUT: u64 = 20;
        let target = peer.target_instance_id();
        if target.is_empty() {
            return Ok(0);
        }
        let merkle_svc = MerkleService::new(self.db.clone(), self.instance_id.clone());
        let root_req = MerkleRequest {
            entity_type: entity_type.to_string(),
            level: 0,
            bucket: None,
        };

        let local_root = merkle_svc.get_state(&root_req).await?;
        let remote_root = self
            .relay
            .get_merkle_state_via_relay(target, &root_req, RELAY_TIMEOUT)
            .await
            .map_err(|e| e.to_string())?;
        if local_root.hash == remote_root.hash {
            return Ok(0);
        }

        let (buckets_to_pull, _push) =
            merkle::compare_trees(&local_root.children, &remote_root.children);

        let mut pull_ids: Vec<String> = Vec::new();
        for bucket in &buckets_to_pull {
            let req = MerkleRequest {
                entity_type: entity_type.to_string(),
                level: 1,
                bucket: Some(bucket.clone()),
            };
            let local_bucket = merkle_svc.get_state(&req).await?;
            let remote_bucket = self
                .relay
                .get_merkle_state_via_relay(target, &req, RELAY_TIMEOUT)
                .await
                .map_err(|e| e.to_string())?;
            let (need_pull, _) =
                merkle::compare_trees(&local_bucket.children, &remote_bucket.children);
            pull_ids.extend(need_pull);
        }

        if pull_ids.is_empty() {
            return Ok(0);
        }

        let mut exchanged = 0usize;
        for chunk in pull_ids.chunks(SYNC_BATCH) {
            let entities = match self
                .relay
                .pull_entities_via_relay(target, entity_type, chunk, RELAY_TIMEOUT)
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    warn!("{}: relay pull chunk from {} failed: {}", entity_type, target, e);
                    break;
                }
            };
            for entity in &entities {
                let id_field = match entity_type {
                    "registered_device" => Some("device_id"),
                    "order" => Some("order_id"),
                    _ => None,
                };
                let eid_opt = id_field
                    .and_then(|f| entity.get(f).and_then(|v| v.as_str()).map(String::from))
                    .or_else(|| entity.get("id").and_then(extract_entity_leaf_id));
                if let Some(eid) = eid_opt {
                    match crate::sync::conflict::resolve_and_upsert(
                        &self.db,
                        entity_type,
                        &eid,
                        entity.clone(),
                        &self.instance_id,
                    )
                    .await
                    {
                        Ok(outcome) => {
                            use crate::sync::conflict::ResolveOutcome;
                            let checksum_of: Option<&serde_json::Value> = match &outcome {
                                ResolveOutcome::Wrote => { exchanged += 1; Some(entity) }
                                ResolveOutcome::AlreadyEqual(local) => Some(local),
                                ResolveOutcome::LocalNewer(local) => Some(local),
                                ResolveOutcome::Tombstoned => None,
                            };
                            if let Some(v) = checksum_of {
                                if let Err(e) = merkle_svc.record_checksum(entity_type, &eid, v).await {
                                    warn!("Checksum update failed for {}:{}: {}", entity_type, eid, e);
                                }
                            }
                        }
                        Err(e) => warn!("relay conflict {}:{} failed: {}", entity_type, eid, e),
                    }
                }
            }
        }

        if exchanged > 0 {
            info!("{}: relay-synced {} entities from {}", entity_type, exchanged, target);
        }
        Ok(exchanged)
    }

    // ─── Task Queue (reverse-fetch for NAT traversal) ─────────────────────────

    /// Poll peers for tasks assigned to us, execute them, and push results back.
    /// Returns total tasks processed across all peers.
    pub async fn process_tasks(&self) -> anyhow::Result<usize> {
        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        if peers.is_empty() {
            return Ok(0);
        }

        // Honour the same per-peer backoff that gates merkle sync — otherwise
        // fetch_tasks keeps hitting an unreachable peer every cycle and
        // floods the log even though the entity-sync half already gave up.
        let now = chrono::Utc::now();
        let active_peers: Vec<_> = {
            let health = self.peer_health.lock().await;
            peers
                .iter()
                .filter(|p| {
                    let url = p.peer_url().to_string();
                    match health.get(&url).and_then(|s| s.skip_until) {
                        Some(skip_until) if now < skip_until => {
                            debug!(
                                "Tasks: skipping {} ({}s remaining in backoff)",
                                url,
                                (skip_until - now).num_seconds()
                            );
                            false
                        }
                        _ => true,
                    }
                })
                .cloned()
                .collect()
        };

        if active_peers.is_empty() {
            return Ok(0);
        }

        let mut processed = 0usize;

        for peer in &active_peers {
            let tasks = match peer.fetch_tasks(&self.instance_id).await {
                Ok(t) => t,
                Err(e) => {
                    debug!("Failed to fetch tasks from {}: {}", peer.peer_url(), e);
                    continue;
                }
            };

            for task in &tasks {
                let action = task.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let task_id = task.get("id").and_then(|v| v.as_str()).unwrap_or("");

                if action == "request_raw_docs" {
                    let ticket_id = task.get("ticket_id").and_then(|v| v.as_str()).unwrap_or("");
                    if ticket_id.is_empty() || task_id.is_empty() {
                        continue;
                    }

                    // Query local document_raw for this ticket (parent + threads)
                    let docs: Vec<Value> = match self.db
                        .query("SELECT record::id(id) AS id, type, ticket_id, payload, updated_at FROM document_raw WHERE record::id(id) = $tid OR ticket_id = $tid ORDER BY updated_at ASC")
                        .bind(("tid", ticket_id.to_string()))
                        .await
                        .and_then(|mut r| r.take(0))
                    {
                        Ok(d) => d,
                        Err(e) => {
                            warn!("Task {}: failed to query local docs for ticket {}: {}", task_id, ticket_id, e);
                            continue;
                        }
                    };

                    if docs.is_empty() {
                        debug!("Task {}: no local docs for ticket {}, skipping", task_id, ticket_id);
                        continue;
                    }

                    // PII_VAULT.md P0#4: a document_raw payload is a whole
                    // customer ticket/thread in clear — envelope-encrypt before
                    // it leaves over this point-to-point push. No local mesh
                    // data key = refuse the push rather than leak plaintext.
                    let docs = match crate::utils::crypto::data_key() {
                        Some(key) => crate::utils::crypto::encrypt_raw_doc_payloads(docs, &key),
                        None => {
                            warn!(
                                "Task {}: raw docs not served: no mesh data key (ticket {})",
                                task_id, ticket_id
                            );
                            continue;
                        }
                    };

                    // Push documents to the requesting peer
                    match peer.push_entities("document_raw", &docs, &self.instance_id).await {
                        Ok(n) => {
                            info!("Task {}: pushed {} document_raw records for ticket {} to {}", task_id, n, ticket_id, peer.peer_url());
                        }
                        Err(e) => {
                            warn!("Task {}: push to {} failed: {}", task_id, peer.peer_url(), e);
                            continue;
                        }
                    }

                    // Mark task as completed
                    if let Err(e) = peer.complete_task(task_id).await {
                        warn!("Task {}: failed to delete from peer: {}", task_id, e);
                    }

                    processed += 1;
                } else {
                    debug!("Unknown task action '{}', skipping task {}", action, task_id);
                }
            }
        }

        if processed > 0 {
            info!("Task queue: processed {} task(s)", processed);
        }
        Ok(processed)
    }

    // ─── Outbox (background retry for failed pushes) ─────────────────────────

    /// Load the live record for an outbox row whose payload is empty, shaped the
    /// way a peer's `apply_pushed_entities` expects (`id` = the bare record-id
    /// leaf via `record::id(id)`, carrying `_vclock`/`home_instance_id`). Matched
    /// by business `uuid` first (POS fiscal rows key the outbox on it) then by
    /// the record-id leaf. `entity_type` is validated against the mesh table set
    /// before it reaches the (un-bindable) table position of the query.
    async fn load_entity_for_outbox(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Option<Value> {
        if !is_mesh_entity_type(entity_type) {
            return None;
        }
        let q = format!(
            "SELECT *, record::id(id) AS id FROM {} \
             WHERE uuid = $eid OR record::id(id) = $eid LIMIT 1",
            entity_type
        );
        self.db
            .query(q)
            .bind(("eid", entity_id.to_string()))
            .await
            .ok()
            .and_then(|mut r| r.take::<Vec<Value>>(0).ok())
            .and_then(|rows| rows.into_iter().next())
    }

    /// Process pending outbox records: for each, find a peer and push directly.
    ///
    /// On success the record is deleted. On failure `error_count` is incremented
    /// and `next_attempt_at` is pushed back with exponential backoff.
    pub async fn process_outbox(&self) -> anyhow::Result<usize> {
        crate::metrics::tick(crate::metrics::M::OutboxProcess);
        // NOTE: OutboxRecord uses String for `id`, `next_attempt_at`, and
        // `created_at`, but SurrealDB stores `id` as a Thing and the two
        // timestamps as `datetime`. Without explicit projection / coercion
        // the deserialize fails ("Expected string, got record/datetime")
        // and the outbox stalls.
        let pending: Vec<OutboxRecord> = self
            .db
            .query(
                "SELECT *, \
                        record::id(id) AS id, \
                        type::string(next_attempt_at) AS next_attempt_at, \
                        type::string(created_at) AS created_at \
                 FROM sync_outbox \
                 WHERE next_attempt_at <= time::now() \
                 ORDER BY created_at ASC",
            )
            .await
            .map_err(|e| anyhow::anyhow!("Outbox query failed: {}", e))?
            .take(0)
            .map_err(|e| anyhow::anyhow!("Outbox deserialize failed: {}", e))?;

        if pending.is_empty() {
            return Ok(0);
        }

        // Discover peers once for the whole outbox batch
        let peers = mesh_client::discover_peers(
            &self.relay,
            &self.instance_id,
            self.sync_secret.as_deref(),
        )
        .await;

        if peers.is_empty() {
            debug!("Outbox: no peers online, deferring {} record(s)", pending.len());
            return Ok(0);
        }

        let total = pending.len();
        let mut sent = 0usize;

        for record in pending {
            // Defensive payload hydration: an outbox row whose payload is empty
            // (a legacy `{}` enqueue, or a producer that stored only the id) is
            // useless to a peer — push_entities would apply an entity with no
            // fields. When `entity_id` is present, load the live record and push
            // THAT instead of the empty object. Producers should enqueue the
            // full record (see `pos::services::transaction::enqueue_outbox`);
            // this is the belt-and-braces path, hence warn-level.
            let payload = if payload_is_empty(&record.payload) && !record.entity_id.is_empty() {
                match self
                    .load_entity_for_outbox(&record.entity_type, &record.entity_id)
                    .await
                {
                    Some(v) => {
                        warn!(
                            "Outbox: hydrated empty payload for {}:{} from local DB",
                            record.entity_type, record.entity_id
                        );
                        v
                    }
                    None => {
                        warn!(
                            "Outbox: {}:{} has an empty payload and no live record — dropping",
                            record.entity_type, record.entity_id
                        );
                        let _: Option<Value> = self
                            .db
                            .delete(("sync_outbox", record.id.as_str()))
                            .await
                            .unwrap_or(None);
                        continue;
                    }
                }
            } else {
                record.payload.clone()
            };

            // Try first available peer
            let mut pushed = false;
            for peer in &peers {
                match peer
                    .push_entities(
                        &record.entity_type,
                        &[payload.clone()],
                        &self.instance_id,
                    )
                    .await
                {
                    Ok(_) => {
                        // Delete the successfully sent record
                        let _: Option<Value> = self
                            .db
                            .delete(("sync_outbox", record.id.as_str()))
                            .await
                            .unwrap_or(None);
                        sent += 1;
                        pushed = true;
                        break;
                    }
                    Err(e) => {
                        debug!(
                            "Outbox push to {} failed: {}",
                            peer.peer_url(),
                            e
                        );
                        // Cross-NAT fallback: queue the push on the relay so the
                        // peer's mesh_relay_poller can apply it once it polls.
                        // We dispatch only — don't block waiting for the ack —
                        // because outbox sends are fire-and-forget and the next
                        // sync_cycle's merkle pass will reconcile if anything
                        // was missed. Without target_instance_id we can't address
                        // the relay queue, so skip silently.
                        if !peer.target_instance_id().is_empty() {
                            match self
                                .relay
                                .mesh_dispatch(
                                    peer.target_instance_id(),
                                    "push",
                                    serde_json::json!({
                                        "entity_type": record.entity_type,
                                        "entities": [payload.clone()],
                                        "source_instance": self.instance_id,
                                    }),
                                )
                                .await
                            {
                                Ok(_) => {
                                    let _: Option<Value> = self
                                        .db
                                        .delete(("sync_outbox", record.id.as_str()))
                                        .await
                                        .unwrap_or(None);
                                    sent += 1;
                                    pushed = true;
                                    debug!(
                                        "Outbox push via relay queued for {}",
                                        peer.target_instance_id()
                                    );
                                    break;
                                }
                                Err(e2) => {
                                    debug!(
                                        "Relay-routed push to {} also failed: {}",
                                        peer.target_instance_id(),
                                        e2
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if !pushed {
                let new_count = record.error_count + 1;
                let backoff_secs = 10i64 * 2i64.pow(new_count.min(6) as u32);

                if let Err(e2) = self
                    .db
                    .query(
                        "UPDATE sync_outbox SET \
                            error_count = $count, \
                            next_attempt_at = time::now() + $backoff \
                         WHERE id = $id",
                    )
                    .bind(("count", new_count))
                    .bind(("backoff", format!("{}s", backoff_secs)))
                    .bind(("id", record.id.clone()))
                    .await
                {
                    warn!("Failed to update outbox record backoff: {}", e2);
                }
            }
        }

        if sent > 0 {
            info!("Outbox: pushed {}/{} record(s) to peers", sent, total);
        }
        Ok(sent)
    }

    // ─── Real-Time Live Query Watcher ─────────────────────────────────────────

    /// Watch every `SYNC_ENTITY_TYPES` table via LIVE SELECT and keep
    /// `entity_checksum` in lockstep with their content. Spawns one task per
    /// table so a stalled stream on one table doesn't block the others.
    ///
    /// Cache nodes skip this — they don't maintain a full merkle tree.
    ///
    /// On CREATE/UPDATE: recompute content hash, UPSERT the matching
    /// `entity_checksum` row (skip-unchanged path same as `refresh_checksums`).
    /// On DELETE: drop the corresponding `entity_checksum` row.
    ///
    /// This replaces the scheduled `refresh_checksums()` call inside
    /// `sync_cycle()` as the primary up-to-date mechanism — scheduled refresh
    /// stays as a safety net in case a live stream silently drops events.
    pub fn spawn_live_watchers(self: &std::sync::Arc<Self>) {
        if self.is_cache_node() {
            debug!("spawn_live_watchers: skipped (node_role=cache)");
            return;
        }
        let watched = sync_entity_types();
        for entity_type in &watched {
            let engine = std::sync::Arc::clone(self);
            let et = entity_type.to_string();
            tokio::spawn(async move {
                // A live stream that ends is abnormal (every synced table is
                // DEFINEd at boot). Without respawn a dropped stream leaves
                // the merkle tree silently stale until the next integrity
                // sweep — now hours, not the next 60 s tick. Retry forever
                // with capped backoff; a run that survived >60 s resets it.
                let mut delay = std::time::Duration::from_secs(5);
                loop {
                    let started = std::time::Instant::now();
                    match engine.watch_one_entity(&et).await {
                        Ok(()) => warn!(
                            "[live-watch] {} stream ended — respawn in {:?}",
                            et, delay
                        ),
                        Err(e) => warn!(
                            "[live-watch] {} terminated: {} — respawn in {:?}",
                            et, e, delay
                        ),
                    }
                    if started.elapsed() > std::time::Duration::from_secs(60) {
                        delay = std::time::Duration::from_secs(5);
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_secs(300));
                }
            });
        }
        info!(
            "spawn_live_watchers: started {} live streams",
            watched.len()
        );
    }

    async fn watch_one_entity(&self, entity_type: &str) -> anyhow::Result<()> {
        let merkle_svc = merkle::MerkleService::new(self.db.clone(), self.instance_id.clone());

        let query = format!("LIVE SELECT * FROM {}", entity_type);
        let mut response = match self.db.query(&query).await {
            Ok(r) => r,
            Err(e) => {
                // Table may not exist yet on a fresh node — treat as expected.
                debug!("[live-watch] {}: LIVE SELECT setup failed: {}", entity_type, e);
                return Ok(());
            }
        };
        let mut stream = response.stream::<surrealdb::Notification<Value>>(0)?;

        info!("[live-watch] {} → entity_checksum bridge active", entity_type);

        let id_field: Option<&'static str> = match entity_type {
            "registered_device" => Some("device_id"),
            "order" => Some("order_id"),
            _ => None,
        };

        while let Some(result) = stream.next().await {
            crate::metrics::tick(crate::metrics::M::LiveWatchEvent);
            match result {
                Ok(notification) => {
                    let action = notification.action.to_string();
                    let data = &notification.data;

                    // Resolve canonical leaf id.
                    let eid_opt = id_field
                        .and_then(|f| data.get(f).and_then(|v| v.as_str()).map(String::from))
                        .or_else(|| data.get("id").and_then(extract_entity_leaf_id));
                    let Some(eid) = eid_opt else {
                        continue;
                    };

                    if action == "Delete" {
                        // Only an AUTHORITATIVE (non-cache) delete becomes a tombstone
                        // that propagates. A cache EVICTION or the delete of an entity
                        // with no checksum must NOT tombstone — that would broadcast a
                        // delete for data the owner still holds (= data loss). Gate on
                        // the entity_checksum: is_cache=true or absent → just drop the
                        // checksum (evict_cache removes it BEFORE the row, so we see it
                        // gone here and skip); is_cache=false → real delete → tombstone.
                        let cache_or_absent: Option<bool> = self
                            .db
                            .query(
                                "SELECT VALUE (is_cache = true) FROM entity_checksum \
                                 WHERE entity_type = $et AND entity_id = $eid LIMIT 1",
                            )
                            .bind(("et", entity_type.to_string()))
                            .bind(("eid", eid.clone()))
                            .await
                            .ok()
                            .and_then(|mut r| r.take::<Vec<Value>>(0).ok())
                            .and_then(|rows| rows.into_iter().next())
                            .and_then(|v| v.as_bool());
                        if cache_or_absent == Some(false) {
                            let next = crate::sync::conflict::next_local_vclock(
                                data.get("_vclock"),
                                &self.instance_id,
                            );
                            if let Err(e) = merkle_svc.record_tombstone(entity_type, &eid, &next).await {
                                warn!("[live-watch] {} tombstone {} failed: {}", entity_type, eid, e);
                            } else {
                                debug!("[live-watch] {} delete → tombstone {}", entity_type, eid);
                            }
                        } else {
                            let _ = self
                                .db
                                .query("DELETE entity_checksum WHERE entity_type = $et AND entity_id = $eid")
                                .bind(("et", entity_type.to_string()))
                                .bind(("eid", eid.clone()))
                                .await;
                            merkle::invalidate_root_cache(entity_type);
                            debug!("[live-watch] {} delete → drop checksum (cache/absent) {}", entity_type, eid);
                        }
                        continue;
                    }

                    // CREATE / UPDATE → recompute + upsert
                    if let Some(hash) = merkle::compute_content_hash(data) {
                        if let Err(e) = merkle_svc
                            .upsert_checksum(entity_type, &eid, &hash)
                            .await
                        {
                            warn!(
                                "[live-watch] {}: upsert checksum {}: {}",
                                entity_type, eid, e
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!("[live-watch] {} stream error: {}", entity_type, e);
                }
            }
        }

        warn!("[live-watch] {} stream ended unexpectedly", entity_type);
        Ok(())
    }

    /// Watch `sync_outbox` via LIVE SELECT. Triggers an immediate `process_outbox()`
    /// whenever a record is created or updated, giving zero-latency P2P push.
    /// Falls back to the 60s polling loop if this stream ends or errors.
    pub async fn watch_outbox(&self) -> anyhow::Result<()> {
        info!("[Sync] Starting real-time LIVE SELECT watcher for sync_outbox");

        let mut response = self.db.query("LIVE SELECT * FROM sync_outbox").await?;
        let mut stream = response.stream::<surrealdb::Notification<Value>>(0)?;

        while let Some(result) = stream.next().await {
            crate::metrics::tick(crate::metrics::M::OutboxEvent);
            match result {
                Ok(notification) => {
                    let action = notification.action.to_string();
                    if action == "Create" || action == "Update" {
                        debug!("[Sync] Live event ({}) in sync_outbox, triggering immediate push", action);
                        if let Err(e) = self.process_outbox().await {
                            warn!("[Sync] Real-time outbox push failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("[Sync] Live stream error: {}", e);
                }
            }
        }

        warn!("[Sync] Live query stream for sync_outbox ended unexpectedly");
        Ok(())
    }

    // ─── Accessors ───────────────────────────────────────────────────────────

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub fn mesh_id(&self) -> &str {
        &self.mesh_id
    }

    pub fn db(&self) -> &SurrealDb {
        &self.db
    }

    pub fn relay(&self) -> &RelayClient {
        &self.relay
    }
}

/// An outbox payload that carries no entity data — the legacy `{}` enqueue, a
/// JSON null, or a non-object. Such a payload can't be applied by a peer, so
/// `process_outbox` hydrates it from the local DB before pushing.
fn payload_is_empty(payload: &Value) -> bool {
    match payload {
        Value::Null => true,
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

/// Lightweight struct for deserializing outbox rows from SurrealDB.
#[derive(Debug, Clone, serde::Deserialize, surrealdb::types::SurrealValue)]
struct OutboxRecord {
    id: String,
    entity_type: String,
    entity_id: String,
    payload: Value,
    error_count: u32,
    #[allow(dead_code)]
    next_attempt_at: String,
    #[allow(dead_code)]
    created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_fiscal_records_are_registered_for_mesh() {
        // POS sales and storno decisions must merkle-reconcile across the mesh.
        assert!(SYNC_ENTITY_TYPES.contains(&"transaction"));
        assert!(SYNC_ENTITY_TYPES.contains(&"storno_log"));
        assert!(is_mesh_entity_type("transaction"));
        assert!(is_mesh_entity_type("storno_log"));
    }

    #[test]
    fn node_local_fiscal_state_never_meshes() {
        // The TSE/WAL per-register state must NEVER replicate (node-local).
        assert!(!SYNC_ENTITY_TYPES.contains(&"pending_fiscal_operations"));
        assert!(!SYNC_ENTITY_TYPES.contains(&"tse_state"));
        assert!(!is_mesh_entity_type("pending_fiscal_operations"));
        assert!(!is_mesh_entity_type("tse_state"));
    }

    #[test]
    fn payload_is_empty_detects_hydration_targets() {
        assert!(payload_is_empty(&serde_json::json!({})));
        assert!(payload_is_empty(&Value::Null));
        assert!(!payload_is_empty(&serde_json::json!({ "uuid": "x" })));
    }

    #[test]
    fn empty_root_skip_unknown_key_checks_immediately() {
        let mut map = HashMap::new();
        assert!(!consume_empty_root_skip(&mut map, "eck1|item"));
    }

    #[test]
    fn empty_root_skip_consumes_exactly_the_armed_credits() {
        let mut map = HashMap::new();
        map.insert("eck1|item".to_string(), 3u8);
        assert!(consume_empty_root_skip(&mut map, "eck1|item"));
        assert!(consume_empty_root_skip(&mut map, "eck1|item"));
        assert!(consume_empty_root_skip(&mut map, "eck1|item"));
        // Credits spent — this cycle does the real check and the entry is gone,
        // so a still-empty root re-arms and a non-empty one syncs normally.
        assert!(!consume_empty_root_skip(&mut map, "eck1|item"));
        assert!(!map.contains_key("eck1|item"));
    }

    #[test]
    fn empty_root_skip_keys_are_independent() {
        let mut map = HashMap::new();
        map.insert("eck1|item".to_string(), 1u8);
        assert!(consume_empty_root_skip(&mut map, "eck1|item"));
        assert!(!consume_empty_root_skip(&mut map, "eck2|item"));
        assert!(!consume_empty_root_skip(&mut map, "eck1|order"));
    }

    // ── Futility backoff state machine (layer 1) ──────────────────────────────

    fn futile() -> PassResult {
        PassResult { pulled_n: 5, writes_n: 0 }
    }

    #[test]
    fn futility_three_passes_trigger_5min_then_escalate() {
        let now = chrono::Utc::now();
        let mut st = FutilityState::default();

        // First two futile passes accumulate but do NOT park.
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 1);
        assert!(st.skip_until.is_none());
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 2);
        assert!(st.skip_until.is_none());

        // Third futile pass parks for 5 minutes.
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 3);
        assert_eq!(st.skip_until, Some(now + chrono::Duration::minutes(5)));

        // Fourth → 30 minutes.
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 4);
        assert_eq!(st.skip_until, Some(now + chrono::Duration::minutes(30)));

        // Fifth → 1 hour cap, and it stays capped.
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 5);
        assert_eq!(st.skip_until, Some(now + chrono::Duration::hours(1)));
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 6);
        assert_eq!(st.skip_until, Some(now + chrono::Duration::hours(1)), "capped at 1h");
    }

    #[test]
    fn futility_write_resets() {
        let now = chrono::Utc::now();
        let mut st = FutilityState::default();
        st = futility_next(&st, futile(), now);
        st = futility_next(&st, futile(), now);
        st = futility_next(&st, futile(), now); // parked
        assert_eq!(st.consecutive_futile, 3);

        // A pass with a real write resets completely.
        let after_write = futility_next(&st, PassResult { pulled_n: 5, writes_n: 2 }, now);
        assert_eq!(after_write, FutilityState::default());
    }

    #[test]
    fn futility_pulled_zero_resets() {
        let now = chrono::Utc::now();
        let mut st = FutilityState::default();
        st = futility_next(&st, futile(), now);
        st = futility_next(&st, futile(), now);
        assert_eq!(st.consecutive_futile, 2);

        // Trees agree (pulled 0) → reset, even though nothing was written.
        let after_agree = futility_next(&st, PassResult { pulled_n: 0, writes_n: 0 }, now);
        assert_eq!(after_agree, FutilityState::default());
    }

    #[test]
    fn futility_skip_delay_schedule() {
        assert_eq!(futility_skip_delay(0), None);
        assert_eq!(futility_skip_delay(2), None);
        assert_eq!(futility_skip_delay(3), Some(chrono::Duration::minutes(5)));
        assert_eq!(futility_skip_delay(4), Some(chrono::Duration::minutes(30)));
        assert_eq!(futility_skip_delay(5), Some(chrono::Duration::hours(1)));
        assert_eq!(futility_skip_delay(99), Some(chrono::Duration::hours(1)));
    }

    #[test]
    fn boot_wipe_forced_by_env() {
        assert!(boot_should_wipe(Some("1"), Some("abc"), "abc"));
        assert!(boot_should_wipe(Some("true"), Some("abc"), "abc"));
        assert!(boot_should_wipe(Some("TRUE"), Some("abc"), "abc"));
    }

    #[test]
    fn boot_wipe_env_off_values_do_not_force() {
        // Only 1/true force; anything else defers to the schema comparison.
        assert!(!boot_should_wipe(Some("0"), Some("abc"), "abc"));
        assert!(!boot_should_wipe(Some("no"), Some("abc"), "abc"));
        assert!(!boot_should_wipe(None, Some("abc"), "abc"));
    }

    #[test]
    fn boot_wipe_on_missing_or_changed_schema() {
        // Pre-marker DB (first boot after upgrade) → wipe once.
        assert!(boot_should_wipe(None, None, "abc"));
        // Schema changed → wipe to rebuild clean.
        assert!(boot_should_wipe(None, Some("old"), "abc"));
    }

    #[test]
    fn boot_no_wipe_on_warm_restart_same_schema() {
        // The common case: same schema as last boot, no force → cheap refresh.
        assert!(!boot_should_wipe(None, Some("abc"), "abc"));
    }
}
