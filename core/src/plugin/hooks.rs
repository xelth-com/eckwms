//! Typed hook ABI contracts (design §3). This module defines the wire types and
//! the abi-version gate ONLY. The host-side application of a hook's output —
//! e.g. validating an `INGEST_TRANSFORM` `meta_patch` against the writable-field
//! allow-list — lives at the wms call site (a later phase), not here.
//!
//! Wire format is JSON in / JSON out, wrapped in a versioned envelope
//! `{ "abi": 1, ... }`. An envelope whose `abi` is not [`HOOK_ABI_VERSION`] is
//! rejected before the body is trusted, so an old host and a new plugin (or vice
//! versa) fail loudly instead of silently misreading each other.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use super::PluginError;

/// The single ABI version this build speaks.
pub const HOOK_ABI_VERSION: u32 = 1;

/// The hook surfaces the runtime knows about. Each maps to the exact wasm export
/// name a plugin must provide for that hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    /// Transform an inbound external payload into a proposed metadata patch
    /// (design §3, v1). Export name: `ingest_transform`.
    IngestTransform,

    /// Propose price adjustments on a cart snapshot BEFORE the TSE sign
    /// (owner decision 2026-10-06). Firm-specific pricing (happy hours, tiers,
    /// loyalty) lives in a customer plugin, never in core. Export name:
    /// `pos_price_rules`. HOT PATH — runs inline on the sale, tight budget,
    /// and a failure is "no adjustments", never a blocked sale.
    PosPriceRules,

    /// Propose extra receipt header/footer lines on a finished-transaction
    /// snapshot (owner decision 2026-10-06). Export name: `pos_receipt_render`.
    /// HOT PATH — a failure is "no extra lines", never a blocked sale.
    PosReceiptRender,
}

impl HookKind {
    /// The wasm export name a plugin must expose to serve this hook.
    pub fn export_name(self) -> &'static str {
        match self {
            HookKind::IngestTransform => "ingest_transform",
            HookKind::PosPriceRules => "pos_price_rules",
            HookKind::PosReceiptRender => "pos_receipt_render",
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

// ===========================================================================
// POS hooks (owner decision 2026-10-06) — runtime side only.
//
// Both are HOT-PATH hooks (run inline on the sale), so the engine runs them
// with `PluginLimits::pos_hot_path()` (see engine.rs). Determinism: the only
// host functions the engine ever links are `host_log` and `host_anonymize`
// (see host.rs) — NEITHER reads a clock nor any entropy source, and no WASI /
// I/O surface is compiled in. The wall-clock time a plugin needs is handed in
// as `business_time` (RFC3339) in the input, never read from a clock inside
// the sandbox. Identical input therefore yields identical output.
//
// Neither hook can block a sale: any plugin trap / timeout / fuel-or-memory
// trip, malformed output, or host-side validation rejection collapses to the
// typed [`PosHookSkip`] outcome ("no adjustments / no extra lines"). The pos
// crate consumes [`finalize_pos_price_rules`] / [`finalize_pos_receipt_render`]
// (next wave); this module owns the contract + validation only.
// ===========================================================================

/// At most this many adjustments in one `pos_price_rules` proposal.
const MAX_PRICE_ADJUSTMENTS: usize = 32;
/// Max `char`s in an adjustment `label`.
const MAX_LABEL_CHARS: usize = 64;
/// Max `char`s in an adjustment `rule_id`.
const MAX_RULE_ID_CHARS: usize = 64;

/// At most this many receipt header lines.
const MAX_HEADER_LINES: usize = 10;
/// At most this many receipt footer lines.
const MAX_FOOTER_LINES: usize = 20;
/// Max `char`s per receipt line, measured AFTER control-stripping + trimming.
const MAX_RECEIPT_LINE_CHARS: usize = 48;
/// ESC — ESC/POS command introducer. Its presence in a proposed receipt line
/// is treated as an injection attempt and rejects the whole proposal.
const ESC: char = '\u{1b}';
/// GS — the other ESC/POS command introducer; rejected like [`ESC`].
const GS: char = '\u{1d}';

/// One cart line as the `pos_price_rules` hook sees it. `tax_rate` is present
/// for the plugin to read, but is deliberately NOT echoed into the output type:
/// a plugin can price, never retax (GoBD/TSE red line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosCartItem {
    /// 0-based position of this line in `items` — the value an adjustment's
    /// `item_index` refers to.
    pub index: usize,
    pub item_id: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
    pub quantity: f64,
    pub unit_price: f64,
    pub tax_rate: f64,
    pub line_total: f64,
}

/// `pos_price_rules` input: a cart snapshot taken BEFORE the TSE sign. The
/// plugin receives `business_time` (RFC3339) explicitly — it never reads a
/// clock (none is exposed), which is what keeps the hook deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosPriceRulesInput {
    /// RFC3339 business time; the plugin's only notion of "now".
    pub business_time: String,
    pub currency: String,
    pub actor_role: String,
    #[serde(default)]
    pub customer_tags: Vec<String>,
    pub items: Vec<PosCartItem>,
    pub subtotal: f64,
}

/// What a single adjustment targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentTarget {
    /// One cart line, selected by `item_index`.
    Item,
    /// The cart subtotal as a whole.
    Cart,
}

/// How an adjustment changes the targeted total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentKind {
    /// Reduce by `value`% (value in `(0, 100]`).
    Percent,
    /// Reduce by an absolute `value` (`value >= 0`).
    Absolute,
    /// Replace the targeted total with `value` (`value >= 0`).
    FixedPrice,
}

/// One proposed adjustment. A PROPOSAL — the host validates
/// ([`validate_pos_price_rules`]) and applies; the plugin never writes a total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceAdjustment {
    pub target: AdjustmentTarget,
    /// Required + in range for `target = item`; MUST be absent for
    /// `target = cart`.
    #[serde(default)]
    pub item_index: Option<usize>,
    pub kind: AdjustmentKind,
    pub value: f64,
    pub label: String,
    pub rule_id: String,
}

/// `pos_price_rules` output — a proposal the host validates before applying.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PosPriceRulesOutput {
    #[serde(default)]
    pub adjustments: Vec<PriceAdjustment>,
}

/// `pos_receipt_render` input: a finished-transaction snapshot plus locale and
/// business time. `transaction` is opaque JSON — the plugin reads what it needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosReceiptRenderInput {
    pub transaction: Value,
    pub locale: String,
    /// RFC3339 business time; see the determinism note above.
    pub business_time: String,
}

/// `pos_receipt_render` output — proposed extra lines the host validates and
/// normalizes before applying.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PosReceiptRenderOutput {
    #[serde(default)]
    pub header_lines: Vec<String>,
    #[serde(default)]
    pub footer_lines: Vec<String>,
}

/// Validated + normalized receipt lines (control chars stripped, ends trimmed).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PosReceiptLines {
    pub header_lines: Vec<String>,
    pub footer_lines: Vec<String>,
}

/// Why a POS hook contributed nothing. Carried by the `Skipped` arm of either
/// outcome; it is information for logs/audit and NEVER an error that blocks the
/// sale.
#[derive(Debug, Clone)]
pub enum PosHookSkip {
    /// The plugin exceeded its wall-clock budget.
    Timeout,
    /// The plugin exhausted its fuel budget.
    FuelExhausted,
    /// The plugin trapped, failed to load/instantiate, OOM'd, or its bytes
    /// could not be resolved. Message carries the detail.
    Failed(String),
    /// The plugin ran but its output was not valid JSON for this contract.
    Malformed(String),
    /// Host validation rejected the proposal (see [`ProposalRejected`]).
    Rejected(String),
}

impl PosHookSkip {
    /// Map a [`PluginError`] from `PluginEngine::run` onto a skip reason.
    fn from_plugin_error(err: PluginError) -> Self {
        match err {
            PluginError::Timeout(_) => PosHookSkip::Timeout,
            PluginError::FuelExhausted => PosHookSkip::FuelExhausted,
            other => PosHookSkip::Failed(other.to_string()),
        }
    }
}

/// Outcome of a `pos_price_rules` call, as the pos side consumes it.
#[derive(Debug, Clone)]
pub enum PosPriceRulesOutcome {
    /// Validated adjustments to apply (possibly empty).
    Applied(Vec<PriceAdjustment>),
    /// Nothing to apply — the sale proceeds unchanged.
    Skipped(PosHookSkip),
}

/// Outcome of a `pos_receipt_render` call, as the pos side consumes it.
#[derive(Debug, Clone)]
pub enum PosReceiptRenderOutcome {
    /// Validated/normalized extra lines to print (possibly empty).
    Applied(PosReceiptLines),
    /// Nothing extra to print — the receipt renders unchanged.
    Skipped(PosHookSkip),
}

/// Why a proposal was rejected in full. Rejection is all-or-nothing: on the
/// first violation the entire proposal is discarded (the ingest `meta_patch`
/// allow-list rejects the same way).
#[derive(Debug, Error, PartialEq)]
pub enum ProposalRejected {
    #[error("too many adjustments: {count} > {max}")]
    TooManyAdjustments { count: usize, max: usize },

    #[error("adjustment {index}: item_index {got:?} out of range (cart has {items} item(s))")]
    ItemIndexOutOfRange {
        index: usize,
        got: Option<usize>,
        items: usize,
    },

    #[error("adjustment {index}: a cart-target adjustment must not carry item_index")]
    CartAdjustmentHasItemIndex { index: usize },

    #[error("adjustment {index}: percent {value} not in (0, 100]")]
    PercentOutOfRange { index: usize, value: f64 },

    #[error("adjustment {index}: {value} must be >= 0")]
    NegativeValue { index: usize, value: f64 },

    #[error("adjustment {index}: would drive a total negative ({resulting})")]
    DrivesTotalNegative { index: usize, resulting: f64 },

    #[error("adjustment {index}: {field} exceeds {max} chars")]
    TextTooLong {
        index: usize,
        field: &'static str,
        max: usize,
    },

    #[error("adjustment {index}: {field} contains control characters")]
    TextNotPrintable { index: usize, field: &'static str },

    #[error("{section} has too many lines: {count} > {max}")]
    TooManyLines {
        section: &'static str,
        count: usize,
        max: usize,
    },

    #[error("{section} line {index}: {chars} chars exceeds {max}")]
    ReceiptLineTooLong {
        section: &'static str,
        index: usize,
        chars: usize,
        max: usize,
    },

    #[error("{section} line {index}: ESC/GS control byte (ESC/POS injection)")]
    ControlInjection { section: &'static str, index: usize },
}

/// Serialize a POS hook input into the abi-tagged envelope the plugin receives
/// (same envelope shape as [`encode_ingest_input`]).
fn encode_envelope<T: Serialize>(input: &T) -> Result<Vec<u8>, HookError> {
    let mut obj = match serde_json::to_value(input)? {
        Value::Object(m) => m,
        other => {
            let mut m = Map::new();
            m.insert("input".to_string(), other);
            m
        }
    };
    obj.insert("abi".to_string(), Value::from(HOOK_ABI_VERSION));
    Ok(serde_json::to_vec(&Value::Object(obj))?)
}

/// Decode + abi-check a POS hook input envelope (reader side of
/// [`encode_envelope`]).
fn decode_envelope<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, HookError> {
    let AbiPeek { abi } = serde_json::from_slice(bytes)?;
    if abi != HOOK_ABI_VERSION {
        return Err(HookError::UnsupportedAbi { found: abi });
    }
    Ok(serde_json::from_slice(bytes)?)
}

/// Encode a `pos_price_rules` input envelope.
pub fn encode_pos_price_rules_input(input: &PosPriceRulesInput) -> Result<Vec<u8>, HookError> {
    encode_envelope(input)
}

/// Decode + abi-check a `pos_price_rules` input envelope (the plugin's reader
/// side; also used in tests).
pub fn decode_pos_price_rules_input(bytes: &[u8]) -> Result<PosPriceRulesInput, HookError> {
    decode_envelope(bytes)
}

/// Decode a `pos_price_rules` output produced by a plugin (no abi tag — it
/// answers on the abi the host handed it).
pub fn decode_pos_price_rules_output(bytes: &[u8]) -> Result<PosPriceRulesOutput, HookError> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Encode a `pos_receipt_render` input envelope.
pub fn encode_pos_receipt_render_input(
    input: &PosReceiptRenderInput,
) -> Result<Vec<u8>, HookError> {
    encode_envelope(input)
}

/// Decode + abi-check a `pos_receipt_render` input envelope.
pub fn decode_pos_receipt_render_input(bytes: &[u8]) -> Result<PosReceiptRenderInput, HookError> {
    decode_envelope(bytes)
}

/// Decode a `pos_receipt_render` output produced by a plugin.
pub fn decode_pos_receipt_render_output(bytes: &[u8]) -> Result<PosReceiptRenderOutput, HookError> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Resolve the base total an adjustment applies to, validating its target /
/// `item_index` in the process.
fn resolve_base(
    index: usize,
    adj: &PriceAdjustment,
    input: &PosPriceRulesInput,
) -> Result<f64, ProposalRejected> {
    match adj.target {
        AdjustmentTarget::Item => {
            let idx = adj
                .item_index
                .ok_or(ProposalRejected::ItemIndexOutOfRange {
                    index,
                    got: None,
                    items: input.items.len(),
                })?;
            match input.items.get(idx) {
                Some(item) => Ok(item.line_total),
                None => Err(ProposalRejected::ItemIndexOutOfRange {
                    index,
                    got: Some(idx),
                    items: input.items.len(),
                }),
            }
        }
        AdjustmentTarget::Cart => {
            if adj.item_index.is_some() {
                return Err(ProposalRejected::CartAdjustmentHasItemIndex { index });
            }
            Ok(input.subtotal)
        }
    }
}

/// Validate a free-text field (`label` / `rule_id`): length-bounded and
/// printable (no control characters).
fn check_text_field(
    index: usize,
    field: &'static str,
    value: &str,
    max: usize,
) -> Result<(), ProposalRejected> {
    if value.chars().count() > max {
        return Err(ProposalRejected::TextTooLong { index, field, max });
    }
    if value.chars().any(char::is_control) {
        return Err(ProposalRejected::TextNotPrintable { index, field });
    }
    Ok(())
}

/// Validate a `pos_price_rules` proposal against the cart it was computed on.
///
/// All-or-nothing: on the first violation the whole proposal is rejected. On
/// success returns the adjustments to apply (the host applies them; `tax_rate`
/// is untouchable — it is not in the output type at all). Each adjustment is
/// checked against the ORIGINAL cart total it references (independent, not
/// compounded) so the check is deterministic regardless of apply order.
pub fn validate_pos_price_rules(
    output: &PosPriceRulesOutput,
    input: &PosPriceRulesInput,
) -> Result<Vec<PriceAdjustment>, ProposalRejected> {
    if output.adjustments.len() > MAX_PRICE_ADJUSTMENTS {
        return Err(ProposalRejected::TooManyAdjustments {
            count: output.adjustments.len(),
            max: MAX_PRICE_ADJUSTMENTS,
        });
    }
    for (i, adj) in output.adjustments.iter().enumerate() {
        let base = resolve_base(i, adj, input)?;

        match adj.kind {
            AdjustmentKind::Percent => {
                // `!(…)` so NaN (all comparisons false) is rejected too.
                if !(adj.value > 0.0 && adj.value <= 100.0) {
                    return Err(ProposalRejected::PercentOutOfRange {
                        index: i,
                        value: adj.value,
                    });
                }
            }
            AdjustmentKind::Absolute | AdjustmentKind::FixedPrice => {
                if !(adj.value >= 0.0) {
                    return Err(ProposalRejected::NegativeValue {
                        index: i,
                        value: adj.value,
                    });
                }
            }
        }

        let resulting = match adj.kind {
            AdjustmentKind::Percent => base * (1.0 - adj.value / 100.0),
            AdjustmentKind::Absolute => base - adj.value,
            AdjustmentKind::FixedPrice => adj.value,
        };
        if resulting < 0.0 {
            return Err(ProposalRejected::DrivesTotalNegative {
                index: i,
                resulting,
            });
        }

        check_text_field(i, "label", &adj.label, MAX_LABEL_CHARS)?;
        check_text_field(i, "rule_id", &adj.rule_id, MAX_RULE_ID_CHARS)?;
    }
    Ok(output.adjustments.clone())
}

/// Normalize + validate one receipt section's lines. ESC/GS bytes reject the
/// whole proposal (ESC/POS injection); any other control char is stripped and
/// surrounding whitespace trimmed; a line still over the char cap rejects.
fn normalize_lines(
    section: &'static str,
    lines: &[String],
) -> Result<Vec<String>, ProposalRejected> {
    let mut out = Vec::with_capacity(lines.len());
    for (i, raw) in lines.iter().enumerate() {
        if raw.contains(ESC) || raw.contains(GS) {
            return Err(ProposalRejected::ControlInjection { section, index: i });
        }
        let stripped: String = raw.chars().filter(|c| !c.is_control()).collect();
        let trimmed = stripped.trim();
        let chars = trimmed.chars().count();
        if chars > MAX_RECEIPT_LINE_CHARS {
            return Err(ProposalRejected::ReceiptLineTooLong {
                section,
                index: i,
                chars,
                max: MAX_RECEIPT_LINE_CHARS,
            });
        }
        out.push(trimmed.to_string());
    }
    Ok(out)
}

/// Validate + normalize a `pos_receipt_render` proposal. All-or-nothing, like
/// [`validate_pos_price_rules`].
pub fn validate_pos_receipt_render(
    output: &PosReceiptRenderOutput,
) -> Result<PosReceiptLines, ProposalRejected> {
    if output.header_lines.len() > MAX_HEADER_LINES {
        return Err(ProposalRejected::TooManyLines {
            section: "header",
            count: output.header_lines.len(),
            max: MAX_HEADER_LINES,
        });
    }
    if output.footer_lines.len() > MAX_FOOTER_LINES {
        return Err(ProposalRejected::TooManyLines {
            section: "footer",
            count: output.footer_lines.len(),
            max: MAX_FOOTER_LINES,
        });
    }
    Ok(PosReceiptLines {
        header_lines: normalize_lines("header", &output.header_lines)?,
        footer_lines: normalize_lines("footer", &output.footer_lines)?,
    })
}

/// Turn the raw result of a `pos_price_rules` engine call into the outcome the
/// pos side consumes. ANY failure (engine error, malformed output, validation
/// rejection) becomes [`PosPriceRulesOutcome::Skipped`] — the sale is never
/// blocked.
pub fn finalize_pos_price_rules(
    result: Result<Vec<u8>, PluginError>,
    input: &PosPriceRulesInput,
) -> PosPriceRulesOutcome {
    let bytes = match result {
        Ok(b) => b,
        Err(e) => return PosPriceRulesOutcome::Skipped(PosHookSkip::from_plugin_error(e)),
    };
    let raw = match decode_pos_price_rules_output(&bytes) {
        Ok(o) => o,
        Err(e) => return PosPriceRulesOutcome::Skipped(PosHookSkip::Malformed(e.to_string())),
    };
    match validate_pos_price_rules(&raw, input) {
        Ok(adjustments) => PosPriceRulesOutcome::Applied(adjustments),
        Err(e) => PosPriceRulesOutcome::Skipped(PosHookSkip::Rejected(e.to_string())),
    }
}

/// Turn the raw result of a `pos_receipt_render` engine call into the outcome
/// the pos side consumes. ANY failure becomes
/// [`PosReceiptRenderOutcome::Skipped`] — the receipt is never blocked.
pub fn finalize_pos_receipt_render(
    result: Result<Vec<u8>, PluginError>,
) -> PosReceiptRenderOutcome {
    let bytes = match result {
        Ok(b) => b,
        Err(e) => return PosReceiptRenderOutcome::Skipped(PosHookSkip::from_plugin_error(e)),
    };
    let raw = match decode_pos_receipt_render_output(&bytes) {
        Ok(o) => o,
        Err(e) => return PosReceiptRenderOutcome::Skipped(PosHookSkip::Malformed(e.to_string())),
    };
    match validate_pos_receipt_render(&raw) {
        Ok(lines) => PosReceiptRenderOutcome::Applied(lines),
        Err(e) => PosReceiptRenderOutcome::Skipped(PosHookSkip::Rejected(e.to_string())),
    }
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

#[cfg(test)]
mod pos_tests {
    use super::*;
    use serde_json::json;

    fn item(index: usize, category: &str, line_total: f64) -> PosCartItem {
        PosCartItem {
            index,
            item_id: format!("sku-{index}"),
            name: format!("Item {index}"),
            category: Some(category.to_string()),
            quantity: 1.0,
            unit_price: line_total,
            tax_rate: 19.0,
            line_total,
        }
    }

    fn cart() -> PosPriceRulesInput {
        PosPriceRulesInput {
            business_time: "2026-10-06T18:00:00+02:00".to_string(),
            currency: "EUR".to_string(),
            actor_role: "cashier".to_string(),
            customer_tags: vec![],
            items: vec![item(0, "drinks", 10.0), item(1, "food", 20.0)],
            subtotal: 30.0,
        }
    }

    fn adj(
        target: AdjustmentTarget,
        item_index: Option<usize>,
        kind: AdjustmentKind,
        value: f64,
    ) -> PriceAdjustment {
        PriceAdjustment {
            target,
            item_index,
            kind,
            value,
            label: "Happy Hour".to_string(),
            rule_id: "happy_hour_drinks".to_string(),
        }
    }

    #[test]
    fn export_names_are_stable() {
        assert_eq!(HookKind::PosPriceRules.export_name(), "pos_price_rules");
        assert_eq!(HookKind::PosReceiptRender.export_name(), "pos_receipt_render");
    }

    #[test]
    fn price_rules_input_envelope_roundtrips_with_abi() {
        let input = cart();
        let bytes = encode_pos_price_rules_input(&input).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["abi"], json!(HOOK_ABI_VERSION));
        let back = decode_pos_price_rules_input(&bytes).unwrap();
        assert_eq!(back.currency, "EUR");
        assert_eq!(back.items.len(), 2);
    }

    #[test]
    fn receipt_input_envelope_roundtrips_with_abi() {
        let input = PosReceiptRenderInput {
            transaction: json!({ "id": "tx-1", "total": 30.0 }),
            locale: "de-DE".to_string(),
            business_time: "2026-10-06T18:00:00+02:00".to_string(),
        };
        let bytes = encode_pos_receipt_render_input(&input).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["abi"], json!(HOOK_ABI_VERSION));
        let back = decode_pos_receipt_render_input(&bytes).unwrap();
        assert_eq!(back.locale, "de-DE");
        assert_eq!(back.transaction["id"], json!("tx-1"));
    }

    #[test]
    fn unknown_abi_rejected_on_pos_input() {
        let bytes = serde_json::to_vec(&json!({
            "abi": 2, "business_time": "x", "currency": "EUR", "actor_role": "c",
            "items": [], "subtotal": 0.0
        }))
        .unwrap();
        match decode_pos_price_rules_input(&bytes) {
            Err(HookError::UnsupportedAbi { found: 2 }) => {}
            other => panic!("expected UnsupportedAbi(2), got {other:?}"),
        }
    }

    #[test]
    fn happy_hour_percent_on_drinks_validates() {
        let input = cart();
        let out = PosPriceRulesOutput {
            adjustments: vec![adj(AdjustmentTarget::Item, Some(0), AdjustmentKind::Percent, 20.0)],
        };
        let applied = validate_pos_price_rules(&out, &input).expect("20% off drinks is valid");
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].value, 20.0);
        assert_eq!(applied[0].rule_id, "happy_hour_drinks");
    }

    #[test]
    fn cart_percent_validates_and_cart_with_item_index_rejected() {
        let input = cart();
        let ok = PosPriceRulesOutput {
            adjustments: vec![adj(AdjustmentTarget::Cart, None, AdjustmentKind::Percent, 10.0)],
        };
        assert!(validate_pos_price_rules(&ok, &input).is_ok());

        let bad = PosPriceRulesOutput {
            adjustments: vec![adj(AdjustmentTarget::Cart, Some(0), AdjustmentKind::Percent, 10.0)],
        };
        assert!(matches!(
            validate_pos_price_rules(&bad, &input),
            Err(ProposalRejected::CartAdjustmentHasItemIndex { index: 0 })
        ));
    }

    #[test]
    fn negative_driving_absolute_rejected() {
        let input = cart();
        let out = PosPriceRulesOutput {
            // item 0 line_total is 10.0; subtract 9999 -> negative.
            adjustments: vec![adj(
                AdjustmentTarget::Item,
                Some(0),
                AdjustmentKind::Absolute,
                9999.0,
            )],
        };
        assert!(matches!(
            validate_pos_price_rules(&out, &input),
            Err(ProposalRejected::DrivesTotalNegative { index: 0, .. })
        ));
    }

    #[test]
    fn item_index_out_of_range_rejected() {
        let input = cart();
        let out = PosPriceRulesOutput {
            adjustments: vec![adj(AdjustmentTarget::Item, Some(99), AdjustmentKind::Percent, 10.0)],
        };
        assert!(matches!(
            validate_pos_price_rules(&out, &input),
            Err(ProposalRejected::ItemIndexOutOfRange { index: 0, got: Some(99), items: 2 })
        ));
    }

    #[test]
    fn percent_out_of_range_and_nan_rejected() {
        let input = cart();
        for bad in [0.0, 100.1, -5.0, f64::NAN] {
            let out = PosPriceRulesOutput {
                adjustments: vec![adj(AdjustmentTarget::Item, Some(0), AdjustmentKind::Percent, bad)],
            };
            assert!(
                matches!(
                    validate_pos_price_rules(&out, &input),
                    Err(ProposalRejected::PercentOutOfRange { .. })
                ),
                "percent {bad} should be rejected"
            );
        }
    }

    #[test]
    fn too_many_adjustments_rejected() {
        let input = cart();
        let out = PosPriceRulesOutput {
            adjustments: (0..MAX_PRICE_ADJUSTMENTS + 1)
                .map(|_| adj(AdjustmentTarget::Cart, None, AdjustmentKind::Percent, 1.0))
                .collect(),
        };
        assert!(matches!(
            validate_pos_price_rules(&out, &input),
            Err(ProposalRejected::TooManyAdjustments { .. })
        ));
    }

    #[test]
    fn non_printable_label_rejected() {
        let input = cart();
        let mut a = adj(AdjustmentTarget::Item, Some(0), AdjustmentKind::Percent, 10.0);
        a.label = "bad\u{7}bell".to_string();
        let out = PosPriceRulesOutput { adjustments: vec![a] };
        assert!(matches!(
            validate_pos_price_rules(&out, &input),
            Err(ProposalRejected::TextNotPrintable { field: "label", .. })
        ));
    }

    #[test]
    fn receipt_three_footer_lines_accepted_and_trimmed() {
        let out = PosReceiptRenderOutput {
            header_lines: vec![],
            footer_lines: vec![
                "  Thank you!  ".to_string(),
                "Visit again".to_string(),
                "www.example.de".to_string(),
            ],
        };
        let lines = validate_pos_receipt_render(&out).expect("3 footer lines are fine");
        assert_eq!(lines.footer_lines.len(), 3);
        assert_eq!(lines.footer_lines[0], "Thank you!"); // trimmed
    }

    #[test]
    fn receipt_esc_injection_rejected() {
        let out = PosReceiptRenderOutput {
            header_lines: vec![],
            footer_lines: vec!["\u{1b}@evil".to_string()],
        };
        assert!(matches!(
            validate_pos_receipt_render(&out),
            Err(ProposalRejected::ControlInjection { section: "footer", index: 0 })
        ));
    }

    #[test]
    fn receipt_gs_injection_rejected() {
        let out = PosReceiptRenderOutput {
            header_lines: vec!["\u{1d}V\u{0}".to_string()],
            footer_lines: vec![],
        };
        assert!(matches!(
            validate_pos_receipt_render(&out),
            Err(ProposalRejected::ControlInjection { section: "header", index: 0 })
        ));
    }

    #[test]
    fn receipt_line_too_long_rejected_after_trim() {
        let out = PosReceiptRenderOutput {
            header_lines: vec![],
            footer_lines: vec!["x".repeat(MAX_RECEIPT_LINE_CHARS + 1)],
        };
        assert!(matches!(
            validate_pos_receipt_render(&out),
            Err(ProposalRejected::ReceiptLineTooLong { section: "footer", index: 0, .. })
        ));
    }

    #[test]
    fn receipt_too_many_lines_rejected() {
        let out = PosReceiptRenderOutput {
            header_lines: (0..MAX_HEADER_LINES + 1).map(|_| "hi".to_string()).collect(),
            footer_lines: vec![],
        };
        assert!(matches!(
            validate_pos_receipt_render(&out),
            Err(ProposalRejected::TooManyLines { section: "header", .. })
        ));
    }

    #[test]
    fn receipt_control_chars_stripped_not_rejected() {
        let out = PosReceiptRenderOutput {
            header_lines: vec![],
            footer_lines: vec!["a\tb\u{0}c".to_string()], // tab + NUL stripped, not ESC/GS
        };
        let lines = validate_pos_receipt_render(&out).expect("non-ESC/GS controls are stripped");
        assert_eq!(lines.footer_lines[0], "abc");
    }

    #[test]
    fn finalize_price_rules_maps_timeout_to_skip() {
        let input = cart();
        let outcome = finalize_pos_price_rules(Err(PluginError::Timeout(50)), &input);
        assert!(matches!(outcome, PosPriceRulesOutcome::Skipped(PosHookSkip::Timeout)));
    }

    #[test]
    fn finalize_price_rules_maps_malformed_output_to_skip() {
        let input = cart();
        let outcome = finalize_pos_price_rules(Ok(b"not json".to_vec()), &input);
        assert!(matches!(outcome, PosPriceRulesOutcome::Skipped(PosHookSkip::Malformed(_))));
    }

    #[test]
    fn finalize_price_rules_valid_output_applied() {
        let input = cart();
        let bytes = serde_json::to_vec(&json!({
            "adjustments": [{
                "target": "item", "item_index": 0, "kind": "percent",
                "value": 20.0, "label": "Happy Hour", "rule_id": "hh"
            }]
        }))
        .unwrap();
        match finalize_pos_price_rules(Ok(bytes), &input) {
            PosPriceRulesOutcome::Applied(a) => assert_eq!(a.len(), 1),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn finalize_receipt_render_rejects_esc_as_skip() {
        let bytes = serde_json::to_vec(&json!({
            "header_lines": [], "footer_lines": ["\u{1b}@"]
        }))
        .unwrap();
        assert!(matches!(
            finalize_pos_receipt_render(Ok(bytes)),
            PosReceiptRenderOutcome::Skipped(PosHookSkip::Rejected(_))
        ));
    }
}
