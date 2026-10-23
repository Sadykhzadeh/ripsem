use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use bitcode::{Decode, Encode};
use rayon::prelude::*;

use crate::chunk::Chunking;
use crate::cli::show_path;
use crate::config::Backend;
use crate::lexical::{TermHash, TermVector, Weights};
use crate::quant::{self, QuantizedVector};
use crate::symbol::{Declaration, Symbol};

/// Changes whenever the bytes of an index would, including on a bitcode
/// upgrade: bitcode keeps its encoding only within one 0.x series.
pub const FORMAT_VERSION: u32 = 11;

/// Every index file starts with these bytes and then [`FORMAT_VERSION`] as a
/// little-endian u32, so a file from another ripsem is recognised before its
/// body is decoded.
const MAGIC: [u8; 6] = *b"ripsem";

/// Index files without the header are bincode and begin with their own
/// format version, 1 to 6, as a little-endian u32.
const HEADERLESS_VERSIONS: std::ops::RangeInclusive<u32> = 1..=6;

const RRF_DAMPING: f32 = 60.0;
const CANDIDATES_PER_RESULT: usize = 8;
const MIN_CANDIDATES: usize = 64;

#[derive(Debug, Clone, Encode, Decode)]
pub struct Record {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub file_hash: u64,
    /// The chunk's own text, so an edit elsewhere in its file does not embed
    /// it again.
    pub text_hash: u64,
    pub vector: QuantizedVector,
    pub terms: TermVector,
    pub symbol: Option<Symbol>,
    pub declarations: Vec<Declaration>,
}

impl Record {
    /// Tests describe behaviour in the words a question uses and call the
    /// code under test by name, so they match nearly as well as the code
    /// itself without being what is looked for.
    pub fn is_test(&self) -> bool {
        const DIRS: [&str; 7] = [
            "test",
            "tests",
            "__tests__",
            "spec",
            "specs",
            "testing",
            "testdata",
        ];
        const SUFFIXES: [&str; 4] = ["test", "tests", "spec", "specs"];
        let path = self.path.to_ascii_lowercase();
        let (dirs, file) = path.rsplit_once('/').unwrap_or(("", &path));
        let stem = file.split('.').next().unwrap_or(file);
        let named_like_a_class = [".java", ".kt", ".scala", ".cs", ".php", ".swift"]
            .iter()
            .any(|ext| file.ends_with(ext));
        let suffixed = |name: &str, separator: char| {
            SUFFIXES.iter().any(|s| {
                name.strip_suffix(s)
                    .is_some_and(|rest| rest.ends_with(separator))
            })
        };
        dirs.split('/')
            .any(|dir| DIRS.contains(&dir) || suffixed(dir, '.'))
            || stem.starts_with("test_")
            || suffixed(stem, '_')
            || file.contains(".test.")
            || file.contains(".spec.")
            || (named_like_a_class && SUFFIXES.iter().any(|s| stem.ends_with(s)))
    }

    /// The declaration this record is one piece of, when it is too large for
    /// one chunk: the pieces carry its symbol without holding all of it.
    pub fn piece_of(&self) -> Option<&Symbol> {
        let symbol = self.symbol.as_ref()?;
        let whole = self.declarations.iter().any(|d| {
            d.symbol == *symbol && self.start_line <= d.start_line && d.end_line <= self.end_line
        });
        (!whole).then_some(symbol)
    }

    /// The declaration a query names, by the name typed or spelled out in
    /// words, among those the record holds. `Road::quote_cost` names both
    /// `Road` and its method; the one named in full wins, then one that is not
    /// an `impl` block, then the longer name.
    pub fn declaration_named(&self, names: &[TermHash]) -> Option<&Declaration> {
        if names.is_empty() {
            return None;
        }
        let named = |name: &str| crate::lexical::name_key(name).is_some_and(|k| names.contains(&k));
        self.declarations
            .iter()
            .filter(|d| named(d.symbol.leaf()))
            .max_by_key(|d| {
                (
                    named(&d.symbol.name),
                    !d.is_extension(),
                    d.symbol.leaf().len(),
                )
            })
    }
}

#[derive(Debug, Encode, Decode)]
pub struct Index {
    pub dims: usize,
    pub backend: Backend,
    pub model: String,
    pub chunking: Chunking,
    pub records: Vec<Record>,
    pub ann: Option<Vec<u8>>,
    /// How similar text about something else entirely scores against this
    /// index: a cosine is reported as how far it rises above this. `None`
    /// until measured.
    pub noise_floor: Option<f32>,
    /// Chunks embedded since the noise floor was measured.
    pub embedded_since_floor: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub score: f32,
    pub record_idx: usize,
}

impl Index {
    pub fn new(dims: usize, backend: Backend, model: String) -> Self {
        Self {
            dims,
            backend,
            model,
            chunking: Chunking::default(),
            records: Vec::new(),
            ann: None,
            noise_floor: None,
            embedded_since_floor: 0,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading index {}", show_path(path)))?;
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let corrupt = |e: &dyn std::fmt::Display| {
            anyhow::anyhow!(
                "index is unreadable or corrupt: {e}; run `ripsem index --force` to rebuild"
            )
        };
        let (version, body) = match bytes.strip_prefix(MAGIC.as_slice()) {
            Some(rest) => {
                let (version, body) = rest
                    .split_first_chunk::<4>()
                    .ok_or_else(|| corrupt(&"the header is cut short"))?;
                (u32::from_le_bytes(*version), body)
            }
            None => match bytes.first_chunk::<4>().map(|v| u32::from_le_bytes(*v)) {
                Some(version) if HEADERLESS_VERSIONS.contains(&version) => (version, &[][..]),
                _ => return Err(corrupt(&"no ripsem header")),
            },
        };
        match version.cmp(&FORMAT_VERSION) {
            Ordering::Less => {
                anyhow::bail!("index format is from an older ripsem; run `ripsem index --force`")
            }
            Ordering::Greater => anyhow::bail!(
                "index format is from a newer ripsem; run `ripsem index --force` to rebuild it \
                 for this one"
            ),
            Ordering::Equal => bitcode::decode(body).map_err(|e| corrupt(&e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = bitcode::encode(self);
        let tmp = temp_sibling(path);
        std::fs::File::create(&tmp)
            .and_then(|mut file| {
                file.write_all(&MAGIC)?;
                file.write_all(&FORMAT_VERSION.to_le_bytes())?;
                file.write_all(&body)
            })
            .with_context(|| format!("writing index {}", show_path(&tmp)))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("replacing index {}", show_path(path)))?;
        Ok(())
    }

    /// Whether any indexed file lies under the repo-relative `prefix`.
    pub fn covers(&self, prefix: &str) -> bool {
        self.records.iter().any(|r| path_in_scope(&r.path, prefix))
    }

    /// Keys are borrowed, so a file's path is not copied once per chunk.
    pub fn file_hashes(&self) -> HashMap<&str, u64> {
        self.records
            .iter()
            .map(|r| (r.path.as_str(), r.file_hash))
            .collect()
    }

    /// Whether a query embedded by `model` on `backend` can be compared with
    /// this index. Different models give incomparable vectors even at equal
    /// dimensions, and the hash backend gives its own under any model name.
    pub fn built_for(&self, backend: Backend, model: &str) -> bool {
        self.backend == backend && self.model == model
    }

    /// Names what embedded this index, as messages about a mismatch show it.
    pub fn built_with(&self) -> String {
        embedded_by(self.backend, &self.model)
    }

    pub fn search(&self, query: &[f32], k: usize) -> Vec<Hit> {
        self.search_scoped(query, k, None)
    }

    pub fn search_scoped(&self, query: &[f32], k: usize, prefix: Option<&str>) -> Vec<Hit> {
        self.search_fused(query, &[], &[], k, prefix)
    }

    /// `names` are the query's [`crate::lexical::query_names`].
    pub fn search_fused(
        &self,
        query: &[f32],
        terms: &[TermHash],
        names: &[TermHash],
        k: usize,
        prefix: Option<&str>,
    ) -> Vec<Hit> {
        let depth = candidate_depth(k);
        let dense = self.dense_ranking(query, depth, prefix);
        let lexical = self.lexical_ranking(terms, depth, prefix);
        self.fuse(&dense, &lexical, query, names, k)
    }

    /// Fuses a dense ranking from the nearest-neighbour graph with a full
    /// lexical scan, so the graph changes how candidates are found, not how
    /// results rank.
    pub fn search_fused_with(
        &self,
        dense: &[Hit],
        query: &[f32],
        terms: &[TermHash],
        names: &[TermHash],
        k: usize,
    ) -> Vec<Hit> {
        let lexical = self.lexical_ranking(terms, candidate_depth(k), None);
        self.fuse(dense, &lexical, query, names, k)
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
        names: &[TermHash],
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
        let wants_tests = lexical.terms.iter().any(|t| TEST_WORDS.contains(t));
        let mut candidates: Vec<Candidate> = agreement
            .into_iter()
            .map(|(record_idx, agreement)| {
                let record = &self.records[record_idx];
                let presence = record.terms.presence(&lexical.terms);
                Candidate {
                    record_idx,
                    agreement,
                    declares_something: record.symbol.is_some(),
                    named: match record.declaration_named(names) {
                        None => Named::No,
                        Some(d) if d.is_extension() => Named::Extended,
                        Some(_) => Named::Declared,
                    },
                    test: !wants_tests && record.is_test(),
                    cosine: quant::dot(&record.vector, query),
                    coverage: lexical.weights.coverage(presence),
                    share: lexical.weights.share(presence),
                }
            })
            .collect();
        self.credit_named_files(&mut candidates, lexical);
        candidates.sort_unstable_by(Candidate::rank);
        self.best_of_each_place(candidates, k)
    }

    /// A file named after what the query asks about is likely the answer:
    /// "how is a route registered" is answered in `routes.go`, whose methods
    /// need not say "route" in their bodies. Each candidate gains agreement in
    /// proportion to the share of the query, weighted as BM25 weighs it, that
    /// its file name or directory holds, up to the agreement of the best
    /// candidate. A share under a tenth is one common word and counts for
    /// nothing.
    fn credit_named_files(&self, candidates: &mut [Candidate], lexical: &LexicalRanking) {
        if lexical.terms.is_empty() {
            return;
        }
        let best = candidates.iter().map(|c| c.agreement).fold(0.0, f32::max);
        let mut shares: HashMap<&str, f32> = HashMap::new();
        for candidate in candidates.iter_mut() {
            let path = self.records[candidate.record_idx].path.as_str();
            let share = *shares.entry(path).or_insert_with(|| {
                let named =
                    crate::lexical::presence_in(&lexical.terms, &crate::lexical::path_terms(path));
                lexical.weights.coverage(named)
            });
            if share >= MIN_PATH_SHARE {
                candidate.agreement += best * share;
            }
        }
    }

    /// Text about cooking or sport is as unrelated to any code as a query
    /// can be. The second best of the closest matches such text finds is the
    /// floor, so one probe that happens to land near a stray comment does not
    /// set it.
    pub fn measure_noise_floor(&mut self, probes: &[Vec<f32>]) {
        let mut closest: Vec<f32> = probes
            .iter()
            .map(|probe| {
                self.records
                    .par_iter()
                    .map(|r| quant::dot(&r.vector, probe))
                    .reduce(|| f32::MIN, f32::max)
            })
            .filter(|c| c.is_finite() && *c > f32::MIN)
            .collect();
        closest.sort_by(|a, b| b.total_cmp(a));
        self.noise_floor = Some(closest.get(1).or(closest.first()).copied().unwrap_or(0.0));
        self.embedded_since_floor = 0;
    }

    /// Keeps the better-ranked of any records that cover the same lines (a
    /// declaration and its doc record, or overlapping windows), and of the
    /// pieces of one declaration too large for a chunk. Done before the cut to
    /// `k`, so the caller still gets `k` distinct places.
    ///
    /// Each file places its best record before any file places a second, so
    /// `k` results reach as many files as the ranking holds. A reader, or an
    /// agent, decides which file to open from the list, and five pieces of one
    /// file leave four slots that could have named another.
    fn best_of_each_place(&self, ranked: Vec<Candidate>, k: usize) -> Vec<Hit> {
        let mut taken: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
        let mut split: std::collections::HashSet<(&str, &Symbol)> =
            std::collections::HashSet::new();
        // `k` comes from the caller, so cap the reservation at the index size.
        let mut out = Vec::with_capacity(k.min(self.records.len()));
        let mut admit = |candidate: &Candidate, out: &mut Vec<Hit>| {
            let record = &self.records[candidate.record_idx];
            let places = taken.entry(record.path.as_str()).or_default();
            if places
                .iter()
                .any(|&(start, end)| record.start_line <= end && record.end_line >= start)
            {
                return;
            }
            if let Some(symbol) = record.piece_of()
                && !split.insert((record.path.as_str(), symbol))
            {
                return;
            }
            places.push((record.start_line, record.end_line));
            let reported = candidate.reported(self.noise_floor.unwrap_or(0.0));
            let score = out
                .last()
                .map_or(reported, |above: &Hit| reported.min(above.score));
            out.push(Hit {
                score,
                record_idx: candidate.record_idx,
            });
        };
        let mut files: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut later = Vec::new();
        for candidate in ranked {
            if out.len() == k {
                return out;
            }
            if files.contains(self.records[candidate.record_idx].path.as_str()) {
                later.push(candidate);
                continue;
            }
            let before = out.len();
            admit(&candidate, &mut out);
            if out.len() > before {
                files.insert(self.records[candidate.record_idx].path.as_str());
            }
        }
        for candidate in later {
            if out.len() == k {
                break;
            }
            admit(&candidate, &mut out);
        }
        out
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

const MIN_PATH_SHARE: f32 = 0.1;

static TEST_WORDS: std::sync::LazyLock<Vec<TermHash>> =
    std::sync::LazyLock::new(|| crate::lexical::query_terms("test tests testing spec specs"));

struct Candidate {
    record_idx: usize,
    agreement: f32,
    cosine: f32,
    coverage: f32,
    share: f32,
    declares_something: bool,
    named: Named,
    test: bool,
}

/// How a record answers a query that names something. A type is declared once
/// and extended by any number of `impl` blocks, and the declaration is what the
/// name asks for.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Named {
    No,
    Extended,
    Declared,
}

impl Candidate {
    /// A chunk holding every informative query term is what an exact search
    /// would return, so nothing lacking those terms may outrank it. Below that,
    /// ordering falls back to how strongly the two arms agree.
    fn holds_every_term(&self) -> bool {
        self.coverage >= COMPLETE_COVERAGE
    }

    fn confidence(&self) -> f32 {
        self.cosine.max(self.share)
    }

    /// The score a caller sees: how far the cosine rises from the index's
    /// noise floor towards an identical text, or the share of the query found
    /// literally, whichever is higher. A hit never shows more than the one
    /// above it, so a threshold keeps a prefix of the ranking.
    fn reported(&self, floor: f32) -> f32 {
        let floor = floor.clamp(0.0, 0.99);
        let above = ((self.cosine - floor) / (1.0 - floor)).clamp(0.0, 1.0);
        above.max(self.share)
    }

    /// Below an exact match, a chunk that belongs to a declaration outranks one
    /// that does not. Questions about behaviour match changelogs and docs more
    /// closely than the code that implements it, and free prose is not what a
    /// code search is for. Doc records carry their declaration's symbol, so they
    /// are unaffected. An exact match stays first even without a declaration:
    /// a CI workflow, a config file or a language without a parser declares
    /// nothing, and the literal string is still what was asked for.
    ///
    /// A record declaring what the query names comes before records that only
    /// use the name: the definition is what an agent reads next.
    fn rank(a: &Self, b: &Self) -> Ordering {
        b.holds_every_term()
            .cmp(&a.holds_every_term())
            .then_with(|| b.named.cmp(&a.named))
            .then_with(|| a.test.cmp(&b.test))
            .then_with(|| b.declares_something.cmp(&a.declares_something))
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

/// How many candidates each arm contributes. Deeper than `k`, so a chunk that
/// only one arm ranks highly still reaches the merge.
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

/// Keeps the best `k`. Partitioning them off first is linear in the index;
/// sorting everything is not.
fn sort_and_truncate(scored: &mut Vec<Hit>, k: usize) {
    let by_score = |a: &Hit, b: &Hit| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    if k == 0 {
        scored.clear();
        return;
    }
    if scored.len() > k {
        scored.select_nth_unstable_by(k - 1, by_score);
        scored.truncate(k);
    }
    scored.sort_unstable_by(by_score);
}

/// `bge-small-en-v1.5 (Hash backend)`, in the form `ripsem init` prints.
pub fn embedded_by(backend: Backend, model: &str) -> String {
    format!("{model} ({backend:?} backend)")
}

fn path_in_scope(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// Named per process: `ripsem watch` and `ripsem index` can save at once, and
/// with one shared name each would truncate the file the other is writing.
fn temp_sibling(path: &Path) -> std::path::PathBuf {
    let suffix = format!(".{}.tmp", std::process::id());
    match path.file_name() {
        Some(name) => {
            let mut tmp = name.to_os_string();
            tmp.push(suffix);
            path.with_file_name(tmp)
        }
        None => path.with_extension(&suffix[1..]),
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
            text_hash: 0,
            vector: quant::quantize(&vector),
            terms: TermVector::new(text, path, None),
            symbol: None,
            declarations: Vec::new(),
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
        let mut index = Index::new(3, Backend::Hash, "test".into());
        index.records.push(record("x.rs", vec![1.0, 0.0, 0.0]));
        index.records.push(record("y.rs", vec![0.0, 1.0, 0.0]));
        index.records.push(record("z.rs", vec![0.0, 0.0, 1.0]));
        index
    }

    #[test]
    fn scoped_search_only_returns_matching_prefix() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
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
        let mut index = Index::new(3, Backend::Hash, "test".into());
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

        let hits = index.search_fused(&[1.0, 0.0, 0.0], &terms("parse_header"), &[], 2, None);
        assert_eq!(index.records[hits[0].record_idx].path, "a.rs");

        let plain = index.search_scoped(&[1.0, 0.0, 0.0], 2, None);
        assert!((plain[0].score - plain[1].score).abs() < 1e-6);
    }

    /// A literal match can sit below thousands of unrelated chunks in the dense
    /// ranking, so it has to arrive from the lexical arm.
    #[test]
    fn a_literal_match_survives_a_hostile_dense_ranking() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
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

        let fused = index.search_fused(&query, &terms("evict_least_recently_used"), &[], 10, None);
        assert_eq!(
            paths(&index, &fused)[0],
            "cache.rs",
            "the lexical arm must carry the literal match to the top"
        );
        assert_eq!(fused[0].score, 1.0, "full term coverage reports as certain");
    }

    fn placed(path: &str, start: usize, end: usize, vector: Vec<f32>) -> Record {
        Record {
            start_line: start,
            end_line: end,
            ..text_record(path, path, vector)
        }
    }

    #[test]
    fn a_declaration_outranks_free_prose_that_matches_better() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        let mut prose = text_record("HISTORY.md", "get and post helpers", vec![1.0, 0.0, 0.0]);
        prose.symbol = None;
        let mut code = text_record("api.py", "def get(url)", vec![0.6, 0.8, 0.0]);
        code.symbol = Some(Symbol {
            kind: "fn".into(),
            name: "get".into(),
        });
        index.records.push(prose);
        index.records.push(code);

        let hits = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(
            index.records[hits[0].record_idx].path, "api.py",
            "the closer match is a changelog, and a code search is not for those"
        );
    }

    /// Every call site holds the name as fully as the definition does, and
    /// may be closer to the query, but the definition is what is asked for.
    #[test]
    fn the_record_declaring_a_name_outranks_those_using_it() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        for i in 0..3 {
            let mut caller = text_record(
                &format!("src/caller{i}.rs"),
                "fn caller() { quote_cost() }",
                vec![1.0, 0.0, 0.0],
            );
            caller.symbol = Some(Symbol {
                kind: "fn".into(),
                name: "caller".into(),
            });
            index.records.push(caller);
        }
        let mut defining = text_record(
            "src/road.rs",
            "fn preview() {} fn quote_cost() {}",
            vec![0.0, 1.0, 0.0],
        );
        let declare = |name: &str| Declaration {
            symbol: Symbol {
                kind: "fn".into(),
                name: name.into(),
            },
            start_line: 1,
            end_line: 1,
        };
        defining.symbol = Some(declare("preview").symbol);
        defining.declarations = vec![declare("preview"), declare("quote_cost")];
        index.records.push(defining);

        let query = "quote_cost";
        let names = crate::lexical::query_names(query);
        let hits = index.search_fused(&[1.0, 0.0, 0.0], &terms(query), &names, 4, None);
        assert_eq!(paths(&index, &hits)[0], "src/road.rs");
    }

    #[test]
    fn test_files_are_told_apart_by_their_path() {
        let test = |path: &str| text_record(path, "", vec![1.0]).is_test();
        for path in [
            "tests/cli.rs",
            "src/__tests__/load.ts",
            "pkg/router_test.go",
            "test_auth.py",
            "lib/session_spec.rb",
            "src/api.test.ts",
            "src/main/java/BoxTest.java",
            "src/Polly.Specs/Retry.cs",
            "src/RetryTests.cs",
            "src/Polly.Core.Tests/Retry.cs",
        ] {
            assert!(test(path), "{path}");
        }
        for path in [
            "src/testing_utils.rs",
            "src/contest.py",
            "lib/attest.go",
            "src/auth.rs",
        ] {
            assert!(!test(path), "{path}");
        }
    }

    /// A test describes the behaviour in the question's words and calls the
    /// code by name, so it matches as well as the code does.
    #[test]
    fn tests_rank_below_code_unless_the_query_asks_for_them() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        let declared = |path: &str, vector: Vec<f32>| {
            let mut record = text_record(path, "redirect follow location", vector);
            record.symbol = Some(Symbol {
                kind: "fn".into(),
                name: "follow".into(),
            });
            record
        };
        index
            .records
            .push(declared("tests/test_redirects.py", vec![1.0, 0.0, 0.0]));
        index
            .records
            .push(declared("src/sessions.py", vec![0.8, 0.6, 0.0]));

        let first = |query: &str| {
            let hits = index.search_fused(&[1.0, 0.0, 0.0], &terms(query), &[], 2, None);
            paths(&index, &hits)[0].clone()
        };
        assert_eq!(first("follow redirect location"), "src/sessions.py");
        assert_eq!(
            first("test that follows a redirect"),
            "tests/test_redirects.py"
        );
    }

    #[test]
    fn the_noise_floor_is_the_second_closest_unrelated_match() {
        let mut index = sample_index();
        index.measure_noise_floor(&[
            vec![0.9, 0.1, 0.0],
            vec![0.0, 0.6, 0.0],
            vec![0.0, 0.0, 0.3],
        ]);
        let floor = index.noise_floor.unwrap();
        assert!((floor - 0.6).abs() < 0.02, "{floor}");
    }

    /// Ranking and score answer different questions, but a score that rose
    /// down the list would make a threshold drop a hit and keep the one below.
    #[test]
    fn scores_fall_with_rank_and_start_from_the_noise_floor() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        let mut prose = text_record("NOTES.md", "unrelated", vec![1.0, 0.0, 0.0]);
        prose.symbol = None;
        let mut code = text_record("a.rs", "unrelated", vec![0.6, 0.8, 0.0]);
        code.symbol = Some(Symbol {
            kind: "fn".into(),
            name: "a".into(),
        });
        index.records.push(prose);
        index.records.push(code);
        index.noise_floor = Some(0.5);

        let hits = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(paths(&index, &hits), ["a.rs", "NOTES.md"]);
        assert!((hits[0].score - 0.2).abs() < 0.02, "{}", hits[0].score);
        assert!(hits[1].score <= hits[0].score);
    }

    /// A function too large for one chunk is cut into pieces that all carry
    /// its name. One place in the results is enough for it.
    #[test]
    fn the_pieces_of_one_declaration_take_one_place() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        for (start, vector) in [(1, vec![1.0, 0.0, 0.0]), (41, vec![0.9, 0.1, 0.0])] {
            let mut piece = placed("big.rs", start, start + 39, vector);
            piece.symbol = Some(Symbol {
                kind: "fn".into(),
                name: "resolve".into(),
            });
            index.records.push(piece);
        }
        let mut other = placed("small.rs", 1, 5, vec![0.5, 0.5, 0.0]);
        other.symbol = Some(Symbol {
            kind: "fn".into(),
            name: "other".into(),
        });
        index.records.push(other);

        let hits = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(paths(&index, &hits), ["big.rs", "small.rs"]);
    }

    /// A type and its `impl` blocks share a name but are separate places.
    #[test]
    fn a_type_and_its_impl_are_two_places() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        for (start, kind, vector) in [
            (1, "struct", vec![1.0, 0.0, 0.0]),
            (50, "impl", vec![0.9, 0.1, 0.0]),
        ] {
            let mut record = placed("config.rs", start, start + 10, vector);
            let symbol = Symbol {
                kind: kind.into(),
                name: "Config".into(),
            };
            record.declarations = vec![Declaration {
                symbol: symbol.clone(),
                start_line: start,
                end_line: start + 10,
            }];
            record.symbol = Some(symbol);
            index.records.push(record);
        }

        let hits = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn a_literal_match_outside_code_outranks_declarations_lacking_it() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        index.records.push(text_record(
            ".github/workflows/ci.yml",
            "- uses: Swatinem/rust-cache@v2",
            vec![0.0, 0.0, 1.0],
        ));
        for i in 0..5 {
            let mut code = text_record(
                &format!("src/m{i}.rs"),
                "fn cache() {}",
                vec![1.0, 0.0, 0.0],
            );
            code.symbol = Some(Symbol {
                kind: "fn".into(),
                name: "cache".into(),
            });
            index.records.push(code);
        }

        let hits = index.search_fused(&[1.0, 0.0, 0.0], &terms("Swatinem"), &[], 3, None);
        assert_eq!(paths(&index, &hits)[0], ".github/workflows/ci.yml");
    }

    /// Prose ranks below code but is not dropped: without declarations (no
    /// parser for the language) it is all there is.
    #[test]
    fn prose_still_fills_the_results_when_there_is_no_code() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        for i in 0..4 {
            let mut record = text_record(
                "notes.md",
                "retry backoff",
                vec![1.0 - i as f32 * 0.1, 0.0, 0.0],
            );
            record.symbol = None;
            record.start_line = i * 10 + 1;
            record.end_line = i * 10 + 5;
            index.records.push(record);
        }

        assert_eq!(index.search(&[1.0, 0.0, 0.0], 3).len(), 3);
    }

    #[test]
    fn each_file_places_one_result_before_any_places_a_second() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        index
            .records
            .push(placed("a.rs", 1, 10, vec![1.0, 0.0, 0.0]));
        index
            .records
            .push(placed("a.rs", 20, 30, vec![0.95, 0.05, 0.0]));
        index
            .records
            .push(placed("b.rs", 1, 10, vec![0.7, 0.3, 0.0]));

        let hits = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(paths(&index, &hits), vec!["a.rs", "b.rs"]);
        let hits = index.search(&[1.0, 0.0, 0.0], 3);
        assert_eq!(paths(&index, &hits), vec!["a.rs", "b.rs", "a.rs"]);
    }

    #[test]
    fn a_file_named_after_the_question_outranks_an_equal_match_elsewhere() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        index.records.push(text_record(
            "src/util.rs",
            "fn register(route) {}",
            vec![1.0, 0.0, 0.0],
        ));
        index.records.push(text_record(
            "src/routes.rs",
            "fn register(route) {}",
            vec![0.9, 0.1, 0.0],
        ));
        let hits = index.search_fused(&[1.0, 0.0, 0.0], &terms("register route"), &[], 2, None);
        assert_eq!(paths(&index, &hits), vec!["src/routes.rs", "src/util.rs"]);
    }

    #[test]
    fn two_records_describing_the_same_lines_yield_one_result() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        index
            .records
            .push(placed("a.rs", 1, 10, vec![1.0, 0.0, 0.0]));
        index
            .records
            .push(placed("a.rs", 8, 15, vec![0.9, 0.1, 0.0]));
        index
            .records
            .push(placed("a.rs", 20, 30, vec![0.8, 0.2, 0.0]));

        let hits = index.search(&[1.0, 0.0, 0.0], 10);
        assert_eq!(hits.len(), 2, "the overlapping pair collapses into one");
        assert_eq!(index.records[hits[0].record_idx].start_line, 1);
        assert_eq!(index.records[hits[1].record_idx].start_line, 20);
    }

    #[test]
    fn the_same_lines_in_different_files_are_both_kept() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        index
            .records
            .push(placed("a.rs", 1, 10, vec![1.0, 0.0, 0.0]));
        index
            .records
            .push(placed("b.rs", 1, 10, vec![0.9, 0.1, 0.0]));

        assert_eq!(index.search(&[1.0, 0.0, 0.0], 10).len(), 2);
    }

    /// Duplicates are collapsed before the cut to `k`, not after.
    #[test]
    fn a_full_set_of_results_survives_the_duplicates() {
        let mut index = Index::new(3, Backend::Hash, "test".into());
        for i in 0..10 {
            let line = i * 10 + 1;
            let vector = vec![1.0 - i as f32 * 0.01, 0.0, 0.0];
            index
                .records
                .push(placed("a.rs", line, line + 5, vector.clone()));
            index.records.push(placed("a.rs", line, line + 5, vector));
        }

        assert_eq!(index.search(&[1.0, 0.0, 0.0], 5).len(), 5);
    }

    #[test]
    fn a_query_with_no_indexable_terms_falls_back_to_pure_similarity() {
        let index = sample_index();
        let hits = index.search_fused(&[0.9, 0.1, 0.0], &terms("a b c"), &[], 2, None);
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

    /// `k` comes from `-k` or an MCP client. A huge value must return what
    /// exists, not abort on a capacity overflow.
    #[test]
    fn a_k_larger_than_the_index_returns_everything_it_has() {
        let index = sample_index();
        assert_eq!(index.search(&[1.0, 0.0, 0.0], usize::MAX).len(), 3);
    }

    #[test]
    fn keeps_the_best_k_in_order_without_sorting_the_rest() {
        let scores = [0.3, 0.9, 0.1, 0.7, 0.5, 0.8, 0.2];
        let hits = || -> Vec<Hit> {
            scores
                .iter()
                .enumerate()
                .map(|(record_idx, &score)| Hit { score, record_idx })
                .collect()
        };
        let kept = |k: usize| {
            let mut scored = hits();
            sort_and_truncate(&mut scored, k);
            scored.iter().map(|h| h.score).collect::<Vec<f32>>()
        };
        assert_eq!(kept(3), vec![0.9, 0.8, 0.7]);
        assert_eq!(kept(1), vec![0.9]);
        assert_eq!(kept(7), vec![0.9, 0.8, 0.7, 0.5, 0.3, 0.2, 0.1]);
        assert_eq!(kept(50), kept(7));
        assert!(kept(0).is_empty());
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
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["index.bin"]);
    }

    fn with_header(version: u32, body: &[u8]) -> Vec<u8> {
        [MAGIC.as_slice(), &version.to_le_bytes(), body].concat()
    }

    #[test]
    fn a_version_other_than_this_one_says_which_side_it_is_on() {
        let body = bitcode::encode(&sample_index());
        let err = |version| {
            Index::decode(&with_header(version, &body))
                .unwrap_err()
                .to_string()
        };
        assert!(err(FORMAT_VERSION - 1).contains("older ripsem"));
        assert!(err(FORMAT_VERSION + 1).contains("newer ripsem"));
        assert!(Index::decode(&with_header(FORMAT_VERSION, &body)).is_ok());
    }

    /// An empty index as ripsem wrote it before the header: bincode's legacy
    /// layout of format version 6, 3 dims, model "test", no records, no graph.
    #[test]
    fn an_index_from_before_the_header_is_reported_as_older() {
        let mut headerless = Vec::new();
        headerless.extend_from_slice(&6u32.to_le_bytes());
        headerless.extend_from_slice(&3u64.to_le_bytes());
        headerless.extend_from_slice(&4u64.to_le_bytes());
        headerless.extend_from_slice(b"test");
        headerless.extend_from_slice(&0u64.to_le_bytes());
        headerless.push(0);

        let dir = TempDir::new();
        let path = dir.path().join(".ripsem/index.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, headerless).unwrap();
        let err = Index::load(&path).unwrap_err().to_string();
        assert!(err.contains("from an older ripsem"), "got: {err}");
        assert!(err.contains("index --force"), "got: {err}");
    }

    #[test]
    fn a_cut_short_header_or_body_is_corrupt() {
        let err = Index::decode(&MAGIC[..]).unwrap_err().to_string();
        assert!(err.contains("unreadable or corrupt"), "got: {err}");
        let err = Index::decode(&with_header(FORMAT_VERSION, b"\x01"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unreadable or corrupt"), "got: {err}");
    }

    /// The header cannot tell one bitcode release from another, so a change in
    /// the bytes has to come with a new FORMAT_VERSION and fails here until it
    /// does. Update both together.
    #[test]
    fn the_encoding_is_pinned_to_the_format_version() {
        let bytes = bitcode::encode(&sample_index());
        assert_eq!(
            (FORMAT_VERSION, crate::fingerprint::content_hash(&bytes)),
            (11, 0x8aa4_2b7a_98ca_a8a0)
        );
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
    fn answers_only_queries_from_the_same_backend_and_model() {
        let index = sample_index();
        assert!(index.built_for(Backend::Hash, "test"));
        assert!(!index.built_for(Backend::Fastembed, "test"));
        assert!(!index.built_for(Backend::Hash, "other"));
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
        let mut index = Index::new(DIMS, Backend::Hash, "bench".into());
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

        let _ = index.search_fused(&queries[0].0, &queries[0].1, &[], 20, None);

        let start = std::time::Instant::now();
        for (vector, query_terms) in &queries {
            let _ = index.search_fused(vector, query_terms, &[], 20, None);
        }
        let elapsed = start.elapsed();
        println!(
            "bench_fused_search: {RECORDS} records, {QUERIES} queries → {:.2?} total ({:.2?}/query)",
            elapsed,
            elapsed / QUERIES as u32
        );
    }
}
