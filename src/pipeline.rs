use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use regex::Regex;

use crate::config::Config;
use crate::index::{Index, Record};
use crate::lexical::TermHash;
use crate::scope::Scope;
use crate::{chunk, embed, fingerprint, walk};

#[derive(Debug)]
pub struct IndexStats {
    /// Files the index holds after this run. A file with nothing but
    /// whitespace in it is walked but yields no chunk, so it is not one.
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
    // A file the index holds at this hash has chunks, so it is not blank and
    // need not be read again.
    let present: Vec<(String, u64)> = walk::collect_hashed(config)?
        .par_iter()
        .filter_map(|(path, hash)| {
            let rel = relative(&config.root, path);
            if indexed.get(rel.as_str()) != Some(hash) {
                let bytes = std::fs::read(path).ok()?;
                if chunk::is_blank(&walk::decode_text(&bytes)) {
                    return None;
                }
            }
            Some((rel, *hash))
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

/// A window of the ranking. `next` is the score of the hit that comes right
/// after it, when there is one, which is how a caller knows there is more to
/// ask for.
#[derive(Debug)]
pub struct Page {
    pub hits: Vec<SearchHit>,
    pub next: Option<f32>,
}

impl Page {
    /// Whether another page has anything to show once hits scoring below
    /// `min_score` are left out. Scores never rise down the ranking, so a hit
    /// under the threshold means that everything after it is under it too.
    pub fn more(&self, min_score: Option<f32>) -> bool {
        self.next
            .is_some_and(|score| min_score.is_none_or(|min| score >= min))
    }
}

const SNIPPET_BYTES: usize = 400;

/// How many of the first hits are shown as their code, and how much of it.
const SHOWN_IN_FULL: usize = 3;
const FULL_LINES: usize = 40;
const FULL_BYTES: usize = 3000;

/// Hits fetched beyond the number asked for, to replace those a wider hit
/// above them covers.
const SPARE_HITS: usize = 5;

/// How deep into the ranking a text pattern looks, in places, and the factor
/// of the end of the page it grows by when a page reaches further. Each place
/// costs a read of its file, once per file, so this is a few hundred reads at
/// most; a pattern that few places match cannot show more than they do.
pub const TEXT_DEPTH: usize = 200;
const TEXT_DEPTH_FACTOR: usize = 8;

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
    index_and_keep(config, force).map(|(stats, _)| stats)
}

/// [`index_repo`] for a process that exits soon after, such as `ripsem index`.
pub fn index_repo_once(config: &Config, force: bool) -> Result<IndexStats> {
    index_and_keep(config, force).map(|(stats, index)| {
        leave_to_exit(index);
        stats
    })
}

/// Freeing every record of a large index one by one takes about as long as a
/// refresh that finds nothing to do (177 ms for 211k chunks), and the process
/// is about to give all of its memory back at once anyway.
fn leave_to_exit<T>(value: T) {
    std::mem::forget(value);
}

/// Like [`index_repo`], and also hands back the index as it now stands on
/// disk, so a search that refreshed first does not read and decode it again.
/// `None` when the records in memory are not in the order the file holds them,
/// which an ANN graph built over that file depends on.
fn index_and_keep(config: &Config, force: bool) -> Result<(IndexStats, Option<Index>)> {
    // Loaded on the first chunk that needs a vector: a reindex that finds
    // nothing changed, or only files removed, never needs the model.
    let mut embedder: Option<Box<dyn embed::Embedder>> = None;
    let dims = config.dims();
    // The walk waits on the disk and the decode on one core, so they overlap.
    let (files, mut prev) = rayon::join(
        || walk::collect_hashed(config),
        || match !force && config.index_path().exists() {
            true => Index::load(&config.index_path()).ok(),
            false => None,
        },
    );
    let files = files?;
    let chunking = chunk::Chunking::of(config);
    if let Some(index) = &mut prev
        && index.dims == dims
        && index.built_for(config.backend, &config.model)
        && index.chunking == chunking
        && index.ann.is_some() == wants_ann(index.records.len())
        && (index.noise_floor.is_some() || index.records.is_empty())
        && let Some(indexed) = nothing_changed(config, &files, index)
    {
        config.create_data_dir()?;
        // Once, so the next load does not convert the old layout again.
        if std::mem::take(&mut index.outdated) {
            index.save(&config.index_path())?;
        }
        let stats = IndexStats {
            files: indexed,
            embedded: 0,
            reused: indexed,
            chunks: index.records.len(),
            vectors: 0,
        };
        return Ok((stats, prev));
    }
    let mut prev_chunks: HashMap<String, Vec<Record>> = HashMap::new();
    let mut prev_ann = None;
    let mut prev_floor = None;
    let mut embedded_since_floor = 0;
    let mut reusable = false;
    let mut recut: Vec<Record> = Vec::new();
    let mut prev_order: Vec<String> = Vec::new();
    if let Some(prev) = prev
        && prev.dims == dims
        && prev.built_for(config.backend, &config.model)
    {
        prev_floor = prev.noise_floor;
        embedded_since_floor = prev.embedded_since_floor;
        if prev.chunking == chunking {
            reusable = true;
            prev_ann = prev.ann;
            prev_order = file_order(&prev.records);
            // Looked up by `&str`, so a path is copied once per file rather
            // than once per record.
            for record in prev.records {
                match prev_chunks.get_mut(record.path.as_str()) {
                    Some(records) => records.push(record),
                    None => {
                        prev_chunks.insert(record.path.to_string(), vec![record]);
                    }
                }
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
        .map(|(i, (path, hash))| {
            let rel = relative(&config.root, path);
            let changed = prev_hash.get(rel.as_str()) != Some(hash);
            (i, *hash, changed)
        })
        .collect();

    let unchanged: std::collections::HashSet<String> = scanned
        .iter()
        .filter(|(_, _, changed)| !changed)
        .map(|&(i, _, _)| relative(&config.root, &files[i].0))
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
    for batch in scanned.chunks(PREPARE_BATCH) {
        let prepared: Vec<Prepared> = batch
            .par_iter()
            .map(|&(i, hash, changed)| {
                let path = &files[i].0;
                let rel = relative(&config.root, path);
                if !changed && prev_chunks.contains_key(&rel) {
                    return Prepared::Unchanged(rel);
                }
                let Ok(bytes) = std::fs::read(path) else {
                    return Prepared::Skipped;
                };
                let text = walk::decode_text(&bytes);
                let chunks = chunk::chunk_source(path, &text, config);
                if chunks.is_empty() {
                    return Prepared::Skipped;
                }
                let pending = chunks
                    .into_iter()
                    .map(|c| {
                        let text_hash = fingerprint::content_hash(c.text.as_bytes());
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
                        (pending, c.text)
                    })
                    .collect();
                Prepared::Cut(pending)
            })
            .collect();

        for file in prepared {
            pb.inc(1);
            match file {
                Prepared::Unchanged(rel) => {
                    if let Some(records) = prev_chunks.get_mut(&rel) {
                        index.records.append(records);
                    }
                    reused += 1;
                }
                Prepared::Skipped => {}
                Prepared::Cut(chunks) => {
                    embedded += 1;
                    for (pending, text) in chunks {
                        queue.push(pending, text);
                        if queue.is_full() {
                            queue.flush(config, &mut embedder, &mut index, &pb)?;
                        }
                    }
                }
            }
        }
    }
    queue.flush(config, &mut embedder, &mut index, &pb)?;
    pb.finish_and_clear();

    let stats = IndexStats {
        files: embedded + reused,
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
        if file_order(&index.records) != prev_order {
            return Ok((stats, None));
        }
        index.noise_floor = prev_floor;
        index.embedded_since_floor = embedded_since_floor;
        index.ann = prev_ann;
        return Ok((stats, Some(index)));
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
    Ok((stats, Some(index)))
}

/// How many files `index` holds, when the walk found exactly those files
/// with the same contents and nothing else that has anything to index. The
/// index is then right as it stands: taking it apart by file and putting it
/// back together, which is most of the work of an update that changes
/// nothing, can be left out.
fn nothing_changed(
    config: &Config,
    files: &[(std::path::PathBuf, u64)],
    index: &Index,
) -> Option<usize> {
    let mut held: HashMap<&str, u64> = HashMap::new();
    let mut last = None;
    for record in &index.records {
        if last != Some(record.path.as_str()) {
            last = Some(record.path.as_str());
            // A file whose records lie in two places is put back in one.
            if held
                .insert(record.path.as_str(), record.file_hash)
                .is_some()
            {
                return None;
            }
        }
    }
    let mut found = 0;
    let mut others = Vec::new();
    for (path, hash) in files {
        match held.get(relative(&config.root, path).as_str()) {
            Some(known) if known == hash => found += 1,
            Some(_) => return None,
            None => others.push(path),
        }
    }
    if found != held.len() {
        return None;
    }
    // A file the index does not hold is new, or one that had nothing in it
    // last time either, such as a blank one.
    others
        .par_iter()
        .all(|path| match std::fs::read(path) {
            Ok(bytes) => chunk::chunk_source(path, &walk::decode_text(&bytes), config).is_empty(),
            Err(_) => true,
        })
        .then_some(found)
}

/// Each file's path once, in the order its first record appears.
fn file_order(records: &[Record]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for r in records {
        if order.last().map(String::as_str) != Some(r.path.as_str()) {
            order.push(r.path.to_string());
        }
    }
    order
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

/// Files cut into chunks together on all cores, then queued in walk order.
const PREPARE_BATCH: usize = 256;

/// What one scanned file contributes to the index.
enum Prepared {
    Unchanged(String),
    Cut(Vec<(Pending, String)>),
    Skipped,
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
            // Chunks of one file arrive together and share its path.
            let path = match index.records.last() {
                Some(last) if last.path == p.path => last.path.clone(),
                _ => p.path.into(),
            };
            index.records.push(Record {
                path,
                start_line: p.start_line,
                end_line: p.end_line,
                snippet: p.snippet.into(),
                file_hash: p.file_hash,
                vector: p.vector.unwrap_or_else(|| crate::quant::quantize(&vector)),
                terms: p.terms,
                symbol: p.symbol,
                declarations: p.declarations.into(),
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
    searcher_over(config, index, embedder, index_mtime)
}

fn searcher_over(
    config: &Config,
    index: Index,
    embedder: Arc<LazyEmbedder>,
    index_mtime: Option<std::time::SystemTime>,
) -> Result<Searcher> {
    let index_path = config.index_path();
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
        scope: Option<&Scope>,
    ) -> Result<Vec<SearchHit>> {
        Ok(self.query_page(query, k, 0, scope, None)?.hits)
    }

    /// `limit` hits from the ranking after skipping the first `offset`, taken
    /// after every filter. The ranking reaches as deep as `offset + limit`
    /// asks, up to the places the index holds, so an offset beyond the last
    /// one is an empty page.
    ///
    /// The candidates each arm contributes grow with the depth asked for, so a
    /// page is a window of the ranking as it is for that depth. A chunk that
    /// holds every term the query has and that only a deeper search finds can
    /// move a page that was read at a shallower one.
    ///
    /// With a `text` pattern only the places whose chunk matches it count. It
    /// is tried on the best [`TEXT_DEPTH`] places, or [`TEXT_DEPTH_FACTOR`]
    /// times as many as the page ends at when that is more, and a place
    /// ranked below them is not found by it.
    pub fn query_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        scope: Option<&Scope>,
        text: Option<&Regex>,
    ) -> Result<Page> {
        // Every record ties on an empty query, so any answer would be noise.
        if query.trim().is_empty() {
            anyhow::bail!("the query is empty; say what to look for");
        }
        // A mistyped scope would otherwise answer "no matches", which reads as
        // the code not existing.
        if let Some(scope) = scope
            && !self.index.covers(scope)
        {
            anyhow::bail!(scope.nothing_matches());
        }
        let qv = self.embedder.get()?.embed_query(query)?;
        let terms = crate::lexical::query_terms(query);
        let names = crate::lexical::query_names(query);
        let words = query_words(query);
        let end = offset.saturating_add(limit);
        // Chunk text is not in memory when the ranking is cut, so a pattern
        // is tried on a deeper stretch of it than the page asks for.
        let wanted = match text {
            Some(_) => end.saturating_mul(TEXT_DEPTH_FACTOR).max(TEXT_DEPTH),
            None => end,
        };
        let k = wanted.saturating_add(SPARE_HITS);

        let mut hits = self.ranked(&qv, &terms, &names, k, scope);
        if let Some(text) = text {
            hits = self.matching(hits, text, end.saturating_add(SPARE_HITS));
        }
        Ok(self.places(hits, &names, &words, offset, end))
    }

    /// The best `k` places for a query embedded as `qv`, through the
    /// nearest-neighbour graph when there is one and nothing limits the scope.
    fn ranked(
        &self,
        qv: &[f32],
        terms: &[TermHash],
        names: &[TermHash],
        k: usize,
        scope: Option<&Scope>,
    ) -> Vec<crate::index::Hit> {
        #[cfg(feature = "hnsw")]
        if scope.is_none()
            && let Some(map) = &self.ann_map
        {
            let depth = crate::index::candidate_depth(k);
            let dense: Vec<crate::index::Hit> = crate::ann::search_map(map, qv, depth)
                .into_iter()
                .map(|(record_idx, score)| crate::index::Hit { score, record_idx })
                .collect();
            return self.index.search_fused_with(&dense, qv, terms, names, k);
        }
        self.index.search_fused(qv, terms, names, k, scope)
    }

    /// The hits from place `skip` up to place `end` of the ranking. A hit
    /// reported as a whole declaration covers the records inside it that rank
    /// below, such as the rest of a long enum's variants, so the places above
    /// the page are worked out too, without their snippets. The first hits of
    /// a page are shown as their code.
    fn places(
        &self,
        hits: Vec<crate::index::Hit>,
        names: &[TermHash],
        words: &[String],
        skip: usize,
        end: usize,
    ) -> Page {
        let mut out: Vec<SearchHit> = Vec::with_capacity(end.min(hits.len()));
        let mut wider: Vec<(String, usize, usize)> = Vec::new();
        let mut next = None;
        for h in hits {
            let record = &self.index.records[h.record_idx];
            let covered = wider.iter().any(|(path, start, end)| {
                path.as_str() == record.path.as_str()
                    && *start <= record.start_line
                    && record.end_line <= *end
            });
            if covered {
                continue;
            }
            if out.len() == end {
                next = Some(h.score);
                break;
            }
            let mut hit = self.hit(h.record_idx, h.score, names);
            if hit.end_line > record.end_line {
                wider.push((hit.path.clone(), hit.start_line, hit.end_line));
            }
            if out.len() >= skip {
                let shown = match out.len() - skip < SHOWN_IN_FULL {
                    true => self.code(record, &hit),
                    false => self.near(record, &hit, words),
                };
                let shown = shown.or_else(|| self.piece(h.record_idx, &hit, words));
                if let Some(shown) = shown {
                    hit.snippet = shown;
                }
            }
            out.push(hit);
        }
        out.drain(..skip.min(out.len()));
        Page { hits: out, next }
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

    /// A hit below the first few shows the start of its code, which for a
    /// long chunk is often a signature and a doc comment. When the query's
    /// words appear only further down, it shows the code from there.
    fn near(&self, record: &Record, hit: &SearchHit, words: &[String]) -> Option<String> {
        let head: Vec<&str> = hit.snippet.lines().collect();
        if words.is_empty() || best_line(&head, words).is_some() {
            return None;
        }
        let code = self.lines(record, hit.start_line, hit.end_line, usize::MAX)?;
        let lines: Vec<&str> = code.lines().collect();
        let from = best_line(&lines, words)?.saturating_sub(1);
        let shown = truncate(&dedent(&lines[from..].join("\n")), SNIPPET_BYTES);
        Some(match from {
            0 => shown,
            _ => format!("…\n{shown}"),
        })
    }

    /// A piece of a line too long for one chunk shows the stretch around the
    /// query's words, when they lie beyond the start the index kept of it.
    fn piece(&self, record_idx: usize, hit: &SearchHit, words: &[String]) -> Option<String> {
        let record = &self.index.records[record_idx];
        if words.is_empty()
            || !record.snippet.ends_with('…')
            || record.start_line != record.end_line
            || (hit.start_line, hit.end_line) != (record.start_line, record.end_line)
            || best_line(&[record.snippet.as_str()], words).is_some()
        {
            return None;
        }
        let bytes = std::fs::read(self.root.join(record.path.as_str())).ok()?;
        if fingerprint::content_hash(&bytes) != record.file_hash {
            return None;
        }
        let text = walk::decode_text(&bytes);
        let line = text.lines().nth(record.start_line.checked_sub(1)?)?;
        if line.len() <= chunk::MAX_CHUNK_BYTES {
            return None;
        }
        let before = self.index.records[..record_idx]
            .iter()
            .rev()
            .take_while(|r| r.path == record.path && r.start_line == record.start_line)
            .count();
        let mut rest = line;
        let mut pieces = Vec::new();
        while !rest.is_empty() {
            let (piece, tail) = rest.split_at(chunk::cut_at(rest));
            if !chunk::is_blank(piece) {
                pieces.push(piece);
            }
            rest = tail;
        }
        around(pieces.get(before)?, words, SNIPPET_BYTES)
    }

    /// A record holding the declaration the query names is reported as that
    /// declaration: its label, its lines and a snippet that starts there. The
    /// first piece of a declaration too large for one chunk reports all of it,
    /// unless it is a container, whose members answer for themselves.
    fn hit(&self, record_idx: usize, score: f32, names: &[TermHash]) -> SearchHit {
        let r = &self.index.records[record_idx];
        let whole = || SearchHit {
            path: r.path.to_string(),
            start_line: r.start_line,
            end_line: r.end_line,
            score,
            snippet: r.snippet.to_string(),
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
            true => Some(r.snippet.to_string()),
            false => self.lines(r, start_line, end_line, SNIPPET_BYTES),
        };
        match snippet {
            Some(snippet) => SearchHit {
                path: r.path.to_string(),
                start_line,
                end_line,
                score,
                snippet,
                symbol: Some(declaration.symbol.clone()),
            },
            None => whole(),
        }
    }

    /// The file a record was cut from, as it was indexed; `None` when it was
    /// changed since or cannot be read.
    fn indexed_bytes(&self, record: &Record) -> Option<Vec<u8>> {
        // The index is data on disk, and a repository can ship one: a path that
        // leaves the root is not a file this index could have been built from.
        let inside = Path::new(record.path.as_str())
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !inside {
            return None;
        }
        let bytes = std::fs::read(self.root.join(record.path.as_str())).ok()?;
        (fingerprint::content_hash(&bytes) == record.file_hash).then_some(bytes)
    }

    /// The hits, in their order, whose chunk the pattern matches, up to
    /// `needed` of them. Chunk text is not held in memory, only a snippet of
    /// it, so each is read from its file, which is read once however many
    /// chunks it has. A chunk whose file changed since the index was built is
    /// matched on its snippet instead.
    fn matching(
        &self,
        hits: Vec<crate::index::Hit>,
        text: &Regex,
        needed: usize,
    ) -> Vec<crate::index::Hit> {
        let mut files: HashMap<&str, Option<Vec<u8>>> = HashMap::new();
        let mut kept = Vec::new();
        for h in hits {
            if kept.len() == needed {
                break;
            }
            let record = &self.index.records[h.record_idx];
            let bytes = files
                .entry(record.path.as_str())
                .or_insert_with(|| self.indexed_bytes(record));
            let found = match bytes {
                Some(bytes) => {
                    let file = walk::decode_text(bytes);
                    let lines: Vec<&str> = file
                        .lines()
                        .skip(record.start_line.saturating_sub(1))
                        .take((record.end_line + 1).saturating_sub(record.start_line))
                        .collect();
                    text.is_match(&lines.join("\n"))
                }
                None => text.is_match(&record.snippet),
            };
            if found {
                kept.push(h);
            }
        }
        kept
    }

    /// Lines of an indexed file as they were indexed. A file changed since then
    /// has nothing to offer: its lines may have moved.
    fn lines(&self, record: &Record, start: usize, end: usize, budget: usize) -> Option<String> {
        if start == 0 || end < start {
            return None;
        }
        let bytes = self.indexed_bytes(record)?;
        let text = walk::decode_text(&bytes);
        let wanted: Vec<&str> = text.lines().skip(start - 1).take(end + 1 - start).collect();
        // A line that long was indexed in pieces, and its start is not the
        // part a hit is about.
        if wanted.iter().any(|l| l.len() > chunk::MAX_CHUNK_BYTES) {
            return None;
        }
        Some(truncate(&dedent(&wanted.join("\n")), budget))
    }
}

pub fn search(config: &Config, query: &str, k: usize) -> Result<Vec<SearchHit>> {
    open(config)?.query(query, k)
}

/// Like [`search`], limited to a [`Scope`] (its prefix a normalized path, see
/// `mcp::scope_prefix`), after updating the index for files changed since it
/// was built. `None` searches the whole index.
pub fn search_scoped(
    config: &Config,
    query: &str,
    k: usize,
    scope: Option<&Scope>,
) -> Result<Vec<SearchHit>> {
    Ok(search_page(config, query, k, 0, scope, None)?.hits)
}

/// Like [`search_scoped`], for the page of `limit` hits that starts at place
/// `offset` of the ranking, of the hits whose chunk matches `text` when there
/// is one; see [`Searcher::query_page`].
pub fn search_page(
    config: &Config,
    query: &str,
    limit: usize,
    offset: usize,
    scope: Option<&Scope>,
    text: Option<&Regex>,
) -> Result<Page> {
    if query.trim().is_empty() {
        anyhow::bail!("the query is empty; say what to look for");
    }
    // A repository's first search builds its index rather than sending the
    // user off to run `ripsem index`, and so does a directory `ripsem init`
    // opted in. Anywhere else the directory may be a home folder, which nobody
    // wants embedded by accident.
    let opted_in = config.root.join(".git").exists() || config.data_dir().is_dir();
    if !config.index_path().exists() && opted_in {
        eprintln!("no index yet; building one, which later searches only update");
        index_repo_once(config, false)?;
    }
    let embedder = Arc::new(LazyEmbedder::new(config));
    // Loading the model and decoding the index are independent, so they run
    // side by side. Without an index there is nothing to search and no reason
    // to load (or download) a model.
    let mut current = None;
    if config.index_path().exists() {
        embedder.warm();
        current = refresh(config);
    }
    let searcher = match current {
        Some(index) => {
            let mtime = file_mtime(&config.index_path());
            searcher_over(config, index, Arc::clone(&embedder), mtime)
        }
        None => open_with(config, Arc::clone(&embedder)),
    };
    let found = searcher.and_then(|searcher| {
        let found = searcher.query_page(query, limit, offset, scope, text);
        leave_to_exit(searcher.index);
        found
    });
    if found.is_err() {
        embedder.settle();
    }
    found
}

/// Brings the index up to date with the tree while the model loads, so an
/// edit made since the last `ripsem index` is found where it is now. When
/// nothing changed this only walks and hashes the files. A failure, such as a
/// read-only checkout, leaves the search to the index as it is. Returns the
/// index it ended with, when it has one that matches the file.
fn refresh(config: &Config) -> Option<Index> {
    match index_and_keep(config, false) {
        Ok((stats, index)) => {
            if stats.embedded > 0 {
                eprintln!(
                    "updated the index: {} changed",
                    crate::cli::plural(stats.embedded, "file")
                );
            }
            index
        }
        Err(e) => {
            eprintln!("warning: could not update the index: {e:#}");
            None
        }
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

/// The words of a query a line of code is looked for by, lowercased. Short
/// ones such as `a` or `of` would be found on almost any line.
pub(crate) fn query_words(query: &str) -> Vec<String> {
    let mut words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    words
}

/// The line naming most of `words`, the first of them on a tie, or `None`
/// when no line names any.
pub(crate) fn best_line(lines: &[&str], words: &[String]) -> Option<usize> {
    let mut best = None;
    let mut most = 0;
    for (i, line) in lines.iter().enumerate() {
        let line = line.to_lowercase();
        let n = words.iter().filter(|w| line.contains(w.as_str())).count();
        if n > most {
            best = Some(i);
            most = n;
        }
    }
    best
}

/// About `max` bytes of `text` around the place its window holds most of
/// `words`, the first on a tie, cut on character boundaries and marked with
/// an ellipsis where text was left out. `None` when no word is in it.
fn around(text: &str, words: &[String], max: usize) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let back = max / 4;
    let window = |at: usize| {
        let mut from = at.saturating_sub(back);
        while !text.is_char_boundary(from) {
            from += 1;
        }
        let mut to = (from + max).min(text.len());
        while !text.is_char_boundary(to) {
            to -= 1;
        }
        (from, to)
    };
    let (from, to) = words
        .iter()
        .filter_map(|w| lower.find(w.as_str()))
        .map(window)
        .max_by_key(|&(from, to)| {
            let named = words
                .iter()
                .filter(|w| lower[from..to].contains(w.as_str()));
            (named.count(), std::cmp::Reverse(from))
        })?;
    let head = if from > 0 { "…" } else { "" };
    let tail = if to < text.len() { "…" } else { "" };
    Some(format!("{head}{}{tail}", &text[from..to]))
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

/// The models read at most 512 tokens, a few bytes each, of a longer text.
fn model_bytes(text: &str) -> usize {
    text.len().min(2048)
}

const REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

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
    fn a_record_naming_a_file_outside_the_root_reads_nothing() {
        let dir = crate::testutil::TempDir::new();
        dir.write("project/inside.txt", "one\ntwo\n");
        dir.write("secret.txt", "classified\n");
        let config = Config {
            root: dir.path().join("project"),
            ..hash_config()
        };
        let record = |path: &str, bytes: &[u8]| Record {
            path: path.into(),
            start_line: 1,
            end_line: 1,
            snippet: String::new().into(),
            file_hash: fingerprint::content_hash(bytes),
            text_hash: 0,
            vector: crate::quant::quantize(&[]),
            terms: Default::default(),
            symbol: None,
            declarations: Default::default(),
        };
        let searcher = Searcher {
            index: Index::new(0, Backend::Hash, config.model.clone()),
            root: config.root.clone(),
            embedder: Arc::new(LazyEmbedder::new(&config)),
            index_path: config.index_path(),
            index_mtime: None,
            model: config.model.clone(),
            #[cfg(feature = "hnsw")]
            ann_map: None,
        };
        let inside = record("inside.txt", b"one\ntwo\n");
        assert_eq!(searcher.lines(&inside, 2, 2, 100).as_deref(), Some("two"));
        assert_eq!(searcher.lines(&inside, 0, 2, 100), None);
        assert_eq!(searcher.lines(&inside, 2, 1, 100), None);
        // The hash matches, as it would if the index were written to match.
        let outside = record("../secret.txt", b"classified\n");
        assert_eq!(searcher.lines(&outside, 1, 1, 100), None);
        let absolute = dir.path().join("secret.txt").to_string_lossy().into_owned();
        assert_eq!(
            searcher.lines(&record(&absolute, b"classified\n"), 1, 1, 100),
            None
        );
    }

    #[test]
    fn query_words_drop_short_and_repeated_words() {
        assert_eq!(
            query_words("where is the Index, the index of a repo?"),
            vec!["index", "repo", "the", "where"]
        );
    }

    #[test]
    fn a_lower_hit_shows_the_code_its_query_words_are_on() {
        let dir = crate::testutil::TempDir::new();
        let mut text = String::from("/// Draws the bar.\nfn draw() {\n");
        for i in 0..30 {
            text.push_str(&format!("    let step_{i} = {i};\n"));
        }
        text.push_str("    render_progress(total);\n}\n");
        dir.write("draw.rs", &text);
        let config = Config {
            root: dir.path().to_path_buf(),
            ..hash_config()
        };
        let record = Record {
            path: "draw.rs".into(),
            start_line: 1,
            end_line: 34,
            snippet: truncate(&text, SNIPPET_BYTES).into(),
            file_hash: fingerprint::content_hash(text.as_bytes()),
            text_hash: 0,
            vector: crate::quant::quantize(&[]),
            terms: Default::default(),
            symbol: None,
            declarations: Default::default(),
        };
        let searcher = Searcher {
            index: Index::new(0, Backend::Hash, config.model.clone()),
            root: config.root.clone(),
            embedder: Arc::new(LazyEmbedder::new(&config)),
            index_path: config.index_path(),
            index_mtime: None,
            model: config.model.clone(),
            #[cfg(feature = "hnsw")]
            ann_map: None,
        };
        let hit = SearchHit {
            path: record.path.to_string(),
            start_line: 1,
            end_line: 34,
            score: 0.5,
            snippet: record.snippet.to_string(),
            symbol: None,
        };
        let shown = searcher
            .near(&record, &hit, &query_words("render progress"))
            .unwrap();
        assert_eq!(
            shown,
            "…\n    let step_29 = 29;\n    render_progress(total);\n}"
        );
        assert_eq!(searcher.near(&record, &hit, &query_words("draws")), None);
        assert_eq!(searcher.near(&record, &hit, &query_words("nowhere")), None);
        assert_eq!(searcher.near(&record, &hit, &[]), None);
    }

    #[test]
    fn the_index_kept_after_a_refresh_is_the_one_on_disk() {
        let dir = crate::testutil::TempDir::new();
        dir.write("a.rs", "fn alpha() {}\n");
        dir.write("b.rs", "fn beta() {}\n");
        let config = Config {
            root: dir.path().to_path_buf(),
            ..hash_config()
        };
        let on_disk = || {
            Index::load(&config.index_path())
                .unwrap()
                .encode_body()
                .unwrap()
        };

        let (_, built) = index_and_keep(&config, false).unwrap();
        assert_eq!(built.unwrap().encode_body().unwrap(), on_disk());

        let (stats, kept) = index_and_keep(&config, false).unwrap();
        assert_eq!(stats.embedded, 0);
        assert_eq!(kept.unwrap().encode_body().unwrap(), on_disk());

        dir.write("b.rs", "fn beta() { gamma() }\n");
        let (stats, updated) = index_and_keep(&config, false).unwrap();
        assert_eq!(stats.embedded, 1);
        assert_eq!(updated.unwrap().encode_body().unwrap(), on_disk());
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

    #[test]
    fn an_index_in_the_previous_format_is_written_again_once() {
        let dir = crate::testutil::TempDir::new();
        dir.write("a.rs", "alpha bravo charlie delta echo foxtrot");
        let config = Config {
            root: dir.path().to_path_buf(),
            ..hash_config()
        };
        index_repo(&config, false).unwrap();
        let path = config.index_path();
        Index::load(&path).unwrap().save_in_previous_format(&path);
        assert!(Index::load(&path).unwrap().outdated);

        let again = index_repo(&config, false).unwrap();
        assert_eq!((again.reused, again.embedded), (1, 0));
        let current = std::fs::read(&path).unwrap();
        assert!(!Index::load(&path).unwrap().outdated);

        index_repo(&config, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), current);
    }
}
