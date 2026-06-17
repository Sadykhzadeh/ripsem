use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::config::Config;
use crate::index::{Index, Record};
use crate::{chunk, embed, fingerprint, walk};

pub struct IndexStats {
    pub files: usize,
    pub embedded: usize,
    pub reused: usize,
    pub chunks: usize,
}

pub struct SearchHit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f32,
    pub snippet: String,
}

pub fn index_repo(config: &Config, force: bool) -> Result<IndexStats> {
    let embedder = embed::build(config)?;
    let dims = embedder.dims();
    let files = walk::collect(config);

    let prev = if !force && config.index_path().exists() {
        Index::load(&config.index_path()).ok()
    } else {
        None
    };
    let mut prev_chunks: HashMap<String, Vec<Record>> = HashMap::new();
    if let Some(prev) = prev {
        if prev.dims == dims && prev.model == config.model {
            for record in prev.records {
                prev_chunks.entry(record.path.clone()).or_default().push(record);
            }
        }
    }

    let mut index = Index::new(dims, config.model.clone());
    let mut embedded = 0;
    let mut reused = 0;

    let pb = progress(files.len() as u64);
    for path in &files {
        pb.inc(1);
        let rel = relative(&config.root, path);
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let hash = fingerprint::content_hash(&bytes);

        if let Some(records) = prev_chunks.get_mut(&rel) {
            if records.first().map(|r| r.file_hash) == Some(hash) {
                index.records.append(records);
                reused += 1;
                continue;
            }
        }

        let text = String::from_utf8_lossy(&bytes);
        let chunks = chunk::chunk_file(text.as_ref(), config.chunk_lines, config.chunk_overlap);
        if chunks.is_empty() {
            continue;
        }
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let vectors = embedder.embed_documents(&texts)?;
        for (c, vector) in chunks.into_iter().zip(vectors) {
            index.records.push(Record {
                path: rel.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                snippet: truncate(&c.text, 400),
                file_hash: hash,
                vector,
            });
        }
        embedded += 1;
    }
    pb.finish_and_clear();

    index.save(&config.index_path())?;
    Ok(IndexStats {
        files: files.len(),
        embedded,
        reused,
        chunks: index.records.len(),
    })
}

pub struct Searcher {
    index: Index,
    embedder: Box<dyn embed::Embedder>,
}

pub fn open(config: &Config) -> Result<Searcher> {
    let index = Index::load(&config.index_path())
        .context("no index yet; run `ripsem index` in this directory first")?;
    let embedder = embed::build(config)?;
    Ok(Searcher { index, embedder })
}

impl Searcher {
    pub fn query(&self, query: &str, k: usize) -> Result<Vec<SearchHit>> {
        let qv = self.embedder.embed_query(query)?;
        Ok(self
            .index
            .search(&qv, k)
            .into_iter()
            .map(|h| {
                let r = &self.index.records[h.record_idx];
                SearchHit {
                    path: r.path.clone(),
                    start_line: r.start_line,
                    end_line: r.end_line,
                    score: h.score,
                    snippet: r.snippet.clone(),
                }
            })
            .collect())
    }
}

pub fn search(config: &Config, query: &str, k: usize) -> Result<Vec<SearchHit>> {
    open(config)?.query(query, k)
}

fn relative(root: &Path, path: &Path) -> String {
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
    if let Ok(style) =
        ProgressStyle::with_template("{spinner} {pos}/{len} files  {wide_msg}")
    {
        pb.set_style(style);
    }
    pb
}
