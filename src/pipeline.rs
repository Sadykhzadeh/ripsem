use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
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
    let embedder = embed::build(config)?;
    let dims = embedder.dims();
    let files = walk::collect(config)?;

    let prev = if !force && config.index_path().exists() {
        Index::load(&config.index_path()).ok()
    } else {
        None
    };
    let mut prev_chunks: HashMap<String, Vec<Record>> = HashMap::new();
    if let Some(prev) = prev {
        if prev.dims == dims && prev.model == config.model {
            for record in prev.records {
                prev_chunks
                    .entry(record.path.clone())
                    .or_default()
                    .push(record);
            }
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

    let mut index = Index::new(dims, config.model.clone());
    let mut embedded = 0;
    let mut reused = 0;
    let mut pending: Vec<Pending> = Vec::new();
    let mut texts: Vec<String> = Vec::new();

    let pb = progress(scanned.len() as u64);
    for (i, hash, changed) in scanned {
        pb.inc(1);
        let path = &files[i];
        let rel = relative(&config.root, path);

        if !changed {
            if let Some(records) = prev_chunks.get_mut(&rel) {
                index.records.append(records);
                reused += 1;
                continue;
            }
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
            pending.push(Pending {
                path: rel.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                snippet: truncate(&c.text, 400),
                file_hash: hash,
                terms: crate::lexical::TermVector::new(&c.text, &rel),
                symbol: c.symbol,
            });
            texts.push(c.text);
            if texts.len() >= EMBED_BATCH {
                flush(embedder.as_ref(), &mut pending, &mut texts, &mut index)?;
            }
        }
    }
    flush(embedder.as_ref(), &mut pending, &mut texts, &mut index)?;
    pb.finish_and_clear();

    #[cfg(feature = "hnsw")]
    if index.records.len() >= crate::ann::MIN_RECORDS {
        let vectors: Vec<Vec<f32>> = index
            .records
            .iter()
            .map(|r| crate::quant::dequantize(&r.vector))
            .collect();
        index.ann = crate::ann::build(vectors);
    }

    index.save(&config.index_path())?;
    Ok(IndexStats {
        files: files.len(),
        embedded,
        reused,
        chunks: index.records.len(),
    })
}

const EMBED_BATCH: usize = 256;

struct Pending {
    path: String,
    start_line: usize,
    end_line: usize,
    snippet: String,
    file_hash: u64,
    terms: crate::lexical::TermVector,
    symbol: Option<crate::symbol::Symbol>,
}

fn flush(
    embedder: &dyn embed::Embedder,
    pending: &mut Vec<Pending>,
    texts: &mut Vec<String>,
    index: &mut Index,
) -> Result<()> {
    if texts.is_empty() {
        return Ok(());
    }
    let vectors = embedder.embed_documents(texts)?;
    for (p, vector) in pending.drain(..).zip(vectors) {
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
    texts.clear();
    Ok(())
}

pub struct Searcher {
    index: Index,
    // Built on the first query rather than at open time: opening an index to
    // check its status or refresh it shouldn't pay the model-load cost.
    config: Config,
    embedder: std::sync::OnceLock<Box<dyn embed::Embedder>>,
    index_path: std::path::PathBuf,
    index_mtime: Option<std::time::SystemTime>,
    // The embedder is built once for the model the index was opened with; a
    // hot-reload that swapped in a different same-dim model would be incomparable.
    model: String,
    #[cfg(feature = "hnsw")]
    ann_map: Option<instant_distance::HnswMap<crate::ann::Embedding, u32>>,
}

pub fn open(config: &Config) -> Result<Searcher> {
    let index_path = config.index_path();
    let index = Index::load(&index_path)
        .context("no index yet; run `ripsem index` in this directory first")?;
    let index_mtime = file_mtime(&index_path);
    let model = index.model.clone();
    #[cfg(feature = "hnsw")]
    let ann_map = index.ann.as_deref().and_then(crate::ann::load);
    Ok(Searcher {
        index,
        config: config.clone(),
        embedder: std::sync::OnceLock::new(),
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
    pub fn refresh(&mut self) -> Result<bool> {
        let mtime = file_mtime(&self.index_path);
        if mtime == self.index_mtime {
            return Ok(false);
        }
        let index = Index::load(&self.index_path)?;
        if index.dims != self.index.dims {
            return Ok(false);
        }
        // A same-dim index rebuilt with a different model produces vectors that
        // are not comparable to queries from our embedder; keep the old index.
        if index.model != self.model {
            return Ok(false);
        }
        #[cfg(feature = "hnsw")]
        {
            self.ann_map = index.ann.as_deref().and_then(crate::ann::load);
        }
        self.index = index;
        self.index_mtime = mtime;
        Ok(true)
    }

    /// The embedding model this searcher was opened with. A hot-reload that
    /// would change this is rejected (see [`Searcher::refresh`]).
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn record_count(&self) -> usize {
        self.index.records.len()
    }

    /// Build the embedding model on first use and return it. Subsequent calls
    /// reuse the same instance.
    fn embedder(&self) -> Result<&dyn embed::Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e.as_ref());
        }
        let built = embed::build(&self.config)?;
        // A concurrent caller may have won the race; either way one instance wins.
        let _ = self.embedder.set(built);
        Ok(self.embedder.get().expect("embedder set above").as_ref())
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
        let qv = self.embedder()?.embed_query(query)?;
        let terms = crate::lexical::query_terms(query);

        #[cfg(feature = "hnsw")]
        if prefix.is_none() {
            if let Some(map) = &self.ann_map {
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

/// Like [`search`], but limit results to a repo-relative path prefix (already
/// normalized — see `mcp::scope_prefix`). `None` searches the whole index.
pub fn search_scoped(
    config: &Config,
    query: &str,
    k: usize,
    prefix: Option<&str>,
) -> Result<Vec<SearchHit>> {
    open(config)?.query_scoped(query, k, prefix)
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
    use super::relative;
    use std::path::Path;

    #[test]
    fn relative_strips_root_and_uses_forward_slashes() {
        let root = Path::new("/repo");
        assert_eq!(
            relative(root, Path::new("/repo/src/main.rs")),
            "src/main.rs"
        );
        // Backslashes in a component are normalised to forward slashes so the
        // output matches index/search paths on Windows.
        assert_eq!(relative(Path::new("a"), Path::new("a/b\\c")), "b/c");
        // A path outside root is returned as-is (still slash-normalised).
        assert_eq!(relative(root, Path::new("/other/x.rs")), "/other/x.rs");
    }
}
