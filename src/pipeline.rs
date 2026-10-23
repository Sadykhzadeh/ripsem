use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use crate::config::Config;
use crate::index::{Index, Record};
use crate::lexical::TermHash;
use crate::{chunk, embed, fingerprint, walk};

#[derive(Debug)]
pub struct IndexStats {
    pub files: usize,
    /// Files read and cut again: new or changed.
    pub embedded: usize,
    pub reused: usize,
    pub chunks: usize,
    /// Chunks the model embedded. A changed file keeps the vectors of the
    /// chunks whose text it still holds.
    pub vectors: usize,
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
            if chunk::is_blank(&walk::decode_text(&bytes)) {
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

const SNIPPET_BYTES: usize = 400;

/// How many of the first hits are shown as their code, and how much of it.
const SHOWN_IN_FULL: usize = 3;
const FULL_LINES: usize = 40;
const FULL_BYTES: usize = 3000;

/// Hits fetched beyond the number asked for, to replace those a wider hit
/// above them covers.
const SPARE_HITS: usize = 5;

/// Queries about anything but code, to measure how closely unrelated text
/// matches an index.
const UNRELATED: [&str; 12] = [
    "a recipe for banana bread with walnuts",
    "the history of the roman empire",
    "how to change a flat bicycle tire",
    "symptoms of seasonal allergies in spring",
    "the best hiking trails in the alps",
    "rules for playing chess openings",
    "watercolor painting techniques for beginners",
    "migration patterns of arctic birds",
    "planning a budget for a family vacation",
    "the plot of a classic detective novel",
    "growing tomatoes on a balcony",
    "tips for a job interview",
];

/// The noise floor is measured again once more than one chunk in this many
/// has been embedded since it last was, counted across runs. One edit moves it
/// too little to matter.
const NOISE_DRIFT: usize = 10;

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
    let mut prev_floor = None;
    let mut embedded_since_floor = 0;
    let mut reusable = false;
    let mut recut: Vec<Record> = Vec::new();
    let chunking = chunk::Chunking::of(config);
    if let Some(prev) = prev
        && prev.dims == dims
        && prev.built_for(config.backend, &config.model)
    {
        prev_floor = prev.noise_floor;
        embedded_since_floor = prev.embedded_since_floor;
        if prev.chunking == chunking {
            reusable = true;
            prev_ann = prev.ann;
            for record in prev.records {
                prev_chunks
                    .entry(record.path.clone())
                    .or_default()
                    .push(record);
            }
        } else {
            recut = prev.records;
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

    let unchanged: std::collections::HashSet<String> = scanned
        .iter()
        .filter(|(_, _, changed)| !changed)
        .map(|&(i, _, _)| relative(&config.root, &files[i]))
        .collect();
    let known_vectors: HashMap<u64, crate::quant::QuantizedVector> = prev_chunks
        .iter()
        .filter(|(path, _)| !unchanged.contains(path.as_str()))
        .flat_map(|(_, records)| records)
        .chain(&recut)
        .map(|r| (r.text_hash, r.vector.clone()))
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
        let text = walk::decode_text(&bytes);
        let chunks = chunk::chunk_source(path, &text, config);
        if chunks.is_empty() {
            continue;
        }
        embedded += 1;
        for c in chunks {
            let text = model_text(&rel, c.symbol.as_ref(), &c.text);
            let text_hash = fingerprint::content_hash(text.as_bytes());
            let pending = Pending {
                path: rel.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                snippet: truncate(&dedent(&c.text), SNIPPET_BYTES),
                file_hash: hash,
                terms: crate::lexical::TermVector::new(
                    &c.text,
                    &rel,
                    c.symbol.as_ref().map(|s| s.name.as_str()),
                ),
                symbol: c.symbol,
                declarations: c.declarations,
                text_hash,
                vector: known_vectors.get(&text_hash).cloned(),
            };
            queue.push(pending, text);
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
        vectors: queue.embedded,
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
        && prev_ann.is_some() == wants_ann(index.records.len())
        && (prev_floor.is_some() || index.records.is_empty());
    if unchanged {
        return Ok(stats);
    }

    index.noise_floor = prev_floor;
    index.embedded_since_floor = embedded_since_floor + queue.embedded;
    let drifted = index.embedded_since_floor * NOISE_DRIFT > index.records.len();
    if !index.records.is_empty() && (prev_floor.is_none() || drifted) {
        let model = match &embedder {
            Some(built) => built,
            None => embedder.insert(embed::build(config)?),
        };
        let probes = UNRELATED
            .iter()
            .map(|text| model.embed_query(text))
            .collect::<Result<Vec<_>>>()?;
        index.measure_noise_floor(&probes);
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

/// Chunks per model call at most. The runtime pads every text to the longest
/// in the call, so groups of similar length waste little; much below this,
/// per-call overhead dominates for short texts.
const EMBED_GROUP: usize = 32;

/// Bytes per model call at most, counting every text at the length of the
/// longest. The runtime's memory grows with the padded batch, and with the
/// square of its length in attention, so long chunks go a few at a time.
const EMBED_BYTES: usize = 8192;

/// Splits indices sorted by text length into model calls within
/// [`EMBED_GROUP`] and [`EMBED_BYTES`]. A text longer than the byte budget
/// goes alone.
fn embed_groups<'a>(order: &'a [usize], texts: &[String]) -> Vec<&'a [usize]> {
    let mut groups = Vec::new();
    let mut start = 0;
    for end in 1..=order.len() {
        let size = end - start;
        let closes = match order.get(end) {
            None => true,
            Some(&next) => size == EMBED_GROUP || (size + 1) * texts[next].len() > EMBED_BYTES,
        };
        if closes {
            groups.push(&order[start..end]);
            start = end;
        }
    }
    groups
}

struct Pending {
    path: String,
    start_line: usize,
    end_line: usize,
    snippet: String,
    file_hash: u64,
    terms: crate::lexical::TermVector,
    symbol: Option<crate::symbol::Symbol>,
    declarations: Vec<crate::symbol::Declaration>,
    text_hash: u64,
    vector: Option<crate::quant::QuantizedVector>,
}

/// Chunks waiting for their vectors, kept in the order the walk found them.
#[derive(Default)]
struct Queue {
    pending: Vec<Pending>,
    texts: Vec<String>,
    embedded: usize,
    /// Chunks queued so far that need the model, the progress total.
    awaiting: usize,
    /// Progress is measured in the bytes the model reads: chunks go to it
    /// shortest first, so counting chunks would promise the end long before
    /// it comes.
    awaiting_bytes: usize,
    embedded_bytes: usize,
    reported: Option<std::time::Instant>,
}

impl Queue {
    fn push(&mut self, pending: Pending, text: String) {
        if pending.vector.is_none() {
            self.awaiting += 1;
            self.awaiting_bytes += model_bytes(&text);
        }
        self.pending.push(pending);
        self.texts.push(text);
    }

    /// The first index of a large repository embeds for minutes. A terminal
    /// shows the bar; a log, which the bar does not draw into, gets a line
    /// every [`REPORT_EVERY`] once embedding has lasted that long.
    fn report(&mut self, pb: &ProgressBar) {
        pb.set_length(self.awaiting_bytes as u64);
        pb.set_position(self.embedded_bytes as u64);
        pb.set_message(format!(
            "{}/{} chunks embedded",
            self.embedded, self.awaiting
        ));
        if std::io::stderr().is_terminal() {
            return;
        }
        let now = std::time::Instant::now();
        let last = *self.reported.get_or_insert(now);
        if now.duration_since(last) >= REPORT_EVERY {
            self.reported = Some(now);
            eprintln!("embedded {} of {} chunks", self.embedded, self.awaiting);
        }
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
        let mut order: Vec<usize> = (0..self.texts.len())
            .filter(|&i| self.pending[i].vector.is_none())
            .collect();
        let mut vectors: Vec<Vec<f32>> = vec![Vec::new(); self.texts.len()];
        if !order.is_empty() {
            self.embed(&mut order, &mut vectors, config, embedder, index.dims, pb)?;
        }
        for (p, vector) in self.pending.drain(..).zip(vectors) {
            index.records.push(Record {
                path: p.path,
                start_line: p.start_line,
                end_line: p.end_line,
                snippet: p.snippet,
                file_hash: p.file_hash,
                vector: p.vector.unwrap_or_else(|| crate::quant::quantize(&vector)),
                terms: p.terms,
                symbol: p.symbol,
                declarations: p.declarations,
                text_hash: p.text_hash,
            });
        }
        self.texts.clear();
        Ok(())
    }

    fn embed(
        &mut self,
        order: &mut [usize],
        vectors: &mut [Vec<f32>],
        config: &Config,
        embedder: &mut Option<Box<dyn embed::Embedder>>,
        dims: usize,
        pb: &ProgressBar,
    ) -> Result<()> {
        let embedder = match embedder {
            Some(built) => built,
            None => {
                let built = embed::build_for(config, self.awaiting)?;
                if built.dims() != dims {
                    anyhow::bail!(
                        "model {} embeds {} dimensions, expected {}",
                        config.model,
                        built.dims(),
                        dims
                    );
                }
                embedder.insert(built)
            }
        };
        pb.set_style(embedding_style());
        self.report(pb);
        order.sort_by_key(|&i| self.texts[i].len());
        for group in embed_groups(order, &self.texts) {
            let texts: Vec<String> = group
                .iter()
                .map(|&i| std::mem::take(&mut self.texts[i]))
                .collect();
            self.embedded_bytes += texts.iter().map(|t| model_bytes(t)).sum::<usize>();
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
            self.report(pb);
        }
        Ok(())
    }
}

/// The embedding model, built at most once however many threads ask for it.
struct LazyEmbedder {
    config: Config,
    built: OnceLock<Box<dyn embed::Embedder>>,
    building: Mutex<()>,
    warming: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LazyEmbedder {
    fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            built: OnceLock::new(),
            building: Mutex::new(()),
            warming: Mutex::new(None),
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
        let handle = std::thread::spawn(move || {
            let _ = embedder.get();
        });
        *self.warming.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// Waits for a model [`LazyEmbedder::warm`] started. A process that exits
    /// while the runtime is still being set up on another thread crashes on
    /// the way out.
    fn settle(&self) {
        let handle = self
            .warming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

pub struct Searcher {
    index: Index,
    root: std::path::PathBuf,
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
    // Taken before the decode: a rebuild landing during it leaves a newer
    // mtime than the one recorded, so the next refresh still picks it up.
    let index_mtime = file_mtime(&index_path);
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
    let model = index.model.clone();
    #[cfg(feature = "hnsw")]
    let ann_map = index.ann.as_deref().and_then(crate::ann::load);
    Ok(Searcher {
        index,
        root: config.root.clone(),
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
        // Every record ties on an empty query, so any answer would be noise.
        if query.trim().is_empty() {
            anyhow::bail!("the query is empty; say what to look for");
        }
        // A mistyped scope would otherwise answer "no matches", which reads as
        // the code not existing.
        if let Some(prefix) = prefix
            && !self.index.covers(prefix)
        {
            anyhow::bail!("nothing is indexed under '{prefix}'");
        }
        let qv = self.embedder.get()?.embed_query(query)?;
        let terms = crate::lexical::query_terms(query);
        let names = crate::lexical::query_names(query);
        let wanted = k;
        let k = k.saturating_add(SPARE_HITS);

        #[cfg(feature = "hnsw")]
        if prefix.is_none()
            && let Some(map) = &self.ann_map
        {
            let depth = crate::index::candidate_depth(k);
            let dense: Vec<crate::index::Hit> = crate::ann::search_map(map, &qv, depth)
                .into_iter()
                .map(|(record_idx, score)| crate::index::Hit { score, record_idx })
                .collect();
            let hits = self.index.search_fused_with(&dense, &qv, &terms, &names, k);
            return Ok(self.places(hits, &names, wanted));
        }

        let hits = self.index.search_fused(&qv, &terms, &names, k, prefix);
        Ok(self.places(hits, &names, wanted))
    }

    /// A hit reported as a whole declaration covers the records inside it
    /// that rank below, such as the rest of a long enum's variants.
    fn places(&self, hits: Vec<crate::index::Hit>, names: &[TermHash], k: usize) -> Vec<SearchHit> {
        let mut out: Vec<SearchHit> = Vec::with_capacity(k.min(hits.len()));
        let mut wider: Vec<(String, usize, usize)> = Vec::new();
        for h in hits {
            if out.len() == k {
                break;
            }
            let record = &self.index.records[h.record_idx];
            let covered = wider.iter().any(|(path, start, end)| {
                *path == record.path && *start <= record.start_line && record.end_line <= *end
            });
            if covered {
                continue;
            }
            let mut hit = self.hit(h.record_idx, h.score, names);
            if hit.end_line > record.end_line {
                wider.push((hit.path.clone(), hit.start_line, hit.end_line));
            }
            if out.len() < SHOWN_IN_FULL
                && let Some(code) = self.code(record, &hit)
            {
                hit.snippet = code;
            }
            out.push(hit);
        }
        out
    }

    /// The first hits are shown as their code, up to [`FULL_LINES`]: whoever
    /// asked reads the answer without opening the file, which for an agent is
    /// a whole turn saved. A doc record's snippet is its prose, so this is
    /// also where its code appears.
    fn code(&self, record: &Record, hit: &SearchHit) -> Option<String> {
        let end = hit.end_line.min(hit.start_line + FULL_LINES - 1);
        let mut code = self.lines(record, hit.start_line, end, FULL_BYTES)?;
        if end < hit.end_line && !code.ends_with('…') {
            code.push_str("\n…");
        }
        Some(code)
    }

    /// A record holding the declaration the query names is reported as that
    /// declaration: its label, its lines and a snippet that starts there. The
    /// first piece of a declaration too large for one chunk reports all of it,
    /// unless it is a container, whose members answer for themselves.
    fn hit(&self, record_idx: usize, score: f32, names: &[TermHash]) -> SearchHit {
        let r = &self.index.records[record_idx];
        let whole = || SearchHit {
            path: r.path.clone(),
            start_line: r.start_line,
            end_line: r.end_line,
            score,
            snippet: r.snippet.clone(),
            symbol: r.symbol.clone(),
        };
        let Some(declaration) = r.declaration_named(names) else {
            return whole();
        };
        let start_line = declaration.start_line.max(r.start_line);
        let end_line = match declaration.is_container() {
            true => declaration.end_line.min(r.end_line),
            false => declaration.end_line,
        }
        .max(start_line);
        let snippet = match (start_line, end_line) == (r.start_line, r.end_line) {
            true => Some(r.snippet.clone()),
            false => self.lines(r, start_line, end_line, SNIPPET_BYTES),
        };
        match snippet {
            Some(snippet) => SearchHit {
                path: r.path.clone(),
                start_line,
                end_line,
                score,
                snippet,
                symbol: Some(declaration.symbol.clone()),
            },
            None => whole(),
        }
    }

    /// Lines of an indexed file as they were indexed. A file changed since then
    /// has nothing to offer: its lines may have moved.
    fn lines(&self, record: &Record, start: usize, end: usize, budget: usize) -> Option<String> {
        let bytes = std::fs::read(self.root.join(&record.path)).ok()?;
        if fingerprint::content_hash(&bytes) != record.file_hash {
            return None;
        }
        let text = walk::decode_text(&bytes);
        let wanted: Vec<&str> = text.lines().skip(start - 1).take(end + 1 - start).collect();
        Some(truncate(&dedent(&wanted.join("\n")), budget))
    }
}

pub fn search(config: &Config, query: &str, k: usize) -> Result<Vec<SearchHit>> {
    open(config)?.query(query, k)
}

/// Like [`search`], limited to a normalized repo-relative path prefix (see
/// `mcp::scope_prefix`), after updating the index for files changed since it
/// was built. `None` searches the whole index.
pub fn search_scoped(
    config: &Config,
    query: &str,
    k: usize,
    prefix: Option<&str>,
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        anyhow::bail!("the query is empty; say what to look for");
    }
    // A repository's first search builds its index rather than sending the
    // user off to run `ripsem index`. Outside a repository the directory may
    // be a home folder, which nobody wants embedded by accident.
    if !config.index_path().exists() && config.root.join(".git").exists() {
        eprintln!("no index yet; building one, which later searches only update");
        index_repo(config, false)?;
    }
    let embedder = Arc::new(LazyEmbedder::new(config));
    // Loading the model and decoding the index are independent, so they run
    // side by side. Without an index there is nothing to search and no reason
    // to load (or download) a model.
    if config.index_path().exists() {
        embedder.warm();
        refresh(config);
    }
    let found = open_with(config, Arc::clone(&embedder))
        .and_then(|searcher| searcher.query_scoped(query, k, prefix));
    if found.is_err() {
        embedder.settle();
    }
    found
}

/// Brings the index up to date with the tree while the model loads, so an
/// edit made since the last `ripsem index` is found where it is now. When
/// nothing changed this only walks and hashes the files. A failure, such as a
/// read-only checkout, leaves the search to the index as it is.
fn refresh(config: &Config) {
    match index_repo(config, false) {
        Ok(stats) if stats.embedded > 0 => eprintln!(
            "updated the index: {} changed",
            crate::cli::plural(stats.embedded, "file")
        ),
        Ok(_) => {}
        Err(e) => eprintln!("warning: could not update the index: {e:#}"),
    }
}

/// Display `path` relative to `root` with forward slashes, matching the
/// repo-relative paths the index and search output use.
pub(crate) fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Strips the indentation every line shares, so a method deep inside a class
/// reads from the margin and the snippet spends no bytes on it.
fn dedent(text: &str) -> String {
    let indent = |line: &str| line.len() - line.trim_start_matches([' ', '\t']).len();
    let common = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(indent)
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|line| line.get(common.min(indent(line))..).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
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
    if let Ok(style) = ProgressStyle::with_template("{spinner} {pos}/{len} files") {
        pb.set_style(style);
    }
    pb
}

fn embedding_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {bar:30} {msg}, {eta} left")
        .unwrap_or_else(|_| ProgressStyle::default_bar())
}

/// What the model reads for a chunk: its path and the declaration it belongs
/// to, then its text. A method's body rarely names its file or its type, and a
/// question about behaviour often does.
fn model_text(path: &str, symbol: Option<&crate::symbol::Symbol>, text: &str) -> String {
    match symbol {
        Some(symbol) => format!("{path}\n{}\n{text}", symbol.label()),
        None => format!("{path}\n{text}"),
    }
}

/// The models read at most 512 tokens, a few bytes each, of a longer text.
fn model_bytes(text: &str) -> usize {
    text.len().min(2048)
}

const REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Backend;

    #[test]
    fn the_model_reads_a_chunk_under_its_path_and_declaration() {
        let symbol = crate::symbol::Symbol {
            kind: "fn".into(),
            name: "Session::refresh".into(),
        };
        assert_eq!(
            model_text("src/auth.rs", Some(&symbol), "body"),
            "src/auth.rs\nfn Session::refresh\nbody"
        );
        assert_eq!(model_text("README.md", None, "text"), "README.md\ntext");
    }

    fn hash_config() -> Config {
        Config {
            backend: Backend::Hash,
            ..Default::default()
        }
    }

    #[test]
    fn a_snippet_keeps_only_the_indentation_its_lines_do_not_share() {
        let method = "    def load(self):\n\n        return self.path\n";
        assert_eq!(dedent(method), "def load(self):\n\n    return self.path");
        assert_eq!(dedent("\tif x:\n\t\ty()"), "if x:\n\ty()");
        assert_eq!(dedent("a\n  b"), "a\n  b");
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
    fn long_texts_are_embedded_a_few_at_a_time() {
        let mut texts: Vec<String> = (0..40).map(|_| "x".repeat(100)).collect();
        texts.extend((0..10).map(|_| "y".repeat(2000)));
        texts.push("z".repeat(10_000));
        let order: Vec<usize> = (0..texts.len()).collect();
        let sizes: Vec<usize> = embed_groups(&order, &texts)
            .iter()
            .map(|g| g.len())
            .collect();
        // 32 short ones, then the other 8, since a long one would pad all nine
        // to 2000 bytes. The long ones go four at a time, the longest alone.
        assert_eq!(sizes, [32, 8, 4, 4, 2, 1]);
        assert_eq!(sizes.iter().sum::<usize>(), texts.len());
        assert!(embed_groups(&[], &texts).is_empty());
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
