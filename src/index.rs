use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lexical::{TermHash, TermVector, Weights};
use crate::quant::{self, QuantizedVector};
use crate::symbol::Symbol;

pub const FORMAT_VERSION: u32 = 5;

const RRF_DAMPING: f32 = 60.0;
const CANDIDATES_PER_RESULT: usize = 8;
const MIN_CANDIDATES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub file_hash: u64,
    pub vector: QuantizedVector,
    pub terms: TermVector,
    pub symbol: Option<Symbol>,
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

#[derive(Debug, Clone, Copy)]
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
        self.search_fused(query, &[], k, prefix)
    }

    pub fn search_fused(
        &self,
        query: &[f32],
        terms: &[TermHash],
        k: usize,
        prefix: Option<&str>,
    ) -> Vec<Hit> {
        let depth = candidate_depth(k);
        let dense = self.dense_ranking(query, depth, prefix);
        let lexical = self.lexical_ranking(terms, depth, prefix);
        self.fuse(&dense, &lexical, query, k)
    }

    /// Fuse an externally produced dense ranking — the approximate-nearest-
    /// neighbour graph's candidates — with a full lexical scan, so switching to
    /// the graph changes only how the dense arm is found, not how results rank.
    pub fn search_fused_with(
        &self,
        dense: &[Hit],
        query: &[f32],
        terms: &[TermHash],
        k: usize,
    ) -> Vec<Hit> {
        let lexical = self.lexical_ranking(terms, candidate_depth(k), None);
        self.fuse(dense, &lexical, query, k)
    }

    pub fn dense_ranking(&self, query: &[f32], depth: usize, prefix: Option<&str>) -> Vec<Hit> {
        let mut ranked: Vec<Hit> = self
            .records
            .par_iter()
            .enumerate()
            .filter(|(_, r)| prefix.is_none_or(|p| path_in_scope(&r.path, p)))
            .map(|(i, r)| Hit {
                score: quant::dot(&r.vector, query),
                record_idx: i,
            })
            .collect();
        sort_and_truncate(&mut ranked, depth);
        ranked
    }

    pub fn lexical_ranking(
        &self,
        terms: &[TermHash],
        depth: usize,
        prefix: Option<&str>,
    ) -> LexicalRanking {
        if terms.is_empty() || self.records.is_empty() {
            return LexicalRanking::empty();
        }
        let presence: Vec<u64> = self
            .records
            .par_iter()
            .map(|r| match prefix {
                Some(p) if !path_in_scope(&r.path, p) => 0,
                _ => r.terms.presence(terms),
            })
            .collect();
        let weights = Weights::new(&presence, terms.len(), self.average_term_count());
        let mut ranked: Vec<Hit> = presence
            .par_iter()
            .enumerate()
            .filter(|&(_, &bits)| bits != 0)
            .map(|(i, &bits)| Hit {
                score: weights.bm25(terms, &self.records[i].terms, bits),
                record_idx: i,
            })
            .collect();
        sort_and_truncate(&mut ranked, depth);
        LexicalRanking {
            ranked,
            weights,
            terms: terms.to_vec(),
        }
    }

    pub(crate) fn fuse(
        &self,
        dense: &[Hit],
        lexical: &LexicalRanking,
        query: &[f32],
        k: usize,
    ) -> Vec<Hit> {
        let mut agreement: HashMap<usize, f32> = HashMap::new();
        for ranking in [dense, &lexical.ranked] {
            for (position, hit) in ranking.iter().enumerate() {
                if hit.record_idx < self.records.len() {
                    *agreement.entry(hit.record_idx).or_insert(0.0) +=
                        1.0 / (RRF_DAMPING + position as f32 + 1.0);
                }
            }
        }
        let mut candidates: Vec<Candidate> = agreement
            .into_iter()
            .map(|(record_idx, agreement)| {
                let record = &self.records[record_idx];
                Candidate {
                    record_idx,
                    agreement,
                    cosine: quant::dot(&record.vector, query),
                    coverage: match lexical.terms.is_empty() {
                        true => 0.0,
                        false => lexical
                            .weights
                            .coverage(record.terms.presence(&lexical.terms)),
                    },
                }
            })
            .collect();
        candidates.sort_unstable_by(Candidate::rank);
        candidates.truncate(k);
        candidates
            .into_iter()
            .map(|c| Hit {
                score: c.confidence(),
                record_idx: c.record_idx,
            })
            .collect()
    }

    fn average_term_count(&self) -> f32 {
        let total: u64 = self
            .records
            .par_iter()
            .map(|r| r.terms.length() as u64)
            .sum();
        total as f32 / self.records.len().max(1) as f32
    }
}

const COMPLETE_COVERAGE: f32 = 0.999;

struct Candidate {
    record_idx: usize,
    agreement: f32,
    cosine: f32,
    coverage: f32,
}

impl Candidate {
    /// A chunk holding every informative term of the query is not a guess: it is
    /// what an exact search would have returned, and nothing lacking those terms
    /// may outrank it. Below that bar neither arm is authoritative, so ordering
    /// falls back to how strongly the two of them agree.
    fn holds_every_term(&self) -> bool {
        self.coverage >= COMPLETE_COVERAGE
    }

    fn confidence(&self) -> f32 {
        self.cosine.max(self.coverage)
    }

    fn rank(a: &Self, b: &Self) -> Ordering {
        b.holds_every_term()
            .cmp(&a.holds_every_term())
            .then_with(|| {
                b.agreement
                    .partial_cmp(&a.agreement)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| {
                b.confidence()
                    .partial_cmp(&a.confidence())
                    .unwrap_or(Ordering::Equal)
            })
            .then(a.record_idx.cmp(&b.record_idx))
    }
}

/// Each arm needs a pool deeper than `k`, so a chunk only one of them ranks
/// highly still reaches the merge.
pub fn candidate_depth(k: usize) -> usize {
    k.saturating_mul(CANDIDATES_PER_RESULT).max(MIN_CANDIDATES)
}

pub struct LexicalRanking {
    ranked: Vec<Hit>,
    weights: Weights,
    terms: Vec<TermHash>,
}

impl LexicalRanking {
    pub fn hits(&self) -> &[Hit] {
        &self.ranked
    }

    fn empty() -> Self {
        Self {
            ranked: Vec::new(),
            weights: Weights::new(&[], 0, 1.0),
            terms: Vec::new(),
        }
    }
}

fn sort_and_truncate(scored: &mut Vec<Hit>, k: usize) {
    scored.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(k);
}

fn path_in_scope(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
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
        text_record(path, path, vector)
    }

    fn text_record(path: &str, text: &str, vector: Vec<f32>) -> Record {
        Record {
            path: path.into(),
            start_line: 1,
            end_line: 1,
            snippet: text.into(),
            file_hash: 0,
            vector: quant::quantize(&vector),
            terms: TermVector::new(text, path),
            symbol: None,
        }
    }

    fn terms(query: &str) -> Vec<TermHash> {
        crate::lexical::query_terms(query)
    }

    fn paths(index: &Index, hits: &[Hit]) -> Vec<String> {
        hits.iter()
            .map(|h| index.records[h.record_idx].path.clone())
            .collect()
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
    fn a_literal_match_outranks_an_equally_similar_chunk() {
        let mut index = Index::new(3, "test".into());
        index.records.push(text_record(
            "a.rs",
            "fn parse_header() {}",
            vec![1.0, 0.0, 0.0],
        ));
        index.records.push(text_record(
            "b.rs",
            "fn unrelated() {}",
            vec![1.0, 0.0, 0.0],
        ));

        let hits = index.search_fused(&[1.0, 0.0, 0.0], &terms("parse_header"), 2, None);
        assert_eq!(index.records[hits[0].record_idx].path, "a.rs");

        let plain = index.search_scoped(&[1.0, 0.0, 0.0], 2, None);
        assert!((plain[0].score - plain[1].score).abs() < 1e-6);
    }

    /// A chunk whose text literally contains the query cannot be reachable only
    /// through vector similarity: the dense arm ranks it below thousands of
    /// unrelated chunks, so it has to arrive from the lexical arm instead.
    #[test]
    fn a_literal_match_survives_a_hostile_dense_ranking() {
        let mut index = Index::new(3, "test".into());
        index.records.push(text_record(
            "cache.rs",
            "fn evict_least_recently_used(cache) {}",
            vec![0.0, 0.0, 1.0],
        ));
        for i in 0..5_000 {
            index.records.push(text_record(
                &format!("noise/module_{i}.rs"),
                &format!("fn filler_{i}() {{}}"),
                vec![1.0, 0.0, 0.0],
            ));
        }

        let query = [1.0, 0.0, 0.0];
        let dense_only = index.search_scoped(&query, 10, None);
        assert!(
            !paths(&index, &dense_only).contains(&"cache.rs".to_string()),
            "the dense arm alone must not reach the literal match"
        );

        let fused = index.search_fused(&query, &terms("evict_least_recently_used"), 10, None);
        assert_eq!(
            paths(&index, &fused)[0],
            "cache.rs",
            "the lexical arm must carry the literal match to the top"
        );
        assert_eq!(fused[0].score, 1.0, "full term coverage reports as certain");
    }

    #[test]
    fn a_query_with_no_indexable_terms_falls_back_to_pure_similarity() {
        let index = sample_index();
        let hits = index.search_fused(&[0.9, 0.1, 0.0], &terms("a b c"), 2, None);
        assert_eq!(index.records[hits[0].record_idx].path, "x.rs");
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

    /// Run: cargo test --release bench_fused_search -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_fused_search() {
        const RECORDS: usize = 20_000;
        const DIMS: usize = 64;
        const QUERIES: usize = 50;

        struct Lcg(u64);
        impl Lcg {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0
            }
            fn next_signed_f32(&mut self) -> f32 {
                (self.next() >> 32) as f32 / u32::MAX as f32 * 2.0 - 1.0
            }
        }

        let mut rng = Lcg(0xdeadbeef_cafebabe);
        let mut index = Index::new(DIMS, "bench".into());
        for i in 0..RECORDS {
            let vector: Vec<f32> = (0..DIMS).map(|_| rng.next_signed_f32()).collect();
            let path = format!("src/module_{}.rs", i % 200);
            let snippet = format!(
                "fn handle_request_{i}(request) {{ let parsed = parse_body(request); \
                 let session = lookup_session(parsed.token); respond(session) }}"
            );
            index.records.push(text_record(&path, &snippet, vector));
        }

        let queries: Vec<(Vec<f32>, Vec<TermHash>)> = (0..QUERIES)
            .map(|i| {
                let vector: Vec<f32> = (0..DIMS).map(|_| rng.next_signed_f32()).collect();
                (
                    vector,
                    terms(&format!("handle_request_{} lookup_session token", i * 7)),
                )
            })
            .collect();

        let _ = index.search_fused(&queries[0].0, &queries[0].1, 20, None);

        let start = std::time::Instant::now();
        for (vector, query_terms) in &queries {
            let _ = index.search_fused(vector, query_terms, 20, None);
        }
        let elapsed = start.elapsed();
        println!(
            "bench_fused_search: {RECORDS} records, {QUERIES} queries → {:.2?} total ({:.2?}/query)",
            elapsed,
            elapsed / QUERIES as u32
        );
    }
}
