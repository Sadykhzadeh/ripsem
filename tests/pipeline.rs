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
    // Records are reused only for the backend that embedded them.
    let mut index = ripsem::index::Index::load(&config.index_path()).unwrap();
    index.backend = Backend::Fastembed;
    index.save(&config.index_path()).unwrap();

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
fn a_reindex_with_nothing_to_do_still_writes_the_data_gitignore() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo foxtrot");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();
    let gitignore = config.data_dir().join(".gitignore");
    assert!(gitignore.exists());

    // A data directory that holds an index but no `.gitignore`.
    std::fs::remove_file(&gitignore).unwrap();
    let again = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(again.reused, 1);
    assert!(gitignore.exists());
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
fn a_searcher_answers_the_same_after_warming_the_model() {
    let dir = TempDir::new();
    dir.write("auth.rs", "verify password create session token");
    dir.write("math.rs", "add subtract multiply divide numbers");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let cold = pipeline::open(&config).unwrap();
    let warmed = pipeline::open(&config).unwrap();
    warmed.warm();

    let paths = |s: &pipeline::Searcher| -> Vec<String> {
        s.query("verify password session token", 5)
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect()
    };
    assert_eq!(paths(&cold), paths(&warmed));
    assert_eq!(paths(&warmed)[0], "auth.rs");
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

    // A scope that matches no file is a mistake to report, not an empty answer.
    let err = pipeline::search_scoped(&config, "verify password", 10, Some("srcc")).unwrap_err();
    assert!(
        err.to_string().contains("nothing is indexed under 'srcc'"),
        "{err}"
    );
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

    let err = searcher.refresh().unwrap_err().to_string();
    assert!(err.contains("all-MiniLM-L6-v2"), "got: {err}");
    assert_eq!(searcher.model(), original_model);
    assert_eq!(searcher.record_count(), original_records);

    // Until the file changes again it is not read again: decoding a large
    // index on every request would cost each search as much as opening one.
    // Bytes that do not decode, under the same mtime, show it is not read.
    let refused = modified(&config.index_path());
    std::fs::write(config.index_path(), b"not an index").unwrap();
    std::fs::File::options()
        .write(true)
        .open(config.index_path())
        .unwrap()
        .set_modified(refused)
        .unwrap();
    assert!(!searcher.refresh().unwrap());
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
fn a_search_finds_code_written_after_the_index_was_built() {
    let dir = TempDir::new();
    dir.write("auth.rs", "fn authenticate(user) { verify password }");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    dir.write("beacon.rs", "fn zebraquux_beacon() { singular }");
    let hits = pipeline::search_scoped(&config, "zebraquux beacon", 5, None).unwrap();
    assert_eq!(hits[0].path, "beacon.rs");
}

#[test]
fn the_first_search_in_a_repository_builds_its_index() {
    let dir = TempDir::new();
    std::fs::create_dir(dir.path.join(".git")).unwrap();
    dir.write("auth.rs", "fn authenticate(user) { verify password }");
    let config = config_for(&dir.path);

    let hits = pipeline::search_scoped(&config, "verify password", 5, None).unwrap();
    assert_eq!(hits[0].path, "auth.rs");
    assert!(config.index_path().exists());
}

#[test]
fn a_search_outside_a_repository_does_not_build_an_index() {
    let dir = TempDir::new();
    dir.write("auth.rs", "fn authenticate(user) { verify password }");
    let config = config_for(&dir.path);

    let err = pipeline::search_scoped(&config, "verify password", 5, None).unwrap_err();
    assert!(err.to_string().contains("ripsem index"));
    assert!(!config.index_path().exists());
}

/// The hash backend gives its own vectors under any model name, so an index it
/// built cannot answer a query embedded by the model, or the other way round.
#[test]
fn opening_an_index_built_by_another_backend_is_a_clear_error() {
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie delta echo");
    let mut config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    config.backend = Backend::Fastembed;
    let err = match pipeline::open(&config) {
        Ok(_) => panic!("an index from another backend must not open"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("index --force"), "got: {err}");
    assert!(err.contains("Hash backend"), "got: {err}");
    assert!(err.contains("Fastembed backend"), "got: {err}");
}

#[test]
fn switching_backend_reembeds_without_force() {
    use ripsem::index::Index;

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    dir.write("b.rs", "delta echo foxtrot");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    // The same files and model, embedded by the other backend.
    let mut other = Index::load(&config.index_path()).unwrap();
    other.backend = Backend::Fastembed;
    other.save(&config.index_path()).unwrap();

    let stats = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(stats.reused, 0);
    assert_eq!(stats.embedded, 2);
    assert!(pipeline::open(&config).is_ok());
}

/// An index file from a ripsem that wrote another format: searching names the
/// cause, and a plain `ripsem index` replaces it.
#[test]
fn an_index_from_an_older_ripsem_is_reported_and_rebuilt() {
    let dir = TempDir::new();
    dir.write("auth.rs", "fn authenticate(user) { verify password }");
    let config = config_for(&dir.path);
    std::fs::create_dir_all(config.data_dir()).unwrap();
    // The first bytes of a format 6 index, which had no header.
    std::fs::write(config.index_path(), [6, 0, 0, 0, 0, 1, 0, 0]).unwrap();

    let err = format!("{:#}", pipeline::search(&config, "verify", 5).unwrap_err());
    assert!(err.contains("from an older ripsem"), "got: {err}");
    assert!(!err.contains("no index yet"), "got: {err}");

    let stats = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(stats.embedded, 1);
    let hits = pipeline::search(&config, "verify password", 5).unwrap();
    assert_eq!(hits[0].path, "auth.rs");
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

#[test]
fn changing_the_chunk_settings_chunks_every_file_again() {
    use ripsem::index::Index;

    let dir = TempDir::new();
    let lines: String = (1..=30)
        .map(|n| format!("row {n} of the notes\n"))
        .collect();
    dir.write("a.txt", &lines);
    dir.write("b.txt", &lines);
    let mut config = config_for(&dir.path);
    let first = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(first.chunks, 2);

    config.chunk_lines = 10;
    config.chunk_overlap = 0;
    let built = Index::load(&config.index_path()).unwrap();
    let drift = pipeline::drift(&config, &built).unwrap();
    assert_eq!(drift.modified, 2, "a rebuild would cut both files again");

    let second = pipeline::index_repo(&config, false).unwrap();
    assert_eq!((second.embedded, second.reused), (2, 0));
    assert_eq!(second.chunks, 6);
    let rebuilt = Index::load(&config.index_path()).unwrap();
    assert!(pipeline::drift(&config, &rebuilt).unwrap().is_current());

    let third = pipeline::index_repo(&config, false).unwrap();
    assert_eq!((third.embedded, third.reused), (0, 2));
}

#[test]
fn an_empty_query_is_an_error_not_a_ranking() {
    let dir = TempDir::new();
    dir.write("auth.rs", "fn authenticate(user) { verify password }");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    for query in ["", "   \n"] {
        let err = pipeline::search(&config, query, 5).unwrap_err();
        assert!(err.to_string().contains("query is empty"), "got: {err}");
    }
    assert!(!pipeline::search(&config, "???", 5).unwrap().is_empty());
}

/// What `ripsem mcp` runs beside the server: the first index is built without
/// anyone running `ripsem index`, and a file written afterwards is searchable
/// without anyone running it again.
#[test]
fn the_index_keeps_itself_current() {
    let dir = TempDir::new();
    dir.write("auth.rs", "fn authenticate(user) { verify password }");
    let config = config_for(&dir.path);
    config.save().unwrap();
    ripsem::watch::spawn(config.clone());

    let finds = |query: &str, path: &str| {
        pipeline::search(&config, query, 5)
            .map(|hits| hits.iter().any(|h| h.path == path))
            .unwrap_or(false)
    };
    let within = |seconds: u64, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    };

    assert!(within(30, &|| finds("verify password", "auth.rs")));
    dir.write(
        "billing.rs",
        "fn charge_invoice(customer) { stripe ledger }",
    );
    assert!(within(30, &|| finds("stripe ledger", "billing.rs")));
}

/// The field case: three small functions share one chunk, and the one asked
/// for is neither the first nor the only place its name occurs.
#[cfg(feature = "treesitter")]
/// Windows tools save text as UTF-16 with a byte order mark. The file is text,
/// so it is indexed, and its code is found and shown like any other file's.
#[test]
fn a_utf16_file_is_indexed_as_its_text() {
    let dir = TempDir::new();
    let src = "fn verify_password(user) {\n    check_secret(user)\n}\n";
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(src.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(dir.path.join("login.rs"), &bytes).unwrap();
    dir.write(
        "math.rs",
        "fn add(a, b) { return arithmetic sum product divide remainder }",
    );
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let hits = pipeline::search(&config, "verify password", 5).unwrap();
    assert!(
        !hits.is_empty() && hits[0].path == "login.rs",
        "got {hits:#?}"
    );
    assert_eq!((hits[0].start_line, hits[0].end_line), (1, 3));
    assert!(
        hits[0].snippet.contains("check_secret"),
        "{}",
        hits[0].snippet
    );
}

#[cfg(feature = "treesitter")]
#[test]
fn a_search_by_name_reports_the_declaration_not_its_chunk() {
    let dir = TempDir::new();
    let mut src = String::from(
        "struct Road;\n\nimpl Road {\n    fn preview(&self) -> u32 {\n        self.quote_cost() + 1\n    }\n\n",
    );
    for i in 0..6 {
        src.push_str(&format!(
            "    fn filler_{i}(&self) -> u32 {{\n        {i}\n    }}\n\n"
        ));
    }
    src.push_str("    pub fn quote_cost(&self) -> u32 {\n        let base = 3;\n        base * 2\n    }\n}\n");
    dir.write("road.rs", &src);
    dir.write(
        "ui.rs",
        "fn show(road: &Road) {\n    let total = road.quote_cost();\n    println!(\"{total}\");\n}\n",
    );
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let declared_at = src
        .lines()
        .position(|l| l.contains("pub fn quote_cost"))
        .unwrap()
        + 1;
    for query in [
        "quote_cost",
        "fn quote_cost",
        "Road::quote_cost",
        "quote cost",
    ] {
        let hits = pipeline::search(&config, query, 5).unwrap();
        let first = &hits[0];
        assert_eq!(
            (first.path.as_str(), first.start_line, first.end_line),
            ("road.rs", declared_at, declared_at + 3),
            "{query}: {hits:#?}"
        );
        assert_eq!(
            first.symbol.as_ref().map(|s| s.label()),
            Some("fn Road::quote_cost".to_string()),
            "{query}"
        );
        assert!(
            first.snippet.starts_with("pub fn quote_cost"),
            "{query}: {}",
            first.snippet
        );
    }
}

/// An edit to one function embeds that function again, not its whole file.
#[cfg(feature = "treesitter")]
#[test]
fn an_edit_embeds_only_the_chunks_it_changed() {
    let dir = TempDir::new();
    let body = |edited: &str| {
        let mut src = String::new();
        for i in 0..8 {
            src.push_str(&format!("fn step_{i}() -> u32 {{\n"));
            for line in 0..30 {
                src.push_str(&format!("    let v{line} = {i} + {line};\n"));
            }
            if i == 5 {
                src.push_str(edited);
            }
            src.push_str("    0\n}\n\n");
        }
        src
    };
    dir.write("big.rs", &body(""));
    let config = config_for(&dir.path);
    let first = pipeline::index_repo(&config, false).unwrap();
    assert!(first.vectors >= 8, "{first:?}");

    dir.write("big.rs", &body("    // changed\n"));
    let second = pipeline::index_repo(&config, false).unwrap();
    assert_eq!((second.embedded, second.vectors), (1, 1), "{second:?}");
    assert_eq!(second.chunks, first.chunks);

    let fresh = TempDir::new();
    fresh.write("big.rs", &body("    // changed\n"));
    let rebuilt = pipeline::index_repo(&config_for(&fresh.path), false).unwrap();
    let a = ripsem::index::Index::load(&config.index_path()).unwrap();
    let b = ripsem::index::Index::load(&config_for(&fresh.path).index_path()).unwrap();
    assert_eq!(rebuilt.chunks, second.chunks);
    for (x, y) in a.records.iter().zip(&b.records) {
        assert_eq!(
            (x.start_line, x.end_line, x.text_hash),
            (y.start_line, y.end_line, y.text_hash)
        );
        assert_eq!(
            (x.vector.scale, &x.vector.data),
            (y.vector.scale, &y.vector.data),
            "a reused vector differs from a fresh one"
        );
    }
}

/// A declaration longer than a chunk is cut into pieces; a search for its name
/// reports the whole of it from its first line.
#[cfg(feature = "treesitter")]
#[test]
fn a_search_for_a_large_declaration_reports_all_of_it() {
    let dir = TempDir::new();
    let mut src = String::from("pub enum Command {\n");
    for i in 0..80 {
        src.push_str(&format!("    Variant{i},\n"));
    }
    src.push_str("}\n\nimpl Command {\n    fn run(&self) {}\n}\n");
    dir.write("cli.rs", &src);
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let hits = pipeline::search(&config, "Command", 5).unwrap();
    let first = &hits[0];
    assert_eq!((first.start_line, first.end_line), (1, 82), "{hits:#?}");
    assert_eq!(
        first.symbol.as_ref().map(|s| s.label()),
        Some("enum Command".to_string())
    );
    assert!(
        first.snippet.starts_with("pub enum Command {"),
        "{}",
        first.snippet
    );
    assert!(
        hits[1..]
            .iter()
            .all(|h| h.path != "cli.rs" || h.end_line > 82 || h.start_line < 1),
        "a piece of the declaration repeats its lines: {hits:#?}"
    );
}

/// Other chunk settings cut files again, but a chunk whose text the new cut
/// leaves as it was keeps its vector.
#[test]
fn new_chunk_settings_keep_the_vectors_of_unchanged_chunks() {
    let dir = TempDir::new();
    let body: String = (0..200).map(|i| format!("line {i}\n")).collect();
    dir.write("notes.txt", &body);
    let config = Config {
        chunk_lines: 40,
        chunk_overlap: 0,
        ..config_for(&dir.path)
    };
    let first = pipeline::index_repo(&config, false).unwrap();
    assert_eq!(first.vectors, 5);

    let wider = Config {
        chunk_lines: 80,
        ..config
    };
    let second = pipeline::index_repo(&wider, false).unwrap();
    assert_eq!(
        (second.chunks, second.vectors),
        (3, 2),
        "lines 161-200 are a window under both cuts"
    );
    let back = pipeline::index_repo(
        &Config {
            chunk_lines: 40,
            ..wider
        },
        false,
    )
    .unwrap();
    assert_eq!((back.chunks, back.vectors), (5, 4));
}

#[test]
fn a_repository_with_nothing_to_index_is_written_once() {
    let dir = TempDir::new();
    dir.write("empty.txt", "   \n");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();
    let written = std::fs::metadata(config.index_path())
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    pipeline::index_repo(&config, false).unwrap();
    let again = std::fs::metadata(config.index_path())
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(written, again);
}

#[test]
fn a_huge_result_count_is_not_reserved_up_front() {
    let dir = TempDir::new();
    dir.write("a.rs", "fn alpha() {}\n");
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();
    let hits = pipeline::search(&config, "alpha", usize::MAX).unwrap();
    assert_eq!(hits.len(), 1);
}

/// A search for a large block's name reports the whole block, and the
/// methods inside it, which carry its name too, are still answers of their own.
#[cfg(feature = "treesitter")]
#[test]
fn a_named_block_does_not_hide_the_methods_inside_it() {
    let dir = TempDir::new();
    let mut src = String::from("impl Searcher {\n");
    for i in 0..20 {
        src.push_str(&format!(
            "    fn helper{i}(&self) -> u32 {{\n        {i}\n    }}\n\n"
        ));
    }
    src.push_str("    fn refresh(&self, mtime: u64) -> bool {\n        mtime > 0\n    }\n}\n");
    dir.write("searcher.rs", &src);
    let config = config_for(&dir.path);
    pipeline::index_repo(&config, false).unwrap();

    let refresh = src.lines().position(|l| l.contains("fn refresh")).unwrap() + 1;
    let hits = pipeline::search(&config, "Searcher", 5).unwrap();
    assert!(
        hits.iter().any(|h| h.start_line <= refresh
            && refresh <= h.end_line
            && h.symbol.as_ref().is_some_and(|s| s.kind == "fn")),
        "{hits:#?}"
    );
}
