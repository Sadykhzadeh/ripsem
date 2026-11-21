use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ripsem::config::{Backend, Config};
use ripsem::watch;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ripsem-watch-{}-{}", std::process::id(), n));
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

/// A search that arrives while the watcher is still being set up must be told
/// the index is on its way, not that there is none. The indexing flag is
/// process-wide, so this test is alone in its binary: a watch left running by
/// another test would move the flag under it.
#[test]
fn the_first_index_counts_as_underway_from_the_moment_the_watch_starts() {
    let dir = TempDir::new();
    dir.write("a.rs", "fn a() {}\n");
    let config = Config {
        root: dir.path.clone(),
        backend: Backend::Hash,
        ..Default::default()
    };
    config.create_data_dir().unwrap();

    watch::spawn(config);
    assert!(watch::indexing());

    let deadline = Instant::now() + Duration::from_secs(20);
    while watch::indexing() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !watch::indexing(),
        "the flag has to clear once the index is built"
    );
}
