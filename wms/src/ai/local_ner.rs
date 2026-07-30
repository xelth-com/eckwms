//! On-device German NER for person-name masking (feature `local-embed`).
//!
//! Replaces the per-document Gemini `extract_and_anonymize` call with a
//! candle-native token-classification pass over `mschiesser/ner-bert-german`
//! (base `bert-base-multilingual-cased`, classes PER/LOC/ORG). We only harvest
//! **PER** entities — LOC/ORG stay in the clear because LOC captures cities that
//! must survive for the embedding's coarse-geo signal; street addresses are
//! masked deterministically by `anonymizer::scrub_pii_regex` instead.
//!
//! candle-transformers ships `bert::BertModel` (encoder) but no
//! token-classification head, so we bolt on a `Linear` classifier loaded from
//! the safetensors' `classifier.{weight,bias}` and decode the BIO tags
//! (id2label from config.json) with WordPiece (`##`) subtoken merging — the same
//! shape as candle's debertav2 token-classification example.
//!
//! Selected at runtime by `ECK_PII_NER=local`. Model files are read from
//! `ECK_NER_MODEL_DIR` when that env var is set (offline / LAN-only nodes get
//! `tokenizer.json`/`config.json`/`model.safetensors` by scp), otherwise they
//! are downloaded and cached via hf-hub on first use.

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor, D};
use candle_nn::{linear, Linear, Module, VarBuilder};
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use std::sync::Mutex;
use tokio::sync::OnceCell;
use tracing::{info, warn};

/// Canonical model id (used only for the one-time init log). Kept short.
pub const MODEL_NAME: &str = "ner-bert-german";
/// HuggingFace repo id used for the hf-hub download path.
const HF_REPO: &str = "mschiesser/ner-bert-german";
/// BERT hard limit is 512 positions. We window the token stream at 450 tokens
/// (+CLS +SEP = 452 ≤ 512) with a 50-token overlap so an entity straddling a
/// window boundary is still seen whole in at least one window.
const WINDOW_TOKENS: usize = 450;
const OVERLAP_TOKENS: usize = 50;

/// Runtime PII-NER backend selector. `ECK_PII_NER=local` → on-device German NER
/// (requires the `local-embed` feature, which gates this whole module);
/// anything else, or an unset var, → the cloud (Gemini) extraction path. Parsed
/// once and cached (the value can't change mid-process), which also makes the
/// "asked for local but built cloud-only" warning fire exactly once. Mirrors
/// `embeddings::embed_mode`.
pub fn ner_mode() -> &'static str {
    use std::sync::OnceLock;
    static MODE: OnceLock<&'static str> = OnceLock::new();
    *MODE.get_or_init(|| {
        let raw = std::env::var("ECK_PII_NER").ok();
        let mode = resolve_ner_mode(raw.as_deref());
        let asked_local = raw
            .as_deref()
            .map(|v| v.trim().eq_ignore_ascii_case("local"))
            .unwrap_or(false);
        if asked_local && mode == "gemini" {
            warn!(
                "[NER] ECK_PII_NER=local requested but this binary was built \
                 without the `local-embed` feature — using Gemini PII extraction"
            );
        }
        mode
    })
}

/// Pure mode resolver: maps the raw `ECK_PII_NER` value to `"local"`/`"gemini"`
/// with the feature gate applied, and no process-global state. Split out from
/// `ner_mode` so it can be unit-tested without racing other tests on the env.
/// Because the whole module is `#[cfg(feature = "local-embed")]`, the
/// `not(feature)` arm only ever compiles when someone lifts this fn out of the
/// gated module; it is kept for symmetry with `resolve_embed_mode`.
fn resolve_ner_mode(env_val: Option<&str>) -> &'static str {
    let want_local = env_val
        .map(|v| v.trim().eq_ignore_ascii_case("local"))
        .unwrap_or(false);
    if !want_local {
        return "gemini";
    }
    #[cfg(feature = "local-embed")]
    {
        "local"
    }
    #[cfg(not(feature = "local-embed"))]
    {
        "gemini"
    }
}

/// Encoder + classifier head, driven behind one `Mutex` (candle inference is
/// CPU-bound and serialized; a single embed/NER worker drives this in
/// production so contention is a non-issue).
struct NerInner {
    model: BertModel,
    classifier: Linear,
}

/// Process-wide singleton. `tokenizers::Tokenizer` is `!Sync` so it gets its own
/// `Mutex`; the model+head share a second one.
pub struct LocalNer {
    tokenizer: Mutex<tokenizers::Tokenizer>,
    inner: Mutex<NerInner>,
    /// Label per class id, e.g. `["O","B-PER","I-PER","B-ORG",...]`.
    id2label: Vec<String>,
    cls_id: u32,
    sep_id: u32,
}

static NER: OnceCell<LocalNer> = OnceCell::const_new();

impl LocalNer {
    /// Blocking load: reads tokenizer/config/weights from `ECK_NER_MODEL_DIR`
    /// when set, else downloads+caches them from the HuggingFace Hub. Runs inside
    /// `spawn_blocking` because both hf-hub and the safetensors mmap are sync.
    fn load_blocking() -> Result<Self> {
        let started = std::time::Instant::now();

        let (tokenizer_path, config_path, weights_path, source) =
            match std::env::var("ECK_NER_MODEL_DIR") {
                Ok(dir) if !dir.trim().is_empty() => {
                    let dir = std::path::PathBuf::from(dir.trim());
                    let source = format!("dir:{}", dir.display());
                    (
                        dir.join("tokenizer.json"),
                        dir.join("config.json"),
                        dir.join("model.safetensors"),
                        source,
                    )
                }
                _ => {
                    use hf_hub::{api::sync::Api, Repo, RepoType};
                    let repo = Api::new()
                        .context("hf-hub Api init failed")?
                        .repo(Repo::new(HF_REPO.to_string(), RepoType::Model));
                    let tokenizer = repo
                        .get("tokenizer.json")
                        .context("download tokenizer.json")?;
                    let config = repo.get("config.json").context("download config.json")?;
                    let weights = repo
                        .get("model.safetensors")
                        .context("download model.safetensors")?;
                    (tokenizer, config, weights, format!("hub:{HF_REPO}"))
                }
            };

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to load tokenizer from {}: {e}",
                tokenizer_path.display()
            )
        })?;
        let cls_id = tokenizer
            .token_to_id("[CLS]")
            .context("tokenizer has no [CLS] token")?;
        let sep_id = tokenizer
            .token_to_id("[SEP]")
            .context("tokenizer has no [SEP] token")?;

        let config_str = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read config from {}", config_path.display()))?;
        // The candle bert `Config` ignores unknown keys (no deny_unknown_fields),
        // so the token-classification config parses straight into it.
        let bert_config: BertConfig =
            serde_json::from_str(&config_str).context("parse bert config.json")?;
        let cfg_json: serde_json::Value =
            serde_json::from_str(&config_str).context("parse config.json as value")?;
        let id2label = parse_id2label(&cfg_json)?;
        let num_labels = id2label.len();
        anyhow::ensure!(num_labels >= 2, "config.json id2label has <2 classes");
        let hidden_size = bert_config.hidden_size;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path.clone()], DType::F32, &device)
                .with_context(|| format!("mmap safetensors from {}", weights_path.display()))?
        };
        // Weights live under `bert.*`; passing the `bert` sub-builder makes
        // `BertModel::load` find `bert.embeddings`/`bert.encoder` on the first
        // try. The classification head sits at the root as `classifier.*`.
        let model = BertModel::load(vb.pp("bert"), &bert_config).context("build bert encoder")?;
        let classifier = linear(hidden_size, num_labels, vb.pp("classifier"))
            .context("load classifier head (classifier.weight/bias)")?;

        info!(
            model = MODEL_NAME,
            source = %source,
            num_labels,
            load_ms = started.elapsed().as_millis() as u64,
            "[NER] local NER ready"
        );

        Ok(Self {
            tokenizer: Mutex::new(tokenizer),
            inner: Mutex::new(NerInner { model, classifier }),
            id2label,
            cls_id,
            sep_id,
        })
    }

    /// Tokenize → (windowed) BERT forward → linear classifier → argmax → BIO
    /// decode into `(PER, ORG)` entity lists. LOC stays ignored (cities must
    /// survive for the coarse-geo signal). Blocking (CPU-bound); callers wrap
    /// it in `spawn_blocking`.
    fn extract_blocking(&self, text: &str) -> Result<(Vec<String>, Vec<String>)> {
        if text.trim().is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        // Encode WITHOUT special tokens — we add [CLS]/[SEP] per window so a
        // long input can be split across several ≤512-token forward passes.
        let encoding = {
            let tok = self.tokenizer.lock().unwrap();
            tok.encode(text, false)
                .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?
        };
        let ids: Vec<u32> = encoding.get_ids().to_vec();
        let tokens: Vec<String> = encoding.get_tokens().to_vec();
        if ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let device = Device::Cpu;
        let n = ids.len();
        let mut merged_per: Vec<String> = Vec::new();
        let mut merged_org: Vec<String> = Vec::new();

        let mut start = 0usize;
        loop {
            let end = (start + WINDOW_TOKENS).min(n);

            let mut window_ids = Vec::with_capacity(end - start + 2);
            window_ids.push(self.cls_id);
            window_ids.extend_from_slice(&ids[start..end]);
            window_ids.push(self.sep_id);

            let input = Tensor::new(window_ids.as_slice(), &device)?.unsqueeze(0)?; // (1, w+2)
            let token_type_ids = input.zeros_like()?;

            let label_ids: Vec<u32> = {
                let inner = self.inner.lock().unwrap();
                let seq = inner.model.forward(&input, &token_type_ids, None)?; // (1, w+2, hidden)
                let logits = inner.classifier.forward(&seq)?; // (1, w+2, num_labels)
                logits.squeeze(0)?.argmax(D::Minus1)?.to_vec1::<u32>()?
            };

            // window_ids[0] is CLS and window_ids[last] is SEP; positions
            // 1..=(end-start) line up with tokens[start..end].
            let win_tokens: Vec<&str> = tokens[start..end].iter().map(|s| s.as_str()).collect();
            let win_labels: Vec<&str> = (1..=(end - start))
                .map(|i| {
                    self.id2label
                        .get(label_ids[i] as usize)
                        .map(|s| s.as_str())
                        .unwrap_or("O")
                })
                .collect();
            merged_per.extend(merge_bio_entities(&win_tokens, &win_labels, "PER"));
            merged_org.extend(merge_bio_entities(&win_tokens, &win_labels, "ORG"));

            if end == n {
                break;
            }
            start += WINDOW_TOKENS - OVERLAP_TOKENS;
        }

        // Normalize, drop too-short / pure-punctuation noise, dedup (order-stable).
        // PER additionally sheds glued honorifics so the PPRL token is computed
        // over the bare name; ORG spans are kept verbatim (they are used as
        // protected regions, not hashed).
        let clean = |merged: Vec<String>, strip: bool| -> Vec<String> {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for e in merged {
                let e = if strip {
                    strip_honorifics(e.trim())
                } else {
                    e.trim().split_whitespace().collect::<Vec<_>>().join(" ")
                };
                if e.chars().count() < 2 {
                    continue;
                }
                if e.chars().all(|c| !c.is_alphanumeric()) {
                    continue; // pure punctuation
                }
                if seen.insert(e.to_string()) {
                    out.push(e.to_string());
                }
            }
            out
        };
        Ok((clean(merged_per, true), clean(merged_org, false)))
    }
}

/// Strip leading German honorifics/titles the base model glues into PER spans
/// ("Herrn Müller", "Herr Dr . Klaus Hoffmann"). Two reasons: (1) the PPRL
/// token must be computed over the BARE name so the same person collapses to
/// the same token as the Gemini extractor and the summary participant
/// dictionary produce; (2) a title-only capture ("Dr") must reduce to empty
/// and be dropped by the length filter, not become a bogus Name token.
/// The eval showed the model emits "Dr ." with a detached dot — strip those too.
fn strip_honorifics(mut s: &str) -> String {
    const TITLES: &[&str] = &[
        "herr", "herrn", "frau", "fräulein", "dr", "prof", "dipl", "ing", "mag",
    ];
    loop {
        let trimmed = s.trim_start_matches(|c: char| c == '.' || c.is_whitespace());
        let Some(word) = trimmed.split_whitespace().next() else {
            return String::new();
        };
        let bare = word.trim_end_matches('.');
        if TITLES.contains(&bare.to_lowercase().as_str()) {
            s = &trimmed[word.len()..];
        } else {
            // Collapse any leftover whitespace runs (the detached-dot stripping
            // can leave doubles) into single spaces.
            return trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
}

/// Build `id -> label` from the config's `id2label` object (keys are stringified
/// ids). Missing slots default to `"O"`.
fn parse_id2label(cfg: &serde_json::Value) -> Result<Vec<String>> {
    let map = cfg
        .get("id2label")
        .and_then(|v| v.as_object())
        .context("config.json missing id2label object")?;
    let mut pairs: Vec<(usize, String)> = Vec::with_capacity(map.len());
    for (k, v) in map {
        let idx: usize = k
            .parse()
            .with_context(|| format!("non-numeric id2label key {k:?}"))?;
        pairs.push((idx, v.as_str().unwrap_or("O").to_string()));
    }
    let len = pairs.iter().map(|(i, _)| i + 1).max().unwrap_or(0);
    let mut out = vec!["O".to_string(); len];
    for (i, label) in pairs {
        if i < out.len() {
            out[i] = label;
        }
    }
    Ok(out)
}

/// Backward-compatible PER-only wrapper (unit tests + older callers).
#[cfg(test)]
fn merge_bio_person_entities(tokens: &[&str], labels: &[&str]) -> Vec<String> {
    merge_bio_entities(tokens, labels, "PER")
}

/// Pure BIO decoder — factored out of the model path so it is unit-testable on
/// synthetic label sequences. Consumes parallel `tokens`/`labels` slices (one
/// entry per WordPiece token, specials already stripped) and returns the
/// whitespace-normalized text of every entity of `class` ("PER"/"ORG"/"LOC").
///
/// Rules: a token starting with `##` is a WordPiece continuation and is merged
/// into the current word with no space; the label of a word's FIRST subtoken
/// decides the word (standard HF convention). `B-<class>` opens a new entity;
/// `I-<class>` extends the open entity (or opens one if none is active); any
/// other label closes the current entity.
fn merge_bio_entities(tokens: &[&str], labels: &[&str], class: &str) -> Vec<String> {
    // Joiner punctuation that binds name parts with no surrounding space, so a
    // WordPiece-split "Weber - Schmidt" is rebuilt as "Weber-Schmidt" (otherwise
    // the spaced form fails to match the source text at masking time). Other
    // punctuation keeps normal spacing.
    let is_joiner = |t: &str| matches!(t, "-" | "'" | "’" | "–" | "/");

    let mut out: Vec<String> = Vec::new();
    let mut cur: Option<String> = None;
    let mut prev_joiner = false;

    let b_label = format!("B-{class}");
    let i_label = format!("I-{class}");

    for (tok, label) in tokens.iter().zip(labels.iter()) {
        if let Some(piece) = tok.strip_prefix("##") {
            // Continuation subtoken — glue onto the open entity's last word, if
            // any. A continuation of a non-entity word is simply ignored.
            if let Some(buf) = cur.as_mut() {
                buf.push_str(piece);
                prev_joiner = false;
            }
            continue;
        }

        let is_class = *label == b_label || *label == i_label;
        let is_begin = label.starts_with("B-");
        if is_class {
            match cur.as_mut() {
                // I-PER extending the current PER entity → a new word. Add a
                // space unless this token (or the previous one) is a joiner.
                Some(buf) if !is_begin => {
                    let joiner = is_joiner(tok);
                    if !(joiner || prev_joiner) {
                        buf.push(' ');
                    }
                    buf.push_str(tok);
                    prev_joiner = joiner;
                }
                // B-PER, or an orphan I-PER with no open entity → start fresh.
                _ => {
                    if let Some(done) = cur.take() {
                        out.push(done);
                    }
                    cur = Some((*tok).to_string());
                    prev_joiner = is_joiner(tok);
                }
            }
        } else if let Some(done) = cur.take() {
            out.push(done);
            prev_joiner = false;
        }
    }
    if let Some(done) = cur.take() {
        out.push(done);
    }

    out.into_iter()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Lazily initialize (and, on first use, download) the process-wide NER model.
/// `NER` is a `static`, so the returned reference is `'static`.
async fn ner() -> Result<&'static LocalNer> {
    NER.get_or_try_init(|| async {
        tokio::task::spawn_blocking(LocalNer::load_blocking)
            .await
            .context("local NER load task panicked")?
    })
    .await
}

/// Extract the deduplicated set of PER (person-name) entities from `text` using
/// the on-device German NER model. LOC/ORG are deliberately ignored. The model
/// is loaded (and downloaded if absent) on the first call; both the load and the
/// CPU-bound forward pass run on the blocking pool so they never stall an async
/// worker thread.
pub async fn extract_person_names(text: &str) -> Result<Vec<String>> {
    Ok(extract_entities(text).await?.0)
}

/// Extract `(PER, ORG)` entities from `text`. ORG spans are what lets the
/// subject masker tell a person from a person-shaped BRAND ("Emil Krause
/// Fitness") — orgs are PROTECTED from masking, persons are tokenized. LOC is
/// still ignored (cities stay clear). Same lazy model load as
/// [`extract_person_names`].
pub async fn extract_entities(text: &str) -> Result<(Vec<String>, Vec<String>)> {
    let ner = ner().await?;
    let owned = text.to_string();
    tokio::task::spawn_blocking(move || ner.extract_blocking(&owned))
        .await
        .context("local NER task panicked")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_ner_mode_parses_env_without_global_state() {
        // Pure parser — no env mutation, so it can't race other tests. With the
        // default feature set `local-embed` is compiled in, so "local" resolves
        // to local; a cloud-only build resolves everything to gemini.
        let local_expected = if cfg!(feature = "local-embed") {
            "local"
        } else {
            "gemini"
        };
        assert_eq!(resolve_ner_mode(None), "gemini");
        assert_eq!(resolve_ner_mode(Some("")), "gemini");
        assert_eq!(resolve_ner_mode(Some("gemini")), "gemini");
        assert_eq!(resolve_ner_mode(Some("cloud")), "gemini");
        assert_eq!(resolve_ner_mode(Some("local")), local_expected);
        assert_eq!(resolve_ner_mode(Some("  LOCAL  ")), local_expected);
    }

    #[test]
    fn bio_merge_single_and_multi_word_persons() {
        // "Hans" B-PER, then "Mü"/"##ller" one word tagged I-PER on the head.
        let tokens = ["Hans", "Mü", "##ller", "meldet", "einen", "Defekt"];
        let labels = ["B-PER", "I-PER", "I-PER", "O", "O", "O"];
        let ents = merge_bio_person_entities(&tokens, &labels);
        assert_eq!(ents, vec!["Hans Müller".to_string()]);
    }

    #[test]
    fn bio_merge_ignores_loc_and_org() {
        // LOC (Berlin) and ORG (ZetaBody) must never surface as person entities.
        let tokens = ["Berlin", "ZetaBody", "GmbH", "Julia", "Weber"];
        let labels = ["B-LOC", "B-ORG", "I-ORG", "B-PER", "I-PER"];
        let ents = merge_bio_person_entities(&tokens, &labels);
        assert_eq!(ents, vec!["Julia Weber".to_string()]);
    }

    #[test]
    fn bio_merge_splits_adjacent_entities_on_new_begin() {
        // Two people back-to-back separated by a B- tag, plus a subtoken-only
        // continuation of a non-entity word (must be dropped).
        let tokens = ["Anna", "Bea", "wo", "##hnt", "hier"];
        let labels = ["B-PER", "B-PER", "O", "O", "O"];
        let ents = merge_bio_person_entities(&tokens, &labels);
        assert_eq!(ents, vec!["Anna".to_string(), "Bea".to_string()]);
    }

    #[test]
    fn bio_merge_orphan_inside_is_treated_as_begin() {
        let tokens = ["Kwon"];
        let labels = ["I-PER"];
        let ents = merge_bio_person_entities(&tokens, &labels);
        assert_eq!(ents, vec!["Kwon".to_string()]);
    }

    #[test]
    fn bio_merge_glues_hyphenated_surname() {
        // WordPiece splits the hyphen into its own token; the rebuilt name must
        // have NO spaces around it so it still matches "Julia Weber-Schmidt" in
        // the source text at masking time.
        let tokens = ["Julia", "Weber", "-", "Schmidt"];
        let labels = ["B-PER", "I-PER", "I-PER", "I-PER"];
        let ents = merge_bio_person_entities(&tokens, &labels);
        assert_eq!(ents, vec!["Julia Weber-Schmidt".to_string()]);
    }

    #[test]
    fn bio_merge_extracts_org_entities() {
        // The same label stream yields orgs for class=ORG and persons for
        // class=PER — the subject masker consumes both sides.
        let tokens = ["Emil", "Krause", "Fitness", "und", "Julia", "Weber"];
        let labels = ["B-ORG", "I-ORG", "I-ORG", "O", "B-PER", "I-PER"];
        assert_eq!(
            merge_bio_entities(&tokens, &labels, "ORG"),
            vec!["Emil Krause Fitness".to_string()]
        );
        assert_eq!(
            merge_bio_entities(&tokens, &labels, "PER"),
            vec!["Julia Weber".to_string()]
        );
    }

    #[test]
    fn strip_honorifics_bares_the_name() {
        // The base model glues titles into PER spans (eval findings #1). The
        // PPRL token must be computed over the bare name so the same person
        // collapses to the same token across extractors.
        assert_eq!(strip_honorifics("Herrn Müller"), "Müller");
        assert_eq!(strip_honorifics("Herr Dr . Klaus Hoffmann"), "Klaus Hoffmann");
        assert_eq!(strip_honorifics("Frau Nguyen"), "Nguyen");
        assert_eq!(strip_honorifics("Dr. Sabine Krüger"), "Sabine Krüger");
        // Title-only capture reduces to empty → dropped by the length filter.
        assert_eq!(strip_honorifics("Dr"), "");
        assert_eq!(strip_honorifics("Dr ."), "");
        // Non-titles stay untouched.
        assert_eq!(strip_honorifics("Familie Schäfer"), "Familie Schäfer");
        assert_eq!(strip_honorifics("Sabine Krüger"), "Sabine Krüger");
    }

    // ── Ignored eval harnesses (drive the fine-tune decision) ──────────────
    //
    // Both download ~700 MB of mBERT weights on first run and need the
    // `local-embed` feature. Run with:
    //   cargo test -p wms --bin wms -- --ignored eval_ner --nocapture

    /// 10 realistic German support-ticket-style texts (greetings, signatures,
    /// addresses, device serials that must NOT be flagged). Prints the PER
    /// entities the local model finds per text plus per-text wall time.
    #[tokio::test]
    #[ignore]
    async fn eval_ner_local() {
        std::env::set_var("SYNC_SECRET", "eval_secret");
        for (i, text) in EVAL_TEXTS.iter().enumerate() {
            let t0 = std::time::Instant::now();
            let names = extract_person_names(text)
                .await
                .expect("extract_person_names failed");
            let ms = t0.elapsed().as_millis();
            println!("\n[text {}] ({ms} ms)\n  {}", i + 1, text.replace('\n', " / "));
            println!("  PER entities: {names:?}");
        }
    }

    /// Side-by-side: local NER PER entities vs. the existing Gemini
    /// `extract_and_anonymize` fingerprints, for manual comparison. Skips the
    /// Gemini half when no GEMINI_API_KEY is present.
    #[tokio::test]
    #[ignore]
    async fn eval_ner_vs_gemini() {
        dotenvy::dotenv().ok();
        std::env::set_var("SYNC_SECRET", "eval_secret");

        let http = reqwest::Client::new();
        let key = std::env::var("GEMINI_API_KEY").ok().filter(|k| !k.is_empty());
        let gen_model = std::env::var("GEMINI_GENERATION_MODEL")
            .unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string());
        let auth = key.as_deref().map(eck_core::ai::AiAuth::studio);
        if auth.is_none() {
            println!("⚠ GEMINI_API_KEY not set — printing local NER only");
        }

        for (i, text) in EVAL_TEXTS.iter().enumerate() {
            let t0 = std::time::Instant::now();
            let local = extract_person_names(text)
                .await
                .expect("extract_person_names failed");
            let ms = t0.elapsed().as_millis();
            println!("\n[text {}] ({ms} ms local)\n  {}", i + 1, text.replace('\n', " / "));
            println!("  LOCAL PER: {local:?}");

            if let Some(auth) = &auth {
                match crate::ai::embeddings::extract_and_anonymize(&http, auth, text, &gen_model)
                    .await
                {
                    Ok((anon, fps)) => {
                        println!("  GEMINI fingerprints: {fps:?}");
                        println!("  GEMINI masked text: {}", anon.replace('\n', " / "));
                    }
                    Err(e) => println!("  GEMINI error: {e}"),
                }
            }
        }
    }

    const EVAL_TEXTS: [&str; 10] = [
        "Sehr geehrte Damen und Herren,\nmein ZetaBody 770 (Seriennummer ZB770-2023-04581) \
         startet nicht mehr. Bitte um Rückruf.\nMit freundlichen Grüßen\nDr. Andreas Vogel",
        "Hallo, hier spricht Frau Petra Baumgartner vom Fitnessstudio Aktiv in Köln. \
         Das Gerät zeigt Fehlercode E-14 nach dem Einschalten.",
        "Guten Tag,\ndie Reparatur von Herrn Müller ist abgeschlossen. Das Ersatzteil \
         (Artikel 90011234) wurde getauscht. Rechnung folgt an die Praxis Dr. Sabine Krüger.",
        "Kundin: Julia Weber-Schmidt\nAdresse: Bahnhofstraße 12a, 40210 Düsseldorf\n\
         Problem: Display flackert, ZetaBody 570 Modell 2021.",
        "Servicebericht: Techniker Thomas Wagner hat das Netzteil geprüft. \
         Der Betreiber, das Reha-Zentrum Sonnenhof, meldet weiterhin Ausfälle.",
        "Betreff: dringende Anfrage\nSehr geehrter Herr Dr. Klaus Hoffmann,\nwir bestätigen \
         den Termin am Dienstag. Seriennummer des Geräts: 22-INB-77-0091.",
        "Liebes Team, unser ZetaBody im Studio München-Ost (Leopoldstraße 5) macht Probleme. \
         Ansprechpartner ist Herr Öztürk unter der bekannten Nummer.",
        "Rücksendung genehmigt. Absender: Apotheke am Marktplatz, z. Hd. Frau Nguyen, \
         Marktplatz 3, 70173 Stuttgart. Gerät: ZetaBody H20N.",
        "Notiz: Anruf von Michael Braun erhalten. Er beschwert sich, dass die letzte \
         Wartung durch die Firma MedTech Solutions GmbH nicht dokumentiert wurde.",
        "Reklamation eingegangen von Familie Schäfer. Das Gerät wurde am 03.04.2024 \
         geliefert (Lieferschein 5001234). Kontakt: hans.schaefer@example.de.",
    ];
}
