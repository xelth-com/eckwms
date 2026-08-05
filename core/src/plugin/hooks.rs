//! Typed hook ABI contracts (design §3). This module defines the wire types and
//! the abi-version gate ONLY. The host-side application of a hook's output —
//! e.g. validating an `INGEST_TRANSFORM` `meta_patch` against the writable-field
//! allow-list — lives at the wms call site (a later phase), not here.
//!
//! Wire format is JSON in / JSON out, wrapped in a versioned envelope
//! `{ "abi": 1, ... }`. An envelope whose `abi` is not [`HOOK_ABI_VERSION`] is
//! rejected before the body is trusted, so an old host and a new plugin (or vice
//! versa) fail loudly instead of silently misreading each other.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// The single ABI version this build speaks.
pub const HOOK_ABI_VERSION: u32 = 1;

/// The hook surfaces the runtime knows about. Each maps to the exact wasm export
/// name a plugin must provide for that hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    /// Transform an inbound external payload into a proposed metadata patch
    /// (design §3, v1). Export name: `ingest_transform`.
    IngestTransform,
}

impl HookKind {
    /// The wasm export name a plugin must expose to serve this hook.
    pub fn export_name(self) -> &'static str {
        match self {
            HookKind::IngestTransform => "ingest_transform",
        }
    }
}

/// Errors decoding/validating a hook envelope.
#[derive(Debug, Error)]
pub enum HookError {
    #[error("unsupported hook abi {found} (this build speaks abi {})", HOOK_ABI_VERSION)]
    UnsupportedAbi { found: u32 },

    #[error("hook payload is not valid JSON for this contract: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Just the abi field, peeled off first so we can reject an unknown version
/// before deserializing (and trusting) the rest of the envelope.
#[derive(Deserialize)]
struct AbiPeek {
    abi: u32,
}

/// `INGEST_TRANSFORM` input (design §3). The plugin runs BEFORE PII tokenization
/// and sees the verbatim external payload; everything it emits is re-masked by
/// the host pipeline afterward.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestTransformInput {
    /// Origin of the payload, e.g. "zoho" | "exact".
    pub source: String,
    /// The verbatim external record.
    pub raw_payload: Value,
    /// Metadata already extracted by the host, for the plugin to build on.
    pub existing_meta: Value,
}

/// `INGEST_TRANSFORM` output (design §3). A *proposal*, not a write: the host
/// validates `meta_patch` against a writable-field allow-list before applying.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestTransformOutput {
    /// Proposed metadata field → value assignments.
    #[serde(default)]
    pub meta_patch: Map<String, Value>,
    /// Free-form notes for the audit trail / operator.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Serialize an `INGEST_TRANSFORM` input into the abi-tagged envelope bytes the
/// plugin receives.
pub fn encode_ingest_input(input: &IngestTransformInput) -> Result<Vec<u8>, HookError> {
    // Build `{ "abi": N, source, raw_payload, existing_meta }`.
    let mut obj = match serde_json::to_value(input)? {
        Value::Object(m) => m,
        other => {
            // IngestTransformInput always serializes to an object; defensive only.
            let mut m = Map::new();
            m.insert("input".to_string(), other);
            m
        }
    };
    obj.insert("abi".to_string(), Value::from(HOOK_ABI_VERSION));
    Ok(serde_json::to_vec(&Value::Object(obj))?)
}

/// Decode + abi-check an `INGEST_TRANSFORM` input envelope (the reader side of
/// [`encode_ingest_input`]). The `abi` tag is ignored by
/// [`IngestTransformInput`]'s own fields, so decoding the same bytes twice is
/// harmless.
pub fn decode_ingest_input(bytes: &[u8]) -> Result<IngestTransformInput, HookError> {
    let AbiPeek { abi } = serde_json::from_slice(bytes)?;
    if abi != HOOK_ABI_VERSION {
        return Err(HookError::UnsupportedAbi { found: abi });
    }
    Ok(serde_json::from_slice(bytes)?)
}

/// Decode an `INGEST_TRANSFORM` output produced by a plugin. Output carries no
/// abi tag — the plugin answers on the abi the host handed it in the input.
pub fn decode_ingest_output(bytes: &[u8]) -> Result<IngestTransformOutput, HookError> {
    Ok(serde_json::from_slice(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn export_name_is_stable() {
        assert_eq!(HookKind::IngestTransform.export_name(), "ingest_transform");
    }

    #[test]
    fn ingest_input_envelope_roundtrips_with_abi() {
        let input = IngestTransformInput {
            source: "zoho".to_string(),
            raw_payload: json!({ "acme model": "770", "serial_number": "X1" }),
            existing_meta: json!({}),
        };
        let bytes = encode_ingest_input(&input).unwrap();
        // The abi tag is present on the wire.
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["abi"], json!(HOOK_ABI_VERSION));
        // And it round-trips back to the typed body.
        let back = decode_ingest_input(&bytes).unwrap();
        assert_eq!(back.source, "zoho");
        assert_eq!(back.raw_payload["serial_number"], json!("X1"));
    }

    #[test]
    fn unknown_abi_is_rejected() {
        let bytes = serde_json::to_vec(&json!({
            "abi": 999, "source": "zoho", "raw_payload": {}, "existing_meta": {}
        }))
        .unwrap();
        match decode_ingest_input(&bytes) {
            Err(HookError::UnsupportedAbi { found: 999 }) => {}
            other => panic!("expected UnsupportedAbi(999), got {other:?}"),
        }
    }

    #[test]
    fn output_parses_patch_and_notes() {
        let bytes = serde_json::to_vec(&json!({
            "meta_patch": { "device_model": "770" },
            "notes": ["mapped from custom field"]
        }))
        .unwrap();
        let out = decode_ingest_output(&bytes).unwrap();
        assert_eq!(out.meta_patch.get("device_model"), Some(&json!("770")));
        assert_eq!(out.notes, vec!["mapped from custom field".to_string()]);
        // Empty object → default (empty patch, no notes).
        let empty = decode_ingest_output(b"{}").unwrap();
        assert!(empty.meta_patch.is_empty() && empty.notes.is_empty());
    }
}
