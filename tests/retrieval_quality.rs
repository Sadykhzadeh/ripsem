#![cfg(feature = "fastembed")]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ripsem::config::{Backend, Config};
use ripsem::index::{Hit, Index};
use ripsem::pipeline;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Each measurement rebuilds the cached corpus index in place, so tests running
/// in parallel would overwrite each other's index and report numbers for the
/// wrong corpus.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn exclusively<T>(measurement: impl FnOnce() -> T) -> T {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    measurement()
}

const DEPTH: usize = 5;
const OWN_REVISION: &str = "corpus/v1";

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Described,
    Identifier,
    Message,
    Recalled,
}

use Shape::{Described, Identifier, Message, Recalled};

const SHAPES: [Shape; 4] = [Described, Identifier, Message, Recalled];

/// A description asks in words the file may not contain. An identifier is typed
/// from memory. A message is a string the developer saw in output. A recalled
/// query is a name remembered wrong — the shape that defeats exact search and
/// plain similarity alike.
struct Query {
    shape: Shape,
    text: &'static str,
    /// Any of these files answers the question. More than one is listed only
    /// where the code genuinely spreads the answer across them; a single target
    /// means the definition, not the places that merely mention it.
    targets: &'static [&'static str],
}

enum Source {
    /// ripsem's own source at a pinned tag. Both what the benchmark measures and
    /// what development changes, so it is never read from the working tree.
    Own { pathspec: &'static str },
    /// A real project at a release tag, cloned once into `target/corpora`. The
    /// first run needs network; later runs do not.
    Pinned {
        url: &'static str,
        tag: &'static str,
    },
}

struct Corpus {
    name: &'static str,
    language: &'static str,
    source: Source,
    queries: &'static [Query],
}

const fn query(shape: Shape, text: &'static str, targets: &'static [&'static str]) -> Query {
    Query {
        shape,
        text,
        targets,
    }
}

const RIPSEM: &[Query] = &[
    query(
        Described,
        "how do we decide a file is text rather than binary",
        &["src/walk.rs"],
    ),
    query(
        Described,
        "keep the index up to date while the developer edits files",
        &["src/watch.rs"],
    ),
    query(
        Described,
        "shrink the stored vectors so the index takes less disk",
        &["src/quant.rs"],
    ),
    query(
        Described,
        "cut a source file along function boundaries",
        &["src/semantic.rs"],
    ),
    query(
        Described,
        "which embedding models can be chosen and how big are they",
        &["src/model.rs"],
    ),
    query(
        Described,
        "speak to a coding agent over standard input and output",
        &["src/mcp.rs"],
    ),
    query(
        Described,
        "avoid redoing work for files that have not changed",
        &["src/pipeline.rs"],
    ),
    query(
        Described,
        "map a file extension to a language parser",
        &["src/lang.rs"],
    ),
    query(Identifier, "content_hash", &["src/fingerprint.rs"]),
    query(Identifier, "QuantizedVector", &["src/quant.rs"]),
    query(Identifier, "scope_prefix", &["src/mcp.rs"]),
    query(Identifier, "split_identifier", &["src/lexical.rs"]),
    query(Identifier, "OverrideBuilder", &["src/walk.rs"]),
    query(Identifier, "human_size", &["src/cli.rs"]),
    query(
        Message,
        "index is unreadable or corrupt; run ripsem index --force to rebuild",
        &["src/index.rs"],
    ),
    query(
        Message,
        "no index yet; run ripsem index in this directory first",
        &["src/pipeline.rs"],
    ),
    query(
        Message,
        "unknown backend, expected hash or fastembed",
        &["src/cli.rs"],
    ),
    query(
        Recalled,
        "checksum of the file contents",
        &["src/fingerprint.rs"],
    ),
    query(
        Recalled,
        "inverse document frequency term weighting",
        &["src/lexical.rs"],
    ),
    query(
        Recalled,
        "approximate nearest neighbour graph",
        &["src/ann.rs"],
    ),
    query(
        Recalled,
        "atomic rename so a half written index is never read",
        &["src/index.rs"],
    ),
];

const REQUESTS: &[Query] = &[
    query(
        Described,
        "how is the basic auth header encoded",
        &["src/requests/auth.py"],
    ),
    query(
        Described,
        "where are redirects followed after a 302",
        &["src/requests/sessions.py"],
    ),
    query(
        Described,
        "a dictionary whose keys ignore case",
        &["src/requests/structures.py"],
    ),
    query(
        Described,
        "connection pooling and retries for http",
        &["src/requests/adapters.py"],
    ),
    query(
        Described,
        "turn a plain dict of cookies into a cookie jar",
        &["src/requests/cookies.py"],
    ),
    query(
        Described,
        "the short helpers for get and post",
        &["src/requests/api.py"],
    ),
    query(
        Described,
        "collect environment versions for a bug report",
        &["src/requests/help.py"],
    ),
    query(
        Described,
        "path to the bundled certificate authority file",
        &["src/requests/certs.py"],
    ),
    query(Identifier, "_basic_auth_str", &["src/requests/auth.py"]),
    query(
        Identifier,
        "CaseInsensitiveDict",
        &["src/requests/structures.py"],
    ),
    query(
        Identifier,
        "extract_zipped_paths",
        &["src/requests/utils.py"],
    ),
    query(Identifier, "PreparedRequest", &["src/requests/models.py"]),
    query(Identifier, "dispatch_hook", &["src/requests/hooks.py"]),
    query(Identifier, "super_len", &["src/requests/utils.py"]),
    query(
        Message,
        "Exceeded 30 redirects",
        &["src/requests/sessions.py"],
    ),
    query(
        Message,
        "Invalid URL: No scheme supplied",
        &["src/requests/models.py"],
    ),
    query(
        Recalled,
        "http status code names lookup table",
        &["src/requests/status_codes.py"],
    ),
    query(
        Recalled,
        "digest access authentication",
        &["src/requests/auth.py"],
    ),
    query(
        Recalled,
        "multipart form data body encoding",
        &["src/requests/models.py"],
    ),
    query(
        Recalled,
        "check whether a string is pure ascii",
        &["src/requests/_internal_utils.py"],
    ),
];

const MUX: &[Query] = &[
    query(
        Described,
        "match an incoming request against the registered routes",
        &["mux.go"],
    ),
    query(
        Described,
        "compile a path template with variables into a regular expression",
        &["regexp.go"],
    ),
    query(
        Described,
        "wrap handlers so code runs before each one",
        &["middleware.go"],
    ),
    query(
        Described,
        "build a url from a named route and its parameters",
        &["route.go"],
    ),
    query(Identifier, "SetURLVars", &["test_helpers.go"]),
    query(Identifier, "CORSMethodMiddleware", &["middleware.go"]),
    query(Identifier, "braceIndices", &["regexp.go"]),
    query(Identifier, "MethodNotAllowedHandler", &["mux.go"]),
    query(
        Message,
        "mux: number of parameters must be multiple of 2",
        &["mux.go"],
    ),
    query(Message, "mux: missing name or pattern", &["regexp.go"]),
    query(
        Recalled,
        "read the path variables out of a request",
        &["mux.go"],
    ),
    query(
        Recalled,
        "trailing slash redirect behaviour",
        &["mux.go", "route.go"],
    ),
];

const GOT: &[Query] = &[
    query(
        Described,
        "how long to wait before retrying a failed request",
        &["source/core/calculate-retry-delay.ts"],
    ),
    query(
        Described,
        "abort a request when it takes too long",
        &["source/core/timed-out.ts"],
    ),
    query(
        Described,
        "parse the link header used for pagination",
        &["source/core/parse-link-header.ts"],
    ),
    query(
        Described,
        "convert a url into node http request options",
        &["source/core/utils/url-to-options.ts"],
    ),
    query(
        Described,
        "work out the size of the request body",
        &["source/core/utils/get-body-size.ts"],
    ),
    query(
        Described,
        "forward events from one emitter to another",
        &["source/core/utils/proxy-events.ts"],
    ),
    query(
        Described,
        "the error types thrown when a request fails",
        &["source/core/errors.ts"],
    ),
    query(
        Described,
        "decide whether a response status counts as success",
        &["source/core/response.ts"],
    ),
    query(
        Identifier,
        "WeakableMap",
        &["source/core/utils/weakable-map.ts"],
    ),
    query(
        Identifier,
        "isUnixSocketURL",
        &["source/core/utils/is-unix-socket-url.ts"],
    ),
    query(
        Identifier,
        "optionsToUrl",
        &["source/core/utils/options-to-url.ts"],
    ),
    query(Identifier, "MaxRedirectsError", &["source/core/errors.ts"]),
    query(Identifier, "asPromise", &["source/as-promise/index.ts"]),
    query(Identifier, "CancelError", &["source/as-promise/types.ts"]),
    query(
        Message,
        "A retry listener has been attached already",
        &["source/core/index.ts"],
    ),
    query(
        Message,
        "To use the http2 option install Node.js 15.10.0 or above",
        &["source/core/options.ts"],
    ),
    query(
        Recalled,
        "helper that removes listeners it attached earlier",
        &["source/core/utils/unhandle.ts"],
    ),
    query(
        Recalled,
        "detect a form data body",
        &["source/core/utils/is-form-data.ts"],
    ),
    query(
        Recalled,
        "every request option and its default",
        &["source/core/options.ts"],
    ),
    query(Recalled, "cache error type", &["source/core/errors.ts"]),
];

const CORPORA: &[Corpus] = &[
    Corpus {
        name: "ripsem",
        language: "rust",
        source: Source::Own { pathspec: "src" },
        queries: RIPSEM,
    },
    Corpus {
        name: "requests",
        language: "python",
        source: Source::Pinned {
            url: "https://github.com/psf/requests.git",
            tag: "v2.34.2",
        },
        queries: REQUESTS,
    },
    Corpus {
        name: "mux",
        language: "go",
        source: Source::Pinned {
            url: "https://github.com/gorilla/mux.git",
            tag: "v1.8.1",
        },
        queries: MUX,
    },
    Corpus {
        name: "got",
        language: "typescript",
        source: Source::Pinned {
            url: "https://github.com/sindresorhus/got.git",
            tag: "v14.4.5",
        },
        queries: GOT,
    },
];

enum Materialized {
    Temporary(TempDir),
    Cached(PathBuf),
}

impl Materialized {
    fn root(&self) -> PathBuf {
        match self {
            Materialized::Temporary(dir) => dir.path.clone(),
            Materialized::Cached(path) => path.clone(),
        }
    }
}

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn materialize(source: &Source, name: &str) -> Materialized {
    match source {
        Source::Own { pathspec } => {
            let dir = TempDir::new();
            let archive = dir.path.join("corpus.tar");
            run(
                Command::new("git")
                    .args(["archive", "--format=tar", OWN_REVISION])
                    .arg("-o")
                    .arg(&archive)
                    .arg(pathspec)
                    .current_dir(repository()),
                &format!("git archive {OWN_REVISION}; the tag pins the corpus"),
            );
            extract(&archive, &dir.path);
            std::fs::remove_file(&archive).unwrap();
            Materialized::Temporary(dir)
        }
        Source::Pinned { url, tag } => {
            let path = repository().join("target/corpora").join(name);
            if !path.join(".git").exists() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                run(
                    Command::new("git")
                        .args(["clone", "--depth", "1", "--quiet", "--branch", tag, url])
                        .arg(&path),
                    &format!("git clone {url} at {tag}; the first run needs network"),
                );
            }
            Materialized::Cached(path)
        }
    }
}

fn run(command: &mut Command, what: &str) {
    let status = command.status().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(status.success(), "{what}: failed");
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

#[derive(Default, Clone)]
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

    fn line(&self) -> String {
        format!(
            "recall@1={:.2}  recall@{DEPTH}={:.2}  MRR={:.3}  ({} queries)",
            self.recall_at_1(),
            self.recall_at_depth(),
            self.mrr(),
            self.queries
        )
    }
}

/// The best rank any acceptable target reached.
fn rank_of(index: &Index, hits: &[Hit], targets: &[&str]) -> Option<usize> {
    hits.iter()
        .position(|h| targets.contains(&index.records[h.record_idx].path.as_str()))
        .map(|p| p + 1)
}

/// What reaching the answer costs to read. `context` counts the snippets up to
/// and including the first correct hit — everything a caller has to take in
/// before it has the answer. `whole_files` counts the file that answer lives in,
/// which is what gets read instead when a search only points at a filename.
#[derive(Default)]
struct Cost {
    answered: usize,
    context: u64,
    whole_files: u64,
}

impl Cost {
    fn record(&mut self, index: &Index, hits: &[Hit], root: &Path, rank: Option<usize>) {
        let Some(rank) = rank else { return };
        self.answered += 1;
        self.context += hits[..rank]
            .iter()
            .map(|h| index.records[h.record_idx].snippet.len() as u64)
            .sum::<u64>();
        let answer = &index.records[hits[rank - 1].record_idx].path;
        self.whole_files += std::fs::metadata(root.join(answer))
            .map(|m| m.len())
            .unwrap_or(0);
    }

    fn line(&self) -> String {
        let answered = self.answered.max(1) as f64;
        let context = self.context as f64 / answered;
        let whole = self.whole_files as f64 / answered;
        format!(
            "{context:.0} bytes read to the answer against {whole:.0} for the file it is in ({:.1}x less, {} answered)",
            whole / context.max(1.0),
            self.answered
        )
    }
}

const ARMS: [&str; 3] = ["vector only", "lexical only", "fused"];
const FUSED: usize = 2;

struct Evaluation {
    arms: Vec<Score>,
    shapes: Vec<Vec<(Shape, Score)>>,
    per_corpus: Vec<(&'static str, &'static str, usize, Vec<Score>)>,
    cost: Cost,
}

fn evaluate(model: &str, announce_misses: bool) -> Evaluation {
    let mut overall: Vec<Score> = ARMS.iter().map(|_| Score::default()).collect();
    let mut by_shape: Vec<Vec<(Shape, Score)>> = ARMS
        .iter()
        .map(|_| SHAPES.iter().map(|&s| (s, Score::default())).collect())
        .collect();
    let mut report = Vec::new();
    let mut cost = Cost::default();

    for corpus in CORPORA {
        let materialized = materialize(&corpus.source, corpus.name);
        let root = materialized.root();
        let config = Config {
            backend: Backend::Fastembed,
            model: model.to_string(),
            root: root.clone(),
            ..Default::default()
        };
        pipeline::index_repo(&config, true).unwrap();
        let index = Index::load(&config.index_path()).unwrap();
        let embedder = ripsem::embed::build(&config).unwrap();

        let indexed: HashSet<&str> = index.records.iter().map(|r| r.path.as_str()).collect();
        for query in corpus.queries {
            for target in query.targets {
                assert!(
                    indexed.contains(target),
                    "{}: {target} is not in the index; the labels need updating",
                    corpus.name
                );
            }
        }

        let mut here: Vec<Score> = ARMS.iter().map(|_| Score::default()).collect();
        for query in corpus.queries {
            let vector = embedder.embed_query(query.text).unwrap();
            let terms = ripsem::lexical::query_terms(query.text);
            let names = ripsem::lexical::query_names(query.text);
            let ranked = [
                index.dense_ranking(&vector, DEPTH, None),
                index.lexical_ranking(&terms, DEPTH, None).hits().to_vec(),
                index.search_fused(&vector, &terms, &names, DEPTH, None),
            ];
            for (arm, hits) in ranked.iter().enumerate() {
                let rank = rank_of(&index, hits, query.targets);
                here[arm].record(rank);
                overall[arm].record(rank);
                let slot = by_shape[arm]
                    .iter_mut()
                    .find(|(s, _)| *s == query.shape)
                    .unwrap();
                slot.1.record(rank);
                if arm == FUSED {
                    cost.record(&index, hits, &root, rank);
                    if announce_misses && rank.is_none() {
                        eprintln!(
                            "  miss [{}] {:?} (expected {:?})",
                            corpus.name, query.text, query.targets
                        );
                    }
                }
            }
        }
        report.push((corpus.name, corpus.language, index.records.len(), here));
    }

    Evaluation {
        arms: overall,
        shapes: by_shape,
        per_corpus: report,
        cost,
    }
}

#[test]
#[ignore = "clones pinned repositories and downloads the embedding model; run with --ignored --nocapture"]
fn fusion_beats_either_retriever_alone() {
    let run = exclusively(|| evaluate(ripsem::model::DEFAULT_MODEL, true));
    let Evaluation {
        arms: overall,
        shapes: by_shape,
        per_corpus,
        cost,
    } = run;

    for (name, language, chunks, scores) in &per_corpus {
        eprintln!("{name} ({language}, {chunks} chunks)");
        for (arm, label) in ARMS.iter().enumerate() {
            eprintln!("  {label:<14} {}", scores[arm].line());
        }
    }
    eprintln!("all corpora");
    for (arm, label) in ARMS.iter().enumerate() {
        eprintln!("  {label:<14} {}", overall[arm].line());
        for (shape, score) in &by_shape[arm] {
            eprintln!("    {:<12} {}", format!("{shape:?}"), score.line());
        }
    }

    eprintln!("cost         {}", cost.line());

    assert!(
        cost.context < cost.whole_files,
        "reading the results is not cheaper than reading the files they point at"
    );
    let fused = overall[FUSED].mrr();
    assert!(
        fused >= overall[0].mrr(),
        "fusion must not rank worse than the vector arm alone"
    );
    assert!(
        fused >= overall[1].mrr(),
        "fusion must not rank worse than the lexical arm alone"
    );
    assert!(
        overall[FUSED].recall_at_depth() >= 0.86,
        "recall@{DEPTH} too low: {:.2}",
        overall[FUSED].recall_at_depth()
    );
    assert!(fused >= 0.74, "MRR too low: {fused:.3}");
}

/// Measures every model in the catalogue, cheapest first, so a larger model has
/// to earn its download.
#[test]
#[ignore = "downloads several embedding models; run with --ignored --nocapture"]
fn a_larger_model_has_to_earn_its_cost() {
    let mut candidates: Vec<&ripsem::model::ModelInfo> = ripsem::model::catalog().iter().collect();
    candidates.sort_by_key(|info| info.disk_mb);
    let mut results = Vec::new();
    for info in candidates {
        let model = info.id;
        let run = exclusively(|| evaluate(model, false));
        eprintln!(
            "{model:<20} {}d ~{}MB  fused MRR={:.3}  vector-only MRR={:.3}",
            info.dims,
            info.disk_mb,
            run.arms[FUSED].mrr(),
            run.arms[0].mrr()
        );
        for (name, _, _, scores) in &run.per_corpus {
            eprintln!("  {name:<10} fused={:.3}", scores[FUSED].mrr());
        }
        for (shape, score) in &run.shapes[FUSED] {
            eprintln!("  {:<10} fused={:.3}", format!("{shape:?}"), score.mrr());
        }
        results.push((model, run));
    }

    let best = results
        .iter()
        .max_by(|a, b| a.1.arms[FUSED].mrr().total_cmp(&b.1.arms[FUSED].mrr()))
        .map(|(model, _)| *model)
        .unwrap();
    eprintln!("best measured: {best}");
}

/// Prints, for every question the merge does not answer in the top five, whether
/// the answer was anywhere in either arm's deep pool. A wanted file that no arm
/// surfaces at any depth is a retrieval failure and needs different indexing; one
/// that sits at rank 40 is a ranking failure and needs different scoring. The two
/// look identical in an aggregate and are not the same problem.
#[test]
#[ignore = "diagnostic; run with --ignored --nocapture misses"]
fn misses() {
    exclusively(|| {
        const DEEP: usize = 200;
        for corpus in CORPORA {
            let materialized = materialize(&corpus.source, corpus.name);
            let root = materialized.root();
            let config = Config {
                backend: Backend::Fastembed,
                root: root.clone(),
                ..Default::default()
            };
            let index = match Index::load(&config.index_path()) {
                Ok(index) => index,
                Err(_) => {
                    pipeline::index_repo(&config, true).unwrap();
                    Index::load(&config.index_path()).unwrap()
                }
            };
            let embedder = ripsem::embed::build(&config).unwrap();

            for query in corpus.queries {
                let vector = embedder.embed_query(query.text).unwrap();
                let terms = ripsem::lexical::query_terms(query.text);
                let names = ripsem::lexical::query_names(query.text);
                let shown = index.search_fused(&vector, &terms, &names, DEPTH, None);
                if rank_of(&index, &shown, query.targets).is_some() {
                    continue;
                }
                let deep = [
                    ("dense", index.dense_ranking(&vector, DEEP, None)),
                    (
                        "lexical",
                        index.lexical_ranking(&terms, DEEP, None).hits().to_vec(),
                    ),
                    (
                        "fused",
                        index.search_fused(&vector, &terms, &names, DEEP, None),
                    ),
                ];
                let reach: Vec<String> = deep
                    .iter()
                    .map(|(arm, hits)| match rank_of(&index, hits, query.targets) {
                        Some(rank) => format!("{arm}={rank}"),
                        None => format!("{arm}=beyond {DEEP}"),
                    })
                    .collect();
                eprintln!(
                    "\n[{}] {:?} {:?}\n  want {:?}  {}",
                    corpus.name,
                    query.shape,
                    query.text,
                    query.targets,
                    reach.join("  ")
                );
                for (position, hit) in shown.iter().enumerate() {
                    let record = &index.records[hit.record_idx];
                    eprintln!(
                        "  got {}. {}:{}-{} {}",
                        position + 1,
                        record.path,
                        record.start_line,
                        record.end_line,
                        record
                            .symbol
                            .as_ref()
                            .map(ripsem::symbol::Symbol::label)
                            .unwrap_or_default()
                    );
                }
            }
        }
    })
}

struct Outcome {
    score: Score,
    chunks: usize,
    wasted_results: usize,
    total_results: usize,
}

fn measure(config: &Config, queries: &[Query], generated: &HashSet<String>) -> Outcome {
    pipeline::index_repo(config, true).unwrap();
    let index = Index::load(&config.index_path()).unwrap();
    let embedder = ripsem::embed::build(config).unwrap();
    let mut outcome = Outcome {
        score: Score::default(),
        chunks: index.records.len(),
        wasted_results: 0,
        total_results: 0,
    };
    for query in queries {
        let vector = embedder.embed_query(query.text).unwrap();
        let terms = ripsem::lexical::query_terms(query.text);
        let names = ripsem::lexical::query_names(query.text);
        let hits = index.search_fused(&vector, &terms, &names, DEPTH, None);
        outcome.score.record(rank_of(&index, &hits, query.targets));
        outcome.total_results += hits.len();
        outcome.wasted_results += hits
            .iter()
            .filter(|h| generated.contains(&index.records[h.record_idx].path))
            .count();
    }
    outcome
}

/// What the two configurations return is compared, not how they rank. Ranking on
/// a corpus this size cannot settle it: `crates.io-index` occurs 409 times in
/// `Cargo.lock`, so indexing that file happens to give the word `index` the
/// document frequency of a stopword, and dropping it makes the word rare again.
///
/// The claim that does hold is the one the tool is for: a lock file is never the
/// answer to a question about behaviour, and any slot it takes in a result set is
/// context an agent pays for and cannot use.
#[test]
#[ignore = "downloads the embedding model; run with --ignored --nocapture"]
fn skipping_generated_files_stops_them_reaching_results() {
    let _sequential = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = TempDir::new();
    let archive = dir.path.join("corpus.tar");
    run(
        Command::new("git")
            .args(["archive", "--format=tar", OWN_REVISION])
            .arg("-o")
            .arg(&archive)
            .current_dir(repository()),
        "git archive of the whole tree",
    );
    extract(&archive, &dir.path);
    std::fs::remove_file(&archive).unwrap();

    let everything = Config {
        backend: Backend::Fastembed,
        root: dir.path.clone(),
        skip_generated: false,
        ..Default::default()
    };
    let generated = files_only_the_defaults_drop(&everything);
    assert!(
        !generated.is_empty(),
        "the corpus has no generated files to skip"
    );

    let before = measure(&everything, RIPSEM, &generated);
    let after = measure(
        &Config {
            skip_generated: true,
            ..everything
        },
        RIPSEM,
        &generated,
    );

    for (label, outcome) in [("everything", &before), ("skipped", &after)] {
        eprintln!(
            "{label:<11} {} chunks  {} of {} result slots generated  recall@{DEPTH}={:.2}  MRR={:.3}",
            outcome.chunks,
            outcome.wasted_results,
            outcome.total_results,
            outcome.score.recall_at_depth(),
            outcome.score.mrr()
        );
    }

    assert_eq!(
        after.wasted_results, 0,
        "a generated file still reached the results"
    );
    assert!(after.chunks < before.chunks);
    assert!(
        after.score.recall_at_depth() >= before.score.recall_at_depth(),
        "skipping generated files lost a target file"
    );
}

/// The exclusion set, taken from the walker itself rather than restated here, so
/// the test cannot drift from the patterns it is checking.
fn files_only_the_defaults_drop(config: &Config) -> HashSet<String> {
    let indexed = |config: &Config| -> HashSet<String> {
        ripsem::walk::collect(config)
            .unwrap()
            .iter()
            .map(|p| {
                p.strip_prefix(&config.root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    };
    let kept = indexed(&Config {
        skip_generated: true,
        ..config.clone()
    });
    indexed(config).difference(&kept).cloned().collect()
}

struct Declared {
    path: String,
    name: String,
    line: usize,
}

/// Every declaration the parser names in the indexed files, read from the tree
/// rather than from the chunks, so how a file is cut cannot decide the answer.
#[cfg(feature = "treesitter")]
fn declared_in(root: &Path, index: &Index) -> Vec<Declared> {
    fn walk(
        node: tree_sitter::Node,
        text: &str,
        enclosing: Option<&ripsem::symbol::Symbol>,
        path: &str,
        out: &mut Vec<Declared>,
    ) {
        let declared = ripsem::symbol::declaration(&node, text, enclosing);
        if let Some(symbol) = &declared
            && !matches!(symbol.kind.as_str(), "impl" | "mod")
        {
            out.push(Declared {
                path: path.to_string(),
                name: symbol.name.rsplit("::").next().unwrap().to_string(),
                line: node.start_position().row + 1,
            });
        }
        let scope = declared.as_ref().or(enclosing);
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children {
            walk(child, text, scope, path, out);
        }
    }

    let mut paths: Vec<&str> = index.records.iter().map(|r| r.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut out = Vec::new();
    for path in paths {
        let Some(spec) = ripsem::lang::detect(Path::new(path)) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(root.join(path)) else {
            continue;
        };
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&spec.language).unwrap();
        let tree = parser.parse(text.as_bytes(), None).unwrap();
        walk(tree.root_node(), &text, None, path, &mut out);
    }
    out
}

/// Names declared once, at least four characters long, at most `cap` of them
/// spread evenly over the corpus.
#[cfg(feature = "treesitter")]
fn unambiguous(declared: Vec<Declared>, cap: usize) -> Vec<Declared> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for d in &declared {
        *seen.entry(d.name.to_lowercase()).or_default() += 1;
    }
    let single: Vec<Declared> = declared
        .into_iter()
        .filter(|d| d.name.len() >= 4 && seen[&d.name.to_lowercase()] == 1)
        .collect();
    let stride = single.len().div_ceil(cap).max(1);
    single.into_iter().step_by(stride).collect()
}

/// A search by a declared name has found it only when a hit points at the
/// declaration's line and is labelled with that name. A hit on the right lines
/// under another declaration's label reads, to an agent, as somewhere else.
#[cfg(feature = "treesitter")]
#[test]
#[ignore = "clones pinned repositories and downloads the embedding model; run with --ignored --nocapture"]
fn a_declared_name_leads_to_its_declaration() {
    const SHOWN: usize = 10;
    let mut named = Score::default();
    let mut located = Score::default();
    let mut mislabelled = 0usize;
    exclusively(|| {
        for corpus in CORPORA {
            let materialized = materialize(&corpus.source, corpus.name);
            let root = materialized.root();
            let config = Config {
                backend: Backend::Fastembed,
                root: root.clone(),
                ..Default::default()
            };
            pipeline::index_repo(&config, true).unwrap();
            let index = Index::load(&config.index_path()).unwrap();
            let wanted = unambiguous(declared_in(&root, &index), 150);
            let searcher = pipeline::open(&config).unwrap();
            let mut here = Score::default();
            for d in &wanted {
                let hits = searcher.query(&d.name, SHOWN).unwrap();
                let covers = |h: &&pipeline::SearchHit| {
                    h.path == d.path && h.start_line <= d.line && d.line <= h.end_line
                };
                let names = |h: &&pipeline::SearchHit| {
                    h.symbol
                        .as_ref()
                        .is_some_and(|s| s.name.rsplit("::").next() == Some(d.name.as_str()))
                };
                let rank = hits
                    .iter()
                    .position(|h| covers(&h) && names(&h))
                    .map(|p| p + 1);
                let place = hits.iter().position(|h| covers(&h)).map(|p| p + 1);
                if place == Some(1) && rank != Some(1) {
                    mislabelled += 1;
                }
                here.record(rank);
                named.record(rank);
                located.record(place);
            }
            eprintln!("{:<9} {}", corpus.name, here.line());
        }
    });
    eprintln!("declaration  {}", named.line());
    eprintln!("its lines    {}", located.line());
    eprintln!("first hit on the right lines under another name: {mislabelled}");
}
