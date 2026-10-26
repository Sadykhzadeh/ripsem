use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::Embedder;

pub struct FastEmbedder {
    model: Mutex<TextEmbedding>,
    dims: usize,
    query_prefix: Option<String>,
}

impl FastEmbedder {
    /// `gpu` asks for DirectML on Windows; without a usable device the runtime
    /// stays on the CPU. A model that only answers queries skips packing its
    /// weights for repeated runs, which a single short text never earns back.
    pub fn new(model_id: &str, gpu: bool, queries_only: bool) -> Result<Self> {
        let (embedding_model, dims, query_prefix) = map_model(model_id)?;
        let cache = cache_dir();
        std::fs::create_dir_all(&cache).ok();
        let options = TextInitOptions::new(embedding_model)
            .with_cache_dir(cache.clone())
            .with_show_download_progress(true);
        let options = match queries_only {
            true => options.with_session_config("session.disable_prepacking", "1"),
            false => options,
        };
        #[cfg(windows)]
        let options = match gpu {
            true => options.with_execution_providers(vec![ort::ep::DirectML::default().build()]),
            false => options,
        };
        #[cfg(not(windows))]
        let _ = gpu;
        let model = TextEmbedding::try_new(options).with_context(|| {
            format!(
                "could not load the model {model_id} from {}; it is downloaded on first use, \
                 so check the network, or run `ripsem model backend hash` to search without it",
                cache.display()
            )
        })?;
        Ok(Self {
            model: Mutex::new(model),
            dims,
            query_prefix,
        })
    }
}

/// `RIPSEM_MODEL_DIR` when it is set, so the models can live on another disk or
/// in a CI cache; otherwise the platform's cache directory.
fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RIPSEM_MODEL_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    ProjectDirs::from("", "", "ripsem")
        .map(|d| d.cache_dir().join("models"))
        .unwrap_or_else(|| PathBuf::from(".ripsem-models"))
}

fn map_model(id: &str) -> Result<(EmbeddingModel, usize, Option<String>)> {
    let bge = Some("Represent this sentence for searching relevant passages:".to_string());
    Ok(match id {
        "bge-small-en-v1.5" => (EmbeddingModel::BGESmallENV15, 384, bge),
        "all-MiniLM-L6-v2" => (EmbeddingModel::AllMiniLML6V2, 384, None),
        "bge-base-en-v1.5" => (EmbeddingModel::BGEBaseENV15, 768, bge),
        "multilingual-e5-small" => (
            EmbeddingModel::MultilingualE5Small,
            384,
            // No trailing space: embed_query joins prefix and text with " ".
            Some("query:".to_string()),
        ),
        "nomic-embed-text-v1.5" => (EmbeddingModel::NomicEmbedTextV15, 768, None),
        other => return Err(anyhow!("unknown model {other:?}; see `ripsem model list`")),
    })
}

impl Embedder for FastEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let mut model = self.model.lock().expect("embedder mutex poisoned");
        Ok(model.embed(refs, None)?)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let q = match &self.query_prefix {
            Some(p) => format!("{p} {text}"),
            None => text.to_string(),
        };
        let mut model = self.model.lock().expect("embedder mutex poisoned");
        let mut out = model.embed(vec![q.as_str()], None)?;
        out.pop()
            .ok_or_else(|| anyhow!("embedding backend returned no vector"))
    }
}
