//! On-device text-embedding backend (feature `local-embed`).
//!
//! Ports xelth.rs's `LocalEmbedder` to `jinaai/jina-embeddings-v2-base-de` — the
//! standard JinaBERT-with-ALiBi German/English model. Unlike `-base-code` (which
//! uses a custom qk-post-norm architecture and needs a hand-rolled model), the
//! `-de` model is a plain JinaBERT (mean pooler, ALiBi position bias), so
//! candle-transformers' built-in `jina_bert::BertModel` loads its weights
//! directly. No custom model file is required here.
//!
//! Selected at runtime by `ECK_EMBED_MODE=local`. The model files are read from
//! `ECK_EMBED_MODEL_DIR` when that env var is set (offline / LAN-only nodes get
//! `tokenizer.json`/`config.json`/`model.safetensors` by scp), otherwise they
//! are downloaded and cached via hf-hub on first use.

use anyhow::{Context, Result};
use candle_core::{Device, Tensor};
use candle_nn::{Module, VarBuilder};
use candle_transformers::models::jina_bert::{BertModel, Config};
use std::sync::Mutex;
use tokio::sync::OnceCell;
use tracing::info;

/// Canonical id recorded as `embedding_model` on every locally-produced vector
/// (see `embeddings::effective_embed_model`) so a later model migration can tell
/// on-device vectors from cloud ones and requeue across spaces. Must stay in
/// sync with `embeddings::LOCAL_EMBED_MODEL`.
pub const MODEL_NAME: &str = "jina-embeddings-v2-base-de";
/// HuggingFace repo id (org-prefixed) used for the hf-hub download path.
const HF_REPO: &str = "jinaai/jina-embeddings-v2-base-de";
/// Model context window — truncate here so long documents can't blow up the
/// forward pass with an out-of-range shape.
const MAX_TOKENS: usize = 8192;

/// Process-wide singleton. `tokenizers::Tokenizer` is `!Sync` and the candle
/// model is driven through a `Mutex` so `embed` can take `&self`. Inference is
/// CPU-bound and serialized behind these locks; a single embed worker drives
/// this in production, so contention is a non-issue.
pub struct LocalEmbedder {
    tokenizer: Mutex<tokenizers::Tokenizer>,
    model: Mutex<BertModel>,
}

static EMBEDDER: OnceCell<LocalEmbedder> = OnceCell::const_new();

impl LocalEmbedder {
    /// Blocking load: reads tokenizer/config/weights from `ECK_EMBED_MODEL_DIR`
    /// when set, else downloads+caches them from the HuggingFace Hub. Runs inside
    /// `spawn_blocking` because both hf-hub and the safetensors mmap are sync.
    fn load_blocking() -> Result<Self> {
        let started = std::time::Instant::now();

        let (tokenizer_path, config_path, weights_path, source) =
            match std::env::var("ECK_EMBED_MODEL_DIR") {
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

        let mut tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!("failed to load tokenizer from {}: {e}", tokenizer_path.display())
        })?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| anyhow::anyhow!("failed to set truncation: {e}"))?;

        let config_str = std::fs::read_to_string(&config_path)
            .with_context(|| format!("read config from {}", config_path.display()))?;
        let config: Config =
            serde_json::from_str(&config_str).context("parse jina_bert config.json")?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[weights_path.clone()],
                candle_core::DType::F32,
                &device,
            )
            .with_context(|| format!("mmap safetensors from {}", weights_path.display()))?
        };
        let model = BertModel::new(vb, &config).context("build jina_bert model")?;

        info!(
            model = MODEL_NAME,
            source = %source,
            load_ms = started.elapsed().as_millis() as u64,
            "[Embeddings] local embedder ready"
        );

        Ok(Self {
            tokenizer: Mutex::new(tokenizer),
            model: Mutex::new(model),
        })
    }

    /// Tokenize → JinaBERT forward → mean-pool over the sequence dim → L2
    /// normalize. Mean pooling matches the model's `emb_pooler: mean`, and L2
    /// normalization is required because our SurrealDB HNSW index uses
    /// `DIST COSINE`. Blocking (CPU-bound); callers wrap it in `spawn_blocking`.
    fn embed_blocking(&self, text: &str) -> Result<Vec<f32>> {
        let token_ids = {
            let tokenizer = self.tokenizer.lock().unwrap();
            tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?
                .get_ids()
                .to_vec()
        };
        // Guard against a zero-length encoding (empty input) so the mean divisor
        // and the (1, seq) tensor never degenerate.
        let len = token_ids.len().max(1);
        let device = Device::Cpu;
        let tokens = Tensor::new(token_ids.as_slice(), &device)?.unsqueeze(0)?;

        let sequence_output = {
            let model = self.model.lock().unwrap();
            model.forward(&tokens)? // (1, seq, hidden)
        };

        // Mean pooling over dim 1 (sequence).
        let sum = sequence_output.sum(1)?;
        let mean = (sum / len as f64)?.squeeze(0)?;

        // L2 normalization.
        let norm = mean.sqr()?.sum_all()?.sqrt()?;
        let normalized = mean.broadcast_div(&norm)?;

        let out: Vec<f32> = normalized.to_vec1()?;
        Ok(out)
    }
}

/// Lazily initialize (and, on first use, download) the process-wide embedder.
/// `EMBEDDER` is a `static`, so the returned reference is `'static`.
async fn embedder() -> Result<&'static LocalEmbedder> {
    EMBEDDER
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(LocalEmbedder::load_blocking)
                .await
                .context("local embedder load task panicked")?
        })
        .await
}

/// Embed `text` into a 768-dim, L2-normalized vector on-device. The model is
/// loaded (and downloaded if absent) on the first call. Both the load and the
/// CPU-bound forward pass run on the blocking pool so they never stall an async
/// worker thread.
pub async fn embed(text: &str) -> Result<Vec<f32>> {
    let embedder = embedder().await?;
    let owned = text.to_string();
    tokio::task::spawn_blocking(move || embedder.embed_blocking(&owned))
        .await
        .context("local embed task panicked")?
}
