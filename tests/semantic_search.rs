#![cfg(feature = "fastembed")]

use std::path::PathBuf;
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
        let path = std::env::temp_dir().join(format!("ripsem-sem-{}-{}", std::process::id(), n));
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

#[test]
#[ignore = "downloads the embedding model; run with --ignored"]
fn ranks_by_meaning_not_keywords() {
    let dir = TempDir::new();
    dir.write(
        "auth.rs",
        "fn login(credentials) {\n    check the user password\n    issue a session cookie\n    deny access when the secret is wrong\n}\n",
    );
    dir.write(
        "geometry.rs",
        "fn area(shape) {\n    multiply width by height\n    compute the surface of a rectangle\n    return the result in square units\n}\n",
    );

    let config = Config {
        backend: Backend::Fastembed,
        root: dir.path.clone(),
        ..Default::default()
    };

    pipeline::index_repo(&config, true).unwrap();
    let hits = pipeline::search(
        &config,
        "verifying a person's identity before granting entry",
        5,
    )
    .unwrap();

    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].path, "auth.rs",
        "semantic search should rank auth above geometry without shared keywords"
    );
}
