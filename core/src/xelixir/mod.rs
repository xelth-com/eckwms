//! Xelixir on-demand C2 control plane.
//!
//! Transport-independent: a signed `CommandEnvelope` is delivered to the
//! target node either directly (via HTTPS POST) or indirectly through the
//! eck relay's task queue (for NAT'd targets). The target verifies the
//! Ed25519 signature against a per-node allow-list (`XELIXIR_ADMIN_PUBKEYS`),
//! decoupling xelixir routing from any specific mesh / `SYNC_SECRET`.

pub mod admin_cert;
pub mod envelope;
