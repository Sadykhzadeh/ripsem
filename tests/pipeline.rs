use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ripsem::config::{Backend, Config};
use ripsem::pipeline;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ripsem-it-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn write(&self, rel: &str, contents: &str) {
        std::fs::write(self.path.join(rel), contents).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn config_for(root: &Path) -> Config {
    Config {
        backend: Backend::Hash,
        root: root.to_path_buf(),
        ..Default::default()
    }
}

#[test]
fn indexes_and_searches_by_overlapping_terms() {
    let dir = TempDir::new();
    dir.write(
        "auth.rs",
        "fn authenticate(user) { verify password create session token logout }",
    );
    dir.write(
        "math.rs",
        "fn add(a, b) { return arithmetic sum product divide remainder }",
    );
    let config = config_for(&dir.path);

    let stats = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(stats.files, 2);
    assert_eq!(stats.embedded, 2);
    assert_eq!(stats.reused, 0);

    let hits = pipeline::search(&config, "verify password session token", 5).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].path, "auth.rs");
}

#[test]
fn indexes_correctly_across_embedding_batches() {
    let dir = TempDir::new();
    // Each file is 32 windows of 40 lines, so 140 of them queue more chunks
    // than one embedding window holds.
    for file in 0..140 {
        let mut body = String::new();
        for i in 0..1000 {
            if file == 120 && i == 500 {
                body.push_str("zebraquux singular beacon token\n");
            } else {
                body.push_str(&format!("filler row {i}\n"));
            }
        }
        dir.write(&format!("part{file}.txt"), &body);
    }
    let config = config_for(&dir.path);

    let stats = pipeline::index_repo(&config, false).unwrap();
    assert!(stats.chunks > 4096, "{} chunks", stats.chunks);

    let hits = pipeline::search(&config, "zebraquux singular beacon", 5).unwrap();
    assert_eq!(hits[0].path, "part120.txt");
    assert!(hits[0].start_line <= 501 && hits[0].end_line >= 501);
}

/// Chunks are embedded shortest first and stored in walk order, so a slip
/// in putting the vectors back would give a record somebody else's vector.
/// The hash backend is deterministic, so each record's vector can be checked
/// against its own text.
#[test]
fn every_record_keeps_its_own_vector_when_embedded_out_of_order() {
    use ripsem::embed::Embedder;
    use ripsem::embed::hashing::HashEmbedder;
    use ripsem::index::Index;

    let dir = TempDir::new();
    for i in 0..50 {
        let words: Vec<String> = (0..(i * 7 % 23 + 1))
            .map(|w| format!("word{i}x{w}"))
            .collect();
        dir.write(&format!("f{i}.rs"), &words.join(" "));
    }
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let embedder = HashEmbedder::new(config.dims());
    let index = Index::load(&config.index_path()).unwrap();
    assert_eq!(index.records.len(), 50);
    for record in &index.records {
        let own = embedder.embed_query(&record.snippet).unwrap();
        let similarity = ripsem::quant::dot(&record.vector, &own);
        assert!(similarity > 0.99, "{} got {similarity}", record.path);
    }
}

#[test]
fn unchanged_files_are_reused_on_reindex() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo foxtrot");
    dir.write("b.rs", "golf hotel india juliet kilo lima");
    let config = config_for(&dir.path);

    let first = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(first.embedded, 2);

    dir.write(
        "a.rs",
        "alpha bravo charlie delta echo foxtrot mike november",
    );
    let second = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(second.embedded, 1);
    assert_eq!(second.reused, 1);
}

/// A backend this build cannot load stands in for the model here: if a
/// reindex with nothing to embed still built the embedder, it would fail.
#[cfg(not(feature = "fastembed"))]
#[test]
fn a_reindex_with_nothing_to_embed_never_loads_the_model() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo foxtrot");
    dir.write("b.rs", "golf hotel india juliet kilo lima");
    let mut config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    config.backend = Backend::Fastembed;
    let unchanged = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(unchanged.embedded, 0);

    std::fs::remove_file(dir.path.join("b.rs")).unwrap();
    let removed = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(removed.chunks, 1);

    dir.write("a.rs", "alpha bravo charlie delta echo foxtrot mike");
    let err = pipeline::index_repo(&config, false).unwrap_err();
    assert!(err.to_string().contains("fastembed"), "got: {err}");
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// A running server re-reads the index whenever its mtime moves, and the
/// graph is rebuilt whenever the file is written, so an unchanged tree has to
/// leave the file alone. A removal has to reach it all the same. The pauses
/// are for the clock the file is stamped from, as in the hot-reload test.
#[test]
fn an_unchanged_tree_leaves_the_index_file_alone() {
    use ripsem::index::Index;

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo foxtrot");
    dir.write("b.rs", "golf hotel india juliet kilo lima");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();
    let written = modified(&config.index_path());

    std::thread::sleep(std::time::Duration::from_millis(20));
    let again = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(again.reused, 2);
    assert_eq!(again.chunks, 2);
    assert_eq!(modified(&config.index_path()), written);

    std::fs::remove_file(dir.path.join("b.rs")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    pipeline::index_repo(&config, false).unwrap();
    assert_ne!(modified(&config.index_path()), written);
    let on_disk = Index::load(&config.index_path()).unwrap();
    assert!(on_disk.records.iter().all(|r| r.path == "a.rs"));
}

#[test]
fn switching_model_reembeds_without_force() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    dir.write("b.rs", "delta echo foxtrot");
    let mut config = config_for(&dir.path);

    pipeline::index_repo(&config, false).unwrap();

    config.model = "bge-base-en-v1.5".into();
    let after = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(after.reused, 0);
    assert_eq!(after.embedded, 2);
}

#[test]
fn scoped_search_limits_results_to_a_subdirectory() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path.join("src")).unwrap();
    std::fs::create_dir_all(dir.path.join("docs")).unwrap();
    dir.write("src/auth.rs", "verify password create session token");
    dir.write("docs/auth.md", "verify password create session token");
    let config = config_for(&dir.path);

    pipeline::index_repo(&config, false).unwrap();
    let searcher = pipeline::open(&config).unwrap();

    let hits = searcher
        .query_scoped("verify password session token", 10, Some("src"))
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.path.starts_with("src/")));
}

#[test]
fn search_scoped_convenience_limits_results_to_a_prefix() {
    // The `search_scoped` convenience (backing the CLI `search --in`) scopes the
    // whole-index search to a path prefix, returning only matching files.
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path.join("src")).unwrap();
    std::fs::create_dir_all(dir.path.join("docs")).unwrap();
    dir.write("src/auth.rs", "verify password create session token");
    dir.write("docs/auth.md", "verify password create session token");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let scoped =
        pipeline::search_scoped(&config, "verify password session token", 10, Some("src")).unwrap();
    assert!(!scoped.is_empty());
    assert!(scoped.iter().all(|h| h.path.starts_with("src/")));

    // No scope (None) searches the whole index — docs/ is reachable too.
    let all = pipeline::search_scoped(&config, "verify password session token", 10, None).unwrap();
    assert!(all.iter().any(|h| h.path.starts_with("docs/")));
}

#[test]
fn searcher_hot_reloads_a_rebuilt_index() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let mut searcher = pipeline::open(&config).unwrap();
    let before = searcher.query("zulu yankee xray", 5).unwrap();
    assert!(before.iter().all(|h| h.path == "a.rs"));

    std::thread::sleep(std::time::Duration::from_millis(20));
    dir.write("b.rs", "zulu yankee xray whiskey victor");
    pipeline::index_repo(&config, false).unwrap();

    assert!(searcher.refresh().unwrap());
    let after = searcher.query("zulu yankee xray", 5).unwrap();
    assert!(after.iter().any(|h| h.path == "b.rs"));
}

#[test]
fn searcher_rejects_a_same_dim_different_model_index() {
    use ripsem::index::Index;

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let mut searcher = pipeline::open(&config).unwrap();
    let original_model = searcher.model().to_string();
    let original_records = searcher.record_count();

    // Rewrite the on-disk index with the SAME dimensionality but a different
    // model (both 384-d). Cross-model vectors are not comparable, so the
    // long-running searcher must keep its current index.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut rebuilt = Index::load(&config.index_path()).unwrap();
    assert_ne!(rebuilt.model, "all-MiniLM-L6-v2");
    rebuilt.model = "all-MiniLM-L6-v2".into();
    rebuilt.records.clear();
    rebuilt.save(&config.index_path()).unwrap();

    assert!(!searcher.refresh().unwrap());
    assert_eq!(searcher.model(), original_model);
    assert_eq!(searcher.record_count(), original_records);
}

/// Changing the model without rebuilding leaves an index whose vectors came
/// from a different model than the one the query is embedded with. A long-lived
/// searcher already refuses that swap; opening one has to refuse it too, rather
/// than ranking on the comparison of two unrelated spaces.
#[test]
fn opening_an_index_built_with_another_model_is_a_clear_error() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo");
    let mut config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    config.model = "all-MiniLM-L6-v2".into();
    let err = match pipeline::open(&config) {
        Ok(_) => panic!("an index from another model must not open"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("index --force"), "got: {err}");
    assert!(err.contains("all-MiniLM-L6-v2"), "got: {err}");

    // Rebuilding for the configured model clears it.
    pipeline::index_repo(&config, false).unwrap();
    assert_eq!(pipeline::open(&config).unwrap().model(), "all-MiniLM-L6-v2");
}

/// What `status` reports as drift has to be exactly what a rebuild would touch,
/// and has to return to nothing once that rebuild has happened — including for
/// a file the indexer never produces a chunk for.
#[test]
fn drift_counts_what_a_rebuild_would_touch() {
    use ripsem::index::Index;
    use ripsem::pipeline::Drift;

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    dir.write("b.rs", "delta echo foxtrot");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let measured = || {
        let index = Index::load(&config.index_path()).unwrap();
        pipeline::drift(&config, &index).unwrap()
    };
    assert!(measured().is_current());

    dir.write("a.rs", "alpha bravo charlie golf");
    dir.write("c.rs", "hotel india juliet");
    std::fs::remove_file(dir.path.join("b.rs")).unwrap();
    // Whitespace alone yields no chunks, so it is never in the index and must
    // not be reported as something a rebuild would add.
    dir.write("blank.rs", "   \n\n");

    assert_eq!(
        measured(),
        Drift {
            added: 1,
            modified: 1,
            removed: 1
        }
    );

    pipeline::index_repo(&config, false).unwrap();
    assert!(
        measured().is_current(),
        "a rebuild has to settle the count it reported"
    );
}

#[test]
fn empty_repository_indexes_without_error() {
    let dir = TempDir::new();
    let config = config_for(&dir.path);

    let stats = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(stats.files, 0);
    assert_eq!(stats.chunks, 0);

    let hits = pipeline::search(&config, "anything at all", 5).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn searching_without_an_index_is_a_clear_error() {
    let dir = TempDir::new();
    let config = config_for(&dir.path);

    let err = pipeline::search(&config, "where is auth", 5).unwrap_err();
    assert!(err.to_string().contains("ripsem index"));
}

#[test]
fn force_reindex_reembeds_everything() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    dir.write("b.rs", "delta echo foxtrot");
    let config = config_for(&dir.path);

    pipeline::index_repo(&config, false).unwrap();
    let forced = pipeline::index_repo(&config, true).unwrap();
    assert_eq!(forced.embedded, 2);
    assert_eq!(forced.reused, 0);
}
