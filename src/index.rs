use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub file_hash: u64,
    pub vector: Vec<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub dims: usize,
    pub model: String,
    pub records: Vec<Record>,
}

pub struct Hit {
    pub score: f32,
    pub record_idx: usize,
}

impl Index {
    pub fn new(dims: usize, model: String) -> Self {
        Self {
            dims,
            model,
            records: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading index {}", path.display()))?;
        bincode::deserialize(&bytes).context("decoding index")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serialize(self).context("encoding index")?;
        std::fs::write(path, bytes).with_context(|| format!("writing index {}", path.display()))?;
        Ok(())
    }

    pub fn file_hashes(&self) -> HashMap<String, u64> {
        let mut m = HashMap::new();
        for r in &self.records {
            m.insert(r.path.clone(), r.file_hash);
        }
        m
    }

    pub fn search(&self, query: &[f32], k: usize) -> Vec<Hit> {
        let mut scored: Vec<Hit> = self
            .records
            .par_iter()
            .enumerate()
            .map(|(i, r)| Hit {
                score: dot(&r.vector, query),
                record_idx: i,
            })
            .collect();
        scored.sort_unstable_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        scored
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
