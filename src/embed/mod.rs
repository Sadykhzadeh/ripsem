use anyhow::Result;

pub mod hashing;

#[cfg(feature = "fastembed")]
pub mod fastembed;
#[cfg(all(feature = "fastembed", target_os = "linux", target_env = "gnu"))]
mod ort_compat;

use crate::config::{Backend, Config};

pub trait Embedder: Send + Sync {
    fn dims(&self) -> usize;
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
}

/// What a chunk is embedded as: its path on a line of its own, then its code.
/// A chunk rarely says which file or module it belongs to, and a question often
/// does (`how does routing match a path`). When the chunk is cut from a
/// declaration, the line also carries its label (`src/auth/session.rs: fn
/// Session::refresh`): a method cut out of its class has little else that names
/// the class. Only the vector sees the line; the snippet, the lexical terms and
/// the chunk text keep the code alone. A change here is a change to every
/// stored vector, so it comes with a new `chunk::REVISION`, which makes an
/// index embed its chunks again.
pub fn document_input(path: &str, label: Option<&str>, text: &str) -> String {
    match label {
        Some(label) => format!("{path}: {label}\n{text}"),
        None => format!("{path}\n{text}"),
    }
}

/// Chunks waiting for vectors from which [`build_for`] puts the model on the
/// GPU. Starting the device costs more than embedding a few chunks, and far
/// more than one query, so smaller jobs stay on the CPU.
pub const GPU_FROM_CHUNKS: usize = 256;

/// The model for answering queries, one short text at a time.
pub fn build(config: &Config) -> Result<Box<dyn Embedder>> {
    build_with(config, 0, true)
}

/// The model for embedding `chunks` texts in one go.
pub fn build_for(config: &Config, chunks: usize) -> Result<Box<dyn Embedder>> {
    build_with(config, chunks, false)
}

fn build_with(config: &Config, chunks: usize, queries_only: bool) -> Result<Box<dyn Embedder>> {
    match config.backend {
        Backend::Hash => Ok(Box::new(hashing::HashEmbedder::new(config.dims()))),
        Backend::Fastembed => {
            #[cfg(feature = "fastembed")]
            {
                let gpu = chunks >= GPU_FROM_CHUNKS && std::env::var_os("RIPSEM_NO_GPU").is_none();
                Ok(Box::new(fastembed::FastEmbedder::new(
                    &config.model,
                    gpu,
                    queries_only,
                )?))
            }
            #[cfg(not(feature = "fastembed"))]
            {
                let _ = (chunks, queries_only);
                anyhow::bail!(
                    "in {}: backend 'fastembed' needs a build with the model runtime: cargo build --features fastembed",
                    crate::cli::show_path(&config.config_path())
                )
            }
        }
    }
}

/// What to tell a user whose queries embed with [`hashing`]. Its vectors only
/// record which words occur, so a question in prose finds little more than its
/// words, and the results look no different from real ones. A build with the
/// model can still be set to it: `init` in a build without the model writes
/// `backend = "hash"`, and reinstalling does not change the config.
pub fn backend_warning(backend: Backend) -> Option<&'static str> {
    match backend {
        Backend::Fastembed => None,
        Backend::Hash if cfg!(feature = "fastembed") => Some(
            "hashing backend: results match words, not meaning; \
             run `ripsem model backend fastembed`, then `ripsem index`",
        ),
        Backend::Hash => Some(
            "hashing backend: results match words, not meaning; \
             reinstall with `--features fastembed`, \
             then run `ripsem model backend fastembed` and `ripsem index`",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_above_the_code_is_the_path_and_the_label_when_there_is_one() {
        assert_eq!(
            document_input("src/auth/session.rs", Some("fn Session::refresh"), "code"),
            "src/auth/session.rs: fn Session::refresh\ncode"
        );
        assert_eq!(document_input("README.md", None, "text"), "README.md\ntext");
    }

    #[test]
    fn the_readme_states_the_chunk_count_that_asks_for_the_gpu() {
        let readme = include_str!("../../README.md");
        let readme: String = readme.split_whitespace().collect::<Vec<_>>().join(" ");
        let said = format!("embeds {GPU_FROM_CHUNKS} chunks or more");
        assert!(readme.contains(&said), "README should say `{said}`");
    }
}
