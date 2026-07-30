pub mod address_discovery;
pub mod attachments;
pub mod avif;
/// Tenant branding splice for AI prompts (env `ECK_TENANT_BRAND` /
/// `ECK_TENANT_VERTICAL`; neutral when unset).
pub mod branding;
pub mod doc_classify;
pub mod embeddings;
/// On-device embedding backend — only compiled when the `local-embed` feature
/// is enabled (see wms/Cargo.toml). Runtime-selected via `ECK_EMBED_MODE=local`.
#[cfg(feature = "local-embed")]
pub mod local_embed;
/// On-device German NER for person-name masking — shares the `local-embed`
/// feature (same Candle/tokenizers/hf-hub stack). Runtime-selected via
/// `ECK_PII_NER=local`; replaces the per-document Gemini PII extraction.
#[cfg(feature = "local-embed")]
pub mod local_ner;
pub mod pii_policy;
pub mod summarization;
pub mod summarization_batch;
pub mod image_optimizer;
pub mod loop_guard;
pub mod telemetry;
pub mod observer;
pub mod orchestrator;
pub mod optimizer;
pub mod repair_distiller;
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
