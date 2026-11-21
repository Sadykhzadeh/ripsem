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

/// The command for `ripsem` without the `RIPSEM_` variables of the shell that
/// runs the tests, so a setting exported there cannot change what they check.
/// A test that needs one sets it after this.
fn ripsem_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ripsem"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("RIPSEM_") {
            command.env_remove(key);
        }
    }
    command
}

fn ripsem(cwd: &Path, args: &[&str]) -> Output {
    ripsem_command()
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn ripsem_with_stdin(cwd: &Path, args: &[&str], input: &str) -> Output {
    let mut child = ripsem_command()
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

#[cfg(feature = "treesitter")]
#[test]
fn outline_lists_the_declarations_of_a_file_in_order() {
    let dir = TempDir::new();
    dir.write(
        "src/auth.rs",
        "fn verify(p: &str) -> bool {\n    !p.is_empty()\n}\n\nstruct Session {\n    id: u32,\n}\n\nimpl Session {\n    fn refresh(&mut self) {\n        self.id += 1;\n    }\n}\n",
    );
    assert!(ripsem(&dir.path, &["init"]).status.success());
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = ripsem(&dir.path, &["outline", "src/auth.rs"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "1-3  fn verify\n5-7  struct Session\n9-13  impl Session\n10-12  fn Session::refresh\n"
    );

    let below = dir.path.join("src");
    let from_below = ripsem(&below, &["outline", "auth.rs"]);
    assert_eq!(stdout(&from_below), stdout(&out));

    let dir_out = ripsem(&dir.path, &["outline", "src"]);
    assert!(!dir_out.status.success());
    assert!(
        stderr(&dir_out).contains("takes a file"),
        "{}",
        stderr(&dir_out)
    );

    let missing = ripsem(&dir.path, &["outline", "src/nope.rs"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("not in the index"),
        "{}",
        stderr(&missing)
    );

    let ranged = ripsem(&dir.path, &["outline", "src/auth.rs:9-13"]);
    assert_eq!(ranged.status.code(), Some(1));
    assert!(
        stderr(&ranged).contains("run `ripsem outline src/auth.rs`"),
        "{}",
        stderr(&ranged)
    );

    let blank = ripsem(&dir.path, &["outline", " "]);
    assert_eq!(blank.status.code(), Some(1));
    assert!(
        stderr(&blank).contains("the file name is empty"),
        "{}",
        stderr(&blank)
    );
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

    let mut server = ripsem_command()
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

/// A `ripsem mcp` server that stays up between requests, so a file can change
/// while its session is open.
struct McpServer {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl McpServer {
    fn start(dir: &Path) -> Self {
        use std::process::Stdio;

        let mut child = ripsem_command()
            .arg("mcp")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Sends one message and returns the response line the server writes for it.
    fn request(&mut self, message: &str) -> serde_json::Value {
        use std::io::BufRead;

        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of a tool result, as the agent reads it.
fn tool_text(response: &serde_json::Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
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

    assert!(ripsem(&dir.path, &["index"]).status.success());
    let changed = stdout(&ripsem(&dir.path, &["model", "set", "all-MiniLM-L6-v2"]));
    assert!(changed.contains("index --force"), "{changed}");
    let config = std::fs::read_to_string(dir.path.join(".ripsem/config.toml")).unwrap();
    assert!(config.contains("all-MiniLM-L6-v2"), "{config}");

    let again = stdout(&ripsem(&dir.path, &["model", "set", "all-MiniLM-L6-v2"]));
    assert!(!again.contains("needs no rebuild"), "{again}");
    assert!(again.contains("index --force"), "{again}");
}

/// The dimensions column must start at the same place on every row, however
/// long the longest model id is.
#[test]
fn model_list_lines_up_under_the_longest_id() {
    let dir = TempDir::new();
    let listed = stdout(&ripsem(&dir.path, &["model", "list"]));
    let columns: Vec<usize> = listed
        .lines()
        .map(|line| line.find("d  ~").expect(line))
        .collect();
    assert_eq!(columns.len(), 6, "{listed}");
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "{listed}"
    );
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

/// A file named with a control character is listed as an escape, in the file
/// lists and in the changed files `status --verify` names, as search prints it.
#[cfg(unix)]
#[test]
fn a_file_name_with_a_control_character_is_listed_as_an_escape() {
    let dir = TempDir::new();
    dir.write("keep.rs", "fn main() {}\n");
    dir.write("evil\u{1b}[31m.rs", "fn evil() {}\n");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);

    let listed = stdout(&ripsem(&dir.path, &["index", "--dry-run"]));
    assert!(
        !listed.contains('\u{1b}'),
        "the escape must not reach the terminal raw: {listed:?}"
    );
    assert!(
        listed.contains("\\u{1b}"),
        "the name is shown as an escape: {listed:?}"
    );

    assert!(ripsem(&dir.path, &["index"]).status.success());
    std::fs::write(
        dir.path.join("evil\u{1b}[31m.rs"),
        "fn evil() { changed }\n",
    )
    .unwrap();
    let verified = stdout(&ripsem(&dir.path, &["status", "--verify"]));
    assert!(
        !verified.contains('\u{1b}'),
        "the escape must not reach the terminal raw: {verified:?}"
    );
    assert!(
        verified.contains("\\u{1b}"),
        "the changed file is shown as an escape: {verified:?}"
    );
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
fn a_language_with_a_bang_is_left_out_of_a_search() {
    let dir = TempDir::new();
    dir.write(
        "src/auth.rs",
        "fn verify_password(p: &str) -> bool {\n    !p.is_empty()\n}\n",
    );
    dir.write(
        "src/auth.py",
        "def verify_password(p):\n    return bool(p)\n",
    );
    assert!(ripsem(&dir.path, &["init"]).status.success());
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = ripsem(
        &dir.path,
        &[
            "search",
            "verify password",
            "--lang",
            "!rust",
            "-k",
            "5",
            "--context",
            "0",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(found_paths(&out), ["src/auth.py"]);
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
fn a_language_name_covers_every_extension_it_is_written_with() {
    let dir = TempDir::new();
    dir.write("docs/a.md", "# Auth\n\nverify password\n");
    dir.write("docs/b.markdown", "# Auth\n\nverify password\n");
    dir.write("config/c.yml", "verify: password\n");
    dir.write("config/d.yaml", "verify: password\n");
    dir.write("src/auth.rs", "fn login() { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let search = |lang: &str| {
        found_paths(&ripsem(
            &dir.path,
            &[
                "search",
                "verify password",
                "--lang",
                lang,
                "--context",
                "0",
            ],
        ))
    };
    assert_eq!(search("md"), ["docs/a.md", "docs/b.markdown"]);
    assert_eq!(search("markdown"), ["docs/a.md", "docs/b.markdown"]);
    assert_eq!(search(".md"), ["docs/a.md"]);
    assert_eq!(search("yml"), ["config/c.yml", "config/d.yaml"]);
    assert_eq!(search("yaml"), ["config/c.yml", "config/d.yaml"]);
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
fn exit_codes_are_0_for_a_run_1_for_a_failure_and_2_for_bad_usage() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn login() { verify password }");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let hits = ripsem(&dir.path, &["search", "verify password"]);
    assert_eq!(hits.status.code(), Some(0), "{}", stderr(&hits));

    let no_hit = ripsem(
        &dir.path,
        &["search", "verify password", "-e", "nomatchxyz"],
    );
    assert_eq!(no_hit.status.code(), Some(0), "{}", stderr(&no_hit));
    let quiet_no_hit = ripsem(
        &dir.path,
        &["search", "verify password", "-e", "nomatchxyz", "-q"],
    );
    assert_eq!(quiet_no_hit.status.code(), Some(1));

    let bad_regex = ripsem(&dir.path, &["search", "verify", "-e", "("]);
    assert_eq!(bad_regex.status.code(), Some(1), "{}", stderr(&bad_regex));

    let bad_flag = ripsem(&dir.path, &["search", "verify", "--nope"]);
    assert_eq!(bad_flag.status.code(), Some(2), "{}", stderr(&bad_flag));
}

/// Every key of a JSON value, as dotted paths. Elements of an array share the
/// keys of their parent, so the hits of `search --json` are listed without a
/// prefix.
fn json_keys(
    value: &serde_json::Value,
    prefix: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let dotted = match prefix.is_empty() {
                    true => key.clone(),
                    false => format!("{prefix}.{key}"),
                };
                out.insert(dotted.clone());
                json_keys(child, &dotted, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                json_keys(item, prefix, out);
            }
        }
        _ => {}
    }
}

/// The field names in the table under `heading` in the README, up to the next
/// heading.
fn documented_fields(readme: &str, heading: &str) -> std::collections::BTreeSet<String> {
    let start = readme
        .find(heading)
        .unwrap_or_else(|| panic!("README has no `{heading}` section"));
    let body = &readme[start + heading.len()..];
    let body = body.find("\n#").map_or(body, |end| &body[..end]);
    body.lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .map(|cell| cell.trim().trim_matches('`').to_string())
        .filter(|field| !field.is_empty() && field != "Field" && !field.starts_with('-'))
        .collect()
}

#[test]
fn the_json_tables_in_the_readme_name_every_key_the_commands_print() {
    let dir = TempDir::new();
    dir.write(
        "src/retry.rs",
        "fn retry_on_failure(n: u32) -> u32 {\n    n + 1\n}\n",
    );
    dir.write(
        "src/header.rs",
        "fn parse_header(s: &str) -> bool {\n    s.is_empty()\n}\n",
    );
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let readme =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
    let mut cases: Vec<(&str, &[&str])> = vec![
        ("### `search`", &["search", "retry", "--json"]),
        (
            "### `search --count-by-file`",
            &["search", "retry", "--count-by-file", "--json"],
        ),
        ("### `similar`", &["similar", "src/retry.rs", "--json"]),
        ("### `status`", &["status", "--json"]),
    ];
    if cfg!(feature = "treesitter") {
        cases.push(("### `outline`", &["outline", "src/retry.rs", "--json"]));
        cases.push(("### `symbol`", &["symbol", "retry_on_failure", "--json"]));
    }
    for (heading, args) in cases {
        let out = ripsem(&dir.path, args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        let value: serde_json::Value = serde_json::from_str(&stdout(&out))
            .unwrap_or_else(|e| panic!("{args:?} is not JSON: {e}"));
        let mut printed = std::collections::BTreeSet::new();
        json_keys(&value, "", &mut printed);
        let documented = documented_fields(&readme, heading);
        assert_eq!(
            documented, printed,
            "{heading}: the table and the output of {args:?} differ"
        );
    }

    let plain = stdout(&ripsem(&dir.path, &["status", "--json"]));
    let verified = stdout(&ripsem(&dir.path, &["status", "--json", "--verify"]));
    let keys = |text: &str| {
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut found = std::collections::BTreeSet::new();
        json_keys(&value, "", &mut found);
        found
    };
    let added_by_verify: std::collections::BTreeSet<String> =
        keys(&verified).difference(&keys(&plain)).cloned().collect();
    assert_eq!(
        documented_fields(&readme, "### `status --verify`"),
        added_by_verify,
        "the keys that --verify adds to status --json"
    );
}

/// Every `RIPSEM_` name that a Rust file under `dir` mentions.
fn ripsem_names_in_sources(dir: &Path, out: &mut std::collections::BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            ripsem_names_in_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for (at, _) in text.match_indices("RIPSEM_") {
                let name: String = text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                    .collect();
                if name.len() > "RIPSEM_".len() {
                    out.insert(name);
                }
            }
        }
    }
}

#[test]
fn every_ripsem_variable_the_code_reads_is_in_the_readme_table() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut read = std::collections::BTreeSet::new();
    ripsem_names_in_sources(&root.join("src"), &mut read);
    assert!(!read.is_empty());

    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    let documented: std::collections::BTreeSet<String> = readme
        .lines()
        .filter(|line| line.starts_with("| `RIPSEM_"))
        .filter_map(|line| line.split('|').nth(1))
        .map(|cell| cell.trim().trim_matches('`').to_string())
        .collect();
    assert_eq!(
        documented, read,
        "README table and the variables read in src/"
    );
}

#[test]
fn the_readme_shows_a_line_for_each_install_target() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    let cli = <ripsem::cli::Cli as clap::CommandFactory>::command();
    let install = cli.find_subcommand("install").unwrap();
    let targets: Vec<&str> = install.get_subcommands().map(|t| t.get_name()).collect();
    assert!(!targets.is_empty());
    for target in targets {
        let line = format!("ripsem install {target}");
        assert!(readme.contains(&line), "README does not show `{line}`");
    }
}

#[test]
fn the_regex_help_names_the_depth_a_pattern_is_tried_on() {
    let dir = TempDir::new();
    let help = stdout(&ripsem(&dir.path, &["search", "--help"]));
    let help: String = help.split_whitespace().collect::<Vec<_>>().join(" ");
    let depth = format!("best {} places", ripsem::pipeline::TEXT_DEPTH);
    assert!(
        help.contains(&depth),
        "--regex help should say `{depth}`:\n{help}"
    );
}

#[test]
fn the_npm_launcher_staging_and_manifest_name_the_same_packages() {
    let npm = Path::new(env!("CARGO_MANIFEST_DIR")).join("npm");
    let package = regex::Regex::new(r"ripsem-(darwin|linux|win32)-(arm64|x64)").unwrap();
    let names = |file: &str| -> std::collections::BTreeSet<String> {
        let text = std::fs::read_to_string(npm.join(file)).unwrap();
        package
            .find_iter(&text)
            .map(|m| m.as_str().to_string())
            .collect()
    };
    let launcher = names("ripsem/bin/ripsem.js");
    assert!(!launcher.is_empty());
    assert_eq!(names("stage.mjs"), launcher, "npm/stage.mjs");
    assert_eq!(
        names("ripsem/package.json"),
        launcher,
        "npm/ripsem/package.json"
    );
}

#[test]
fn files_with_matches_with_json_prints_a_json_array_of_the_paths() {
    let dir = TempDir::new();
    dir.write(
        "src/retry.rs",
        "fn retry_on_failure(n: u32) -> u32 {\n    n + 1\n}\n",
    );
    dir.write(
        "src/header.rs",
        "fn parse_header(s: &str) -> bool {\n    s.is_empty()\n}\n",
    );
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let out = ripsem(&dir.path, &["search", "retry", "-l", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let paths: Vec<String> = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("not an array of strings: {e}\n{}", stdout(&out)));
    assert_eq!(paths.first().map(String::as_str), Some("src/retry.rs"));
    assert!(!paths.iter().any(|p| p.contains(':')), "{paths:?}");

    let help = stdout(&ripsem(&dir.path, &["search", "--help"]));
    assert!(
        help.contains("JSON array of the paths"),
        "the -l help should say what --json prints:\n{help}"
    );
}

#[test]
fn short_help_is_the_summary_and_long_help_carries_the_examples() {
    let dir = TempDir::new();
    for sub in [
        "init",
        "index",
        "search",
        "similar",
        "watch",
        "status",
        "mcp",
        "model",
        "install",
        "completions",
    ] {
        let short = stdout(&ripsem(&dir.path, &[sub, "-h"]));
        let long = stdout(&ripsem(&dir.path, &[sub, "--help"]));
        assert!(!short.contains("Examples:"), "{sub} -h:\n{short}");
        assert!(long.contains("Examples:"), "{sub} --help:\n{long}");
        assert!(short.len() <= long.len(), "{sub}");
    }
    let short = stdout(&ripsem(&dir.path, &["search", "-h"]));
    let long = stdout(&ripsem(&dir.path, &["search", "--help"]));
    assert!(short.len() < long.len(), "search -h should be shorter");
}

#[test]
fn help_has_examples_for_search_and_short_summaries_at_the_top() {
    let dir = TempDir::new();
    let search = stdout(&ripsem(&dir.path, &["search", "--help"]));
    assert!(search.contains("Examples:"), "{search}");
    assert!(
        search.contains("ripsem search retry on failure -k 5"),
        "{search}"
    );

    let top = stdout(&ripsem(&dir.path, &["--help"]));
    let similar = top
        .lines()
        .find(|line| line.trim_start().starts_with("similar"))
        .unwrap();
    assert!(similar.len() <= 100, "{similar}");
}

#[test]
fn install_takes_opencode_by_the_name_the_docs_use() {
    let dir = TempDir::new();
    let out = ripsem(&dir.path, &["install", "opencode"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dir.path.join("opencode.json").exists());

    let old = ripsem(&dir.path, &["install", "open-code"]);
    assert!(old.status.success(), "{}", stderr(&old));
}

#[test]
fn ripgrep_short_flags_for_count_and_type_are_accepted() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn login() { verify password }");
    dir.write("src/auth.py", "def login(): verify password");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    ripsem(&dir.path, &["index"]);

    let long = stdout(&ripsem(
        &dir.path,
        &[
            "search",
            "verify password",
            "--count-by-file",
            "--context",
            "0",
        ],
    ));
    let short = ripsem(
        &dir.path,
        &["search", "verify password", "-c", "--context", "0"],
    );
    assert!(short.status.success(), "{}", stderr(&short));
    assert_eq!(stdout(&short), long);
    assert!(!long.trim().is_empty());

    let typed = ripsem(
        &dir.path,
        &["search", "verify password", "-t", "rust", "--context", "0"],
    );
    assert_eq!(found_paths(&typed), ["src/auth.rs"]);
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

/// `symbol` finds a name in every file of the index, by its whole qualified
/// name or by its last part, and says where each declaration starts and ends.
#[cfg(feature = "treesitter")]
#[test]
fn symbol_finds_a_name_in_every_file_of_the_index() {
    let dir = TempDir::new();
    dir.write(
        "src/auth.rs",
        "struct Session {\n    id: u32,\n}\n\nimpl Session {\n    fn refresh(&mut self) {\n        self.id += 1;\n    }\n}\n",
    );
    dir.write(
        "src/cache.rs",
        "struct Cache {}\n\nimpl Cache {\n    fn refresh(&mut self) {}\n}\n",
    );
    assert!(ripsem(&dir.path, &["init"]).status.success());
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let by_last_part = ripsem(&dir.path, &["symbol", "refresh"]);
    assert!(by_last_part.status.success(), "{}", stderr(&by_last_part));
    assert_eq!(
        stdout(&by_last_part),
        "src/auth.rs:6-8  fn Session::refresh\nsrc/cache.rs:4-4  fn Cache::refresh\n"
    );

    let whole = ripsem(&dir.path, &["symbol", "Session::refresh"]);
    assert_eq!(stdout(&whole), "src/auth.rs:6-8  fn Session::refresh\n");

    let json = ripsem(&dir.path, &["symbol", "Session::refresh", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["path"], "src/auth.rs");
    assert_eq!(rows[0]["symbol"], "Session::refresh");
    assert_eq!(rows[0]["start_line"], 6);
    assert_eq!(rows[0]["end_line"], 8);

    let none = ripsem(&dir.path, &["symbol", "missing"]);
    assert!(none.status.success(), "{}", stderr(&none));
    assert_eq!(stdout(&none), "");
    assert!(
        stderr(&none).contains("no declaration named missing"),
        "{}",
        stderr(&none)
    );
}

/// `symbol` takes the filters `search` takes, so a name declared in many places
/// can be narrowed to one directory, language or glob.
#[cfg(feature = "treesitter")]
#[test]
fn symbol_takes_the_search_filters() {
    let dir = TempDir::new();
    dir.write(
        "src/auth.rs",
        "struct Session {}\n\nimpl Session {\n    fn refresh(&mut self) {}\n}\n",
    );
    dir.write(
        "src/cache.rs",
        "struct Cache {}\n\nimpl Cache {\n    fn refresh(&mut self) {}\n}\n",
    );
    dir.write("tools/sync.py", "def refresh():\n    pass\n");
    assert!(ripsem(&dir.path, &["init"]).status.success());
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let in_src = ripsem(&dir.path, &["symbol", "refresh", "--in", "src"]);
    assert_eq!(
        stdout(&in_src),
        "src/auth.rs:4-4  fn Session::refresh\nsrc/cache.rs:4-4  fn Cache::refresh\n"
    );

    let python = ripsem(&dir.path, &["symbol", "refresh", "-t", "py"]);
    assert_eq!(stdout(&python), "tools/sync.py:1-2  fn refresh\n");

    let not_python = ripsem(&dir.path, &["symbol", "refresh", "--lang", "!py", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&stdout(&not_python)).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{rows}");

    let one_file = ripsem(&dir.path, &["symbol", "refresh", "-g", "src/cache.rs"]);
    assert_eq!(stdout(&one_file), "src/cache.rs:4-4  fn Cache::refresh\n");

    let nowhere = ripsem(&dir.path, &["symbol", "refresh", "--in", "nowhere"]);
    assert_eq!(nowhere.status.code(), Some(1), "{}", stderr(&nowhere));
    assert!(
        stderr(&nowhere).contains("nothing is indexed under 'nowhere'"),
        "{}",
        stderr(&nowhere)
    );
}

/// Without the treesitter feature a file has no declarations, so `symbol` says
/// so instead of reporting that the name is not declared anywhere.
#[cfg(not(feature = "treesitter"))]
#[test]
fn symbol_says_when_the_build_cannot_name_declarations() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn verify() {}\n");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = ripsem(&dir.path, &["symbol", "verify"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("treesitter"), "{}", stderr(&out));
}

/// Without the treesitter feature a file has no declarations, so `outline`
/// says so instead of printing an empty list as if the file had none.
#[cfg(not(feature = "treesitter"))]
#[test]
fn outline_says_when_the_build_cannot_name_declarations() {
    let dir = TempDir::new();
    dir.write("src/auth.rs", "fn verify() {}\n");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let out = ripsem(&dir.path, &["outline", "src/auth.rs"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("treesitter"), "{}", stderr(&out));
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
        let mut child = ripsem_command()
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
fn an_offset_that_is_not_a_whole_number_says_so() {
    let dir = TempDir::new();
    for value in ["1e3", "1.5", "ten"] {
        let out = ripsem(&dir.path, &["search", "x", "--offset", value]);
        assert_eq!(out.status.code(), Some(2), "{value}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(
            err.contains("must be a whole number, 0 or more"),
            "{value}: {err}"
        );
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
    let out = ripsem_command()
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
        ("src/auth0.rs:9", "has 1 line, so line 9 is out of range"),
        ("src/auth0.rs:3-2", "comes before line 3"),
        ("src/auth0.rs:0", "line numbers start at 1"),
        (
            "src/auth0.rs:abc",
            "a line range is PATH:LINE or PATH:LINE-LINE",
        ),
        (
            "src/auth0.rs:1-",
            "a line range is PATH:LINE or PATH:LINE-LINE",
        ),
        (
            "src/auth0.rs:1-2-3",
            "a line range is PATH:LINE or PATH:LINE-LINE",
        ),
        ("src", "is a directory; `similar` takes a file"),
        ("", "the target is empty"),
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

/// Makes the repository of `dir` and commits every file in it as `base`.
fn commit_all(dir: &TempDir) {
    git(&dir.path, &["init", "-q"]);
    git(&dir.path, &["add", "-A"]);
    git(
        &dir.path,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "base",
        ],
    );
}

#[test]
fn since_limits_a_search_to_the_files_changed_since_a_revision() {
    let dir = paged_tree();
    commit_all(&dir);
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
    assert!(
        !stderr(&outside).contains("fatal:"),
        "git's own message should not be repeated: {}",
        stderr(&outside)
    );

    let without_git = ripsem_command()
        .args(["search", query, "--since", "HEAD"])
        .env("PATH", "")
        .current_dir(&dir.path)
        .output()
        .unwrap();
    assert_eq!(without_git.status.code(), Some(1));
    assert!(
        stderr(&without_git).contains("could not run git"),
        "{}",
        stderr(&without_git)
    );
    assert!(
        !stderr(&without_git).contains("not in a git repository"),
        "{}",
        stderr(&without_git)
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
    ripsem_command()
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

/// A file rewritten with the size and the time it had keeps its old text in
/// the index, and a search must not print that text as the file's contents.
#[test]
fn a_same_size_rewrite_does_not_print_its_old_text() {
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    let set_modified = |dir: &TempDir| {
        std::fs::File::options()
            .write(true)
            .open(dir.path.join("a.rs"))
            .unwrap()
            .set_modified(aged)
            .unwrap();
    };

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    set_modified(&dir);
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    dir.write("a.rs", "omega bravo charlie");
    set_modified(&dir);
    let old = ripsem(&dir.path, &["search", "alpha bravo", "--json"]);
    assert!(
        !stdout(&old).contains("alpha bravo charlie"),
        "{}",
        stdout(&old)
    );
    let new = ripsem(&dir.path, &["search", "omega bravo", "--json", "-k", "1"]);
    assert!(
        stdout(&new).contains("omega bravo charlie"),
        "{}",
        stdout(&new)
    );
}

/// `status --verify` reads the indexed files and names the one whose content
/// changed under the same size and time, which the stat check cannot see. Plain
/// `status` still judges the tree by stat, and the JSON gains the list only
/// when asked for it.
#[test]
fn status_verify_names_a_file_the_stat_cannot_show() {
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    let set_modified = |dir: &TempDir, name: &str| {
        std::fs::File::options()
            .write(true)
            .open(dir.path.join(name))
            .unwrap()
            .set_modified(aged)
            .unwrap();
    };
    let listed = |output: &Output| -> Vec<String> {
        stdout(output)
            .lines()
            .filter_map(|l| l.strip_prefix("  "))
            .map(String::from)
            .collect()
    };

    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    dir.write("b.rs", "delta echo foxtrot");
    set_modified(&dir, "a.rs");
    set_modified(&dir, "b.rs");
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let clean = ripsem(&dir.path, &["status", "--verify"]);
    assert!(clean.status.success());
    assert!(
        stdout(&clean).contains("every indexed file matches"),
        "{}",
        stdout(&clean)
    );

    dir.write("a.rs", "omega bravo charlie");
    set_modified(&dir, "a.rs");
    let plain = ripsem(&dir.path, &["status"]);
    assert!(plain.status.success());
    assert!(
        stdout(&plain).contains("tree:   matches the index"),
        "{}",
        stdout(&plain)
    );
    assert!(!stdout(&plain).contains("verify:"));

    let verified = ripsem(&dir.path, &["status", "--verify"]);
    assert!(verified.status.success());
    assert!(
        stdout(&verified).contains("verify: 1 file changed or gone on disk"),
        "{}",
        stdout(&verified)
    );
    assert_eq!(listed(&verified), ["a.rs"], "{}", stdout(&verified));

    let json = ripsem(&dir.path, &["status", "--verify", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(v["index"]["changed_on_disk"], json!(["a.rs"]));
    let plain_json = ripsem(&dir.path, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&plain_json)).unwrap();
    assert!(v["index"].get("changed_on_disk").is_none());

    std::fs::remove_file(dir.path.join("a.rs")).unwrap();
    let gone = ripsem(&dir.path, &["status", "--verify"]);
    assert!(
        stdout(&gone).contains("verify: 1 file changed or gone on disk"),
        "{}",
        stdout(&gone)
    );
    assert_eq!(listed(&gone), ["a.rs"], "{}", stdout(&gone));
}

/// Each way a file can differ from its indexed self while the index still
/// holds the old text: a rewrite that moves lines under the same size and time,
/// a file that became binary, one deleted, one replaced by a directory, and one
/// renamed with its content. A search answers from the current file or not at
/// all, never with the old text, and never panics.
#[test]
fn a_search_never_answers_from_text_the_file_no_longer_holds() {
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    let set_modified = |dir: &TempDir, name: &str| {
        std::fs::File::options()
            .write(true)
            .open(dir.path.join(name))
            .unwrap()
            .set_modified(aged)
            .unwrap();
    };
    let found = |dir: &TempDir, query: &str| -> serde_json::Value {
        let out = ripsem(&dir.path, &["search", query, "--json"]);
        assert!(out.status.success(), "{query}: {}", stderr(&out));
        assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
        serde_json::from_str(&stdout(&out)).unwrap_or(json!([]))
    };

    let dir = TempDir::new();
    dir.write("moved.rs", "fn alpha() {}\nfn bravo() {}\n");
    dir.write("binary.rs", "fn charlie() {}\n");
    dir.write("gone.rs", "fn delta() {}\n");
    dir.write("dir.rs", "fn echo() {}\n");
    dir.write("renamed.rs", "fn foxtrot() {}\n");
    for name in ["moved.rs", "binary.rs", "gone.rs", "dir.rs", "renamed.rs"] {
        set_modified(&dir, name);
    }
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    dir.write("moved.rs", "fn bravo() {}\nfn alpha() {}\n");
    set_modified(&dir, "moved.rs");
    let hits = found(&dir, "bravo");
    assert_eq!(hits[0]["path"], "moved.rs", "{hits}");
    assert_eq!(hits[0]["start_line"], 1, "{hits}");
    assert!(hits[0]["snippet"].as_str().unwrap().starts_with("fn bravo"));

    std::fs::write(
        dir.path.join("binary.rs"),
        [0u8, 159, 146, 150, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    )
    .unwrap();
    set_modified(&dir, "binary.rs");
    assert!(
        found(&dir, "charlie")
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["path"] != "binary.rs")
    );

    std::fs::remove_file(dir.path.join("gone.rs")).unwrap();
    assert!(
        found(&dir, "delta")
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["path"] != "gone.rs")
    );

    std::fs::remove_file(dir.path.join("dir.rs")).unwrap();
    std::fs::create_dir_all(dir.path.join("dir.rs")).unwrap();
    dir.write("dir.rs/inner.rs", "fn golf() {}\n");
    assert!(
        found(&dir, "echo")
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["path"] != "dir.rs")
    );
    assert_eq!(found(&dir, "golf")[0]["path"], "dir.rs/inner.rs");

    std::fs::rename(dir.path.join("renamed.rs"), dir.path.join("moved-name.rs")).unwrap();
    let hits = found(&dir, "foxtrot");
    assert_eq!(hits[0]["path"], "moved-name.rs", "{hits}");
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .all(|h| h["path"] != "renamed.rs")
    );
}

/// The MCP server keeps its searcher open. A file rewritten with the size and
/// time it had is read again by the search that finds it, so the agent gets the
/// text the file has now and not the old text, with no CLI run in between.
#[test]
fn mcp_search_reads_a_same_size_rewrite_again() {
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    let set_modified = |dir: &TempDir| {
        std::fs::File::options()
            .write(true)
            .open(dir.path.join("a.rs"))
            .unwrap()
            .set_modified(aged)
            .unwrap();
    };
    let dir = TempDir::new();
    dir.write("a.rs", "alpha bravo charlie");
    set_modified(&dir);
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let mut server = McpServer::start(&dir.path);
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;
    server.request(init);
    let before = server.request(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"alpha bravo"}}}"#,
    );
    assert!(
        tool_text(&before).contains("alpha bravo charlie"),
        "{before}"
    );

    dir.write("a.rs", "omega bravo charlie");
    set_modified(&dir);
    let after = server.request(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"omega bravo"}}}"#,
    );
    assert!(tool_text(&after).contains("omega bravo charlie"), "{after}");
    let stale = server.request(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search","arguments":{"query":"alpha bravo"}}}"#,
    );
    assert!(
        !tool_text(&stale).contains("alpha bravo charlie"),
        "{stale}"
    );
}

/// `similar` answers from the target's current text when it changed with the
/// size and time it had. Its new text is the same as `c.rs`, and its old text
/// the same as `d.rs`, so `c.rs` is the answer. The CLI and an MCP session that
/// was already open both give it, with no error and no answer from the old text.
#[test]
fn similar_reads_a_changed_target_again() {
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    let set_modified = |dir: &TempDir, name: &str| {
        std::fs::File::options()
            .write(true)
            .open(dir.path.join(name))
            .unwrap()
            .set_modified(aged)
            .unwrap();
    };
    let indexed = || {
        let dir = TempDir::new();
        dir.write("src/a.rs", "alpha bravo charlie");
        dir.write("src/d.rs", "alpha bravo charlie");
        dir.write("src/c.rs", "omega bravo charlie");
        for name in ["src/a.rs", "src/d.rs", "src/c.rs"] {
            set_modified(&dir, name);
        }
        ripsem(&dir.path, &["init"]);
        ripsem(&dir.path, &["model", "backend", "hash"]);
        assert!(ripsem(&dir.path, &["index"]).status.success());
        dir.write("src/a.rs", "omega bravo charlie");
        set_modified(&dir, "src/a.rs");
        dir
    };

    let dir = indexed();
    let cli = ripsem(&dir.path, &["similar", "src/a.rs", "--json", "-k", "1"]);
    assert!(cli.status.success(), "{}", stderr(&cli));
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout(&cli)).unwrap();
    assert_eq!(rows[0]["path"], "src/c.rs", "{rows:?}");

    let dir = TempDir::new();
    dir.write("src/a.rs", "alpha bravo charlie");
    dir.write("src/d.rs", "alpha bravo charlie");
    dir.write("src/c.rs", "omega bravo charlie");
    for name in ["src/a.rs", "src/d.rs", "src/c.rs"] {
        set_modified(&dir, name);
    }
    ripsem(&dir.path, &["init"]);
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());
    let mut server = McpServer::start(&dir.path);
    server.request(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    );
    dir.write("src/a.rs", "omega bravo charlie");
    set_modified(&dir, "src/a.rs");
    let found = server.request(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"find_similar","arguments":{"path":"src/a.rs","limit":1}}}"#,
    );
    let top = &found["result"]["structuredContent"]["hits"][0];
    assert_eq!(top["path"], "src/c.rs", "{found}");
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
fn a_bit_flipped_index_is_reported_by_status_and_rebuilt_by_search() {
    let dir = TempDir::new();
    dir.write("src/lib.rs", "fn parse_config() {}\n");
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let index = dir.path.join(".ripsem/index.bin");
    let mut bytes = std::fs::read(&index).unwrap();
    bytes[0] ^= 0x01;
    std::fs::write(&index, &bytes).unwrap();

    let status = ripsem(&dir.path, &["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert!(
        stdout(&status).contains("unreadable") && stdout(&status).contains("index --force"),
        "{}",
        stdout(&status)
    );

    let found = ripsem(&dir.path, &["search", "parse_config"]);
    assert!(found.status.success(), "{}", stderr(&found));
    assert!(stdout(&found).contains("src/lib.rs"), "{}", stdout(&found));
    assert!(!stderr(&found).contains("panicked"), "{}", stderr(&found));
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

#[cfg(unix)]
#[test]
fn an_unreadable_file_is_named_on_stderr_and_the_run_still_succeeds() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new();
    dir.write("src/keep.rs", "fn keep() {}\n");
    dir.write("src/locked.rs", "fn locked() {}\n");
    let locked = dir.path.join("src/locked.rs");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
        return;
    }
    assert!(
        ripsem(&dir.path, &["model", "backend", "hash"])
            .status
            .success()
    );

    let out = ripsem(&dir.path, &["index"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("skipped 1 unreadable file: src/locked.rs"),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("1 file: 1 changed"),
        "{}",
        stdout(&out)
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
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

/// The MCP `since` argument answers as `--since` does: `search` and
/// `find_similar` keep only the files that differ from the revision, and a
/// revision the repository does not have is a tool error.
#[test]
fn mcp_search_and_find_similar_take_since() {
    let dir = paged_tree();
    commit_all(&dir);
    dir.write(
        "src/auth0.rs",
        "fn auth0() { verify password create session token }\nfn more() {}\n",
    );
    dir.write("docs/fresh.md", "verify password, written after the commit");

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;
    let call = |id: u64, name: &str, arguments: serde_json::Value| {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } })
            .to_string()
    };
    let search = call(
        2,
        "search",
        json!({ "query": "verify password create session token", "since": "HEAD", "limit": 50 }),
    );
    let similar = call(
        3,
        "find_similar",
        json!({ "path": "src/auth1.rs", "since": "HEAD", "limit": 50 }),
    );
    let unknown = call(
        4,
        "search",
        json!({ "query": "verify password", "since": "no-such-rev" }),
    );
    let responses = mcp_over_stdio(&dir.path, &[init, &search, &similar, &unknown]);

    let paths = |response: &serde_json::Value| -> Vec<String> {
        response["result"]["structuredContent"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["path"].as_str().unwrap().to_string())
            .collect()
    };
    for found in [paths(&responses[1]), paths(&responses[2])] {
        assert!(!found.is_empty(), "{responses:?}");
        assert!(
            found
                .iter()
                .all(|p| p == "src/auth0.rs" || p == "docs/fresh.md"),
            "{found:?}"
        );
    }
    let failure = &responses[3]["result"];
    assert_eq!(failure["isError"], true, "{failure}");
    assert!(
        failure["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown revision 'no-such-rev'"),
        "{failure}"
    );
}

/// Hit headers are coloured when `--color always` asks for it, and a piped
/// search, which `auto` leaves alone, has no escape codes in it.
#[test]
fn color_marks_the_hit_headers_when_asked() {
    let dir = indexed_repository();
    let piped = stdout(&ripsem(&dir.path, &["search", "verify password"]));
    assert!(!piped.contains('\x1b'), "{piped}");
    let never = stdout(&ripsem(
        &dir.path,
        &["search", "verify password", "--color", "never"],
    ));
    assert!(!never.contains('\x1b'), "{never}");
    let always = stdout(&ripsem(
        &dir.path,
        &["search", "verify password", "--color", "always"],
    ));
    assert_eq!(always.lines().count(), piped.lines().count(), "{always}");
    let header = always.lines().next().unwrap();
    assert!(header.starts_with("\x1b[1m"), "{header}");
    assert!(header.contains("\x1b[32m"), "{header}");
    assert_eq!(
        always
            .replace("\x1b[1m", "")
            .replace("\x1b[32m", "")
            .replace("\x1b[1;31m", "")
            .replace("\x1b[0m", ""),
        piped
    );
}

/// The query's words are painted where they sit in a snippet, and only when
/// colour is on.
#[test]
fn color_marks_the_query_words_in_the_snippet() {
    let dir = indexed_repository();
    let always = stdout(&ripsem(
        &dir.path,
        &["search", "verify password", "--color", "always"],
    ));
    let snippet = always.lines().find(|l| l.contains("verify")).unwrap();
    assert!(
        snippet.contains("\x1b[1;31mverify\x1b[0m \x1b[1;31mpassword\x1b[0m"),
        "{always}"
    );
    let piped = stdout(&ripsem(&dir.path, &["search", "verify password"]));
    assert!(!piped.contains('\x1b'), "{piped}");
}

/// A directory made unreadable for the length of a test. Its permissions come
/// back on drop, so the temporary tree can be removed.
#[cfg(unix)]
struct Unreadable(std::path::PathBuf);

#[cfg(unix)]
impl Unreadable {
    fn lock(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self(path.to_path_buf())
    }
}

#[cfg(unix)]
impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;

        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Asks the server until a search answers with `needle`, as the watcher reads a
/// new file in after a short delay, and fails after twenty seconds.
#[cfg(unix)]
fn search_until_found(server: &mut McpServer, query: &str, needle: &str) -> String {
    let started = std::time::Instant::now();
    let message = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"search","arguments":{{"query":"{query}"}}}}}}"#
    );
    loop {
        let text = tool_text(&server.request(&message));
        if text.contains(needle) {
            return text;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "no hit for '{query}' after twenty seconds: {text}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// An unreadable directory under the root does not end the watch: the server
/// still picks up a file added afterwards.
#[cfg(unix)]
#[test]
fn the_server_keeps_watching_past_an_unreadable_directory() {
    let dir = indexed_repository();
    dir.write("vendor/locked/inner.rs", "fn inner() {}\n");
    let locked = dir.path.join("vendor/locked");
    let _lock = Unreadable::lock(&locked);
    if std::fs::read_dir(&locked).is_ok() {
        return;
    }

    let mut server = McpServer::start(&dir.path);
    server.request(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    );
    dir.write("src/fresh.rs", "fn quarantine_ledger_total() {}\n");
    let found = search_until_found(&mut server, "quarantine ledger total", "src/fresh.rs");
    assert!(found.contains("quarantine_ledger_total"), "{found}");
}

/// A symbolic link to a tree elsewhere is not followed, so an unreadable
/// directory in that tree neither ends the watch nor is watched at all.
#[cfg(unix)]
#[test]
fn the_server_does_not_watch_a_tree_a_symlink_points_to() {
    let dir = indexed_repository();
    let outside = TempDir::new();
    outside.write("locked/inner.rs", "fn inner() {}\n");
    let locked = outside.path.join("locked");
    let _lock = Unreadable::lock(&locked);
    if std::fs::read_dir(&locked).is_ok() {
        return;
    }
    std::os::unix::fs::symlink(&outside.path, dir.path.join("linked")).unwrap();

    let mut server = McpServer::start(&dir.path);
    server.request(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    );
    dir.write("src/fresh.rs", "fn symlinked_ledger_total() {}\n");
    let found = search_until_found(&mut server, "symlinked ledger total", "src/fresh.rs");
    assert!(found.contains("symlinked_ledger_total"), "{found}");
}

/// A file whose name is not UTF-8 cannot be named in a hit or read back by its
/// path, so it is left out with the unreadable files, and the rest of the tree
/// is indexed as usual.
#[cfg(unix)]
#[test]
fn a_file_whose_name_is_not_utf8_is_skipped_and_named() {
    use std::os::unix::ffi::OsStrExt;

    let dir = indexed_repository();
    let odd = dir
        .path
        .join("src")
        .join(std::ffi::OsStr::from_bytes(b"caf\xe9.rs"));
    // APFS refuses a name that is not UTF-8, so there is no such file to skip.
    if std::fs::write(&odd, "fn latin_name() {}\n").is_err() {
        return;
    }

    let index = ripsem(&dir.path, &["index"]);
    assert!(index.status.success(), "{}", stderr(&index));
    assert!(
        stderr(&index).contains("skipped 1 unreadable file: src/caf"),
        "{}",
        stderr(&index)
    );
    let found = ripsem(&dir.path, &["search", "authenticate"]);
    assert!(stdout(&found).contains("src/auth.rs"), "{}", stdout(&found));
    assert!(!stdout(&found).contains("latin_name"), "{}", stdout(&found));
}

/// A file can carry terminal control sequences. The text a search prints shows
/// them as escapes, so they cannot clear the screen, set a title or write the
/// clipboard of the terminal that prints them.
#[test]
fn control_characters_in_a_file_are_printed_as_escapes() {
    let dir = TempDir::new();
    dir.write(
        "src/evil.rs",
        "fn evil() { /* \x1b]52;c;ZXZpbA==\x07 \x1b[2J */ }\nfn quiet() {}\n",
    );
    #[cfg(unix)]
    std::fs::write(dir.path.join("src/ti\x1b[31mtle.rs"), "fn titled() {}\n").unwrap();
    assert!(ripsem(&dir.path, &["init"]).status.success());
    ripsem(&dir.path, &["model", "backend", "hash"]);
    assert!(ripsem(&dir.path, &["index"]).status.success());

    let search = stdout(&ripsem(&dir.path, &["search", "evil"]));
    assert!(
        !search.contains('\x1b') && !search.contains('\x07'),
        "{search:?}"
    );
    assert!(search.contains(r"\u{1b}]52;c;ZXZpbA==\u{7}"), "{search}");
    let vimgrep = stdout(&ripsem(&dir.path, &["search", "evil", "--vimgrep"]));
    assert!(!vimgrep.contains('\x1b'), "{vimgrep:?}");
    #[cfg(unix)]
    {
        let names = stdout(&ripsem(&dir.path, &["search", "-l", "titled"]));
        assert!(!names.contains('\x1b'), "{names:?}");
        assert!(names.contains(r"ti\u{1b}[31mtle.rs"), "{names}");
    }
}

/// A project whose own directory name is not UTF-8 is indexed like any other:
/// only the names below the root decide what can be read back.
#[cfg(unix)]
#[test]
fn a_root_whose_path_is_not_utf8_still_indexes_its_files() {
    use std::os::unix::ffi::OsStrExt;

    let dir = TempDir::new();
    let root = dir.path.join(std::ffi::OsStr::from_bytes(b"repo\xff"));
    // APFS refuses a name that is not UTF-8, so there is no such root to index.
    if std::fs::create_dir_all(root.join("src")).is_err() {
        return;
    }
    std::fs::write(
        root.join("src/auth.rs"),
        "fn authenticate(user) { verify password }",
    )
    .unwrap();
    assert!(ripsem(&root, &["init"]).status.success());
    ripsem(&root, &["model", "backend", "hash"]);
    let index = ripsem(&root, &["index"]);
    assert!(index.status.success(), "{}", stderr(&index));
    assert!(!stderr(&index).contains("skipped"), "{}", stderr(&index));
    let found = ripsem(&root, &["search", "authenticate"]);
    assert!(stdout(&found).contains("src/auth.rs"), "{}", stdout(&found));
}

/// A repository's configuration is read and written in the repository only. A
/// link in its place is refused, so a file outside the project is neither
/// read, as its lines would appear in the error, nor overwritten by a
/// `model set`.
#[cfg(unix)]
#[test]
fn a_config_that_is_a_symbolic_link_is_neither_read_nor_written() {
    let dir = TempDir::new();
    dir.write("src/a.rs", "fn a() {}\n");
    assert!(ripsem(&dir.path, &["init"]).status.success());
    let outside = TempDir::new();
    outside.write("creds.txt", "this is = not toml [\n");
    let target = outside.path.join("creds.txt");
    let config = dir.path.join(".ripsem/config.toml");
    std::fs::remove_file(&config).unwrap();
    std::os::unix::fs::symlink(&target, &config).unwrap();

    let status = ripsem(&dir.path, &["status"]);
    assert_eq!(status.status.code(), Some(1), "{}", stdout(&status));
    assert!(
        stderr(&status).contains("not a regular file"),
        "{}",
        stderr(&status)
    );
    assert!(!stderr(&status).contains("not toml"), "{}", stderr(&status));

    let set = ripsem(&dir.path, &["model", "set", "bge-small-en-v1.5"]);
    assert!(!set.status.success(), "{}", stdout(&set));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "this is = not toml [\n"
    );
}

/// A repository's data directory is a directory of the project. A link in its
/// place is refused, so the index and the configuration are not written to a
/// directory outside the project.
#[cfg(unix)]
#[test]
fn a_data_directory_that_is_a_symbolic_link_is_refused() {
    let dir = TempDir::new();
    dir.write("src/a.rs", "fn a() {}\n");
    assert!(ripsem(&dir.path, &["init"]).status.success());
    let outside = TempDir::new();
    std::fs::remove_dir_all(dir.path.join(".ripsem")).unwrap();
    std::os::unix::fs::symlink(&outside.path, dir.path.join(".ripsem")).unwrap();

    let index = ripsem(&dir.path, &["index"]);
    assert_eq!(index.status.code(), Some(1), "{}", stdout(&index));
    assert!(
        stderr(&index).contains("symbolic link"),
        "{}",
        stderr(&index)
    );
    let init = ripsem(&dir.path, &["init"]);
    assert!(!init.status.success(), "{}", stdout(&init));
    assert!(std::fs::read_dir(&outside.path).unwrap().next().is_none());
}

/// `install` writes an agent's files inside the project. A note file or a
/// directory on the way to one that is a link out of the project is refused
/// before anything is written, and a link that stays inside is followed.
#[cfg(unix)]
#[test]
fn install_does_not_write_through_a_link_out_of_the_project() {
    let dir = TempDir::new();
    let outside = TempDir::new();
    let note = outside.path.join("bashrc");
    std::fs::write(&note, "export PATH=$PATH:~/bin\n").unwrap();
    std::os::unix::fs::symlink(&note, dir.path.join("CLAUDE.md")).unwrap();

    let install = ripsem(&dir.path, &["install", "claude-code"]);
    assert_eq!(install.status.code(), Some(1), "{}", stdout(&install));
    assert!(
        stderr(&install).contains("link out of the project"),
        "{}",
        stderr(&install)
    );
    assert_eq!(
        std::fs::read_to_string(&note).unwrap(),
        "export PATH=$PATH:~/bin\n"
    );

    std::fs::remove_file(dir.path.join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink(&outside.path, dir.path.join(".cursor")).unwrap();
    let install = ripsem(&dir.path, &["install", "cursor"]);
    assert!(!install.status.success(), "{}", stdout(&install));
    assert!(!outside.path.join("mcp.json").exists());
    assert!(!outside.path.join("rules").exists());

    std::fs::remove_file(dir.path.join(".cursor")).unwrap();
    dir.write("AGENTS.md", "# Notes\n");
    std::os::unix::fs::symlink(dir.path.join("AGENTS.md"), dir.path.join("CLAUDE.md")).unwrap();
    let install = ripsem(&dir.path, &["install", "claude-code"]);
    assert!(install.status.success(), "{}", stderr(&install));
    assert!(
        std::fs::read_to_string(dir.path.join("AGENTS.md"))
            .unwrap()
            .contains("indexed by ripsem")
    );
}

/// The index is a file of the project. A link in its place is not read through,
/// so a repository cannot point ripsem at a file elsewhere, and the index is
/// built again in the project's own place.
#[cfg(unix)]
#[test]
fn an_index_that_is_a_symbolic_link_is_not_read_through() {
    let dir = TempDir::new();
    dir.write("src/a.rs", "fn a() {}\n");
    assert!(ripsem(&dir.path, &["init"]).status.success());
    assert!(ripsem(&dir.path, &["index"]).status.success());
    let outside = TempDir::new();
    let target = outside.path.join("index.bin");
    std::fs::write(&target, "not ripsem's index\n").unwrap();
    let index = dir.path.join(".ripsem/index.bin");
    std::fs::remove_file(&index).unwrap();
    std::os::unix::fs::symlink(&target, &index).unwrap();

    let status = ripsem(&dir.path, &["status"]);
    assert!(
        stdout(&status).contains("not a regular file"),
        "{}",
        stdout(&status)
    );

    let search = ripsem(&dir.path, &["search", "fn a"]);
    assert!(search.status.success(), "{}", stderr(&search));
    assert!(stdout(&search).contains("src/a.rs"), "{}", stdout(&search));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "not ripsem's index\n"
    );
    assert!(!std::fs::symlink_metadata(&index).unwrap().is_symlink());
}

/// Runs `ripsem` in `dir` and fails the test if it is still running after a
/// minute. A read that blocks on a pipe would otherwise block the test too.
#[cfg(unix)]
fn ripsem_with_deadline(dir: &Path, args: &[&str]) -> Output {
    let mut child = ripsem_command()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            panic!("ripsem {args:?} blocked on a pipe in {}", dir.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// An agent's file that is a pipe is refused before `install` reads it, so the
/// command ends with an error instead of waiting for a writer.
#[cfg(unix)]
#[test]
fn install_refuses_an_agent_file_that_is_a_pipe_without_blocking() {
    let dir = TempDir::new();
    assert!(
        Command::new("mkfifo")
            .arg(dir.path.join("CLAUDE.md"))
            .status()
            .unwrap()
            .success()
    );
    let install = ripsem_with_deadline(&dir.path, &["install", "claude-code"]);
    assert_eq!(install.status.code(), Some(1), "{}", stdout(&install));
    assert!(
        stderr(&install).contains("not a regular file"),
        "{}",
        stderr(&install)
    );
}

#[cfg(unix)]
#[test]
fn init_that_cannot_write_its_config_leaves_no_empty_gitignore() {
    let dir = TempDir::new();
    let out = Command::new("sh")
        .arg("-c")
        .arg("trap '' XFSZ; ulimit -f 0; exec \"$0\" --path \"$1\" init")
        .arg(env!("CARGO_BIN_EXE_ripsem"))
        .arg(&dir.path)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!dir.path.join(".ripsem/.gitignore").exists());
    let again = ripsem(&dir.path, &["init"]);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(dir.path.join(".ripsem/.gitignore").exists());
}
