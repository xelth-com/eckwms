//! Always-on, near-zero-cost runtime counters for self-diagnosing runaway loops.
//!
//! In a fast-moving ("vibe-coded") codebase the most common production failure
//! is an accidental hot loop — a `LIVE SELECT` that self-triggers, a poll with a
//! missing backoff, a non-converging reconciliation that re-runs every cycle.
//! These peg a core while logging little or nothing, so they are hard to catch.
//!
//! Each counter here is a single **relaxed atomic add** — cheap enough to call
//! inside the tightest loop. The wms health monitor samples them at a fixed
//! interval and keeps a rolling history, so an operator can answer two questions
//! that are otherwise very hard: *which* subsystem's rate spiked, and *when* it
//! started (i.e. what deploy/data introduced the loop). Surfaced at
//! `GET /api/health/deep`.
//!
//! To add a probe: add a variant before `Count`, add its name to `NAMES` at the
//! same position, and call [`tick`]/[`add`] at the loop site.

use std::sync::atomic::{AtomicU64, Ordering};

/// Named counters. Keep `Count` last; `NAMES` must stay index-aligned.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum M {
    /// One full mesh sync cycle (`SyncEngine::sync_cycle`).
    SyncCycle = 0,
    /// One `refresh_checksums` pass (re-records entity_checksum before a cycle).
    RefreshChecksums,
    /// One `compute_content_hash` call — invoked per row during checksum
    /// (re)builds and per live-watch event. A runaway sync/merkle path shows up
    /// here as a huge per-interval rate even when nothing else logs.
    MerkleHash,
    /// One live-watch notification handled (`watch_one_entity`).
    LiveWatchEvent,
    /// One `process_outbox` invocation.
    OutboxProcess,
    /// One `sync_outbox` LIVE SELECT event (`watch_outbox`).
    OutboxEvent,
    /// One peer discovery round-trip to the relay.
    DiscoverPeers,
    /// One embedding-worker loop iteration.
    EmbeddingCycle,
    /// Rows the embedding worker actually pulled (candidate rows).
    EmbeddingRows,
    /// One summarization-worker loop iteration.
    SummarizationCycle,
    /// One observer loop iteration.
    ObserverCycle,
    /// One orchestrator `poll_ready_tasks` pass.
    OrchestratorPoll,
    /// One orchestrator task claimed/executed.
    OrchestratorTask,
    /// One merkle root/bucket view actually built from the DB (cache miss).
    /// Steady-state on an idle node this should tick only after writes —
    /// a per-cycle rate ≈ peers×tables means the root cache is not holding.
    MerkleRootBuild,
    /// keep last — array length sentinel.
    Count,
}

/// Number of real counters.
pub const N: usize = M::Count as usize;

/// Stable wire names, index-aligned with [`M`].
pub const NAMES: [&str; N] = [
    "sync_cycle",
    "refresh_checksums",
    "merkle_hash",
    "live_watch_event",
    "outbox_process",
    "outbox_event",
    "discover_peers",
    "embedding_cycle",
    "embedding_rows",
    "summarization_cycle",
    "observer_cycle",
    "orchestrator_poll",
    "orchestrator_task",
    "merkle_root_build",
];

static COUNTERS: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];

/// Increment a counter by one. Cheap enough for hot loops.
#[inline]
pub fn tick(m: M) {
    COUNTERS[m as usize].fetch_add(1, Ordering::Relaxed);
}

/// Increment a counter by `n` (e.g. number of rows processed).
#[inline]
pub fn add(m: M, n: u64) {
    COUNTERS[m as usize].fetch_add(n, Ordering::Relaxed);
}

/// Read all counters (cumulative totals since process start).
pub fn snapshot() -> [u64; N] {
    let mut out = [0u64; N];
    for (i, slot) in COUNTERS.iter().enumerate() {
        out[i] = slot.load(Ordering::Relaxed);
    }
    out
}
