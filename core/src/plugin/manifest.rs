//! The `wasm_plugin` registry record (design §5).
//!
//! Field shape and derives follow the existing synced SurrealValue models
//! (`core/src/models/ai_sop.rs`): `id`/`_vclock` are `Option<Value>`, timestamps
//! are `DateTime<Utc>`. The sha256 IS the plugin's version identity — an upgrade
//! is a new sha on the SAME named record, a rollback is the old sha; there are
//! no `replaced_by` chains.
//!
//! Adding this table to `SYNC_ENTITY_TYPES` (so the record meshes) is a later
//! phase and is deliberately NOT done here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use surrealdb::types::SurrealValue;

/// SurrealDB table the registry records live in.
pub const WASM_PLUGIN_TABLE: &str = "wasm_plugin";

/// One hook a plugin serves: the wasm export name plus the abi it answers on.
///
/// `abi` is stored as `i64` to match SurrealDB's numeric type (the JSON/ABI
/// layer in [`super::hooks`] uses `u32`); convert at the boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, SurrealValue)]
pub struct PluginHookRef {
    /// Exported function name, e.g. "ingest_transform".
    pub name: String,
    /// ABI version this hook answers on.
    pub abi: i64,
}

/// A registered plugin. One record per plugin `name`; `sha256` points at exactly
/// one binary in the CAS.
#[derive(Clone, Debug, Serialize, Deserialize, SurrealValue)]
pub struct WasmPlugin {
    /// Surreal record id. `None` when constructing for an upsert-by-name — the
    /// resource key supplies it — so we skip serializing a null that would
    /// otherwise fight the record key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Stable human name; also the record key (`wasm_plugin:<name>`).
    pub name: String,
    /// Operator-facing version label (informational; sha is the real identity).
    pub version_label: String,
    /// Content address of the wasm binary in the CAS (its version identity).
    pub sha256: String,
    /// Hooks this plugin serves.
    pub hooks: Vec<PluginHookRef>,
    /// Capability tokens the plugin is granted (e.g. ["log", "anonymize"]).
    pub capabilities: Vec<String>,
    /// Whether the plugin is enabled (install lands it disabled; enable flips it).
    pub enabled: bool,
    /// Mesh instance id of the node that installed this record (creator-authority
    /// convention — mirrors `home_instance_id` on other synced entities).
    pub home_instance_id: String,
    /// Mesh vector clock (populated by the sync layer; `None` at install).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _vclock: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_plugin_json_roundtrips_and_omits_null_id() {
        let now = Utc::now();
        let p = WasmPlugin {
            id: None,
            name: "zoho-acme".to_string(),
            version_label: "v1".to_string(),
            sha256: "a".repeat(64),
            hooks: vec![PluginHookRef {
                name: "ingest_transform".to_string(),
                abi: 1,
            }],
            capabilities: vec!["anonymize".to_string(), "log".to_string()],
            enabled: false,
            home_instance_id: "node-1".to_string(),
            _vclock: None,
            created_at: now,
            updated_at: now,
        };
        let v = serde_json::to_value(&p).unwrap();
        // Null id/_vclock are omitted so upsert-by-key isn't fought.
        assert!(v.get("id").is_none(), "null id should be skipped");
        assert!(v.get("_vclock").is_none(), "null _vclock should be skipped");
        assert_eq!(v["hooks"][0]["name"], "ingest_transform");
        assert_eq!(v["hooks"][0]["abi"], 1);

        let back: WasmPlugin = serde_json::from_value(v).unwrap();
        assert_eq!(back.name, "zoho-acme");
        assert_eq!(back.sha256, "a".repeat(64));
        assert_eq!(back.capabilities, vec!["anonymize", "log"]);
        assert!(!back.enabled);
    }
}
