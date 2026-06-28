use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::quant::{self, QuantizedVector};

pub const FORMAT_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub file_hash: u64,
    pub vector: QuantizedVector,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Index {
    pub format_version: u32,
    pub dims: usize,
    pub model: String,
    pub records: Vec<Record>,
    #[serde(default)]
    pub ann: Option<Vec<u8>>,
}

pub struct Hit {
    pub score: f32,
    pub record_idx: usize,
}

impl Index {
    pub fn new(dims: usize, model: String) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            dims,
            model,
            records: Vec::new(),
            ann: None,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading index {}", path.display()))?;
        let index: Index = bincode::deserialize(&bytes).map_err(|e| {
            anyhow::anyhow!(
                "index is unreadable or corrupt: {e}; run `ripsem index --force` to rebuild"
            )
        })?;
        if index.format_version != FORMAT_VERSION {
            anyhow::bail!("index format is from an older ripsem; run `ripsem index --force`");
        }
        Ok(index)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serialize(self).context("encoding index")?;
        let tmp = temp_sibling(path);
        std::fs::write(&tmp, bytes).with_context(|| format!("writing index {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("replacing index {}", path.display()))?;
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
        self.search_scoped(query, k, None)
    }

    pub fn search_scoped(&self, query: &[f32], k: usize, prefix: Option<&str>) -> Vec<Hit> {
        self.search_hybrid(query, &[], 0.0, k, prefix)
    }

    pub fn search_hybrid(
        &self,
        query: &[f32],
        terms: &[String],
        boost: f32,
        k: usize,
        prefix: Option<&str>,
    ) -> Vec<Hit> {
        let mut scored: Vec<Hit> = self
            .records
            .par_iter()
            .enumerate()
            .filter(|(_, r)| prefix.is_none_or(|p| path_in_scope(&r.path, p)))
            .map(|(i, r)| {
                let mut score = quant::dot(&r.vector, query);
                if boost > 0.0 {
                    score += boost * lexical_fraction(r, terms);
                }
                Hit {
                    score,
                    record_idx: i,
                }
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

fn path_in_scope(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

fn lexical_fraction(record: &Record, terms: &[String]) -> f32 {
    if terms.is_empty() {
        return 0.0;
    }
    let snippet = record.snippet.to_lowercase();
    let path = record.path.to_lowercase();
    let present = terms
        .iter()
        .filter(|t| snippet.contains(t.as_str()) || path.contains(t.as_str()))
        .count();
    present as f32 / terms.len() as f32
}

fn temp_sibling(path: &Path) -> std::path::PathBuf {
    match path.file_name() {
        Some(name) => {
            let mut tmp = name.to_os_string();
            tmp.push(".tmp");
            path.with_file_name(tmp)
        }
        None => path.with_extension("tmp"),
    }
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
            vector: quant::quantize(&vector),
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
    fn scoped_search_only_returns_matching_prefix() {
        let mut index = Index::new(3, "test".into());
        index.records.push(record("src/a.rs", vec![1.0, 0.0, 0.0]));
        index
            .records
            .push(record("src/embed/b.rs", vec![1.0, 0.0, 0.0]));
        index.records.push(record("docs/c.md", vec![1.0, 0.0, 0.0]));

        let hits = index.search_scoped(&[1.0, 0.0, 0.0], 10, Some("src"));
        let paths: Vec<&str> = hits
            .iter()
            .map(|h| index.records[h.record_idx].path.as_str())
            .collect();
        assert!(paths.contains(&"src/a.rs"));
        assert!(paths.contains(&"src/embed/b.rs"));
        assert!(!paths.contains(&"docs/c.md"));
    }

    #[test]
    fn scope_prefix_does_not_match_partial_segments() {
        assert!(path_in_scope("src/a.rs", "src"));
        assert!(path_in_scope("src", "src"));
        assert!(!path_in_scope("source/a.rs", "src"));
    }

    #[test]
    fn lexical_boost_breaks_ties_toward_literal_matches() {
        let mut index = Index::new(3, "test".into());
        let mut with_term = record("a.rs", vec![1.0, 0.0, 0.0]);
        with_term.snippet = "fn parse_header() {}".into();
        let mut without = record("b.rs", vec![1.0, 0.0, 0.0]);
        without.snippet = "fn unrelated() {}".into();
        index.records.push(with_term);
        index.records.push(without);

        let terms = vec!["parse_header".to_string()];
        let hits = index.search_hybrid(&[1.0, 0.0, 0.0], &terms, 0.1, 2, None);
        assert_eq!(index.records[hits[0].record_idx].path, "a.rs");

        let plain = index.search_scoped(&[1.0, 0.0, 0.0], 2, None);
        assert!((plain[0].score - plain[1].score).abs() < 1e-6);
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
    fn save_replaces_without_leaving_a_temp_file() {
        let dir = TempDir::new();
        let path = dir.path().join(".ripsem/index.bin");
        sample_index().save(&path).unwrap();
        sample_index().save(&path).unwrap();
        assert!(Index::load(&path).is_ok());
        assert!(!path.with_file_name("index.bin.tmp").exists());
    }

    #[test]
    fn rejects_a_mismatched_format_version() {
        let dir = TempDir::new();
        let path = dir.path().join(".ripsem/index.bin");
        let mut index = sample_index();
        index.format_version = FORMAT_VERSION + 1;
        index.save(&path).unwrap();
        assert!(Index::load(&path).is_err());
    }

    #[test]
    fn corrupt_index_reports_a_decode_error_not_old_format() {
        let dir = TempDir::new();
        let path = dir.path().join(".ripsem/index.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, [0xde, 0xad, 0xbe, 0xef, 0x00, 0xff, 0x42, 0x13]).unwrap();

        let err = Index::load(&path).unwrap_err().to_string();
        assert!(err.contains("unreadable or corrupt"), "got: {err}");
        assert!(!err.contains("from an older ripsem"), "got: {err}");
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
