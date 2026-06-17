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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn record(path: &str, vector: Vec<f32>) -> Record {
        Record {
            path: path.into(),
            start_line: 1,
            end_line: 1,
            snippet: path.into(),
            file_hash: 0,
            vector,
        }
    }

    fn sample_index() -> Index {
        let mut index = Index::new(3, "test".into());
        index.records.push(record("x.rs", vec![1.0, 0.0, 0.0]));
        index.records.push(record("y.rs", vec![0.0, 1.0, 0.0]));
        index.records.push(record("z.rs", vec![0.0, 0.0, 1.0]));
        index
    }

    #[test]
    fn search_ranks_nearest_first() {
        let index = sample_index();
        let hits = index.search(&[0.9, 0.1, 0.0], 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(index.records[hits[0].record_idx].path, "x.rs");
        assert!(hits[0].score >= hits[1].score);
    }

    #[test]
    fn search_truncates_to_k() {
        let index = sample_index();
        assert_eq!(index.search(&[1.0, 0.0, 0.0], 1).len(), 1);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = TempDir::new();
        let path = dir.path().join(".ripsem/index.bin");
        let index = sample_index();
        index.save(&path).unwrap();

        let loaded = Index::load(&path).unwrap();
        assert_eq!(loaded.dims, 3);
        assert_eq!(loaded.records.len(), 3);
        assert_eq!(loaded.records[1].path, "y.rs");
    }

    #[test]
    fn file_hashes_map_every_path() {
        let mut index = sample_index();
        index.records[0].file_hash = 42;
        let hashes = index.file_hashes();
        assert_eq!(hashes.get("x.rs"), Some(&42));
        assert_eq!(hashes.len(), 3);
    }
}
