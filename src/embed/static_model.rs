//! Static embeddings: one learned vector per token, averaged over a text.
//! No network runs at embed time, so a chunk costs a tokenisation and a few
//! hundred additions instead of a transformer pass.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use safetensors::{Dtype, SafeTensors};
use tokenizers::Tokenizer;

use super::Embedder;

pub struct StaticEmbedder {
    tokenizer: Tokenizer,
    table: Vec<f32>,
    dims: usize,
    unk: Option<u32>,
}

impl StaticEmbedder {
    pub fn new(repo: &str, cache: &Path) -> Result<Self> {
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(cache.to_path_buf())
            .with_progress(true)
            .build()?;
        let model = api.model(repo.to_string());
        let tokenizer = Tokenizer::from_file(model.get("tokenizer.json")?)
            .map_err(|e| anyhow!("tokenizer for {repo}: {e}"))?;
        let bytes = std::fs::read(model.get("model.safetensors")?)?;
        let tensors = SafeTensors::deserialize(&bytes).context("model.safetensors")?;
        let embeddings = tensors
            .tensor("embeddings")
            .context("no embeddings tensor")?;
        let [rows, dims]: [usize; 2] = embeddings
            .shape()
            .try_into()
            .map_err(|_| anyhow!("embeddings tensor is not 2-D"))?;
        let raw = embeddings.data();
        let table: Vec<f32> = match embeddings.dtype() {
            Dtype::F32 => raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| f32::from_le_bytes(b))
                .collect(),
            Dtype::F16 => raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| half::f16::from_le_bytes(b).to_f32())
                .collect(),
            other => return Err(anyhow!("unsupported embedding dtype {other:?}")),
        };
        if table.len() != rows * dims {
            return Err(anyhow!(
                "embeddings tensor holds {} values, expected {}",
                table.len(),
                rows * dims
            ));
        }
        let unk = tokenizer.token_to_id("[UNK]");
        Ok(Self {
            tokenizer,
            table,
            dims,
            unk,
        })
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode_fast(text, false)
            .map_err(|e| anyhow!("tokenizer: {e}"))?;
        let mut sum = vec![0f32; self.dims];
        let mut count = 0usize;
        for &id in encoding.get_ids() {
            if Some(id) == self.unk {
                continue;
            }
            let start = id as usize * self.dims;
            let Some(row) = self.table.get(start..start + self.dims) else {
                continue;
            };
            for (s, v) in sum.iter_mut().zip(row) {
                *s += v;
            }
            count += 1;
        }
        let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt();
        if count > 0 && norm > 0.0 {
            sum.iter_mut().for_each(|v| *v /= norm);
        }
        Ok(sum)
    }
}

impl Embedder for StaticEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        use rayon::prelude::*;
        texts.par_iter().map(|t| self.embed_one(t)).collect()
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_one(text)
    }
}
