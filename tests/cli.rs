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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn every_command_says_when_search_matches_words_not_meaning() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    let warned = |output: &Output| stderr(output).contains("warning: hashing backend");

    // `init` writes the build's default backend, which is hashing only in a
    // build without the model.
    let init = ripsem(&dir.path, &["init"]);
    assert!(init.status.success());
    assert_eq!(warned(&init), cfg!(not(feature = "fastembed")));
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(warned(&ripsem(&dir.path, &["index"])));
    assert!(warned(&ripsem(&dir.path, &["search", "verify"])));
    let json = ripsem(&dir.path, &["search", "verify", "--json"]);
    assert!(warned(&json));
    assert!(stdout(&json).trim_start().starts_with('['));
    assert!(warned(&ripsem(&dir.path, &["status"])));
    let status = ripsem(&dir.path, &["status", "--json"]);
    assert!(stdout(&status).contains("\"warning\": \"hashing backend"));

    ripsem(&dir.path, &["model", "backend", "fastembed"]);
    let status = ripsem(&dir.path, &["status"]);
    assert!(!stderr(&status).contains("warning"), "{}", stderr(&status));
    assert!(stdout(&ripsem(&dir.path, &["status", "--json"])).contains("\"warning\": null"));
}

fn git(cwd: &Path, args: &[&str]) -> String {
    // The user's global excludes could hide `.ripsem` and pass this for them.
    let excludes = cwd.join("no-global-excludes");
    let out = Command::new("git")
        .arg("-c")
        .arg(format!("core.excludesFile={}", excludes.display()))
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git is needed for this test");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout(&out)
}

#[test]
fn git_sees_the_config_but_not_the_index() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    git(&dir.path, &["init", "-q"]);
    assert!(ripsem(&dir.path, &["init"]).status.success());
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());
    assert!(dir.path.join(".ripsem/index.bin").exists());

    let untracked = git(
        &dir.path,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert_eq!(
        untracked.lines().collect::<Vec<_>>(),
        [
            "?? .ripsem/.gitignore",
            "?? .ripsem/config.toml",
            "?? src/auth.rs"
        ]
    );
}

/// One session over stdio, as a client of the 2025-06-18 revision runs it.
#[test]
fn the_mcp_server_answers_a_session_from_handshake_to_search() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let session = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"verify password"}}}"#,
    ];
    let mut server = Command::new(env!("CARGO_BIN_EXE_ripsem"))
        .arg("mcp")
        .current_dir(&dir.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    for line in session {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    let output = server.wait_with_output().unwrap();
    assert!(output.status.success());

    let responses: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 3, "the notification gets no answer");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[1]["result"]["tools"][0]["name"], "search");
    let result = &responses[2]["result"];
    assert_eq!(result["isError"], false);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("src/auth.rs:1-1"), "got: {text}");
}

/// Setting what is already set must not send the user to a rebuild that can
/// take minutes.
#[test]
fn setting_the_model_or_backend_it_has_asks_for_no_rebuild() {
    let dir = TempDir::new();
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);

    let same = stdout(&ripsem(&dir.path, &["model", "backend", "hash"]));
    assert!(same.contains("already hash"), "{same}");
    let same = stdout(&ripsem(
        &dir.path,
        &["model", "set", ripsem::model::DEFAULT_MODEL],
    ));
    assert!(same.contains("needs no rebuild"), "{same}");

    let changed = stdout(&ripsem(&dir.path, &["model", "set", "all-MiniLM-L6-v2"]));
    assert!(changed.contains("index --force"), "{changed}");
    let config = std::fs::read_to_string(dir.path.join(".ripsem/config.toml")).unwrap();
    assert!(config.contains("all-MiniLM-L6-v2"), "{config}");
}

#[test]
fn a_path_that_does_not_exist_is_an_error() {
    let dir = TempDir::new();
    for args in [["index"], ["status"], ["init"]] {
        let missing = dir.path.join("tpyo");
        let mut full = vec!["--path", missing.to_str().unwrap()];
        full.extend(args);
        let output = ripsem(&dir.path, &full);
        assert!(!output.status.success(), "{args:?}");
        assert!(!missing.exists(), "{args:?} created the directory");
    }
}

#[test]
fn every_shell_gets_a_completion_script() {
    let dir = TempDir::new();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = ripsem(&dir.path, &["completions", shell]);
        assert!(output.status.success(), "{shell}");
        assert!(stdout(&output).contains("completions"), "{shell}");
    }
}

/// An agent reads the output through a pipe, and a hit shown as three lines
/// sends it to open the file for the rest.
#[cfg(feature = "treesitter")]
#[test]
fn piped_output_shows_the_first_hits_in_full() {
    let dir = TempDir::new();
    let body: String = (0..12)
        .map(|i| format!("    let step_{i} = {i};\n"))
        .collect();
    dir.write(
        "src/lib.rs",
        &format!("fn settle_the_ledger() {{\n{body}    finish();\n}}\n"),
    );
    assert!(ripsem(&dir.path, &["init"]).status.success());
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = stdout(&ripsem(&dir.path, &["search", "settle_the_ledger"]));
    assert!(out.contains("finish();"), "{out}");
    let short = stdout(&ripsem(
        &dir.path,
        &["search", "settle_the_ledger", "--context", "2"],
    ));
    assert!(!short.contains("finish();"), "{short}");
}
