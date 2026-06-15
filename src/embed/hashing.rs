use anyhow::Result;
use xxhash_rust::xxh3::xxh3_64;

use super::Embedder;

pub struct HashEmbedder {
    dims: usize,
}

impl HashEmbedder {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }

    fn embed_one(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dims];
        for token in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
        {
            let lower = token.to_ascii_lowercase();
            let slot = (xxh3_64(lower.as_bytes()) as usize) % self.dims;
            v[slot] += 1.0;
        }
        normalize(&mut v);
        v
    }
}

fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

impl Embedder for HashEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed_one(text))
    }
}
