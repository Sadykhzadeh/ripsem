use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
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
    /// stays on the CPU.
    pub fn new(model_id: &str, gpu: bool) -> Result<Self> {
        let (embedding_model, dims, query_prefix) = map_model(model_id)?;
        let cache = cache_dir();
        std::fs::create_dir_all(&cache).ok();
        let options = TextInitOptions::new(embedding_model)
            .with_cache_dir(cache)
            .with_show_download_progress(true);
        #[cfg(windows)]
        let options = match gpu {
            true => options.with_execution_providers(vec![ort::ep::DirectML::default().build()]),
            false => options,
        };
        #[cfg(not(windows))]
        let _ = gpu;
        let model = TextEmbedding::try_new(options)?;
        Ok(Self {
            model: Mutex::new(model),
            dims,
            query_prefix,
        })
    }
}

fn cache_dir() -> PathBuf {
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
