use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single UI label string for one `(lang, key)` pair (SurrealDB document).
///
/// SurrealDB is the RUNTIME source of truth for dashboard labels; the JS locale
/// files under `wms/web/src/lib/i18n/locales/` are git-versioned SEEDS only.
/// The seed is baked into `wms/assets/i18n_seed.json` (see
/// `tools/gen_i18n_seed.mjs`) and INSERT-if-missing'd at startup — a manual DB
/// edit always wins over the seed.
///
/// The record id is DETERMINISTIC — derived from `(lang, key)` via
/// [`label_record_id`] — so seeding and manual edits are natural UPSERTs and
/// the same label can never be duplicated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct I18nLabel {
    /// Runtime key `"<namespace>.<snake_key>"`, e.g. `"shell.nav_repairs"`.
    pub key: String,
    /// Language code as used by the locale dirs: `en` / `de` / `ko` / `ru` / …
    pub lang: String,
    /// The translated label string.
    pub value: String,
    pub updated_at: DateTime<Utc>,
}

/// Deterministic record-id KEY for a label, derived from `(lang, key)`.
///
/// Mirrors the repo's SHA256 → dashed-UUID addressing style (see
/// [`crate::utils::identity::compute_mesh_id`]): a stable lowercase UUID so
/// CREATE/UPSERT on the same `(lang, key)` always targets one row — no
/// duplicates, and seed-vs-manual edits collapse onto the same record. The
/// `0x1f` unit separator between the two fields prevents `(a, bc)` and
/// `(ab, c)` from colliding onto the same id.
///
/// Returns just the id key (no table prefix); address the row as
/// `i18n_label:⟨key⟩` / `type::thing("i18n_label", label_record_id(..))`.
pub fn label_record_id(lang: &str, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(lang.as_bytes());
    hasher.update([0x1f]); // ASCII unit separator — disambiguates the join
    hasher.update(key.as_bytes());
    let hash = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Uuid::from_bytes(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id must be a stable dashed lowercase UUID and reproduce exactly for
    /// the same inputs (this is what makes seeding an idempotent upsert).
    #[test]
    fn record_id_is_deterministic() {
        let a = label_record_id("de", "shell.nav_repairs");
        let b = label_record_id("de", "shell.nav_repairs");
        assert_eq!(a, b);
        assert_eq!(a.len(), 36); // dashed UUID
        assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }

    /// Different lang OR different key ⇒ different id.
    #[test]
    fn record_id_separates_lang_and_key() {
        let de = label_record_id("de", "shell.nav_repairs");
        let en = label_record_id("en", "shell.nav_repairs");
        let other = label_record_id("de", "shell.nav_devices");
        assert_ne!(de, en);
        assert_ne!(de, other);
    }

    /// The unit separator must prevent the classic concatenation collision:
    /// `(lang="ab", key="c")` and `(lang="a", key="bc")` would share the same
    /// id under naive concatenation, but must not here.
    #[test]
    fn record_id_no_concat_collision() {
        assert_ne!(label_record_id("ab", "c"), label_record_id("a", "bc"));
    }
}
