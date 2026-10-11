use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ripsem-cli-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn write(&self, rel: &str, contents: &str) {
        let target = self.path.join(rel);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, contents).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn ripsem(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ripsem"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn commands_find_the_index_from_a_subdirectory() {
    let dir = TempDir::new();
    dir.write(
        "src/deep/auth.rs",
        "fn authenticate(user) { verify password }",
    );
    assert!(ripsem(&dir.path, &["init"]).status.success());
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let below = dir.path.join("src/deep");
    let found = ripsem(&below, &["search", "verify password", "--json"]);
    assert!(
        found.status.success(),
        "{}",
        String::from_utf8_lossy(&found.stderr)
    );
    assert!(stdout(&found).contains("src/deep/auth.rs"));

    let status = ripsem(&below, &["status"]);
    assert!(stdout(&status).contains("chunks:"), "{}", stdout(&status));
    assert!(!below.join(".ripsem").exists());
}

#[test]
fn an_explicit_path_is_used_as_given() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let below = dir.path.join("src");
    let nested = below.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let out = ripsem(
        &nested,
        &["--path", below.to_str().unwrap(), "search", "verify"],
    );
    assert!(!out.status.success());
}

#[test]
fn init_makes_the_current_directory_a_root_even_inside_another() {
    let dir = TempDir::new();
    dir.write("inner/lib.rs", "fn main() {}");
    assert!(ripsem(&dir.path, &["init"]).status.success());

    let inner = dir.path.join("inner");
    assert!(ripsem(&inner, &["init"]).status.success());
    assert!(inner.join(".ripsem/config.toml").exists());
}
