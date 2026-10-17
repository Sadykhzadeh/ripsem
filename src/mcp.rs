use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf, Prefix};

use anyhow::{Result, anyhow, bail};
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

/// What an agent is told about the `search` tool, so it reaches for it before
/// guessing names with grep. Sent as the server instructions, which reach the
/// model even from a client that loads tool descriptions only on demand, and
/// written by `ripsem install` to CLAUDE.md or a Cursor rule.
pub(crate) const INSTRUCTIONS: &str =
    "This repository is indexed by ripsem. To find where something is
implemented, call the `search` tool of the `ripsem` MCP server first, with an
identifier, an error message or a plain description of the behaviour. Each hit
names a file and a line range; read those lines rather than the whole file. Use
grep when every occurrence of a string is needed.";

/// A directory with a `.ripsem` data directory has opted in, through `ripsem
/// init`, `index` or `install`, and its index is kept current while the server
/// runs. Anywhere else, such as a home directory a client happened to start
/// in, nothing is indexed behind the user's back.
///
/// A server given no `--path` and started outside an index follows the
/// client's workspace roots instead. It moves to the project the first root is
/// in, and again whenever the roots change, until it reaches a project with a
/// `.ripsem` directory, where it stays and keeps the index current.
pub fn serve(config: Config, explicit: bool) -> Result<()> {
    serve_on(
        config,
        explicit,
        crate::watch::spawn,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
}

fn serve_on(
    mut config: Config,
    explicit: bool,
    watch: impl Fn(Config),
    mut reader: impl BufRead,
    mut writer: impl Write,
) -> Result<()> {
    let watching = config.data_dir().is_dir();
    if watching {
        watch(config.clone());
    }
    let mut roots = Roots {
        follow: !explicit && !watching,
        ..Default::default()
    };
    let mut session = None;
    let mut tried = None;

    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        // Bytes that are not UTF-8 make a line that does not parse, not a
        // reason to stop serving.
        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut ask = None;
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => {
                let messages = match &request {
                    Value::Array(batch) => batch.iter().collect(),
                    message => vec![message],
                };
                for message in messages {
                    ask = roots.ask(message).or(ask);
                    let Some(dir) = roots.project(message) else {
                        continue;
                    };
                    if dir != config.root
                        && let Ok(fresh) = Config::load(&dir)
                    {
                        config = fresh;
                        session = None;
                        tried = None;
                    }
                    if dir == config.root && config.data_dir().is_dir() {
                        watch(config.clone());
                        roots.follow = false;
                    }
                }
                hold_the_index(&mut session, &mut tried, &mut config);
                reply(&config, session.as_ref(), &request)
            }
            Err(_) => Some(error(Value::Null, -32700, "parse error")),
        };
        for message in response.into_iter().chain(ask) {
            writeln!(writer, "{}", serde_json::to_string(&message)?)?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// The client's workspace roots, for a server that follows them. A client
/// that offers them in `initialize` is asked once it is initialized and again
/// on each change, and only the answer to the latest question counts.
#[derive(Default)]
struct Roots {
    follow: bool,
    offered: bool,
    asked: u64,
}

impl Roots {
    /// The `roots/list` request this message calls for.
    fn ask(&mut self, message: &Value) -> Option<Value> {
        match message.get("method").and_then(Value::as_str)? {
            "initialize" => {
                self.offered = message
                    .get("params")
                    .and_then(|p| p.get("capabilities"))
                    .and_then(|c| c.get("roots"))
                    .is_some_and(|r| !r.is_null());
                None
            }
            "notifications/initialized" | "notifications/roots/list_changed"
                if self.follow && self.offered =>
            {
                self.asked += 1;
                Some(json!({ "jsonrpc": "2.0", "id": self.asked, "method": "roots/list" }))
            }
            _ => None,
        }
    }

    /// The project the first local root is in, from the answer to the latest
    /// `roots/list`: the nearest directory at or above it with a `.ripsem`
    /// directory, or the root itself.
    fn project(&self, message: &Value) -> Option<PathBuf> {
        let ours = self.follow && self.asked > 0 && message.get("method").is_none();
        if !ours || *message.get("id")? != self.asked {
            return None;
        }
        let dir = message
            .get("result")?
            .get("roots")?
            .as_array()?
            .iter()
            .filter_map(|root| file_uri_path(root.get("uri")?.as_str()?))
            .find_map(|path| {
                path.canonicalize()
                    .ok()
                    .filter(|p| p.is_dir() && !is_network(p))
            })?;
        Some(Config::find_root(&dir))
    }
}

/// The local path a `file://` URI names, percent-decoded, with the slash in
/// front of a Windows drive dropped. A URI with a host other than `localhost`
/// names a network share and is not followed.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file://"))
        .map(|_| &uri[7..])?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let mut bytes = Vec::with_capacity(rest.len());
    let mut raw = rest.bytes();
    while let Some(b) = raw.next() {
        if b == b'%' {
            let hex = [raw.next()?, raw.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    let drive = matches!(path.as_bytes(), [b'/', letter, b':', ..] if letter.is_ascii_alphabetic());
    let path = PathBuf::from(if cfg!(windows) && drive {
        &path[1..]
    } else {
        &path
    });
    (!is_network(&path)).then_some(path)
}

/// Keeps one searcher open across requests, taking it as soon as an index
/// exists. The server often starts before the first `ripsem index` finishes;
/// once it has the index it only re-reads the file when it changes.
///
/// `tried` holds the mtimes of the index and the config at the last failed
/// attempt. Opening decodes the whole file and can fail for a lasting reason
/// (another model, an older format), so a retry waits for either to change.
///
/// `ripsem model` followed by `ripsem index --force` rebuilds the index for
/// another model while the server runs. The held index cannot take it, so the
/// configuration is read again, and when it names another model or backend the
/// server opens the new index under it.
fn hold_the_index(
    session: &mut Option<pipeline::Searcher>,
    tried: &mut Option<[Option<std::time::SystemTime>; 2]>,
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
    let mtime = |path: PathBuf| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let stamps = [mtime(config.index_path()), mtime(config.config_path())];
    if stamps[0].is_none() || Some(stamps) == *tried {
        return;
    }
    *tried = Some(stamps);
    if let Ok(fresh) = Config::load(&config.root) {
        *config = fresh;
    }
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

/// A message without a method is a response, which [`Roots`] takes, or
/// malformed. A malformed one is answered as an invalid request rather than
/// left for the client to wait on.
fn handle(config: &Config, session: Option<&pipeline::Searcher>, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        let response = request.get("result").is_some() || request.get("error").is_some();
        return (!response).then(|| error(id.unwrap_or(Value::Null), -32600, "invalid request"));
    };

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
                "serverInfo": { "name": "ripsem", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS.replace('\n', " ")
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
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "description": format!("Maximum number of results (default {DEFAULT_LIMIT}, at most {MAX_LIMIT}).") },
                "min_score": { "type": "number", "minimum": 0, "maximum": 1, "description": "Drop hits scoring below this (0–1). 0 is no closer than unrelated text in this repository, 1 is every query term found literally; scores fall with rank, so this keeps the top results. Default: no filter." }
            },
            "required": ["query"],
            "additionalProperties": false
        }
    })
}

fn handle_call(
    config: &Config,
    session: Option<&pipeline::Searcher>,
    id: Value,
    request: &Value,
) -> Value {
    match request
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
    {
        Some("search") => {}
        Some(other) => {
            let message = format!("unknown tool '{other}'; the only tool is 'search'");
            return error(id, -32602, &message);
        }
        None => return error(id, -32602, "missing tool name"),
    }

    let args = request
        .get("params")
        .and_then(|p| p.get("arguments"))
        .filter(|a| !a.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Call {
        query,
        limit,
        min_score,
        prefix,
    } = match parse_call(&args, &config.root) {
        Ok(call) => call,
        Err(e) => return failed(id, &e),
    };

    let result = match session {
        Some(s) => s.query_scoped(query, limit, prefix.as_deref()),
        None if crate::watch::indexing() => Err(anyhow!(
            "the index is being built in the background; search again shortly"
        )),
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

const ARGUMENTS: [&str; 4] = ["query", "path", "limit", "min_score"];
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 50;

struct Call<'a> {
    query: &'a str,
    limit: usize,
    min_score: Option<f32>,
    prefix: Option<String>,
}

/// Arguments that do not fit the input schema are a tool error, which the
/// model sees and can correct, rather than a protocol error. A `null`
/// argument is one left out.
fn parse_call<'a>(args: &'a Value, root: &Path) -> Result<Call<'a>> {
    let args = args
        .as_object()
        .ok_or_else(|| anyhow!("the arguments must be an object with a 'query'"))?;
    if let Some(name) = args.keys().find(|k| !ARGUMENTS.contains(&k.as_str())) {
        bail!("unknown argument '{name}'; the arguments are query, path, limit and min_score");
    }
    let given = |name: &str| args.get(name).filter(|v| !v.is_null());

    let query = match given("query") {
        None => bail!("missing 'query'"),
        Some(Value::String(q)) if q.trim().is_empty() => bail!("'query' is empty"),
        Some(Value::String(q)) => q.as_str(),
        Some(other) => bail!("'query' must be a string, not {other}"),
    };
    let limit = match given("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as u64))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| {
                anyhow!("'limit' must be a whole number from 1 to {MAX_LIMIT}, not {v}")
            })?,
    };
    let min_score = match given("min_score") {
        None => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|s| (0.0..=1.0).contains(s))
                .ok_or_else(|| anyhow!("'min_score' must be a number from 0 to 1, not {v}"))?
                as f32,
        ),
    };
    let prefix = match given("path") {
        None => None,
        Some(Value::String(raw)) => scope_prefix(raw, root)?,
        Some(other) => bail!("'path' must be a string, not {other}"),
    };
    Ok(Call {
        query,
        limit,
        min_score,
        prefix,
    })
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
            // Resolving a network path connects to its host, and the path
            // comes from an agent that reads untrusted text.
            Err(_) if is_network(candidate) => return Err(outside()),
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

fn is_network(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(
            prefix.kind(),
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(_)
        )
    )
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

    /// Claude Code defers tool descriptions behind its tool search, so the
    /// instructions are what tells the model the tool is there.
    #[test]
    fn initialize_tells_the_agent_when_to_search() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("initialize", json!({}))).unwrap();
        let text = resp["result"]["instructions"].as_str().unwrap();
        assert!(text.contains("call the `search` tool"), "got: {text}");
        assert!(!text.contains('\n'), "got: {text}");
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
    fn call_with_an_empty_query_is_a_tool_error() {
        let cfg = Config::default();
        let resp = handle(
            &cfg,
            None,
            &req(
                "tools/call",
                json!({ "name": "search", "arguments": { "query": " " } }),
            ),
        )
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("'query' is empty"), "got: {text}");
    }

    fn tool_error(arguments: Value) -> String {
        let cfg = Config::default();
        let resp = handle(
            &cfg,
            None,
            &req(
                "tools/call",
                json!({ "name": "search", "arguments": arguments }),
            ),
        )
        .unwrap();
        assert_eq!(resp["result"]["isError"], true, "{arguments}: {resp}");
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_limit_outside_the_schema_is_a_tool_error() {
        for limit in [
            json!(-1),
            json!(0),
            json!(51),
            json!(1e12),
            json!(2.5),
            json!("10"),
        ] {
            let text = tool_error(json!({ "query": "x", "limit": limit }));
            assert!(
                text.contains("'limit' must be a whole number from 1 to 50"),
                "{limit}: {text}"
            );
        }
    }

    #[test]
    fn a_limit_inside_the_schema_is_taken() {
        let root = Path::new("/repo");
        let limit = |args: Value| parse_call(&args, root).unwrap().limit;
        assert_eq!(limit(json!({ "query": "x" })), 5);
        assert_eq!(limit(json!({ "query": "x", "limit": null })), 5);
        assert_eq!(limit(json!({ "query": "x", "limit": 1 })), 1);
        assert_eq!(limit(json!({ "query": "x", "limit": 50 })), 50);
        assert_eq!(limit(json!({ "query": "x", "limit": 8.0 })), 8);
    }

    #[test]
    fn a_min_score_outside_the_schema_is_a_tool_error() {
        for min_score in [json!("high"), json!(true), json!(-0.1), json!(1.5)] {
            let text = tool_error(json!({ "query": "x", "min_score": min_score }));
            assert!(
                text.contains("'min_score' must be a number from 0 to 1"),
                "{min_score}: {text}"
            );
        }
    }

    #[test]
    fn arguments_of_the_wrong_type_are_a_tool_error() {
        let text = tool_error(json!({ "query": 7 }));
        assert!(text.contains("'query' must be a string"), "got: {text}");
        let text = tool_error(json!({ "query": "x", "path": ["src"] }));
        assert!(text.contains("'path' must be a string"), "got: {text}");
        let text = tool_error(json!("find the parser"));
        assert!(text.contains("must be an object"), "got: {text}");
    }

    /// An argument the schema does not name would otherwise be dropped, and
    /// the model would take the default for what it asked.
    #[test]
    fn an_unknown_argument_is_a_tool_error() {
        let text = tool_error(json!({ "query": "x", "top_k": 20 }));
        assert!(text.contains("unknown argument 'top_k'"), "got: {text}");
    }

    #[test]
    fn the_schema_states_the_bounds_it_enforces() {
        let schema = &tool_schema()["inputSchema"];
        assert_eq!(schema["properties"]["limit"]["minimum"], 1);
        assert_eq!(schema["properties"]["limit"]["maximum"], MAX_LIMIT);
        assert_eq!(schema["properties"]["min_score"]["minimum"], 0);
        assert_eq!(schema["properties"]["min_score"]["maximum"], 1);
        assert_eq!(schema["additionalProperties"], false);
        let named: Vec<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(named.len(), ARGUMENTS.len());
        assert!(ARGUMENTS.iter().all(|a| named.contains(a)));
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
        let message = resp["error"]["message"].as_str().unwrap();
        assert!(message.contains("unknown tool 'other'"), "got: {message}");
        assert!(message.contains("'search'"), "got: {message}");
    }

    #[test]
    fn a_message_without_a_method_is_an_invalid_request() {
        let cfg = Config::default();
        for message in [
            json!({ "jsonrpc": "2.0", "id": 3 }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": 7 }),
            json!(5),
        ] {
            let resp = handle(&cfg, None, &message).unwrap();
            assert_eq!(resp["error"]["code"], -32600, "{message}");
        }
        assert_eq!(
            handle(&cfg, None, &json!({ "jsonrpc": "2.0", "id": 3 })).unwrap()["id"],
            3
        );
        let response = json!({ "jsonrpc": "2.0", "id": 9, "result": {} });
        assert!(handle(&cfg, None, &response).is_none());
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

    #[cfg(windows)]
    #[test]
    fn a_network_scope_is_refused_without_resolving_it() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        for raw in [r"\\host.invalid\share\src", "//host.invalid/share/src"] {
            let err = scope_prefix(raw, &root).unwrap_err();
            assert!(err.to_string().contains("outside"), "{raw}: {err}");
        }
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
        config.save().unwrap();

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
        config.save().unwrap();

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

    /// `ripsem model set` and a first `ripsem index` after the server started
    /// with no index at all.
    #[test]
    fn a_server_without_an_index_reads_the_config_again() {
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

        let mut session = None;
        let mut tried = None;
        hold_the_index(&mut session, &mut tried, &mut config);
        assert!(session.is_none());

        let mut switched = config.clone();
        switched.model = "all-MiniLM-L6-v2".into();
        switched.save().unwrap();
        pipeline::index_repo(&switched, false).unwrap();

        hold_the_index(&mut session, &mut tried, &mut config);
        let searcher = session.as_ref().expect("the new index is held");
        assert_eq!(searcher.model(), "all-MiniLM-L6-v2");
    }

    #[test]
    fn a_line_that_is_not_json_is_a_parse_error() {
        let input = b"{not json\n\xff\xfe\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let dir = crate::testutil::TempDir::new();
        let config = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut out = Vec::new();
        serve_on(config, false, |_| {}, &input[..], &mut out).unwrap();
        let replies: Vec<Value> = out
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[1]["error"]["code"], -32700);
        assert_eq!(replies[2]["id"], 1);
    }

    fn file_uri(path: &Path) -> String {
        let path = path
            .to_string_lossy()
            .replace('\\', "/")
            .replace(' ', "%20");
        let path = path.strip_prefix("//?/").unwrap_or(&path);
        match path.starts_with('/') {
            true => format!("file://{path}"),
            false => format!("file:///{path}"),
        }
    }

    /// Feeds the lines to a server started in `start`, and returns what it
    /// wrote and the roots it started keeping current.
    fn converse(start: &Path, explicit: bool, lines: &[Value]) -> (Vec<Value>, Vec<PathBuf>) {
        let watched = std::cell::RefCell::new(Vec::new());
        let input: String = lines.iter().map(|line| format!("{line}\n")).collect();
        let mut out = Vec::new();
        serve_on(
            Config::load(start).unwrap(),
            explicit,
            |config: Config| watched.borrow_mut().push(config.root),
            input.as_bytes(),
            &mut out,
        )
        .unwrap();
        let written = out
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        (written, watched.into_inner())
    }

    fn indexed_project() -> crate::testutil::TempDir {
        let dir = crate::testutil::TempDir::new();
        dir.write(
            "my src/auth.rs",
            "fn authenticate(user) { verify password }",
        );
        let config = Config {
            backend: crate::config::Backend::Hash,
            root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        config.save().unwrap();
        pipeline::index_repo(&config, false).unwrap();
        dir
    }

    fn roots_answer(id: u64, uris: &[String]) -> Value {
        let roots: Vec<Value> = uris.iter().map(|uri| json!({ "uri": uri })).collect();
        json!({ "jsonrpc": "2.0", "id": id, "result": { "roots": roots } })
    }

    fn opening(capabilities: Value) -> [Value; 2] {
        [
            req("initialize", json!({ "capabilities": capabilities })),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        ]
    }

    fn search_call() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "search", "arguments": { "query": "verify password" } }
        })
    }

    fn roots_requests(written: &[Value]) -> Vec<Value> {
        written
            .iter()
            .filter(|m| m["method"] == "roots/list")
            .map(|m| m["id"].clone())
            .collect()
    }

    /// A shared client starts the server in the home directory and tells it
    /// the project through its roots.
    #[test]
    fn a_server_started_outside_a_project_follows_the_client_roots() {
        let home = crate::testutil::TempDir::new();
        let elsewhere = crate::testutil::TempDir::new();
        let project = indexed_project();
        let mut lines = opening(json!({ "roots": { "listChanged": true } })).to_vec();
        lines.extend([
            roots_answer(1, &[file_uri(elsewhere.path())]),
            json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed" }),
            roots_answer(
                2,
                &[
                    "https://example.com/repo".into(),
                    file_uri(&project.path().join("my src")),
                ],
            ),
            search_call(),
        ]);

        let (written, watched) = converse(home.path(), false, &lines);
        assert_eq!(roots_requests(&written), [json!(1), json!(2)]);
        let found = written.last().unwrap();
        assert_eq!(found["id"], 7);
        let text = found["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("my src/auth.rs"), "got: {found}");
        assert_eq!(watched, [project.path().canonicalize().unwrap()]);
    }

    /// Hands the server one line per read, running each line's step first,
    /// so the files can change between two messages.
    struct Steps(std::collections::VecDeque<(Value, Box<dyn FnOnce()>)>);

    impl std::io::Read for Steps {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some((line, step)) = self.0.pop_front() else {
                return Ok(0);
            };
            step();
            let bytes = format!("{line}\n").into_bytes();
            buf[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }

    /// The user runs `ripsem init` in the project the server has already moved
    /// to, and the next change of roots starts keeping it current.
    #[test]
    fn a_followed_project_initialised_later_is_kept_current() {
        let home = crate::testutil::TempDir::new();
        let project = crate::testutil::TempDir::new();
        let root = project.path().canonicalize().unwrap();
        let init = root.clone();
        let nothing = || -> Box<dyn FnOnce()> { Box::new(|| {}) };
        let [initialize, initialized] = opening(json!({ "roots": {} }));
        let steps = Steps(
            [
                (initialize, nothing()),
                (initialized, nothing()),
                (roots_answer(1, &[file_uri(&root)]), nothing()),
                (
                    json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed" }),
                    Box::new(move || {
                        let config = Config {
                            root: init,
                            ..Default::default()
                        };
                        config.save().unwrap();
                    }) as Box<dyn FnOnce()>,
                ),
                (roots_answer(2, &[file_uri(&root)]), nothing()),
            ]
            .into(),
        );
        let watched = std::cell::RefCell::new(Vec::new());
        let mut out = Vec::new();
        serve_on(
            Config::load(home.path()).unwrap(),
            false,
            |config: Config| watched.borrow_mut().push(config.root),
            std::io::BufReader::new(steps),
            &mut out,
        )
        .unwrap();
        assert_eq!(watched.into_inner(), [root]);
    }

    #[test]
    fn a_server_given_a_project_does_not_ask_for_roots() {
        let elsewhere = crate::testutil::TempDir::new();
        let project = indexed_project();
        let mut lines = opening(json!({ "roots": {} })).to_vec();
        lines.push(roots_answer(1, &[file_uri(elsewhere.path())]));
        lines.push(search_call());

        let (written, watched) = converse(project.path(), true, &lines);
        assert!(roots_requests(&written).is_empty(), "got: {written:?}");
        let text = written.last().unwrap()["result"]["content"][0]["text"].clone();
        assert!(text.as_str().unwrap().contains("auth.rs"), "got: {text}");
        assert_eq!(watched, [project.path()]);

        let (written, _) = converse(elsewhere.path(), true, &lines);
        assert!(roots_requests(&written).is_empty(), "got: {written:?}");

        let (written, watched) = converse(project.path(), false, &lines);
        assert!(roots_requests(&written).is_empty(), "got: {written:?}");
        assert_eq!(watched, [project.path()]);
    }

    #[test]
    fn a_client_without_roots_is_not_asked_for_them() {
        let home = crate::testutil::TempDir::new();
        let (written, watched) = converse(home.path(), false, &opening(json!({})));
        assert_eq!(written.len(), 1, "got: {written:?}");
        assert!(roots_requests(&written).is_empty());
        assert!(watched.is_empty());
    }

    #[test]
    fn a_file_uri_names_a_local_path() {
        let path = |uri: &str| file_uri_path(uri);
        assert_eq!(
            path("file:///home/me/my%20repo"),
            Some(PathBuf::from("/home/me/my repo"))
        );
        assert_eq!(
            path("FILE://localhost/srv/x"),
            Some(PathBuf::from("/srv/x"))
        );
        assert_eq!(path("file:///srv/x?y#z"), Some(PathBuf::from("/srv/x")));
        let drive = if cfg!(windows) {
            "c:/Users/me"
        } else {
            "/c:/Users/me"
        };
        assert_eq!(path("file:///c%3A/Users/me"), Some(PathBuf::from(drive)));
        assert_eq!(path("file://server/share"), None);
        assert_eq!(path("file:///srv/%2"), None);
        assert_eq!(path("https://example.com/repo"), None);
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
