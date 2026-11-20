use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::Embedder;
use crate::cli::show_path;

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
        create_cache(&cache)?;
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

/// A cache the disk refuses is named as such, rather than failing the download
/// with a message that blames the network.
fn create_cache(cache: &Path) -> Result<()> {
    std::fs::create_dir_all(cache)
        .with_context(|| format!("creating the model cache {}", show_path(cache)))
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
        // Trained on queries and code as they come: no prefix on either side.
        "jina-embeddings-v2-base-code" => (EmbeddingModel::JinaEmbeddingsV2BaseCode, 768, None),
        other => return Err(anyhow!("unknown model {other:?}; see `ripsem model list`")),
    })
}

/// The text a query is embedded as: the model's prefix, a space, the query.
fn with_prefix(prefix: Option<&str>, text: &str) -> String {
    match prefix {
        Some(p) => format!("{p} {text}"),
        None => text.to_string(),
    }
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
        let q = with_prefix(self.query_prefix.as_deref(), text);
        let mut model = self.model.lock().expect("embedder mutex poisoned");
        let mut out = model.embed(vec![q.as_str()], None)?;
        out.pop()
            .ok_or_else(|| anyhow!("embedding backend returned no vector"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    #[test]
    fn every_catalogued_model_maps_to_a_model_of_the_same_width() {
        for info in model::catalog() {
            let (mapped, dims, _) = map_model(info.id).unwrap();
            assert_eq!(dims, info.dims, "{}", info.id);
            let known = TextEmbedding::get_model_info(&mapped).unwrap();
            assert_eq!(known.dim, info.dims, "{}", info.id);
        }
    }

    #[test]
    fn a_model_cache_that_cannot_be_created_is_named() {
        let dir = crate::testutil::TempDir::new();
        dir.write("file", "not a directory\n");
        let err = create_cache(&dir.path().join("file").join("models")).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.starts_with("creating the model cache "), "{text}");
        assert!(text.contains("models"), "{text}");
    }

    #[test]
    fn an_unknown_model_is_refused() {
        assert!(map_model("jina-embeddings-v2-base-codee").is_err());
    }

    #[test]
    fn the_code_model_takes_queries_as_they_are() {
        let (_, _, prefix) = map_model("jina-embeddings-v2-base-code").unwrap();
        assert_eq!(prefix, None);
        assert_eq!(
            with_prefix(prefix.as_deref(), "parse a header"),
            "parse a header"
        );
    }

    #[test]
    fn a_query_prefix_is_joined_with_one_space() {
        let (_, _, prefix) = map_model("multilingual-e5-small").unwrap();
        assert_eq!(with_prefix(prefix.as_deref(), "find it"), "query: find it");
        let (_, _, prefix) = map_model("bge-small-en-v1.5").unwrap();
        assert!(
            with_prefix(prefix.as_deref(), "find it")
                .starts_with("Represent this sentence for searching relevant passages: find it")
        );
    }
}
