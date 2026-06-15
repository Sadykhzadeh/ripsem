use anyhow::Result;

pub mod hashing;

#[cfg(feature = "fastembed")]
pub mod fastembed;

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
