use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use crate::config::Config;
use crate::index::{Index, Record};
use crate::{chunk, embed, fingerprint, walk};

#[derive(Debug)]
pub struct IndexStats {
    pub files: usize,
    pub embedded: usize,
    pub reused: usize,
    pub chunks: usize,
}

/// How far the index has fallen behind the files it was built from. Hits carry
/// the line ranges their file had when it was indexed, so a stale index points
/// at places that may have moved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Drift {
    /// On disk, not in the index.
    pub added: usize,
    /// In both, with different content.
    pub modified: usize,
    /// In the index, no longer on disk.
    pub removed: usize,
}

impl Drift {
    pub fn is_current(&self) -> bool {
        self.added + self.modified + self.removed == 0
    }
}

/// Measures drift with the same walk and content hash the incremental index
/// uses, so it reports what a rebuild would touch. A file with no chunks
/// ([`chunk::is_blank`]) is not counted as new, or the count could never reach
/// zero. Under other chunk settings a rebuild cuts every file again, so every
/// indexed file counts as changed.
pub fn drift(config: &Config, index: &Index) -> Result<Drift> {
    let indexed = index.file_hashes();
    let rechunk = index.chunking != chunk::Chunking::of(config);
    let present: Vec<(String, u64)> = walk::collect(config)?
        .par_iter()
        .filter_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            if chunk::is_blank(&String::from_utf8_lossy(&bytes)) {
                return None;
            }
            Some((
                relative(&config.root, path),
                fingerprint::content_hash(&bytes),
            ))
        })
        .collect();

    let mut drift = Drift::default();
    let mut on_disk: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(present.len());
    for (path, hash) in &present {
        on_disk.insert(path.as_str());
        match indexed.get(path.as_str()) {
            Some(known) if known == hash && !rechunk => {}
            Some(_) => drift.modified += 1,
            None => drift.added += 1,
        }
    }
    drift.removed = indexed.keys().filter(|p| !on_disk.contains(*p)).count();
    Ok(drift)
}

#[derive(Debug)]
pub struct SearchHit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f32,
    pub snippet: String,
    pub symbol: Option<crate::symbol::Symbol>,
}

pub fn index_repo(config: &Config, force: bool) -> Result<IndexStats> {
    // Loaded on the first chunk that needs a vector: a reindex that finds
    // nothing changed, or only files removed, never needs the model.
    let mut embedder: Option<Box<dyn embed::Embedder>> = None;
    let dims = config.dims();
    let files = walk::collect(config)?;

    let prev = if !force && config.index_path().exists() {
        Index::load(&config.index_path()).ok()
    } else {
        None
    };
    let mut prev_chunks: HashMap<String, Vec<Record>> = HashMap::new();
    let mut prev_ann = None;
    let mut reusable = false;
    let chunking = chunk::Chunking::of(config);
    if let Some(prev) = prev
        && prev.dims == dims
        && prev.built_for(config.backend, &config.model)
        && prev.chunking == chunking
    {
        reusable = true;
        prev_ann = prev.ann;
        for record in prev.records {
            prev_chunks
                .entry(record.path.clone())
                .or_default()
                .push(record);
        }
    }
    let prev_hash: HashMap<&str, u64> = prev_chunks
        .iter()
        .filter_map(|(p, recs)| recs.first().map(|r| (p.as_str(), r.file_hash)))
        .collect();

    let scanned: Vec<(usize, u64, bool)> = files
        .par_iter()
        .enumerate()
        .filter_map(|(i, path)| {
            let bytes = std::fs::read(path).ok()?;
            let hash = fingerprint::content_hash(&bytes);
            let rel = relative(&config.root, path);
            let changed = prev_hash.get(rel.as_str()) != Some(&hash);
            Some((i, hash, changed))
        })
        .collect();

    let mut index = Index::new(dims, config.backend, config.model.clone());
    index.chunking = chunking;
    let mut embedded = 0;
    let mut reused = 0;
    let mut queue = Queue::default();

    let pb = progress(scanned.len() as u64);
    for (i, hash, changed) in scanned {
        pb.inc(1);
        let path = &files[i];
        let rel = relative(&config.root, path);

        if !changed && let Some(records) = prev_chunks.get_mut(&rel) {
            index.records.append(records);
            reused += 1;
            continue;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&bytes);
        let chunks = chunk::chunk_source(path, text.as_ref(), config);
        if chunks.is_empty() {
            continue;
        }
        embedded += 1;
        for c in chunks {
            let pending = Pending {
                path: rel.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                snippet: truncate(&c.text, 400),
                file_hash: hash,
                terms: crate::lexical::TermVector::new(
                    &c.text,
                    &rel,
                    c.symbol.as_ref().map(|s| s.name.as_str()),
                ),
                symbol: c.symbol,
            };
            queue.push(pending, c.text);
            if queue.is_full() {
                queue.flush(config, &mut embedder, &mut index, &pb)?;
            }
        }
    }
    queue.flush(config, &mut embedder, &mut index, &pb)?;
    pb.finish_and_clear();

    let stats = IndexStats {
        files: files.len(),
        embedded,
        reused,
        chunks: index.records.len(),
    };
    // Before the early return below, so a data directory that lacks its
    // `.gitignore` gets one even when nothing needs reindexing.
    config.create_data_dir()?;
    // Nothing was embedded or dropped, so the file on disk is already right.
    // Rewriting it would rebuild the graph and bump the mtime, which makes a
    // running server reload it.
    let unchanged = reusable
        && embedded == 0
        && prev_chunks.values().all(Vec::is_empty)
        && prev_ann.is_some() == wants_ann(index.records.len());
    if unchanged {
        return Ok(stats);
    }

    #[cfg(feature = "hnsw")]
    if wants_ann(index.records.len()) {
        let vectors: Vec<Vec<f32>> = index
            .records
            .iter()
            .map(|r| crate::quant::dequantize(&r.vector))
            .collect();
        index.ann = crate::ann::build(vectors);
    }

    index.save(&config.index_path())?;
    Ok(stats)
}

fn wants_ann(records: usize) -> bool {
    #[cfg(feature = "hnsw")]
    {
        records >= crate::ann::MIN_RECORDS
    }
    #[cfg(not(feature = "hnsw"))]
    {
        let _ = records;
        false
    }
}

/// Chunks queued before embedding. They are sorted by length first, so a larger
/// window gives groups of closer length.
const EMBED_WINDOW: usize = 4096;

/// Chunks per model call. The runtime pads every text to the longest in the
/// call, so groups of similar length waste little; much below this, per-call
/// overhead dominates.
const EMBED_GROUP: usize = 32;

struct Pending {
    path: String,
    start_line: usize,
    end_line: usize,
    snippet: String,
    file_hash: u64,
    terms: crate::lexical::TermVector,
    symbol: Option<crate::symbol::Symbol>,
}

/// Chunks waiting for their vectors, kept in the order the walk found them.
#[derive(Default)]
struct Queue {
    pending: Vec<Pending>,
    texts: Vec<String>,
    embedded: usize,
}

impl Queue {
    fn push(&mut self, pending: Pending, text: String) {
        self.pending.push(pending);
        self.texts.push(text);
    }

    fn is_full(&self) -> bool {
        self.texts.len() >= EMBED_WINDOW
    }

    /// Embeds shortest first, in groups of [`EMBED_GROUP`], but appends the
    /// records in walk order.
    fn flush(
        &mut self,
        config: &Config,
        embedder: &mut Option<Box<dyn embed::Embedder>>,
        index: &mut Index,
        pb: &ProgressBar,
    ) -> Result<()> {
        if self.texts.is_empty() {
            return Ok(());
        }
        let embedder = match embedder {
            Some(built) => built,
            None => {
                let built = embed::build(config)?;
                if built.dims() != index.dims {
                    anyhow::bail!(
                        "model {} embeds {} dimensions, expected {}",
                        config.model,
                        built.dims(),
                        index.dims
                    );
                }
                embedder.insert(built)
            }
        };
        let mut order: Vec<usize> = (0..self.texts.len()).collect();
        order.sort_by_key(|&i| self.texts[i].len());
        let mut vectors: Vec<Vec<f32>> = vec![Vec::new(); self.texts.len()];
        for group in order.chunks(EMBED_GROUP) {
            let texts: Vec<String> = group
                .iter()
                .map(|&i| std::mem::take(&mut self.texts[i]))
                .collect();
            let embedded = embedder.embed_documents(&texts)?;
            if embedded.len() != texts.len() {
                anyhow::bail!(
                    "the embedding backend returned {} vectors for {} chunks",
                    embedded.len(),
                    texts.len()
                );
            }
            for (&i, vector) in group.iter().zip(embedded) {
                vectors[i] = vector;
            }
            self.embedded += group.len();
            pb.set_message(format!("{} chunks embedded", self.embedded));
        }
        for (p, vector) in self.pending.drain(..).zip(vectors) {
            index.records.push(Record {
                path: p.path,
                start_line: p.start_line,
                end_line: p.end_line,
                snippet: p.snippet,
                file_hash: p.file_hash,
                vector: crate::quant::quantize(&vector),
                terms: p.terms,
                symbol: p.symbol,
            });
        }
        self.texts.clear();
        Ok(())
    }
}

/// The embedding model, built at most once however many threads ask for it.
struct LazyEmbedder {
    config: Config,
    built: OnceLock<Box<dyn embed::Embedder>>,
    building: Mutex<()>,
}

impl LazyEmbedder {
    fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            built: OnceLock::new(),
            building: Mutex::new(()),
        }
    }

    /// Builds the model on first use. A failed build is not remembered, so the
    /// next call tries again.
    fn get(&self) -> Result<&dyn embed::Embedder> {
        if let Some(e) = self.built.get() {
            return Ok(e.as_ref());
        }
        let _turn = self.building.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = self.built.get() {
            return Ok(e.as_ref());
        }
        let built = embed::build(&self.config)?;
        Ok(self.built.get_or_init(|| built).as_ref())
    }

    /// Starts building the model on another thread. Errors are left for the
    /// next [`LazyEmbedder::get`] to report.
    fn warm(self: &Arc<Self>) {
        let embedder = Arc::clone(self);
        std::thread::spawn(move || {
            let _ = embedder.get();
        });
    }
}

pub struct Searcher {
    index: Index,
    // Built on the first query, so opening an index does not pay for the model.
    embedder: Arc<LazyEmbedder>,
    index_path: std::path::PathBuf,
    index_mtime: Option<std::time::SystemTime>,
    // The model the index was opened with. A reload under a different model
    // or backend is refused even when the dimensions match.
    model: String,
    #[cfg(feature = "hnsw")]
    ann_map: Option<instant_distance::HnswMap<crate::ann::Embedding, u32>>,
}

pub fn open(config: &Config) -> Result<Searcher> {
    open_with(config, Arc::new(LazyEmbedder::new(config)))
}

fn open_with(config: &Config, embedder: Arc<LazyEmbedder>) -> Result<Searcher> {
    let index_path = config.index_path();
    if !index_path.exists() {
        anyhow::bail!("no index yet; run `ripsem index` in this directory first");
    }
    // Only a missing file means there is no index. Any other failure carries
    // its own advice, such as rebuilding an index from an older ripsem.
    let index = Index::load(&index_path)?;
    // Queries are embedded with the configured model and backend; against an
    // index built with another the scores would mean nothing.
    if !index.built_for(config.backend, &config.model) {
        anyhow::bail!(
            "index was built with {}, but this directory is configured for {}; \
             run `ripsem index --force` to rebuild it",
            index.built_with(),
            crate::index::embedded_by(config.backend, &config.model)
        );
    }
    let index_mtime = file_mtime(&index_path);
    let model = index.model.clone();
    #[cfg(feature = "hnsw")]
    let ann_map = index.ann.as_deref().and_then(crate::ann::load);
    Ok(Searcher {
        index,
        embedder,
        index_path,
        index_mtime,
        model,
        #[cfg(feature = "hnsw")]
        ann_map,
    })
}

fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Searcher {
    /// Reloads the index if its file has changed, and says whether it did. A
    /// file this searcher cannot use, unreadable or built for another model or
    /// backend, is an error once and is not read again until it changes, while
    /// the index already held keeps answering.
    pub fn refresh(&mut self) -> Result<bool> {
        let mtime = file_mtime(&self.index_path);
        if mtime == self.index_mtime {
            return Ok(false);
        }
        self.index_mtime = mtime;
        let index = Index::load(&self.index_path)?;
        if index.dims != self.index.dims || !index.built_for(self.index.backend, &self.model) {
            anyhow::bail!(
                "index was rebuilt with {}, and this search embeds queries with {}",
                index.built_with(),
                self.index.built_with()
            );
        }
        #[cfg(feature = "hnsw")]
        {
            self.ann_map = index.ann.as_deref().and_then(crate::ann::load);
        }
        self.index = index;
        Ok(true)
    }

    /// The embedding model this searcher was opened with.
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn record_count(&self) -> usize {
        self.index.records.len()
    }

    /// Starts loading the embedding model in the background, so the first
    /// query does not have to wait for it.
    pub fn warm(&self) {
        self.embedder.warm();
    }

    pub fn query(&self, query: &str, k: usize) -> Result<Vec<SearchHit>> {
        self.query_scoped(query, k, None)
    }

    pub fn query_scoped(
        &self,
        query: &str,
        k: usize,
        prefix: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let qv = self.embedder.get()?.embed_query(query)?;
        let terms = crate::lexical::query_terms(query);

        #[cfg(feature = "hnsw")]
        if prefix.is_none()
            && let Some(map) = &self.ann_map
        {
            let depth = crate::index::candidate_depth(k);
            let dense: Vec<crate::index::Hit> = crate::ann::search_map(map, &qv, depth)
                .into_iter()
                .map(|(record_idx, score)| crate::index::Hit { score, record_idx })
                .collect();
            return Ok(self
                .index
                .search_fused_with(&dense, &qv, &terms, k)
                .into_iter()
                .map(|h| self.hit(h.record_idx, h.score))
                .collect());
        }

        Ok(self
            .index
            .search_fused(&qv, &terms, k, prefix)
            .into_iter()
            .map(|h| self.hit(h.record_idx, h.score))
            .collect())
    }

    fn hit(&self, record_idx: usize, score: f32) -> SearchHit {
        let r = &self.index.records[record_idx];
        SearchHit {
            path: r.path.clone(),
            start_line: r.start_line,
            end_line: r.end_line,
            score,
            snippet: r.snippet.clone(),
            symbol: r.symbol.clone(),
        }
    }
}

pub fn search(config: &Config, query: &str, k: usize) -> Result<Vec<SearchHit>> {
    open(config)?.query(query, k)
}

/// Like [`search`], limited to a normalized repo-relative path prefix (see
/// `mcp::scope_prefix`). `None` searches the whole index.
pub fn search_scoped(
    config: &Config,
    query: &str,
    k: usize,
    prefix: Option<&str>,
) -> Result<Vec<SearchHit>> {
    let embedder = Arc::new(LazyEmbedder::new(config));
    // Loading the model and decoding the index are independent, so they run
    // side by side. Without an index there is nothing to search and no reason
    // to load (or download) a model.
    if config.index_path().exists() {
        embedder.warm();
    }
    open_with(config, embedder)?.query_scoped(query, k, prefix)
}

/// Display `path` relative to `root` with forward slashes, matching the
/// repo-relative paths the index and search output use.
pub(crate) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn progress(len: u64) -> ProgressBar {
    let pb = ProgressBar::new(len);
    if let Ok(style) = ProgressStyle::with_template("{spinner} {pos}/{len} files  {wide_msg}") {
        pb.set_style(style);
    }
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Backend;

    fn hash_config() -> Config {
        Config {
            backend: Backend::Hash,
            ..Default::default()
        }
    }

    #[test]
    fn a_warmed_embedder_is_the_one_later_callers_get() {
        let embedder = Arc::new(LazyEmbedder::new(&hash_config()));
        embedder.warm();
        let first = embedder.get().unwrap().embed_query("anything").unwrap();
        let second = embedder.get().unwrap().embed_query("anything").unwrap();
        assert_eq!(first, second);
    }

    #[cfg(not(feature = "fastembed"))]
    #[test]
    fn a_failed_build_is_tried_again_on_the_next_call() {
        let embedder = LazyEmbedder::new(&Config {
            backend: Backend::Fastembed,
            ..Default::default()
        });
        assert!(embedder.get().is_err());
        assert!(embedder.get().is_err());
    }

    #[test]
    fn relative_strips_root_and_uses_forward_slashes() {
        let root = Path::new("/repo");
        assert_eq!(
            relative(root, Path::new("/repo/src/main.rs")),
            "src/main.rs"
        );
        assert_eq!(relative(Path::new("a"), Path::new("a/b\\c")), "b/c");
        // Outside the root: returned as it is.
        assert_eq!(relative(root, Path::new("/other/x.rs")), "/other/x.rs");
    }
}
