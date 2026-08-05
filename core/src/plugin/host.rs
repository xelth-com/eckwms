//! Host functions exposed to plugins (design §4), linked per capability claim,
//! plus the pre-instantiation capability check.
//!
//! The v1 surface is deliberately tiny and has NO I/O:
//! - `host_log(level: i64, msg_ptr: i64)`      — cap "log", rate-limited tracing.
//! - `host_anonymize(text_ptr: i64) -> i64`    — cap "anonymize", PII masking.
//!
//! These live in the wasm import namespace [`HOST_NAMESPACE`]. A plugin that
//! imports one it was NOT granted the capability for is rejected by
//! [`check_capabilities`] *before* instantiation, as
//! [`PluginError::CapabilityDenied`] — not as an opaque link failure.
//!
//! Extism host functions speak the raw offset ABI: a `&str` argument arrives as
//! an `i64` offset into the plugin's kernel memory, and a returned string is an
//! `i64` offset to a freshly-allocated block. We plumb that with the
//! `CurrentPlugin` memory helpers. A host function NEVER panics and NEVER
//! unwinds into wasmtime.

use std::collections::BTreeSet;

use extism::{Error, Function, UserData, ValType};
use tracing::{info, warn};
use wasmparser::{Parser, Payload};

use super::{HostIdentity, PluginCallContext, PluginError};
use crate::utils::anonymizer;

/// Import module the v1 host functions live in (extism's default user namespace).
pub const HOST_NAMESPACE: &str = "extism:host/user";

/// Capability tokens (as they appear in a plugin's `capabilities` claim list).
pub const CAP_LOG: &str = "log";
pub const CAP_ANONYMIZE: &str = "anonymize";

const HOST_LOG: &str = "host_log";
const HOST_ANONYMIZE: &str = "host_anonymize";

/// Max `host_log` calls honoured per invocation; the rest are dropped after one
/// warning line (design §4: rate-limited).
const MAX_LOG_CALLS: u32 = 100;
/// Max bytes of a single log message that reach the tracing backend.
const MAX_LOG_MSG_BYTES: usize = 1024;

/// The capability a given host-function import requires, or `None` if the name
/// is not a host function we provide (an unknown host import → denied).
pub fn required_capability(import_name: &str) -> Option<&'static str> {
    match import_name {
        HOST_LOG => Some(CAP_LOG),
        HOST_ANONYMIZE => Some(CAP_ANONYMIZE),
        _ => None,
    }
}

/// Reject a module that imports a host function it wasn't granted (or an unknown
/// host symbol) BEFORE we spend anything instantiating it. Kernel imports
/// (`extism:host/env`) and any other namespace are ignored here — an unsatisfied
/// non-host import fails later at link time as [`PluginError::InvalidModule`].
pub fn check_capabilities(wasm: &[u8], granted: &BTreeSet<String>) -> Result<(), PluginError> {
    for payload in Parser::new(0).parse_all(wasm) {
        let payload =
            payload.map_err(|e| PluginError::InvalidModule(format!("wasm parse error: {e}")))?;
        if let Payload::ImportSection(reader) = payload {
            for import in reader.into_imports() {
                let import = import
                    .map_err(|e| PluginError::InvalidModule(format!("wasm import parse error: {e}")))?;
                if import.module != HOST_NAMESPACE {
                    continue;
                }
                let granted_ok = required_capability(import.name)
                    .is_some_and(|cap| granted.contains(cap));
                if !granted_ok {
                    return Err(PluginError::CapabilityDenied {
                        import: format!("{}::{}", import.module, import.name),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Build the host functions to link, given the plugin's granted capabilities and
/// the node's baked identity. A capability that isn't granted contributes no
/// symbol — a plugin without "anonymize" simply cannot import `host_anonymize`
/// (and [`check_capabilities`] already rejected it if it tried).
pub fn host_functions(granted: &BTreeSet<String>, identity: &HostIdentity) -> Vec<Function> {
    let mut fns = Vec::new();
    if granted.contains(CAP_LOG) {
        fns.push(make_host_log(identity.clone()));
    }
    if granted.contains(CAP_ANONYMIZE) {
        fns.push(make_host_anonymize(identity.clone()));
    }
    fns
}

/// `host_log(level: i64, msg_ptr: i64)` — best-effort tracing. Logging must
/// never break a plugin call, so this always returns `Ok`, even if it can't
/// read the message or the per-call guard drops it.
fn make_host_log(identity: HostIdentity) -> Function {
    Function::new(
        HOST_LOG,
        [ValType::I64, ValType::I64],
        [],
        UserData::new(identity),
        |plugin, inputs, _outputs, user_data| {
            let level = inputs.first().and_then(|v| v.i64()).unwrap_or(0);
            let msg = match inputs.get(1) {
                Some(v) => match plugin.memory_from_val(v) {
                    Some(handle) => plugin
                        .memory_str(handle)
                        .map(str::to_owned)
                        .unwrap_or_default(),
                    None => String::new(),
                },
                None => String::new(),
            };

            // Per-invocation rate guard, held on the per-call context.
            let (emit, warn_once) = match plugin.host_context::<PluginCallContext>() {
                Ok(ctx) => admit_log(ctx),
                Err(_) => (true, false),
            };
            if warn_once {
                warn!(
                    target: "plugin::host_log",
                    "plugin exceeded {MAX_LOG_CALLS} host_log calls this invocation; further messages dropped"
                );
            }
            if emit {
                let truncated = truncate_to(&msg, MAX_LOG_MSG_BYTES);
                let (node, role) = match user_data.get() {
                    Ok(d) => {
                        let g = d.lock().unwrap_or_else(|p| p.into_inner());
                        (g.instance_id.clone(), g.node_role.clone())
                    }
                    Err(_) => (String::new(), String::new()),
                };
                info!(target: "plugin", node = %node, role = %role, level, "{truncated}");
            }
            Ok(())
        },
    )
}

/// `host_anonymize(text_ptr: i64) -> i64` — mask PII in `text` and return an
/// offset to the masked string. Fails CLOSED: if the masker is unavailable it
/// returns an error rather than ever handing back unmasked text.
fn make_host_anonymize(identity: HostIdentity) -> Function {
    Function::new(
        HOST_ANONYMIZE,
        [ValType::I64],
        [ValType::I64],
        UserData::new(identity),
        |plugin, inputs, outputs, _user_data| {
            let text = match inputs.first() {
                Some(v) => match plugin.memory_from_val(v) {
                    Some(handle) => plugin
                        .memory_str(handle)
                        .map(str::to_owned)
                        .unwrap_or_default(),
                    None => String::new(),
                },
                None => String::new(),
            };

            // Panic firewall: the PPRL masker `.expect`s on SYNC_SECRET. Never
            // let that unwind into wasmtime, and NEVER fail open by returning the
            // raw text — a masking failure must abort the call.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| anonymize_text(&text))) {
                Ok(masked) => {
                    let handle = plugin.memory_new(masked)?;
                    let val = plugin.memory_to_val(handle);
                    if let Some(out) = outputs.get_mut(0) {
                        *out = val;
                    }
                    Ok(())
                }
                Err(_) => Err(Error::msg("host_anonymize unavailable (masker not configured)")),
            }
        },
    )
}

/// Deterministic, no-egress PII masking — the same functions the production
/// maskers layer: the person-name heuristic first, then the regex PII backstop.
fn anonymize_text(text: &str) -> String {
    let (names_masked, _) = anonymizer::mask_person_names_heuristic(text);
    let (scrubbed, _) = anonymizer::scrub_pii_regex(&names_masked);
    scrubbed
}

/// Advance the per-call log guard. Returns `(emit_this, warn_once)`:
/// the first [`MAX_LOG_CALLS`] calls emit; the next one emits a single warning
/// and is dropped; all later calls are dropped silently.
fn admit_log(ctx: &mut PluginCallContext) -> (bool, bool) {
    if ctx.log_calls < MAX_LOG_CALLS {
        ctx.log_calls += 1;
        (true, false)
    } else if !ctx.log_warned {
        ctx.log_warned = true;
        (false, true)
    } else {
        (false, false)
    }
}

/// Truncate `s` to at most `max` bytes, never splitting a UTF-8 char.
fn truncate_to(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_guard_emits_exactly_the_budget_then_warns_once() {
        let mut ctx = PluginCallContext::new("h", "cid", None);
        let mut emitted = 0u32;
        let mut warnings = 0u32;
        for _ in 0..MAX_LOG_CALLS + 50 {
            let (emit, warn) = admit_log(&mut ctx);
            if emit {
                emitted += 1;
            }
            if warn {
                warnings += 1;
            }
        }
        assert_eq!(emitted, MAX_LOG_CALLS, "must emit exactly the budget");
        assert_eq!(warnings, 1, "must warn exactly once at the boundary");
    }

    #[test]
    fn truncate_respects_byte_cap_and_char_boundaries() {
        // ASCII well over the cap → cut to exactly the cap.
        let big = "x".repeat(MAX_LOG_MSG_BYTES + 500);
        assert_eq!(truncate_to(&big, MAX_LOG_MSG_BYTES).len(), MAX_LOG_MSG_BYTES);
        // Multibyte: cut must land on a char boundary (<= cap), never mid-char.
        let multi = "ä".repeat(MAX_LOG_MSG_BYTES); // 2 bytes each
        let cut = truncate_to(&multi, MAX_LOG_MSG_BYTES + 1);
        assert!(cut.len() <= MAX_LOG_MSG_BYTES + 1);
        assert!(multi.starts_with(cut) && cut.chars().all(|c| c == 'ä'));
        // Under the cap → unchanged.
        assert_eq!(truncate_to("short", MAX_LOG_MSG_BYTES), "short");
    }

    #[test]
    fn required_capability_maps_known_host_fns_only() {
        assert_eq!(required_capability("host_log"), Some(CAP_LOG));
        assert_eq!(required_capability("host_anonymize"), Some(CAP_ANONYMIZE));
        assert_eq!(required_capability("host_something_else"), None);
    }

    #[test]
    fn anonymize_text_masks_person_and_structured_pii() {
        std::env::set_var("SYNC_SECRET", "test_secret");
        let out = anonymize_text("Katharina Niedermayer schreibt an a.b@example.de");
        assert!(!out.contains("Katharina"), "person name leaked: {out}");
        assert!(!out.contains("a.b@example.de"), "email leaked: {out}");
        assert!(out.contains("Name_"), "no Name token: {out}");
        assert!(out.contains("Email_"), "no Email token: {out}");
    }
}
