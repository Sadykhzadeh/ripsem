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
    let mut body = String::new();
    for i in 0..9000 {
        if i == 5000 {
            body.push_str("zebraquux singular beacon token\n");
        } else {
            body.push_str(&format!("filler row {i}\n"));
        }
    }
    dir.write("big.txt", &body);
    let config = config_for(&dir.path);

    let stats = pipeline::index_repo(&config, false).unwrap();
    assert!(stats.chunks > 256);

    let hits = pipeline::search(&config, "zebraquux singular beacon", 5).unwrap();
    assert_eq!(hits[0].path, "big.txt");
    assert!(hits[0].start_line <= 5001 && hits[0].end_line >= 5001);
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
