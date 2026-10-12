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

pub fn build(config: &Config) -> Result<Box<dyn Embedder>> {
    match config.backend {
        Backend::Hash => Ok(Box::new(hashing::HashEmbedder::new(config.dims()))),
        Backend::Fastembed => {
            #[cfg(feature = "fastembed")]
            {
                Ok(Box::new(fastembed::FastEmbedder::new(&config.model)?))
            }
            #[cfg(not(feature = "fastembed"))]
            {
                anyhow::bail!(
                    "backend 'fastembed' needs a build with the model runtime: cargo build --features fastembed"
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
             run `ripsem model backend fastembed`, then `ripsem index --force`",
        ),
        Backend::Hash => Some(
            "hashing backend: results match words, not meaning; \
             reinstall with `--features fastembed`, \
             then run `ripsem model backend fastembed` and `ripsem index --force`",
        ),
    }
}
