pub mod address_discovery;
pub mod attachment_image;
pub mod attachments;
pub mod avif;
/// Tenant branding splice for AI prompts (env `ECK_TENANT_BRAND` /
/// `ECK_TENANT_VERTICAL`; neutral when unset).
pub mod branding;
/// Node-local Brain-Chat transcript telemetry (chat_session / chat_turn) +
/// admin/MCP export. NOT mesh-synced — see the module docs.
pub mod chat_log;
pub mod doc_classify;
pub mod embeddings;
/// On-device embedding backend — only compiled when the `local-embed` feature
/// is enabled (see wms/Cargo.toml). Runtime-selected via `ECK_EMBED_MODE=local`.
#[cfg(feature = "local-embed")]
pub mod local_embed;
/// Lazy-load + idle-unload holder shared by `local_ner`/`local_embed` so the
/// heavy Candle weights are dropped after `ECK_*_IDLE_UNLOAD_SECS` of no use.
#[cfg(feature = "local-embed")]
pub mod idle_model;
/// On-device German NER for person-name masking — shares the `local-embed`
/// feature (same Candle/tokenizers/hf-hub stack). Runtime-selected via
/// `ECK_PII_NER=local`; replaces the per-document Gemini PII extraction.
#[cfg(feature = "local-embed")]
pub mod local_ner;
/// Ungated PII-NER routing facade (`local` | `remote` | `gemini`) — callers
/// branch on `pii_mode::pii_ner_mode()` instead of the feature-gated
/// `local_ner::ner_mode()`. See `.eck/PII_REMOTE_MASKING.md`.
pub mod pii_mode;
/// Remote "eck masker" span-service client (`ECK_PII_NER=remote`), ungated so a
/// cloud-only build can be a remote-masking client.
pub mod pii_remote;
pub mod pii_policy;
/// In-memory typo-tolerant name/address resolution over `Name_`/`Address_`
/// tokens (`.eck/PPRL_FUZZY.md` §C/§F). Held in `AppState`.
pub mod pii_fuzzy;
pub mod summarization;
pub mod summarization_batch;
pub mod image_optimizer;
pub mod image_summary;
/// xelixir Knowledge Base MCP client (`kb_brief` recall front door +
/// `kb_record` write path). Shared by the triage/one-shot brain and the
/// repair-lesson distiller.
pub mod kb_client;
pub mod loop_guard;
pub mod telemetry;
pub mod observer;
pub mod orchestrator;
pub mod optimizer;
pub mod repair_distiller;
pub mod stale_reply;
pub mod tools;
pub mod translation;

/// Largest byte index `<= max_bytes` that is a UTF-8 char boundary of `s`.
/// Slicing at a raw byte offset (`&s[..N]`) panics when N lands inside a
/// multi-byte char (e.g. German umlauts) — every AI text truncation must go
/// through this instead.
pub fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::floor_char_boundary;

    #[test]
    fn floor_char_boundary_steps_back_out_of_multibyte_char() {
        // Reproduces the summarization panic: byte 28000 inside 'ö' (27999..28001).
        let s = format!("{}ö tail", "a".repeat(27999));
        let end = floor_char_boundary(&s, 28000);
        assert_eq!(end, 27999);
        let _ = &s[..end]; // must not panic
    }

    #[test]
    fn floor_char_boundary_short_and_exact_strings_pass_through() {
        assert_eq!(floor_char_boundary("short", 28000), 5);
        let s = "aö"; // 3 bytes total
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 2), 1);
    }
}
