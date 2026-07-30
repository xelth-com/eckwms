//! Tiered PII-egress policy — the owner's opt-out ladder for pseudonymization.
//!
//! Two independent axes (surfaces), each with its own env knob:
//!   * `ECK_PII_EMBED_POLICY` — the EMBEDDING egress (vector index source text)
//!   * `ECK_PII_LLM_POLICY`   — the LLM egress (summarization / orchestrator
//!     prompts — what the generative brain reads)
//!
//! Three tiers per surface:
//!   * `masked` (DEFAULT) — today's behavior: PII tokenized before the surface,
//!     regardless of where the backend runs. Absent/unknown values resolve here
//!     (fail closed).
//!   * `clear_local` — clear text allowed ONLY when that surface's backend runs
//!     on-prem (nothing leaves the box, masking buys nothing and costs recall).
//!     With a cloud backend this silently degrades to `masked`, so it is SAFE
//!     to preconfigure on weak hardware today: the moment a local backend is
//!     enabled (`ECK_EMBED_MODE=local`, future `ECK_LLM_MODE=local`), clarity
//!     turns on by itself.
//!   * `clear` — clear text even to a CLOUD backend. This is the "customer has
//!     no secrets and wants accuracy" tier: a deliberate controller decision
//!     (GDPR: the self-hoster owns the tradeoff), so it logs a loud warning at
//!     resolution time. Never make this a default.
//!
//! SCOPE — the knob is read per node but the DECISION is per MESH (= per
//! customer deployment), like `SYNC_SECRET`: `ai_summary` and vectors sync
//! mesh-wide, so mixing masked and clear authors inside one mesh poisons both
//! the HNSW space and the "stored summary is masked" assumption the /mcp and
//! observer surfaces rely on. Configure every node of a mesh identically.
//!
//! What this policy does NOT change:
//!   * /mcp output tokenization (identity fields are tokenized for every tier
//!     regardless — defense in depth even on a no-secrets mesh);
//!   * `meta.subject` masking at distillation (Zone-2 storage contract);
//!   * `pii_fingerprints` — identity tokens are ALWAYS derived (they are the
//!     GDPR-erasure index key, not an egress; erase must keep working on clear
//!     meshes too).
//!
//! Operational notes for a policy FLIP (masked → clear):
//!   * summaries: the seed-hash marker (`summary_seed_hash` appends the policy
//!     when non-default) re-arms a Gemini re-summary per ticket on its next
//!     incremental import; for a full-corpus re-run use the admin re-summarize
//!     tools. The customer pays for the accuracy upgrade — expected.
//!   * vectors: re-embed by nulling `embedding` (the fleet-wide re-embed
//!     signal); do NOT leave pre-flip masked vectors mixed with clear ones in
//!     the same HNSW space longer than the transition.
//!
//! Future (deliberately only a seam here, to be tested on a separate mesh):
//! nodes that cannot embed or think themselves should REDIRECT to the nearest
//! capable node with load redistribution. The data-plane form already exists
//! (ECK_EMBED_WORKER=0 consumers adopt one author's vectors via mesh); the
//! request-time form needs capability+load advertisement in the mesh registry
//! and a resolver — tracked in TECH_DEBT ("PII/AI capability routing").

use tracing::warn;

/// Which egress the caller is about to feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Embedding,
    Llm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiiPolicy {
    Masked,
    ClearLocal,
    Clear,
}

impl PiiPolicy {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "clear" => PiiPolicy::Clear,
            "clear_local" | "clear-local" | "local" => PiiPolicy::ClearLocal,
            // Unknown values fail CLOSED — a typo must never turn masking off.
            _ => PiiPolicy::Masked,
        }
    }
}

fn policy_env(surface: Surface) -> String {
    let var = match surface {
        Surface::Embedding => "ECK_PII_EMBED_POLICY",
        Surface::Llm => "ECK_PII_LLM_POLICY",
    };
    std::env::var(var).unwrap_or_default()
}

/// Whether the given surface's backend runs on-prem right now.
fn backend_is_local(surface: Surface) -> bool {
    match surface {
        Surface::Embedding => std::env::var("ECK_EMBED_MODE")
            .map(|v| v.eq_ignore_ascii_case("local"))
            .unwrap_or(false),
        // No local generative backend exists yet (hardware too weak) — the
        // env name is reserved so `clear_local` starts working the day one
        // ships, with no further code change.
        Surface::Llm => std::env::var("ECK_LLM_MODE")
            .map(|v| v.eq_ignore_ascii_case("local"))
            .unwrap_or(false),
    }
}

/// Pure resolution — kept separate from env reads for testability.
pub fn resolve(policy: PiiPolicy, backend_local: bool) -> bool {
    match policy {
        PiiPolicy::Masked => false,
        PiiPolicy::ClearLocal => backend_local,
        PiiPolicy::Clear => true,
    }
}

/// True when the surface may receive CLEAR (untokenized) PII.
/// Reads env on every call — cheap, and lets ops flip a node without
/// plumbing state; the loud cloud-clear warning is rate-limited to once.
pub fn effective_clear(surface: Surface) -> bool {
    let policy = PiiPolicy::parse(&policy_env(surface));
    let local = backend_is_local(surface);
    let clear = resolve(policy, local);
    if clear && !local && policy == PiiPolicy::Clear {
        warn_cloud_clear_once(surface);
    }
    clear
}

fn warn_cloud_clear_once(surface: Surface) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED_EMBED: AtomicBool = AtomicBool::new(false);
    static WARNED_LLM: AtomicBool = AtomicBool::new(false);
    let flag = match surface {
        Surface::Embedding => &WARNED_EMBED,
        Surface::Llm => &WARNED_LLM,
    };
    if !flag.swap(true, Ordering::Relaxed) {
        warn!(
            "PII policy: {:?} surface is sending CLEAR customer PII to a CLOUD \
             backend (policy=clear). This must be a deliberate, documented \
             customer decision (no-secrets mesh); if it is not, unset the \
             ECK_PII_*_POLICY env now.",
            surface
        );
    }
}

/// Marker mixed into `summary_seed_hash` so a policy flip re-arms summaries.
/// Returns "" for the default (masked) so existing fleet hashes stay stable.
pub fn llm_seed_marker() -> &'static str {
    if effective_clear(Surface::Llm) {
        "pii:clear"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the PURE resolver is tested — env-var tests race across parallel
    // test modules (see the 2026-07-18 SYNC_SECRET pepper flake).
    #[test]
    fn masked_never_clear() {
        assert!(!resolve(PiiPolicy::Masked, false));
        assert!(!resolve(PiiPolicy::Masked, true));
    }

    #[test]
    fn clear_local_follows_backend() {
        assert!(!resolve(PiiPolicy::ClearLocal, false));
        assert!(resolve(PiiPolicy::ClearLocal, true));
    }

    #[test]
    fn clear_is_unconditional() {
        assert!(resolve(PiiPolicy::Clear, false));
        assert!(resolve(PiiPolicy::Clear, true));
    }

    #[test]
    fn unknown_policy_fails_closed() {
        assert_eq!(PiiPolicy::parse("claer"), PiiPolicy::Masked);
        assert_eq!(PiiPolicy::parse(""), PiiPolicy::Masked);
        assert_eq!(PiiPolicy::parse("CLEAR"), PiiPolicy::Clear);
        assert_eq!(PiiPolicy::parse("Clear_Local"), PiiPolicy::ClearLocal);
    }
}
