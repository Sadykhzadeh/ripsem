use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

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

fn ripsem_with_stdin(cwd: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ripsem"))
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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

/// Runs `ripsem mcp` in `dir` over stdio with each line as one message, and
/// returns the responses the server wrote, one per line.
fn mcp_over_stdio(dir: &Path, messages: &[&str]) -> Vec<serde_json::Value> {
    use std::io::Write;
    use std::process::Stdio;

    let mut server = Command::new(env!("CARGO_BIN_EXE_ripsem"))
        .arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    for line in messages {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    let output = server.wait_with_output().unwrap();
    assert!(output.status.success());
    stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A repository with one indexed file, for the MCP tests that search it.
fn indexed_repository() -> TempDir {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());
    dir
}

/// Two files that do the same check, indexed with the hashing backend.
fn repository_with_a_copied_check() -> TempDir {
    let dir = TempDir::new();
    dir.write(
        "src/login.rs",
        "fn login(user: &str, pass: &str) -> bool {\n    verify_password(user, pass)\n}\n",
    );
    dir.write(
        "src/signup.rs",
        "fn signup(user: &str, pass: &str) -> bool {\n    verify_password(user, pass)\n}\n",
    );
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());
    dir
}

/// The place nearest to a function's lines is the copy of it elsewhere, and the
/// function's own lines are not reported back.
#[test]
fn find_similar_reports_the_copy_of_a_place_over_stdio() {
    let dir = repository_with_a_copied_check();
    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_similar","arguments":{"path":"src/login.rs","line":1,"end_line":3}}}"#,
        ],
    );
    let tool = &responses[1]["result"]["tools"][1];
    assert_eq!(tool["name"], "find_similar");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);
    assert_eq!(tool["title"], "Find similar code");
    assert!(tool["outputSchema"].is_object());

    let result = &responses[2]["result"];
    assert_eq!(result["isError"], false, "{result}");
    let hits = result["structuredContent"]["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| h["path"] == "src/signup.rs"),
        "{hits:?}"
    );
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("src/signup.rs:")
    );
}

/// Errors come back as tool errors in the CLI's own words, so an agent can
/// correct the call.
#[test]
fn find_similar_errors_are_tool_errors_with_the_cli_wording() {
    let dir = repository_with_a_copied_check();
    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"find_similar","arguments":{"path":"src/missing.rs"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"find_similar","arguments":{"path":"src/login.rs","line":1,"end_line":40}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_similar","arguments":{"path":"src/login.rs","line":3,"end_line":1}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"find_similar","arguments":{"line":1}}}"#,
        ],
    );
    let text = |i: usize| {
        responses[i]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(responses[0]["result"]["isError"], true);
    assert!(
        text(0).contains("src/missing.rs is not in the index"),
        "{}",
        text(0)
    );
    assert!(
        text(1).contains("src/login.rs has 3 lines, so line 40 is out of range"),
        "{}",
        text(1)
    );
    assert!(
        text(2).contains("line 1 comes before line 3"),
        "{}",
        text(2)
    );
    assert!(text(3).contains("missing 'path'"), "{}", text(3));
}

/// The status report comes back as structured content as well, with the
/// fields every report has.
#[test]
fn index_status_returns_the_report_as_structured_content() {
    let dir = indexed_repository();
    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"index_status","arguments":{}}}"#,
        ],
    );
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["schema"], "ripsem/status/v1");
    assert_eq!(result["structuredContent"]["index"]["present"], true);
    assert!(result["structuredContent"]["updating"].is_boolean());
    let text: serde_json::Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
}

/// A client of a revision that has structured output gets the hits as data as
/// well as text, and the tool advertises the shape it returns.
#[test]
fn a_client_of_a_revision_with_structured_output_gets_the_hits_as_data() {
    let dir = indexed_repository();
    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"verify password"}}}"#,
        ],
    );
    let search = &responses[1]["result"]["tools"][0];
    assert_eq!(search["name"], "search");
    assert_eq!(
        search["outputSchema"]["required"],
        json!(["hits", "next_offset"])
    );

    let result = &responses[2]["result"];
    assert_eq!(result["isError"], false);
    let hit = &result["structuredContent"]["hits"][0];
    assert_eq!(hit["path"], "src/auth.rs");
    assert_eq!(hit["start_line"], 1);
    assert_eq!(hit["end_line"], 1);
    assert!(result["structuredContent"]["next_offset"].is_null());
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("src/auth.rs:1-1")
    );
}

/// A client of an older revision is not sent a shape it does not know, so the
/// schema and the structured result are left out for it.
#[test]
fn a_client_of_an_older_revision_gets_no_structured_output() {
    let dir = indexed_repository();
    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"verify password"}}}"#,
        ],
    );
    assert!(
        responses[1]["result"]["tools"][0]
            .get("outputSchema")
            .is_none()
    );
    assert!(responses[2]["result"].get("structuredContent").is_none());
    assert_eq!(responses[2]["result"]["isError"], false);
}

// Lenient on purpose: the spec only asks clients to hold back before initialize, and refusing would break one that does not.
#[test]
fn a_request_before_initialize_is_answered() {
    let dir = TempDir::new();
    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        ],
    );
    assert_eq!(responses[0]["result"]["tools"][0]["name"], "search");
    assert_eq!(responses[1]["result"]["protocolVersion"], "2025-06-18");
}

/// One session over stdio, as a client of the 2025-06-18 revision runs it.
#[test]
fn the_mcp_server_answers_a_session_from_handshake_to_search() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn authenticate(user) { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let responses = mcp_over_stdio(
        &dir.path,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"verify password"}}}"#,
        ],
    );
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

/// The paths a search prints, sorted.
fn found_paths(output: &std::process::Output) -> Vec<String> {
    let mut paths: Vec<String> = stdout(output)
        .lines()
        .filter_map(|line| line.split(':').next())
        .filter(|path| !path.is_empty() && !path.starts_with(' '))
        .map(str::to_string)
        .collect();
    paths.sort();
    paths
}

#[test]
fn a_language_limits_a_search_to_its_files() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn login() { verify password }");
    dir.write("src/auth.py", "def login(): verify password");
    dir.write("Cargo.toml", "verify password");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let search = |lang: &[&str]| {
        let mut args = vec!["search", "verify password", "--context", "0"];
        args.extend_from_slice(lang);
        found_paths(&ripsem(&dir.path, &args))
    };
    assert_eq!(search(&["--lang", "rust"]), ["src/auth.rs"]);
    assert_eq!(search(&["--lang", "PY"]), ["src/auth.py"]);
    assert_eq!(search(&["--lang", "toml"]), ["Cargo.toml"]);
    assert_eq!(
        search(&["--lang", "rs", "--lang", "py"]),
        ["src/auth.py", "src/auth.rs"]
    );
    assert_eq!(
        search(&["--lang", "rust,toml"]),
        ["Cargo.toml", "src/auth.rs"]
    );

    let counts = stdout(&ripsem(
        &dir.path,
        &[
            "search",
            "verify password",
            "--lang",
            "rust",
            "--count-by-file",
        ],
    ));
    assert_eq!(counts.trim(), "1  src/auth.rs");
}

#[test]
fn a_language_naming_nothing_is_an_error() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn login() { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let empty = ripsem(&dir.path, &["search", "verify", "--lang", ""]);
    assert!(!empty.status.success());
    assert!(stderr(&empty).contains("''"), "{}", stderr(&empty));

    let none = ripsem(&dir.path, &["search", "verify", "--lang", "go"]);
    assert!(!none.status.success());
    assert!(
        stderr(&none).contains("no indexed file matches lang go"),
        "{}",
        stderr(&none)
    );
}

#[test]
fn a_glob_limits_a_search_to_matching_paths() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn login() { verify password }");
    dir.write("src/auth_test.rs", "fn login_test() { verify password }");
    dir.write("docs/auth.md", "verify password");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let search = |globs: &[&str]| {
        let mut args = vec!["search", "verify password", "--context", "0"];
        for glob in globs {
            args.extend(["-g", glob]);
        }
        found_paths(&ripsem(&dir.path, &args))
    };
    assert_eq!(search(&["*.md"]), ["docs/auth.md"]);
    assert_eq!(search(&["src/**"]), ["src/auth.rs", "src/auth_test.rs"]);
    assert_eq!(search(&["*.rs", "!*_test.rs"]), ["src/auth.rs"],);
    assert_eq!(search(&["!src/"]), ["docs/auth.md"]);

    let both = ripsem(
        &dir.path,
        &[
            "search",
            "verify password",
            "--context",
            "0",
            "--lang",
            "rust",
            "--glob",
            "!*_test.rs",
        ],
    );
    assert_eq!(found_paths(&both), ["src/auth.rs"]);

    let json = stdout(&ripsem(
        &dir.path,
        &["search", "verify password", "-g", "*.md", "--json"],
    ));
    assert!(
        json.contains("\"path\": \"docs/auth.md\"") && !json.contains("src/"),
        "{json}"
    );
}

#[test]
fn a_bad_glob_is_an_error() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn login() { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let bad = ripsem(&dir.path, &["search", "verify", "-g", "src/[a"]);
    assert!(!bad.status.success());
    assert!(
        stderr(&bad).contains("invalid glob 'src/[a'"),
        "{}",
        stderr(&bad)
    );

    let none = ripsem(&dir.path, &["search", "verify", "--glob", "*.go"]);
    assert!(!none.status.success());
    assert!(
        stderr(&none).contains("no indexed file matches glob *.go"),
        "{}",
        stderr(&none)
    );
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

/// Ten files that fit the query, five of them in `.rs` files.
fn paged_tree() -> TempDir {
    let dir = TempDir::new();
    for i in 0..5 {
        dir.write(
            &format!("src/auth{i}.rs"),
            &format!("fn auth{i}() {{ verify password create session token }}"),
        );
        dir.write(
            &format!("docs/note{i}.md"),
            &format!("note {i}: verify the setup first"),
        );
    }
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);
    dir
}

/// The `path:start-end` heads of what a search printed.
fn heads(output: &Output) -> Vec<String> {
    stdout(output)
        .lines()
        .filter(|l| !l.starts_with(' '))
        .filter_map(|l| l.split("  ").next())
        .filter(|h| h.contains(".rs:") || h.contains(".md:"))
        .map(str::to_string)
        .collect()
}

#[test]
fn an_offset_reads_the_next_page_and_says_when_more_follow() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let page = |limit: &str, offset: &str, extra: &[&str]| {
        let mut args = vec!["search", query, "-k", limit, "--offset", offset];
        args.extend_from_slice(extra);
        ripsem(&dir.path, &args)
    };
    let all = page("50", "0", &[]);
    assert_eq!(heads(&all).len(), 10, "{}", stdout(&all));
    assert!(!stdout(&all).contains("--offset"), "{}", stdout(&all));

    let mut joined = Vec::new();
    for offset in ["0", "4", "8"] {
        let out = page("4", offset, &[]);
        let text = stdout(&out);
        let next = offset.parse::<usize>().unwrap() + 4;
        let line = format!("more results below this; pass --offset {next} to see them");
        assert_eq!(text.contains(&line), next < 10, "{text}");
        if next < 10 {
            assert_eq!(
                text.trim_end().lines().last(),
                Some(line.as_str()),
                "{text}"
            );
        }
        joined.extend(heads(&out));
    }
    assert_eq!(joined, heads(&all));

    let past = page("4", "10", &[]);
    assert!(past.status.success(), "{}", stderr(&past));
    assert_eq!(stdout(&past).trim(), "no further matches");
}

#[test]
fn a_count_past_the_last_page_says_no_further_matches() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let past = ripsem(
        &dir.path,
        &["search", query, "--count-by-file", "--offset", "40"],
    );
    assert!(past.status.success(), "{}", stderr(&past));
    assert_eq!(stdout(&past).trim(), "no further matches");
}

#[test]
fn files_with_matches_lists_each_file_once_in_ranking_order() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let out = ripsem(&dir.path, &["search", query, "-k", "50", "-l"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let listed: Vec<&str> = text.lines().collect();
    assert_eq!(listed.len(), 10, "{text}");
    let mut unique = listed.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 10, "{text}");

    let json = ripsem(&dir.path, &["search", query, "-k", "50", "-l", "--json"]);
    let paths: Vec<String> = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(paths, listed);

    let none = ripsem(
        &dir.path,
        &["search", "quantum entanglement", "-l", "--min-score", "0.5"],
    );
    assert_eq!(stdout(&none).trim(), "no matches");

    let clash = ripsem(&dir.path, &["search", query, "-l", "--count-by-file"]);
    assert_eq!(clash.status.code(), Some(2), "{}", stderr(&clash));
}

#[test]
fn vimgrep_prints_one_path_line_column_text_line_per_hit() {
    let dir = TempDir::new();
    dir.write(
        "src/my file.rs",
        "\n    fn login(user: &str) {\n\tverify password\n    }\n",
    );
    dir.write("src/other.rs", "fn logout(user: &str) { end session }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = ripsem(&dir.path, &["search", "verify password", "--vimgrep"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    let hits = ripsem(&dir.path, &["search", "verify password", "--json"]);
    let hits: Vec<serde_json::Value> = serde_json::from_str(&stdout(&hits)).unwrap();
    assert_eq!(lines.len(), hits.len(), "{text}");
    for line in &lines {
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        assert_eq!(parts.len(), 4, "{line}");
        assert!(parts[1].parse::<usize>().is_ok(), "{line}");
        assert_eq!(parts[2], "1", "{line}");
        assert!(
            !parts[3].starts_with(' ') && !parts[3].ends_with(' '),
            "{line}"
        );
    }
    assert!(
        lines.contains(&"src/my file.rs:2:1:fn login(user: &str) {"),
        "{text}"
    );
    assert!(!text.contains('\x1b'), "{text}");

    for clash in [
        vec!["search", "verify password", "--vimgrep", "--json"],
        vec!["search", "verify password", "--vimgrep", "-l"],
        vec!["search", "verify password", "--vimgrep", "-q"],
        vec!["search", "verify password", "--vimgrep", "--count-by-file"],
    ] {
        let out = ripsem(&dir.path, &clash);
        assert_eq!(out.status.code(), Some(2), "{clash:?}");
    }
}

#[test]
fn null_ends_each_listed_path_with_a_nul_byte() {
    let dir = TempDir::new();
    dir.write("src/my file.rs", "fn login(user: &str) { verify password }");
    dir.write("src/other.rs", "fn logout(user: &str) { end session }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = ripsem(&dir.path, &["search", "verify password", "-l", "-0"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let plain = ripsem(&dir.path, &["search", "verify password", "-l"]);
    let mut lines: Vec<String> = stdout(&plain).lines().map(str::to_string).collect();
    let mut paths: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8(p.to_vec()).unwrap())
        .collect();
    lines.sort();
    paths.sort();
    assert!(paths.contains(&"src/my file.rs".to_string()), "{paths:?}");
    assert_eq!(paths, lines);
    assert!(out.stdout.ends_with(&[0]));

    let none = ripsem(
        &dir.path,
        &[
            "search",
            "quantum entanglement",
            "-l",
            "-0",
            "--min-score",
            "0.5",
        ],
    );
    assert!(none.status.success());
    assert!(none.stdout.is_empty(), "{}", stdout(&none));

    let bare = ripsem(&dir.path, &["search", "verify password", "-0"]);
    assert_eq!(bare.status.code(), Some(2), "{}", stderr(&bare));
    let json = ripsem(
        &dir.path,
        &["search", "verify password", "-l", "-0", "--json"],
    );
    assert_eq!(json.status.code(), Some(2), "{}", stderr(&json));
}

#[test]
fn similar_finds_the_places_nearest_to_a_location_and_leaves_its_own_out() {
    let dir = paged_tree();
    let found = ripsem(&dir.path, &["similar", "src/auth0.rs"]);
    assert!(found.status.success(), "{}", stderr(&found));
    let shown = heads(&found);
    assert!(!shown.is_empty(), "{}", stdout(&found));
    assert!(
        shown.iter().all(|h| !h.starts_with("src/auth0.rs:")),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|h| h.starts_with("src/auth1.rs:")),
        "{shown:?}"
    );

    let by_line = ripsem(&dir.path, &["similar", "src/auth0.rs:1-1", "--json"]);
    assert!(by_line.status.success(), "{}", stderr(&by_line));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout(&by_line)).unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r["path"] != "src/auth0.rs"));
}

#[test]
fn similar_names_its_errors() {
    let dir = paged_tree();
    for (target, expected) in [
        ("src/nothere.rs", "is not in the index"),
        ("src/auth0.rs:9", "has 1 lines, so line 9 is out of range"),
        ("src/auth0.rs:3-2", "comes before line 3"),
        ("src/auth0.rs:0", "line numbers start at 1"),
    ] {
        let out = ripsem(&dir.path, &["similar", target]);
        assert_eq!(out.status.code(), Some(1), "{target}: {}", stdout(&out));
        assert!(
            stderr(&out).contains(expected),
            "{target}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn similar_takes_the_filters_and_output_of_search() {
    let dir = paged_tree();
    let docs = ripsem(
        &dir.path,
        &["similar", "src/auth0.rs", "--lang", "md", "-l"],
    );
    assert!(docs.status.success(), "{}", stderr(&docs));
    let listed = stdout(&docs);
    assert!(
        !listed.is_empty() && listed.lines().all(|l| l.ends_with(".md")),
        "{listed}"
    );

    let none = ripsem(
        &dir.path,
        &["similar", "src/auth0.rs", "--min-score", "1", "-q"],
    );
    assert_eq!(none.status.code(), Some(1), "{}", stderr(&none));
}

#[test]
fn similar_refreshes_the_index_before_it_reads_a_changed_file() {
    let dir = paged_tree();
    dir.write("src/auth0.rs", "fn auth0() {}\nfn extra() {}\n");
    let out = ripsem(&dir.path, &["similar", "src/auth0.rs:2"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn since_limits_a_search_to_the_files_changed_since_a_revision() {
    let dir = paged_tree();
    git(&dir.path, &["init", "-q"]);
    git(&dir.path, &["add", "-A"]);
    git(
        &dir.path,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
        ],
    );
    dir.write(
        "src/auth0.rs",
        "fn auth0() { verify password create session token }\nfn more() {}\n",
    );
    dir.write("docs/fresh.md", "verify password, written after the commit");

    let query = "verify password create session token";
    let out = ripsem(
        &dir.path,
        &["search", query, "--since", "HEAD", "--json", "-k", "50"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout(&out)).unwrap();
    let paths: Vec<&str> = rows.iter().map(|r| r["path"].as_str().unwrap()).collect();
    assert!(!paths.is_empty(), "{}", stdout(&out));
    assert!(
        paths
            .iter()
            .all(|p| *p == "src/auth0.rs" || *p == "docs/fresh.md"),
        "{paths:?}"
    );
    assert!(paths.contains(&"src/auth0.rs"), "{paths:?}");

    let unknown = ripsem(&dir.path, &["search", query, "--since", "no-such-rev"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr(&unknown).contains("unknown revision 'no-such-rev'"),
        "{}",
        stderr(&unknown)
    );

    let flag = ripsem(&dir.path, &["search", query, "--since=--output=x"]);
    assert_eq!(flag.status.code(), Some(1));
    assert!(
        stderr(&flag).contains("is not a revision"),
        "{}",
        stderr(&flag)
    );

    let plain = paged_tree();
    let outside = ripsem(&plain.path, &["search", query, "--since", "HEAD"]);
    assert_eq!(outside.status.code(), Some(1));
    assert!(
        stderr(&outside).contains("not in a git repository"),
        "{}",
        stderr(&outside)
    );
}

#[test]
fn sort_by_path_orders_the_page_by_path_then_line() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let rows = |args: &[&str]| -> Vec<(String, u64)> {
        let mut all = vec!["search", query, "-k", "50", "--json"];
        all.extend_from_slice(args);
        let found: Vec<serde_json::Value> =
            serde_json::from_str(&stdout(&ripsem(&dir.path, &all))).unwrap();
        found
            .iter()
            .map(|r| {
                (
                    r["path"].as_str().unwrap().to_string(),
                    r["start_line"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    let by_path = rows(&["--sort", "path"]);
    assert_eq!(by_path.len(), 10);
    let mut sorted = by_path.clone();
    sorted.sort();
    assert_eq!(by_path, sorted);
    let mut by_score = rows(&[]);
    by_score.sort();
    assert_eq!(by_path, by_score);

    let bad = ripsem(&dir.path, &["search", query, "--sort", "size"]);
    assert_eq!(bad.status.code(), Some(2), "{}", stderr(&bad));
}

#[test]
fn a_query_piped_on_stdin_is_searched_like_one_typed() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let typed = ripsem(&dir.path, &["search", query, "--json"]);
    let dash = ripsem_with_stdin(&dir.path, &["search", "-", "--json"], &format!("{query}\n"));
    assert!(dash.status.success(), "{}", stderr(&dash));
    assert_eq!(stdout(&dash), stdout(&typed));
    let bare = ripsem_with_stdin(&dir.path, &["search", "--json"], &format!("{query}\r\n"));
    assert_eq!(stdout(&bare), stdout(&typed));

    let empty = ripsem_with_stdin(&dir.path, &["search", "-"], "\n");
    assert_eq!(empty.status.code(), Some(1));
    assert!(
        stderr(&empty).contains("the query is empty"),
        "{}",
        stderr(&empty)
    );
}

#[test]
fn quiet_search_prints_nothing_and_exits_by_whether_a_hit_is_left() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let found = ripsem(&dir.path, &["search", query, "-q"]);
    assert!(found.status.success(), "{}", stderr(&found));
    assert!(stdout(&found).is_empty(), "{}", stdout(&found));

    let none = ripsem(
        &dir.path,
        &["search", "quantum entanglement", "--min-score", "0.5", "-q"],
    );
    assert_eq!(none.status.code(), Some(1), "{}", stderr(&none));
    assert!(stdout(&none).is_empty(), "{}", stdout(&none));
}

#[test]
fn the_json_of_a_page_is_the_same_array_of_hits() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let rows = |args: &[&str]| -> Vec<serde_json::Value> {
        let mut all = vec!["search", query, "--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&stdout(&ripsem(&dir.path, &all))).unwrap()
    };

    let first = rows(&["-k", "4"]);
    assert_eq!(first.len(), 4);
    // A page is the same array of hits as before, with the same fields and no others.
    let fields = [
        "end_line",
        "kind",
        "path",
        "score",
        "snippet",
        "start_line",
        "symbol",
    ];
    for row in &first {
        let mut names: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        assert_eq!(names, fields);
    }
    // A page shorter than the limit is the last one.
    let last = rows(&["-k", "4", "--offset", "8"]);
    assert_eq!(last.len(), 2);
    assert!(rows(&["--offset", "40"]).is_empty());
}

#[test]
fn an_offset_follows_a_language_a_glob_a_path_and_a_threshold() {
    let dir = paged_tree();
    let query = "verify password create session token";
    for filter in [
        &["--lang", "rust"][..],
        &["-g", "*.rs"],
        &["--in", "src"],
        &["--min-score", "1"],
    ] {
        let ask = |limit: &str, offset: &str| {
            let mut args = vec!["search", query, "-k", limit, "--offset", offset];
            args.extend_from_slice(filter);
            ripsem(&dir.path, &args)
        };
        let all = heads(&ask("50", "0"));
        assert_eq!(all.len(), 5, "{filter:?}");
        let (first, second) = (ask("3", "0"), ask("3", "3"));
        assert!(
            stdout(&first).contains("pass --offset 3 to see them"),
            "{filter:?}: {}",
            stdout(&first)
        );
        assert!(!stdout(&second).contains("--offset"), "{filter:?}");
        let joined: Vec<String> = heads(&first).into_iter().chain(heads(&second)).collect();
        assert_eq!(joined, all, "{filter:?}");
    }
}

#[test]
fn an_offset_that_is_not_a_count_is_an_error() {
    let dir = TempDir::new();
    for offset in ["-1", "two", "1.5"] {
        let out = ripsem(&dir.path, &["search", "x", "--offset", offset]);
        assert_eq!(out.status.code(), Some(2), "{offset}: {}", stderr(&out));
    }
}

#[test]
fn a_regex_keeps_the_hits_whose_code_matches() {
    let dir = paged_tree();
    let query = "verify password create session token";
    let all = heads(&ripsem(&dir.path, &["search", query, "-k", "50"]));
    let wanted: Vec<String> = all
        .iter()
        .filter(|h| h.contains("auth2.rs") || h.contains("auth4.rs"))
        .cloned()
        .collect();
    assert_eq!(wanted.len(), 2);

    for flag in ["-e", "--regex"] {
        let out = ripsem(&dir.path, &["search", query, flag, r"auth[24]\("]);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(heads(&out), wanted, "{flag}");
    }
    // Inline flags, other filters and paging all go with it.
    let folded = ripsem(
        &dir.path,
        &["search", query, "-e", "(?i)AUTH", "--lang", "rust"],
    );
    assert_eq!(heads(&folded).len(), 5, "{}", stdout(&folded));
    let page = ripsem(
        &dir.path,
        &["search", query, "-e", "(?i)auth|note", "-k", "6"],
    );
    assert!(stdout(&page).contains("pass --offset 6 to see them"));
    let rest = ripsem(
        &dir.path,
        &[
            "search",
            query,
            "-e",
            "(?i)auth|note",
            "-k",
            "6",
            "--offset",
            "6",
        ],
    );
    assert_eq!(heads(&rest).len(), 4);
    assert!(!stdout(&rest).contains("--offset"));
    let none = ripsem(&dir.path, &["search", query, "-e", "nothing like this"]);
    assert!(none.status.success());
    assert_eq!(stdout(&none).trim(), "no matches");
}

#[test]
fn a_regex_that_does_not_parse_is_an_error_before_any_search() {
    // No project here: the pattern is refused before an index is looked for.
    let dir = TempDir::new();
    for pattern in ["foo(", "[a-", "((a{100}){100}){100}"] {
        let out = ripsem(&dir.path, &["search", "x", "-e", pattern]);
        assert_eq!(out.status.code(), Some(1), "{pattern}");
        let message = stderr(&out);
        assert!(message.contains("invalid regex"), "{pattern}: {message}");
        assert_eq!(message.trim().lines().count(), 1, "{pattern}: {message}");
    }
}

fn ripsem_hashing(cwd: &Path, args: &[&str], value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ripsem"))
        .args(args)
        .current_dir(cwd)
        .env("RIPSEM_HASH_ALL", value)
        .output()
        .unwrap()
}

/// A file rewritten with the size and the time it had is not seen by a run
/// that trusts the stat the index stored. `RIPSEM_HASH_ALL=1` and `index
/// --force` read every file, for `status` as for `index`.
#[test]
fn every_file_is_hashed_when_asked_and_not_otherwise() {
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    let set_modified = |dir: &TempDir, name: &str| {
        std::fs::File::options()
            .write(true)
            .open(dir.path.join(name))
            .unwrap()
            .set_modified(aged)
            .unwrap();
    };
    let modified = |output: &Output| -> u64 {
        let report: serde_json::Value = serde_json::from_str(&stdout(output)).unwrap();
        report["index"]["drift"]["modified"].as_u64().unwrap()
    };

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    dir.write("b.rs", "delta echo foxtrot");
    set_modified(&dir, "a.rs");
    set_modified(&dir, "b.rs");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    let first = stdout(&ripsem(&dir.path, &["index"]));
    assert!(
        first.starts_with("2 files: 2 changed, 0 unchanged"),
        "{first}"
    );

    dir.write("a.rs", "omega bravo charlie");
    set_modified(&dir, "a.rs");
    assert_eq!(modified(&ripsem(&dir.path, &["status", "--json"])), 0);
    for value in ["0", "true", ""] {
        let status = ripsem_hashing(&dir.path, &["status", "--json"], value);
        assert_eq!(modified(&status), 0, "RIPSEM_HASH_ALL={value}");
    }
    assert_eq!(
        modified(&ripsem_hashing(&dir.path, &["status", "--json"], "1")),
        1
    );
    let plain = stdout(&ripsem(&dir.path, &["index"]));
    assert!(
        plain.starts_with("2 files: 0 changed, 2 unchanged"),
        "{plain}"
    );

    let hashed = stdout(&ripsem_hashing(&dir.path, &["index"], "1"));
    assert!(
        hashed.starts_with("2 files: 1 changed, 1 unchanged"),
        "{hashed}"
    );
    assert_eq!(modified(&ripsem(&dir.path, &["status", "--json"])), 0);

    dir.write("b.rs", "sigma echo foxtrot");
    set_modified(&dir, "b.rs");
    let forced = stdout(&ripsem(&dir.path, &["index", "--force"]));
    assert!(
        forced.starts_with("2 files: 2 changed, 0 unchanged"),
        "{forced}"
    );
    let found = ripsem(&dir.path, &["search", "sigma echo", "--json", "-k", "1"]);
    assert!(stdout(&found).contains("b.rs"), "{}", stdout(&found));
}

#[test]
fn a_truncated_index_is_reported_and_rebuilt_without_a_panic() {
    let dir = TempDir::new();
    dir.write("src/lib.rs", "fn parse_config() {}\n");
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let index = dir.path.join(".ripsem/index.bin");
    let bytes = std::fs::read(&index).unwrap();
    std::fs::write(&index, &bytes[..bytes.len() / 2]).unwrap();

    let status = ripsem(&dir.path, &["status"]);
    assert!(
        stdout(&status).contains("unreadable"),
        "{}",
        stdout(&status)
    );
    assert!(!stderr(&status).contains("panicked"), "{}", stderr(&status));

    let rebuilt = ripsem(&dir.path, &["index"]);
    assert!(rebuilt.status.success(), "{}", stderr(&rebuilt));
    assert!(!stdout(&ripsem(&dir.path, &["status"])).contains("unreadable"));
}

#[test]
fn a_config_naming_an_unknown_model_stops_index_and_names_the_file() {
    let dir = TempDir::new();
    dir.write("src/lib.rs", "fn main() {}\n");
    dir.write(".ripsem/config.toml", "model = \"no-such-model\"\n");

    let out = ripsem(&dir.path, &["index"]);
    assert!(!out.status.success());
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("config.toml"), "{err}");
    assert!(err.contains("no-such-model"), "{err}");
}

#[cfg(not(feature = "fastembed"))]
#[test]
fn a_fastembed_backend_in_a_build_without_it_names_the_config_file() {
    let dir = TempDir::new();
    dir.write("src/lib.rs", "fn main() {}\n");
    dir.write(".ripsem/config.toml", "backend = \"fastembed\"\n");

    let out = ripsem(&dir.path, &["index"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("config.toml"), "{err}");
    assert!(err.contains("backend 'fastembed'"), "{err}");
}

#[test]
fn a_dry_run_lists_one_path_per_line() {
    let dir = TempDir::new();
    dir.write("src/lib.rs", "fn main() {}\n");
    dir.write("docs/notes.md", "# notes\n");
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );

    let out = ripsem(&dir.path, &["index", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    let text = stdout(&out);
    let mut listed: Vec<&str> = text
        .lines()
        .filter(|line| !line.ends_with("would be indexed"))
        .collect();
    listed.sort();
    assert_eq!(listed, ["docs/notes.md", "src/lib.rs"]);
}

#[test]
fn a_dry_run_says_why_each_file_it_reaches_was_left_out() {
    let dir = TempDir::new();
    dir.write("keep.rs", "fn main() {}\n");
    dir.write("bundle.js", &"var a=1;".repeat(1200));
    dir.write("big.rs", &"fn a() {}\n".repeat(2000));
    std::fs::write(dir.path.join("blob.dat"), b"\0\x01\x02 binary\n").unwrap();
    dir.write("generated.rs", "fn g() {}\n");
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );
    let config = dir.path.join(".ripsem/config.toml");
    let existing = std::fs::read_to_string(&config).unwrap();
    assert!(existing.contains("max_file_kb = 1024"), "{existing}");
    assert!(existing.contains("exclude = []"), "{existing}");
    let edited = existing
        .replace("max_file_kb = 1024", "max_file_kb = 16")
        .replace("exclude = []", "exclude = [\"generated.rs\"]");
    std::fs::write(&config, edited).unwrap();

    let out = ripsem(&dir.path, &["index", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("1 file would be indexed"), "{text}");
    assert!(
        text.contains(
            "5 files seen, left out: 1 binary, 1 minified, 1 over max_file_kb, \
             1 excluded by pattern"
        ),
        "{text}"
    );
}

#[test]
fn status_says_when_the_index_was_built_in_both_outputs() {
    let dir = TempDir::new();
    dir.write("src/lib.rs", "fn main() {}\n");
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let human = stdout(&ripsem(&dir.path, &["status"]));
    assert!(human.contains("built:  "), "{human}");

    let json = stdout(&ripsem(&dir.path, &["status", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        v["index"]["written_unix"]
            .as_u64()
            .is_some_and(|s| s > 1_600_000_000),
        "{json}"
    );
}
