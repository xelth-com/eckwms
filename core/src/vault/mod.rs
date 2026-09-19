//! The PII vault key plumbing (`.eck/PII_VAULT.md` §2, P0.5).
//!
//! 9eck never holds a key that opens tenant data. Each full node holds only its
//! own KEK (a file, never leaves the node — [`kek`]); the KEK-sealed blob
//! ([`blob`]) is split into Shamir shares ([`shamir`]) held one-per-server on the
//! eck1/2/3 "safe". A booting node fetches any two shares ([`client`]), rebuilds
//! and opens the blob with its KEK, and derives the live ring keys (pepper / mesh
//! data key / vault key) into [`crate::keyring`]. Two eck servers colluding get
//! the KEK-wrapped blob and still nothing — the KEK never leaves the node.

pub mod blob;
pub mod client;
pub mod datafs;
pub mod kek;
pub mod shamir;

pub use blob::{Derived, VaultPayload};
pub use client::{NodeActionRequest, PutSharesRequest, SafeClient, VaultNodeRow, SAFE_TTL_SECS};
