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
    let same = stdout(&ripsem(&dir.path, &["model", "set", "bge-small-en-v1.5"]));
    assert!(same.contains("needs no rebuild"), "{same}");

    let changed = stdout(&ripsem(&dir.path, &["model", "set", "all-MiniLM-L6-v2"]));
    assert!(changed.contains("index --force"), "{changed}");
    let config = std::fs::read_to_string(dir.path.join(".ripsem/config.toml")).unwrap();
    assert!(config.contains("all-MiniLM-L6-v2"), "{config}");
}

#[test]
fn a_file_of_only_whitespace_is_not_counted_as_indexed() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    dir.write("blank.txt", "   \n\n\t\n");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);

    let listed = stdout(&ripsem(&dir.path, &["index", "--dry-run"]));
    assert!(!listed.contains("blank.txt"), "{listed}");
    assert!(listed.contains("1 file would be indexed"), "{listed}");

    let indexed = stdout(&ripsem(&dir.path, &["index"]));
    assert!(
        indexed.starts_with("1 file: 1 changed, 0 unchanged"),
        "{indexed}"
    );
    let again = stdout(&ripsem(&dir.path, &["index"]));
    assert!(
        again.starts_with("1 file: 0 changed, 1 unchanged"),
        "{again}"
    );
    let status = stdout(&ripsem(&dir.path, &["status"]));
    assert!(status.contains("across 1 file\n"), "{status}");
}

#[test]
fn a_scope_is_read_from_the_current_directory() {
    let dir = TempDir::new();
    dir.write("src/auth/login.rs", "fn login() { verify password }");
    dir.write("src/admin.rs", "fn admin() { verify password }");
    dir.write("docs/auth.md", "verify password");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let found = |cwd: &Path, scope: &str| {
        let output = ripsem(
            cwd,
            &["search", "verify password", "--context", "0", "--in", scope],
        );
        let mut paths: Vec<String> = stdout(&output)
            .lines()
            .filter_map(|line| line.split(':').next())
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect();
        paths.sort();
        (paths, stderr(&output))
    };
    let src = dir.path.join("src");
    assert_eq!(found(&src, "auth").0, ["src/auth/login.rs"]);
    assert_eq!(found(&src, ".").0, ["src/admin.rs", "src/auth/login.rs"]);
    assert_eq!(found(&src, "../docs").0, ["docs/auth.md"]);
    assert_eq!(found(&dir.path, "src/auth").0, ["src/auth/login.rs"]);
    let (paths, error) = found(&src, "../..");
    assert!(paths.is_empty() && error.contains("outside"), "{error}");
}

#[test]
fn the_words_of_a_query_need_no_quotes() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let quoted = ripsem(&dir.path, &["search", "verify the password", "--json"]);
    let words = ripsem(
        &dir.path,
        &["search", "verify", "the", "password", "--json"],
    );
    assert!(words.status.success(), "{}", stderr(&words));
    assert!(stdout(&words).contains("src/auth.rs"));
    assert_eq!(stdout(&words), stdout(&quoted));

    let after = ripsem(
        &dir.path,
        &["search", "-k", "1", "verify", "password", "--context", "0"],
    );
    assert_eq!(stdout(&after).lines().filter(|l| !l.is_empty()).count(), 1);
    assert!(!ripsem(&dir.path, &["search"]).status.success());
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

#[test]
fn a_reader_that_goes_away_is_not_a_panic() {
    let dir = TempDir::new();
    for n in 0..400 {
        dir.write(
            &format!("src/module_with_a_long_name_{n:04}.rs"),
            "fn f() {}",
        );
    }
    for args in [&["index", "--dry-run"][..], &["completions", "powershell"]] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ripsem"))
            .args(args)
            .current_dir(&dir.path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
    }
}

#[test]
fn a_limit_or_score_that_cannot_match_is_an_error() {
    let dir = TempDir::new();
    for args in [
        &["search", "x", "-k", "0"][..],
        &["search", "x", "--min-score", "2"],
        &["search", "x", "--min-score", "-0.5"],
        &["search", "x", "--min-score", "NaN"],
    ] {
        let out = ripsem(&dir.path, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
}

#[test]
fn a_path_that_is_a_file_is_not_a_directory() {
    let dir = TempDir::new();
    dir.write("main.rs", "fn main() {}");
    let file = dir.path.join("main.rs");
    let out = ripsem(&dir.path, &["--path", file.to_str().unwrap(), "status"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not a directory"), "{}", stderr(&out));
}

#[cfg(feature = "fastembed")]
#[test]
fn a_model_that_cannot_be_fetched_says_what_to_do() {
    let dir = TempDir::new();
    dir.write("a.rs", "fn a() {}");
    let models = dir.path.join("models");
    let out = Command::new(env!("CARGO_BIN_EXE_ripsem"))
        .args(["index"])
        .current_dir(&dir.path)
        .env("RIPSEM_MODEL_DIR", &models)
        .env("HF_ENDPOINT", "http://127.0.0.1:9")
        .output()
        .unwrap();
    let message = stderr(&out);
    assert!(!out.status.success(), "{message}");
    assert!(models.is_dir(), "the override was not used: {message}");
    for expected in [
        "bge-small-en-v1.5",
        models.to_str().unwrap(),
        "ripsem model backend hash",
    ] {
        assert!(
            message.contains(expected),
            "{expected:?} missing from {message}"
        );
    }
}
