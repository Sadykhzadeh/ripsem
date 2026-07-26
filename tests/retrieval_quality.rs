#![cfg(feature = "fastembed")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ripsem::config::{Backend, Config};
use ripsem::index::{Hit, Index};
use ripsem::pipeline;

static COUNTER: AtomicU64 = AtomicU64::new(0);

const DEPTH: usize = 5;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ripsem-bench-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The corpus is ripsem's own source tree: real code with real identifiers, and
/// files whose purpose a person would describe in words the code never uses.
///
/// It is taken from the last commit rather than the working tree. The tree under
/// development is also the thing being measured, so reading it live would move
/// the corpus with every edit and make two runs incomparable — which is the one
/// thing a benchmark exists to allow.
fn corpus() -> TempDir {
    let dir = TempDir::new();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let archive = dir.path.join("src.tar");
    let status = Command::new("git")
        .args(["archive", "--format=tar", "HEAD", "src"])
        .arg("-o")
        .arg(&archive)
        .current_dir(repository)
        .status()
        .expect("git is required to snapshot the benchmark corpus");
    assert!(status.success(), "git archive HEAD src failed");
    extract(&archive, &dir.path);
    std::fs::remove_file(&archive).unwrap();
    dir
}

/// Minimal tar reader: the archive holds only ripsem's own text files, so the
/// header fields that matter are the path and the size.
fn extract(archive: &Path, into: &Path) {
    let bytes = std::fs::read(archive).unwrap();
    let mut offset = 0;
    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        let name = String::from_utf8_lossy(&header[..100])
            .trim_end_matches('\0')
            .to_string();
        if name.is_empty() {
            break;
        }
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&header[124..136]).trim_matches(['\0', ' ']),
            8,
        )
        .unwrap();
        offset += 512;
        let target = into.join(&name);
        if header[156] == b'5' || name.ends_with('/') {
            std::fs::create_dir_all(&target).unwrap();
        } else {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &bytes[offset..offset + size]).unwrap();
        }
        offset += size.div_ceil(512) * 512;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Described,
    Identifier,
    Message,
    Recalled,
}

use Shape::{Described, Identifier, Message, Recalled};

/// A description asks in words the file may not contain. An identifier is typed
/// from memory. A message is a string the developer saw in output. A recalled
/// query is a name remembered wrong — the shape that defeats exact search and
/// plain similarity alike.
const QUERIES: &[(Shape, &str, &str)] = &[
    (
        Described,
        "how do we decide a file is text rather than binary",
        "src/walk.rs",
    ),
    (
        Described,
        "keep the index up to date while the developer edits files",
        "src/watch.rs",
    ),
    (
        Described,
        "shrink the stored vectors so the index takes less disk",
        "src/quant.rs",
    ),
    (
        Described,
        "cut a source file along function boundaries",
        "src/semantic.rs",
    ),
    (
        Described,
        "which embedding models can be chosen and how big are they",
        "src/model.rs",
    ),
    (
        Described,
        "speak to a coding agent over standard input and output",
        "src/mcp.rs",
    ),
    (
        Described,
        "avoid redoing work for files that have not changed",
        "src/pipeline.rs",
    ),
    (
        Described,
        "map a file extension to a language parser",
        "src/lang.rs",
    ),
    (Identifier, "content_hash", "src/fingerprint.rs"),
    (Identifier, "QuantizedVector", "src/quant.rs"),
    (Identifier, "scope_prefix", "src/mcp.rs"),
    (Identifier, "split_identifier", "src/lexical.rs"),
    (Identifier, "OverrideBuilder", "src/walk.rs"),
    (Identifier, "human_size", "src/cli.rs"),
    (
        Message,
        "index is unreadable or corrupt; run ripsem index --force to rebuild",
        "src/index.rs",
    ),
    (
        Message,
        "no index yet; run ripsem index in this directory first",
        "src/pipeline.rs",
    ),
    (
        Message,
        "unknown backend, expected hash or fastembed",
        "src/cli.rs",
    ),
    (
        Recalled,
        "checksum of the file contents",
        "src/fingerprint.rs",
    ),
    (
        Recalled,
        "inverse document frequency term weighting",
        "src/lexical.rs",
    ),
    (
        Recalled,
        "approximate nearest neighbour graph",
        "src/ann.rs",
    ),
    (
        Recalled,
        "atomic rename so a half written index is never read",
        "src/index.rs",
    ),
];

const SHAPES: [Shape; 4] = [Described, Identifier, Message, Recalled];

#[derive(Default)]
struct Score {
    queries: usize,
    first: usize,
    within_depth: usize,
    reciprocal: f64,
}

impl Score {
    fn record(&mut self, rank: Option<usize>) {
        self.queries += 1;
        match rank {
            Some(1) => {
                self.first += 1;
                self.within_depth += 1;
                self.reciprocal += 1.0;
            }
            Some(r) => {
                self.within_depth += 1;
                self.reciprocal += 1.0 / r as f64;
            }
            None => {}
        }
    }

    fn recall_at_1(&self) -> f64 {
        self.first as f64 / self.queries.max(1) as f64
    }

    fn recall_at_depth(&self) -> f64 {
        self.within_depth as f64 / self.queries.max(1) as f64
    }

    fn mrr(&self) -> f64 {
        self.reciprocal / self.queries.max(1) as f64
    }
}

fn rank_of(index: &Index, hits: &[Hit], expected: &str) -> Option<usize> {
    hits.iter()
        .position(|h| index.records[h.record_idx].path == expected)
        .map(|p| p + 1)
}

fn report(label: &str, overall: &Score, by_shape: &[(Shape, Score)]) {
    eprintln!(
        "{label:<14} recall@1={:.2}  recall@{DEPTH}={:.2}  MRR={:.3}",
        overall.recall_at_1(),
        overall.recall_at_depth(),
        overall.mrr()
    );
    for (shape, score) in by_shape {
        eprintln!(
            "  {:<12} recall@1={:.2}  recall@{DEPTH}={:.2}  MRR={:.3}  ({} queries)",
            format!("{shape:?}"),
            score.recall_at_1(),
            score.recall_at_depth(),
            score.mrr(),
            score.queries
        );
    }
}

#[test]
#[ignore = "downloads the embedding model; run with --ignored --nocapture"]
fn fusion_beats_either_retriever_alone() {
    let dir = corpus();
    let config = Config {
        backend: Backend::Fastembed,
        root: dir.path.clone(),
        ..Default::default()
    };
    pipeline::index_repo(&config, true).unwrap();

    let index = Index::load(&config.index_path()).unwrap();
    let embedder = ripsem::embed::build(&config).unwrap();
    for (_, _, expected) in QUERIES {
        assert!(
            index.records.iter().any(|r| r.path == *expected),
            "corpus is missing {expected}; the labels need updating"
        );
    }

    let arms = ["vector only", "lexical only", "fused"];
    let mut overall: Vec<Score> = arms.iter().map(|_| Score::default()).collect();
    let mut by_shape: Vec<Vec<(Shape, Score)>> = arms
        .iter()
        .map(|_| SHAPES.iter().map(|&s| (s, Score::default())).collect())
        .collect();

    for (shape, query, expected) in QUERIES {
        let vector = embedder.embed_query(query).unwrap();
        let terms = ripsem::lexical::query_terms(query);
        let ranked = [
            index.dense_ranking(&vector, DEPTH, None),
            index.lexical_ranking(&terms, DEPTH, None).hits().to_vec(),
            index.search_fused(&vector, &terms, DEPTH, None),
        ];
        for (arm, hits) in ranked.iter().enumerate() {
            let rank = rank_of(&index, hits, expected);
            overall[arm].record(rank);
            let slot = by_shape[arm].iter_mut().find(|(s, _)| s == shape).unwrap();
            slot.1.record(rank);
            if arm == 2 && rank.is_none() {
                eprintln!("  miss: {query:?} (expected {expected})");
            }
        }
    }

    eprintln!("corpus: {} chunks", index.records.len());
    for (arm, label) in arms.iter().enumerate() {
        report(label, &overall[arm], &by_shape[arm]);
    }

    let fused = overall[2].mrr();
    assert!(
        fused >= overall[0].mrr(),
        "fusion must not rank worse than the vector arm alone"
    );
    assert!(
        fused >= overall[1].mrr(),
        "fusion must not rank worse than the lexical arm alone"
    );
    assert!(
        overall[2].recall_at_depth() >= 0.8,
        "recall@{DEPTH} too low: {:.2}",
        overall[2].recall_at_depth()
    );
    assert!(fused >= 0.7, "MRR too low: {fused:.3}");
}
