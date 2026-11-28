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
    prefixes: Prefixes,
}

/// What a model expects in front of each side. Some models are trained with
/// a marker that says whether a text is a question or something to be found,
/// and give weaker vectors without it; fastembed leaves adding it to us.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Prefixes {
    query: Option<&'static str>,
    document: Option<&'static str>,
}

impl FastEmbedder {
    /// `gpu` asks for DirectML on Windows; without a usable device the runtime
    /// stays on the CPU. A model that only answers queries skips packing its
    /// weights for repeated runs, which a single short text never earns back.
    pub fn new(model_id: &str, gpu: bool, queries_only: bool) -> Result<Self> {
        let (embedding_model, dims, prefixes) = map_model(model_id)?;
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
            prefixes,
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

fn map_model(id: &str) -> Result<(EmbeddingModel, usize, Prefixes)> {
    let bge = Prefixes {
        query: Some("Represent this sentence for searching relevant passages:"),
        document: None,
    };
    // No trailing spaces: `with_prefix` joins prefix and text with " ".
    let e5 = Prefixes {
        query: Some("query:"),
        document: Some("passage:"),
    };
    let nomic = Prefixes {
        query: Some("search_query:"),
        document: Some("search_document:"),
    };
    Ok(match id {
        "bge-small-en-v1.5" => (EmbeddingModel::BGESmallENV15, 384, bge),
        "all-MiniLM-L6-v2" => (EmbeddingModel::AllMiniLML6V2, 384, Prefixes::default()),
        "bge-base-en-v1.5" => (EmbeddingModel::BGEBaseENV15, 768, bge),
        "multilingual-e5-small" => (EmbeddingModel::MultilingualE5Small, 384, e5),
        "nomic-embed-text-v1.5" => (EmbeddingModel::NomicEmbedTextV15, 768, nomic),
        // Trained on queries and code as they come: no prefix on either side.
        "jina-embeddings-v2-base-code" => (
            EmbeddingModel::JinaEmbeddingsV2BaseCode,
            768,
            Prefixes::default(),
        ),
        other => return Err(anyhow!("unknown model {other:?}; see `ripsem model list`")),
    })
}

/// The text a query or document is embedded as: the model's prefix for that
/// side, a space, the text.
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
        let mut model = self.model.lock().expect("embedder mutex poisoned");
        match self.prefixes.document {
            Some(prefix) => {
                let texts: Vec<String> =
                    texts.iter().map(|t| with_prefix(Some(prefix), t)).collect();
                Ok(model.embed(texts, None)?)
            }
            None => {
                let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                Ok(model.embed(refs, None)?)
            }
        }
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let q = with_prefix(self.prefixes.query, text);
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
        let (_, _, prefixes) = map_model("jina-embeddings-v2-base-code").unwrap();
        assert_eq!(prefixes, Prefixes::default());
        assert_eq!(
            with_prefix(prefixes.query, "parse a header"),
            "parse a header"
        );
    }

    #[test]
    fn a_query_prefix_is_joined_with_one_space() {
        let (_, _, prefixes) = map_model("multilingual-e5-small").unwrap();
        assert_eq!(with_prefix(prefixes.query, "find it"), "query: find it");
        let (_, _, prefixes) = map_model("bge-small-en-v1.5").unwrap();
        assert!(
            with_prefix(prefixes.query, "find it")
                .starts_with("Represent this sentence for searching relevant passages: find it")
        );
    }

    /// e5 and nomic are trained with a marker on both sides; a document
    /// embedded without its marker is compared as if it were another query.
    #[test]
    fn models_trained_with_document_markers_get_them() {
        let (_, _, e5) = map_model("multilingual-e5-small").unwrap();
        assert_eq!(
            with_prefix(e5.document, "fn main() {}"),
            "passage: fn main() {}"
        );
        let (_, _, nomic) = map_model("nomic-embed-text-v1.5").unwrap();
        assert_eq!(with_prefix(nomic.query, "find it"), "search_query: find it");
        assert_eq!(
            with_prefix(nomic.document, "fn main() {}"),
            "search_document: fn main() {}"
        );
        for id in ["bge-small-en-v1.5", "bge-base-en-v1.5", "all-MiniLM-L6-v2"] {
            assert_eq!(map_model(id).unwrap().2.document, None, "{id}");
        }
    }
}
