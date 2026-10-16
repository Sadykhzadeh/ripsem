use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::config::Config;
use crate::pipeline;

/// Protocol revisions this server speaks, newest first. Between them they
/// change transports, authorization and client features, and add optional
/// tool fields (structured output, titles, annotations, icons) that a single
/// text-returning tool has no need for. The one difference that reaches a
/// stdio server is that 2025-03-26 lets the client send JSON-RPC batches,
/// which [`reply`] accepts under any revision.
///
/// 2026-07-28 drops `initialize` for per-request metadata. A client of that
/// revision probes with `server/discover` first, and the method-not-found
/// answer tells it to fall back to `initialize`.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

pub fn serve(mut config: Config) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut session = None;
    let mut tried = None;

    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        hold_the_index(&mut session, &mut tried, &mut config);
        if let Some(response) = reply(&config, session.as_ref(), &request) {
            writeln!(writer, "{}", serde_json::to_string(&response)?)?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Keeps one searcher open across requests, taking it as soon as an index
/// exists. The server often starts before the first `ripsem index` finishes;
/// once it has the index it only re-reads the file when it changes.
///
/// `tried` is the index mtime at the last failed attempt. Opening decodes the
/// whole file and can fail for a lasting reason (another model, an older
/// format), so a retry waits for the file to change.
///
/// `ripsem model` followed by `ripsem index --force` rebuilds the index for
/// another model while the server runs. The held index cannot take it, so the
/// configuration is read again, and when it names another model or backend the
/// server opens the new index under it.
fn hold_the_index(
    session: &mut Option<pipeline::Searcher>,
    tried: &mut Option<std::time::SystemTime>,
    config: &mut Config,
) {
    if let Some(searcher) = session {
        if searcher.refresh().is_ok() {
            return;
        }
        match Config::load(&config.root) {
            Ok(fresh) if fresh.backend != config.backend || fresh.model != config.model => {
                *config = fresh;
                *session = None;
                *tried = None;
            }
            _ => return,
        }
    }
    let mtime = std::fs::metadata(config.index_path())
        .and_then(|m| m.modified())
        .ok();
    if mtime.is_none() || mtime == *tried {
        return;
    }
    *tried = mtime;
    *session = pipeline::open(config).ok();
    // The agent usually pauses between the handshake and its first search.
    if let Some(searcher) = session {
        searcher.warm();
    }
}

/// Answers one line: a request, a notification, or a batch of them. A batch is
/// answered with an array of the responses its requests produce, and with
/// nothing when it held only notifications.
fn reply(config: &Config, session: Option<&pipeline::Searcher>, message: &Value) -> Option<Value> {
    match message {
        Value::Array(batch) if batch.is_empty() => Some(error(Value::Null, -32600, "empty batch")),
        Value::Array(batch) => {
            let responses: Vec<Value> = batch
                .iter()
                .filter_map(|request| handle(config, session, request))
                .collect();
            (!responses.is_empty()).then_some(Value::Array(responses))
        }
        request => handle(config, session, request),
    }
}

/// The client's revision when this server speaks it. Otherwise the newest one,
/// which the client either accepts or disconnects on.
fn negotiate(requested: Option<&str>) -> &'static str {
    PROTOCOL_VERSIONS
        .into_iter()
        .find(|&version| Some(version) == requested)
        .unwrap_or(PROTOCOL_VERSIONS[0])
}

fn handle(config: &Config, session: Option<&pipeline::Searcher>, request: &Value) -> Option<Value> {
    let method = request.get("method")?.as_str()?;
    let id = request.get("id").cloned();

    match method {
        "initialize" => Some(ok(
            id?,
            json!({
                "protocolVersion": negotiate(
                    request
                        .get("params")
                        .and_then(|p| p.get("protocolVersion"))
                        .and_then(Value::as_str)
                ),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "ripsem", "version": env!("CARGO_PKG_VERSION") }
            }),
        )),
        "tools/list" => Some(ok(id?, json!({ "tools": [tool_schema()] }))),
        "tools/call" => Some(handle_call(config, session, id?, request)),
        "ping" => Some(ok(id?, json!({}))),
        _ if id.is_some() => Some(error(id.unwrap(), -32601, "method not found")),
        _ => None,
    }
}

fn tool_schema() -> Value {
    json!({
        "name": "search",
        "description": "Find code in this repository. Handles both exact terms (identifiers, error strings, constants) and plain-language descriptions of behaviour in the same call, and returns ranked snippets with file paths and line ranges. Prefer this over grepping and reading whole files: it costs one call instead of guessing at the author's wording.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What you are looking for: an identifier, an error message, or a description of the behaviour." },
                "path": { "type": "string", "description": "Optional subdirectory (relative to the repo) to limit results to, e.g. 'src/embed'." },
                "limit": { "type": "integer", "description": "Maximum number of results (default 5)." },
                "min_score": { "type": "number", "description": "Drop hits below this confidence (0–1): semantic similarity, or the share of query terms found literally, whichever is higher. Default: no filter." }
            },
            "required": ["query"]
        }
    })
}

fn handle_call(
    config: &Config,
    session: Option<&pipeline::Searcher>,
    id: Value,
    request: &Value,
) -> Value {
    let args = request
        .get("params")
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let name = request
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    if name != "search" {
        return error(id, -32602, "unknown tool");
    }

    // Arguments that do not fit the input schema are a tool error, which the
    // model sees and can correct, rather than a protocol error.
    let query = match args.get("query").and_then(|q| q.as_str()) {
        Some(q) => q,
        None => return failed(id, &anyhow!("missing 'query'")),
    };
    let limit = args
        .get("limit")
        .and_then(|l| l.as_u64())
        .map(|l| l as usize)
        .unwrap_or(5);
    let min_score = args
        .get("min_score")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);

    let prefix = match args.get("path").and_then(|p| p.as_str()) {
        Some(raw) => match scope_prefix(raw, &config.root) {
            Ok(prefix) => prefix,
            Err(e) => return failed(id, &e),
        },
        None => None,
    };

    let result = match session {
        Some(s) => s.query_scoped(query, limit, prefix.as_deref()),
        None => {
            pipeline::open(config).and_then(|s| s.query_scoped(query, limit, prefix.as_deref()))
        }
    };

    match result {
        Ok(hits) => {
            let hits = apply_min_score(hits, min_score);
            ok(id, tool_result(&hits))
        }
        Err(e) => failed(id, &e),
    }
}

fn failed(id: Value, e: &anyhow::Error) -> Value {
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": format!("search failed: {e}") }],
            "isError": true
        }),
    )
}

/// Turns a subdirectory argument into a repo-relative, forward-slash prefix.
/// `.`, empty and whitespace mean no scope. Used by the MCP `search` tool and
/// `search --in`.
///
/// Index paths look like `src/embed/x.rs`, so `./src`, `src/../src` and
/// absolute paths into the repository are reduced to that form. A path outside
/// the root is an error rather than a silent whole-index search.
pub(crate) fn scope_prefix(raw: &str, root: &Path) -> Result<Option<String>> {
    let raw = raw.trim();
    let outside = || {
        let root = root.display().to_string();
        anyhow!(
            "'{raw}' is outside the indexed directory {}",
            root.strip_prefix(r"\\?\").unwrap_or(&root)
        )
    };
    let relative: PathBuf = if Path::new(raw).is_absolute() {
        let candidate = Path::new(raw);
        match candidate.strip_prefix(root) {
            Ok(rel) => rel.to_path_buf(),
            // The root is canonical (`\\?\` prefix on Windows), a typed path
            // usually is not.
            Err(_) => candidate
                .canonicalize()
                .ok()
                .and_then(|c| c.strip_prefix(root).ok().map(Path::to_path_buf))
                .ok_or_else(outside)?,
        }
    } else {
        PathBuf::from(raw.replace('\\', "/"))
    };

    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop().ok_or_else(outside)?;
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// Drop hits whose score is below `min`. `None` keeps all.
fn apply_min_score(hits: Vec<pipeline::SearchHit>, min: Option<f32>) -> Vec<pipeline::SearchHit> {
    match min {
        Some(min) => hits.into_iter().filter(|h| h.score >= min).collect(),
        None => hits,
    }
}

fn tool_result(hits: &[pipeline::SearchHit]) -> Value {
    if hits.is_empty() {
        return json!({
            "content": [{ "type": "text", "text": "No matches." }],
            "isError": false
        });
    }
    let mut text = String::new();
    for h in hits {
        text.push_str(&format!("{}:{}-{}", h.path, h.start_line, h.end_line));
        if let Some(symbol) = &h.symbol {
            text.push_str(&format!(" {}", symbol.label()));
        }
        text.push_str(&format!("\n{}\n", h.snippet));
    }
    json!({
        "content": [{ "type": "text", "text": text.trim_end() }],
        "isError": false
    })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    #[test]
    fn initialize_reports_server_info() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("initialize", json!({}))).unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "ripsem");
        assert_eq!(resp["result"]["capabilities"]["tools"], json!({}));
    }

    fn negotiated(params: Value) -> Value {
        let cfg = Config::default();
        handle(&cfg, None, &req("initialize", params)).unwrap()["result"]["protocolVersion"].clone()
    }

    #[test]
    fn initialize_answers_with_the_version_the_client_asked_for() {
        for version in PROTOCOL_VERSIONS {
            assert_eq!(negotiated(json!({ "protocolVersion": version })), version);
        }
    }

    #[test]
    fn initialize_offers_the_newest_version_for_one_it_does_not_speak() {
        assert_eq!(
            negotiated(json!({ "protocolVersion": "2099-01-01" })),
            "2025-11-25"
        );
        assert_eq!(
            negotiated(json!({ "protocolVersion": 20241105 })),
            "2025-11-25"
        );
        assert_eq!(negotiated(json!({})), "2025-11-25");
    }

    /// A 2026-07-28 client probes with `server/discover` and falls back to
    /// `initialize` on any error that is not one of that revision's own.
    #[test]
    fn a_probe_for_the_stateless_revision_is_method_not_found() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("server/discover", json!({}))).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn a_batch_is_answered_with_the_responses_of_its_requests() {
        let cfg = Config::default();
        let batch = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": "two", "method": "tools/list" }
        ]);
        let resp = reply(&cfg, None, &batch).unwrap();
        let responses = resp.as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[1]["id"], "two");
        assert_eq!(responses[1]["result"]["tools"][0]["name"], "search");

        let notes = json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }]);
        assert!(reply(&cfg, None, &notes).is_none());
        assert_eq!(
            reply(&cfg, None, &json!([])).unwrap()["error"]["code"],
            -32600
        );
    }

    #[test]
    fn tools_list_exposes_search() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search");
    }

    #[test]
    fn unknown_method_is_an_error() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("nope", json!({}))).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn notification_without_id_is_ignored() {
        let cfg = Config::default();
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle(&cfg, None, &note).is_none());
    }

    /// A missing argument goes back to the model as a tool error it can act on.
    #[test]
    fn call_without_query_is_a_tool_error() {
        let cfg = Config::default();
        let resp = handle(
            &cfg,
            None,
            &req("tools/call", json!({ "name": "search", "arguments": {} })),
        )
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("missing 'query'"), "got: {text}");
    }

    #[test]
    fn call_to_unknown_tool_is_rejected() {
        let cfg = Config::default();
        let resp = handle(
            &cfg,
            None,
            &req("tools/call", json!({ "name": "other", "arguments": {} })),
        )
        .unwrap();
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn scope_prefix_normalizes_relative_paths() {
        let root = Path::new("/repo");
        let scope = |raw: &str| scope_prefix(raw, root).unwrap();
        assert_eq!(scope("src/embed"), Some("src/embed".into()));
        assert_eq!(scope("src\\embed\\"), Some("src/embed".into()));
        assert_eq!(scope("."), None);
        assert_eq!(scope("  "), None);
        assert_eq!(scope("./src/"), Some("src".into()));
        assert_eq!(scope(".\\src\\embed"), Some("src/embed".into()));
        assert_eq!(scope("src/../docs/./api"), Some("docs/api".into()));
        assert_eq!(scope("src/.."), None);
    }

    #[test]
    fn a_scope_that_leaves_the_root_is_refused() {
        let root = Path::new("/repo");
        assert!(scope_prefix("..", root).is_err());
        assert!(scope_prefix("src/../../other", root).is_err());
    }

    /// The root is canonical (`\\?\C:\…` on Windows) and a typed path is not;
    /// the mismatch must not drop the scope.
    #[test]
    fn an_absolute_scope_is_matched_against_the_canonical_root() {
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write("src/embed/mod.rs", "fn build() {}");
        let root = dir.path().canonicalize().unwrap();

        let inside = dir.path().join("src").join("embed");
        assert_eq!(
            scope_prefix(&inside.to_string_lossy(), &root).unwrap(),
            Some("src/embed".into())
        );
        let elsewhere = std::env::temp_dir();
        let err = scope_prefix(&elsewhere.to_string_lossy(), &root).unwrap_err();
        assert!(err.to_string().contains("outside"), "got: {err}");
    }

    #[test]
    fn a_search_scoped_outside_the_root_is_a_tool_error() {
        let cfg = Config::default();
        let resp = handle(
            &cfg,
            None,
            &req(
                "tools/call",
                json!({ "name": "search", "arguments": { "query": "x", "path": "../elsewhere" } }),
            ),
        )
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("outside"), "got: {text}");
    }

    #[test]
    fn result_text_lists_hits() {
        let hits = vec![pipeline::SearchHit {
            path: "a.rs".into(),
            start_line: 1,
            end_line: 4,
            score: 0.5,
            snippet: "fn main() {}".into(),
            symbol: None,
        }];
        let value = tool_result(&hits);
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("a.rs:1-4"));
        assert!(!text.contains("score"));
        assert!(!text.contains("0.500"));
        assert_eq!(value["isError"], false);
    }

    #[test]
    fn result_text_names_the_declaration_when_known() {
        let hits = vec![pipeline::SearchHit {
            path: "src/pipeline.rs".into(),
            start_line: 258,
            end_line: 293,
            score: 0.8,
            snippet: "let qv = self.embedder()?".into(),
            symbol: Some(crate::symbol::Symbol {
                kind: "fn".into(),
                name: "Searcher::query_scoped".into(),
            }),
        }];
        let text = tool_result(&hits)["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            text.contains("src/pipeline.rs:258-293 fn Searcher::query_scoped"),
            "got: {text}"
        );
    }

    fn make_hit(path: &str, start: usize, end: usize, score: f32) -> pipeline::SearchHit {
        pipeline::SearchHit {
            path: path.into(),
            start_line: start,
            end_line: end,
            score,
            snippet: "x".into(),
            symbol: None,
        }
    }

    #[test]
    fn mcp_apply_min_score_filters() {
        let hits = vec![make_hit("a.rs", 1, 2, 0.9), make_hit("a.rs", 3, 4, 0.3)];
        let kept = apply_min_score(hits, Some(0.5));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].score, 0.9);
    }

    /// The server can start before `ripsem index` has run, and must pick the
    /// index up once it appears.
    #[test]
    fn the_index_is_held_as_soon_as_there_is_one() {
        use crate::config::Backend;
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write("auth.rs", "fn authenticate(user) { verify password }");
        let mut config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };

        let mut session = None;
        let mut tried = None;
        hold_the_index(&mut session, &mut tried, &mut config);
        assert!(session.is_none(), "there is no index to hold yet");
        assert!(
            tried.is_none(),
            "a missing index is not an attempt worth remembering"
        );

        pipeline::index_repo(&config, false).unwrap();
        hold_the_index(&mut session, &mut tried, &mut config);
        assert!(
            session.is_some(),
            "the index exists now and must be held instead of reopened per call"
        );
    }

    /// An index that fails to open stays that way until it is rebuilt, so the
    /// server must not decode it again on every request.
    #[test]
    fn an_index_that_cannot_be_opened_is_not_retried_until_it_changes() {
        use crate::config::Backend;
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write("auth.rs", "fn authenticate(user) { verify password }");
        let mut config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        pipeline::index_repo(&config, false).unwrap();
        // Configured for a model the index was not built with, which is a
        // failure that lasts until `ripsem index --force`.
        config.model = "all-MiniLM-L6-v2".into();

        let mut session = None;
        let mut tried = None;
        hold_the_index(&mut session, &mut tried, &mut config);
        assert!(session.is_none(), "the index is not usable");
        let first = tried;
        assert!(first.is_some(), "the attempt has to be remembered");

        hold_the_index(&mut session, &mut tried, &mut config);
        assert_eq!(tried, first, "the same file must not be decoded again");

        // A rebuild is a new file and is tried again. The pause keeps the
        // mtime from repeating (see `searcher_hot_reloads_a_rebuilt_index`).
        std::thread::sleep(std::time::Duration::from_millis(20));
        pipeline::index_repo(&config, true).unwrap();
        hold_the_index(&mut session, &mut tried, &mut config);
        assert!(session.is_some(), "a rebuilt index has to be picked up");
    }

    /// `ripsem model set` and `ripsem index --force` while the server runs.
    #[test]
    fn the_server_moves_to_an_index_rebuilt_for_another_model() {
        use crate::config::Backend;
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write("auth.rs", "fn authenticate(user) { verify password }");
        let mut config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        config.save().unwrap();
        pipeline::index_repo(&config, false).unwrap();

        let mut session = None;
        let mut tried = None;
        hold_the_index(&mut session, &mut tried, &mut config);
        assert_eq!(session.as_ref().unwrap().model(), "bge-small-en-v1.5");

        let mut switched = Config::load(dir.path()).unwrap();
        switched.model = "all-MiniLM-L6-v2".into();
        switched.save().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        pipeline::index_repo(&switched, true).unwrap();

        hold_the_index(&mut session, &mut tried, &mut config);
        assert_eq!(config.model, "all-MiniLM-L6-v2");
        let searcher = session.as_ref().expect("the rebuilt index is held");
        assert_eq!(searcher.model(), "all-MiniLM-L6-v2");
        let hits = searcher.query("verify password", 5).unwrap();
        assert_eq!(hits[0].path, "auth.rs");
    }

    /// A file the server cannot use and the configuration does not explain,
    /// here one from a newer ripsem, leaves the index it holds answering.
    #[test]
    fn an_unusable_rebuild_leaves_the_held_index_answering() {
        use crate::config::Backend;
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write("auth.rs", "fn authenticate(user) { verify password }");
        let mut config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        config.save().unwrap();
        pipeline::index_repo(&config, false).unwrap();

        let mut session = None;
        let mut tried = None;
        hold_the_index(&mut session, &mut tried, &mut config);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut newer = b"ripsem".to_vec();
        newer.extend_from_slice(&(crate::index::FORMAT_VERSION + 1).to_le_bytes());
        std::fs::write(config.index_path(), newer).unwrap();

        hold_the_index(&mut session, &mut tried, &mut config);
        let searcher = session.as_ref().expect("the held index is kept");
        assert_eq!(
            searcher.query("verify password", 5).unwrap()[0].path,
            "auth.rs"
        );
    }

    #[test]
    fn mcp_apply_min_score_none_keeps_all() {
        let hits = vec![make_hit("a.rs", 1, 2, 0.1), make_hit("b.rs", 1, 2, 0.9)];
        assert_eq!(apply_min_score(hits, None).len(), 2);
    }

    // Drives a real tools/call through handle() against a small hash-backend index.
    #[test]
    fn search_call_against_real_index() {
        use crate::config::{Backend, Config};
        use crate::pipeline;
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write(
            "auth.rs",
            "fn authenticate(user) { verify password create session token logout }",
        );
        dir.write(
            "math.rs",
            "fn add(a, b) { return arithmetic sum product divide remainder }",
        );

        let config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        pipeline::index_repo(&config, false).unwrap();
        let searcher = pipeline::open(&config).unwrap();

        let request = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "search",
                "arguments": { "query": "verify password session token", "limit": 3 }
            }
        });

        let resp = handle(&config, Some(&searcher), &request).unwrap();

        assert!(resp.get("error").is_none(), "unexpected error: {resp}");
        let result = &resp["result"];
        assert_eq!(result["isError"], false);

        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("auth.rs"),
            "expected auth.rs in result, got: {text}"
        );
        assert!(!text.contains("score"), "score should not appear: {text}");

        let hit_count = text.matches(':').count();
        assert!(hit_count <= 3, "too many hits for limit=3: {text}");

        // min_score gates on confidence, so a chunk holding every query term
        // survives even the strictest threshold.
        let certain = handle(
            &config,
            Some(&searcher),
            &json!({
                "jsonrpc": "2.0",
                "id": 43,
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": { "query": "verify password session token", "min_score": 1.0 }
                }
            }),
        )
        .unwrap();
        assert!(
            certain["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("auth.rs"),
            "a chunk containing every query term is a certain match"
        );

        // A query the corpus cannot answer is filtered out instead.
        let unrelated = handle(
            &config,
            Some(&searcher),
            &json!({
                "jsonrpc": "2.0",
                "id": 44,
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": { "query": "quantum chromodynamics lattice", "min_score": 0.5 }
                }
            }),
        )
        .unwrap();
        assert_eq!(
            unrelated["result"]["content"][0]["text"].as_str().unwrap(),
            "No matches."
        );
    }
}
