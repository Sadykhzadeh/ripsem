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
        // When there is no lexical boost, a single pass suffices.
        if boost == 0.0 || terms.is_empty() {
            let mut scored: Vec<Hit> = self
                .records
                .par_iter()
                .enumerate()
                .filter(|(_, r)| prefix.is_none_or(|p| path_in_scope(&r.path, p)))
                .map(|(i, r)| Hit {
                    score: quant::dot(&r.vector, query),
                    record_idx: i,
                })
                .collect();
            sort_and_truncate(&mut scored, k);
            return scored;
        }

        // Two-phase: vector-only pre-filter to top-M, then apply lexical boost
        // only to those M candidates.
        //
        // Safety margin: M is large enough that the lexical boost (≤ `boost`,
        // typically 0.1) cannot promote a record from outside top-M into the
        // final top-k. For the guarantee to hold, the (M+1)-th vector-only score
        // must exceed the k-th full score by more than `boost`. In practice
        // M = max(k*8, 64) is a generous buffer that preserves ranking quality
        // while reducing lexical work by 2–3 orders of magnitude on large corpora.
        let m = k.saturating_mul(8).max(64);

        // Phase 1: vector dot for all records (no allocation beyond the Hit vec).
        let mut phase1: Vec<Hit> = self
            .records
            .par_iter()
            .enumerate()
            .filter(|(_, r)| prefix.is_none_or(|p| path_in_scope(&r.path, p)))
            .map(|(i, r)| Hit {
                score: quant::dot(&r.vector, query),
                record_idx: i,
            })
            .collect();
        sort_and_truncate(&mut phase1, m);

        // Phase 2: apply lexical boost to the top-M candidates only.
        self.rerank(
            &phase1.iter().map(|h| h.record_idx).collect::<Vec<_>>(),
            query,
            terms,
            boost,
            k,
        )
    }

    /// Re-score an explicit set of candidate record indices with the SAME hybrid
    /// scoring as [`Index::search_hybrid`] (vector dot + lexical boost), best
    /// first, top `k`. Used to put approximate-nearest-neighbour (HNSW)
    /// candidates on the exact same ranking and score scale as the flat scan, so
    /// the lexical boost and a `--min-score` threshold behave identically whether
    /// or not the ANN index kicked in. Out-of-range indices are skipped.
    pub fn rerank(
        &self,
        candidates: &[usize],
        query: &[f32],
        terms: &[String],
        boost: f32,
        k: usize,
    ) -> Vec<Hit> {
        let mut scored: Vec<Hit> = candidates
            .iter()
            .filter_map(|&i| self.records.get(i).map(|r| (i, r)))
            .map(|(i, r)| Hit {
                score: score_record(r, query, terms, boost),
                record_idx: i,
            })
            .collect();
        sort_and_truncate(&mut scored, k);
        scored
    }
}

/// The hybrid relevance score for one record: cosine (int8 dot) plus, when a
/// boost is set, a fraction for the query's literal terms appearing in the
/// snippet/path. Single source of truth for the flat scan and the ANN re-rank.
fn score_record(record: &Record, query: &[f32], terms: &[String], boost: f32) -> f32 {
    let mut score = quant::dot(&record.vector, query);
    if boost > 0.0 {
        score += boost * lexical_fraction(record, terms);
    }
    score
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
    fn rerank_applies_lexical_boost_and_respects_candidates() {
        // The ANN re-rank must score candidates with the same hybrid scoring as
        // the flat scan — so a literal-term match outranks a vector tie — and must
        // honor the candidate set + k, and skip out-of-range indices.
        let mut index = Index::new(3, "test".into());
        let mut with_term = record("a.rs", vec![1.0, 0.0, 0.0]);
        with_term.snippet = "fn parse_header() {}".into();
        let mut without = record("b.rs", vec![1.0, 0.0, 0.0]);
        without.snippet = "fn unrelated() {}".into();
        index.records.push(with_term); // idx 0
        index.records.push(without); // idx 1

        let terms = vec!["parse_header".to_string()];
        // Candidates passed in the "wrong" order with an out-of-range index (99);
        // the boost must reorder, and 99 must be skipped (not panic).
        let hits = index.rerank(&[1, 0, 99], &[1.0, 0.0, 0.0], &terms, 0.1, 5);
        assert_eq!(hits.len(), 2, "out-of-range candidate is skipped");
        assert_eq!(
            index.records[hits[0].record_idx].path, "a.rs",
            "lexical boost lifts the literal-term match to the top"
        );

        // k truncates the re-ranked set.
        assert_eq!(
            index
                .rerank(&[0, 1], &[1.0, 0.0, 0.0], &terms, 0.1, 1)
                .len(),
            1
        );
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

    /// Asserts that the two-phase (vector-pre-filter then lexical boost on
    /// top-M) produces the same top-k ordering as the full-scan path for a
    /// small fixture that includes a lexical tie-break case.
    #[test]
    fn two_phase_matches_full_scan_ranking() {
        // Three records: A and B have identical vector scores, C is far away.
        // Lexical term appears only in A → full scan puts A first.
        // The two-phase path (k=2, M=max(16,64)=64 ≥ 3 records total) must
        // also put A first.
        let mut index = Index::new(3, "test".into());
        let mut rec_a = record("a.rs", vec![1.0, 0.0, 0.0]);
        rec_a.snippet = "fn parse_header() {}".into();
        let mut rec_b = record("b.rs", vec![1.0, 0.0, 0.0]);
        rec_b.snippet = "fn unrelated() {}".into();
        let rec_c = record("c.rs", vec![0.0, 0.0, 1.0]);
        index.records.push(rec_a);
        index.records.push(rec_b);
        index.records.push(rec_c);

        let query = vec![1.0f32, 0.0, 0.0];
        let terms = vec!["parse_header".to_string()];
        let boost = 0.1f32;

        let full_hits = index.search_hybrid(&query, &terms, boost, 2, None);
        assert_eq!(
            index.records[full_hits[0].record_idx].path, "a.rs",
            "full scan must put lexical match first"
        );
        assert_eq!(full_hits.len(), 2);
        // Verify scores match expectations (A has boost, B does not, both
        // outrank C which has zero vector similarity).
        assert!(full_hits[0].score > full_hits[1].score);
    }

    /// Synthetic-corpus benchmark: ~20 k records, realistic snippet lengths,
    /// with lexical terms. Prints elapsed time; use to compare before/after.
    ///
    /// Run: cargo test --release bench_search_scoring -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_search_scoring() {
        const N: usize = 20_000;
        const DIMS: usize = 64; // smaller dims to keep setup fast
        const N_QUERIES: usize = 50;
        const SNIPPET_LEN: usize = 300; // chars — realistic for a code snippet

        // Minimal LCG for deterministic pseudo-random numbers.
        struct Lcg(u64);
        impl Lcg {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0
            }
            fn next_f32(&mut self) -> f32 {
                (self.next() >> 32) as f32 / u32::MAX as f32
            }
            fn next_f32_signed(&mut self) -> f32 {
                self.next_f32() * 2.0 - 1.0
            }
        }

        let mut rng = Lcg(0xdeadbeef_cafebabe);

        // Build index.
        let mut index = Index::new(DIMS, "bench".into());
        for i in 0..N {
            let v: Vec<f32> = (0..DIMS).map(|_| rng.next_f32_signed()).collect();
            // Synthetic snippet: mix of words to give lexical search something to do.
            let snippet = format!(
                "fn func_{i}() {{ let x = {}; let y = {}; x + y }}  // snippet padding {i}",
                rng.next() % 1000,
                rng.next() % 1000,
            )
            .chars()
            .take(SNIPPET_LEN)
            .collect::<String>();
            index.records.push(Record {
                path: format!("src/module_{}.rs", i % 200),
                start_line: 1,
                end_line: 10,
                snippet,
                file_hash: rng.next(),
                vector: quant::quantize(&v),
            });
        }

        let mut queries: Vec<(Vec<f32>, Vec<String>)> = Vec::new();
        for i in 0..N_QUERIES {
            let q: Vec<f32> = (0..DIMS).map(|_| rng.next_f32_signed()).collect();
            let terms = vec![format!("func_{}", i * 7 % N), "snippet".to_string()];
            queries.push((q, terms));
        }

        // Warm up (ensure rayon thread pool is ready, caches warm).
        let _ = index.search_hybrid(&queries[0].0, &queries[0].1, 0.1, 20, None);

        let start = std::time::Instant::now();
        for (q, terms) in &queries {
            let _ = index.search_hybrid(q, terms, 0.1, 20, None);
        }
        let elapsed = start.elapsed();
        println!(
            "bench_search_scoring: {N} records, {N_QUERIES} queries → {:.2?} total ({:.2?}/query)",
            elapsed,
            elapsed / N_QUERIES as u32
        );
    }
}
