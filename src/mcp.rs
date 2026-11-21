use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf, Prefix};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::config::Config;
use crate::pipeline;
use crate::scope::Scope;

/// Protocol revisions this server speaks, newest first. Between them they
/// change transports, authorization and client features, and add optional
/// tool fields (structured output, titles, annotations, icons). Icons stay out.
/// Structured output goes to the revisions that define it, see
/// [`STRUCTURED_OUTPUT`].
/// A title and annotations are sent, because a client that is told nothing
/// about a tool asks the user before every call. They sit in fields a client
/// of the 2024-11-05 revision does not know and ignores, and nothing in the
/// tool depends on a client reading them. The one difference that reaches a
/// stdio server is that 2025-03-26 lets the client send JSON-RPC batches,
/// which [`reply`] accepts under any revision.
///
/// 2026-07-28 drops `initialize` for per-request metadata. A client of that
/// revision probes with `server/discover` first, and the method-not-found
/// answer tells it to fall back to `initialize`.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The first revision with `outputSchema` and `structuredContent`. Older
/// revisions never see either.
const STRUCTURED_OUTPUT: &str = "2025-06-18";

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
    let mut revision = PROTOCOL_VERSIONS[0];

    let mut line = Vec::new();
    while let Some(cut) = read_request(&mut reader, &mut line, MAX_REQUEST_BYTES)? {
        if cut {
            let refused = error(Value::Null, -32600, "request is larger than 4 MiB");
            writeln!(writer, "{}", serde_json::to_string(&refused)?)?;
            writer.flush()?;
            continue;
        }
        // Bytes that are not UTF-8 make a line that does not parse, not a
        // reason to stop serving.
        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut ask = None;
        let mut response = match serde_json::from_str::<Value>(trimmed) {
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
                        && let Ok(fresh) = Config::load_again(&dir, &config)
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
        if let Some(agreed) = response
            .as_ref()
            .and_then(|r| r.pointer("/result/protocolVersion"))
            .and_then(Value::as_str)
        {
            revision = negotiate(Some(agreed));
        }
        if let Some(message) = response.as_mut() {
            fit_revision(revision, message);
        }
        for message in response.into_iter().chain(ask) {
            writeln!(writer, "{}", serde_json::to_string(&message)?)?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// The largest request line the server reads. A search is a short question, so
/// anything longer is a mistake or a flood, and holding it whole would let one
/// line take as much memory as the client cares to send.
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

/// Reads one line into `line`, keeping at most `limit` bytes of it. The rest of
/// a longer line is read and dropped, so the line after it still starts where
/// it should. `None` is the end of the input; otherwise the flag says whether
/// the line was cut short.
fn read_request(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut cut = false;
    let mut any = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(any.then_some(cut));
        }
        any = true;
        let (taken, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        let room = limit - line.len();
        if taken > room {
            line.extend_from_slice(&buf[..room]);
            cut = true;
        } else {
            line.extend_from_slice(&buf[..taken]);
        }
        reader.consume(taken);
        if done {
            return Ok(Some(cut));
        }
    }
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
        match Config::load_again(&config.root, config) {
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
    if let Ok(fresh) = Config::load_again(&config.root, config) {
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

/// Takes out what the negotiated revision predates: `outputSchema` on a tool
/// and `structuredContent` on a result, both added in 2025-06-18.
fn fit_revision(revision: &str, message: &mut Value) {
    if revision >= STRUCTURED_OUTPUT {
        return;
    }
    if let Value::Array(batch) = message {
        for item in batch {
            fit_revision(revision, item);
        }
        return;
    }
    if let Some(tools) = message
        .pointer_mut("/result/tools")
        .and_then(Value::as_array_mut)
    {
        for tool in tools {
            if let Some(tool) = tool.as_object_mut() {
                tool.remove("outputSchema");
            }
        }
    }
    if let Some(result) = message.get_mut("result").and_then(Value::as_object_mut) {
        result.remove("structuredContent");
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
    if !id
        .as_ref()
        .is_none_or(|id| id.is_string() || id.is_i64() || id.is_u64())
    {
        return Some(error(Value::Null, -32600, "invalid request"));
    }
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error(id.unwrap_or(Value::Null), -32600, "invalid request"));
    }

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
        "tools/list" => Some(ok(
            id?,
            json!({ "tools": [tool_schema(), similar_schema(), outline_schema(), status_schema()] }),
        )),
        "tools/call" => Some(handle_call(config, session, id?, request)),
        "ping" => Some(ok(id?, json!({}))),
        _ if id.is_some() => Some(error(id.unwrap(), -32601, "method not found")),
        _ => None,
    }
}

/// Hints for a client that a tool changes nothing the user owns. `search`
/// refreshes ripsem's own index cache under `.ripsem/` before it answers,
/// which is the tool's cache and not the user's files.
fn read_only(title: &str) -> Value {
    json!({
        "title": title,
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false
    })
}

fn tool_schema() -> Value {
    json!({
        "name": "search",
        "title": "Search the code",
        "annotations": read_only("Search the code"),
        "outputSchema": hits_schema(),
        "description": "Find code in this repository. Handles both exact terms (identifiers, error strings, constants) and plain-language descriptions of behaviour in the same call, and returns ranked snippets with file paths and line ranges. Prefer this over grepping and reading whole files: it costs one call instead of guessing at the author's wording. To find the other places that do what one hit does, call `find_similar` on that hit instead.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What you are looking for: an identifier, an error message, or a description of the behaviour." },
                "path": { "type": "string", "description": "Optional subdirectory (relative to the repo) to limit results to, e.g. 'src/embed'." },
                "lang": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Only files of these languages or extensions, e.g. 'rust' or ['py', 'ts']; a '!' before a name leaves it out, e.g. '!json'." },
                "glob": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Only paths matching these globs; a leading ! excludes, e.g. ['*.rs', '!*_test.rs']." },
                "regex": { "type": "string", "description": format!("Only hits whose code matches this regular expression (Rust regex syntax, matched against the whole chunk, so `(?i)` makes it case-insensitive), for \"the place that does X and mentions foo\". Looked for among the {} best-ranked places, so it narrows the answer rather than searching for the pattern; use grep when every occurrence is needed.", pipeline::TEXT_DEPTH) },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "default": DEFAULT_LIMIT, "description": format!("Maximum number of results (default {DEFAULT_LIMIT}, at most {MAX_LIMIT}).") },
                "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Results to skip, taken from the ranking after every filter, to read the next page: when more follow, the result ends with the offset to pass. Default 0." },
                "min_score": { "type": "number", "minimum": 0, "maximum": 1, "description": "Drop hits scoring below this (0–1). 0 is no closer than unrelated text in this repository, 1 is every query term found literally; scores fall with rank, so this keeps the top results. Default: no filter." },
                "since": { "type": "string", "description": "Only hits in files that differ from this git revision, as the working tree has them: committed, staged and unstaged changes and untracked files. Use 'main' to review a branch. Needs a git repository." }
            },
            "required": ["query"],
            "additionalProperties": false
        }
    })
}

/// The shape of the hits a search or a similarity query returns as structured
/// content.
fn hits_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "hits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "start_line": { "type": "integer", "minimum": 1 },
                        "end_line": { "type": "integer", "minimum": 1 },
                        "score": { "type": "number" },
                        "symbol": { "type": ["string", "null"] },
                        "snippet": { "type": "string" }
                    },
                    "required": ["path", "start_line", "end_line", "score", "symbol", "snippet"]
                }
            },
            "next_offset": { "type": ["integer", "null"], "minimum": 0 }
        },
        "required": ["hits", "next_offset"]
    })
}

fn similar_schema() -> Value {
    let search = tool_schema();
    let mut properties = json!({
        "path": {
            "type": "string",
            "description": "The file to look from, relative to the repository, e.g. 'src/auth.rs'."
        },
        "line": {
            "type": "integer",
            "minimum": 1,
            "description": "First line of the place to look from, 1-based. Absent, the whole file."
        },
        "end_line": {
            "type": "integer",
            "minimum": 1,
            "description": "Last line of that place, inclusive. Default: the same as `line`."
        }
    });
    for key in [
        "lang",
        "glob",
        "regex",
        "limit",
        "offset",
        "min_score",
        "since",
    ] {
        properties[key] = search["inputSchema"]["properties"][key].clone();
    }
    json!({
        "name": "find_similar",
        "title": "Find similar code",
        "annotations": read_only("Find similar code"),
        "outputSchema": hits_schema(),
        "description": "Find the code in this repository that does what a given place does: give a file, and optionally a line range, and get the nearest places back as ranked snippets. Reach for it after `search` (or a read) has found one relevant place, to find the other places that do the same thing, such as a second handler or a copied check. For a question with no place in hand yet, use `search`.",
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

fn outline_schema() -> Value {
    let declaration = json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string" },
            "symbol": { "type": "string" },
            "start_line": { "type": "integer", "minimum": 1 },
            "end_line": { "type": "integer", "minimum": 1 }
        },
        "required": ["kind", "symbol", "start_line", "end_line"]
    });
    json!({
        "name": "outline",
        "title": "Outline a file",
        "annotations": read_only("Outline a file"),
        "outputSchema": {
            "type": "object",
            "properties": {
                "declarations": { "type": "array", "items": declaration }
            },
            "required": ["declarations"]
        },
        "description": "List the declarations of one file in this repository, in the order they start: each one's kind, name and line range, such as `fn Session::refresh` at 40-72. Call it before reading a long file, to see its layout and then read only the lines that matter. It lists what the index holds, so a file the index leaves out, such as a binary or minified one, is a tool error.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "The file, relative to the repository, e.g. 'src/auth.rs'." }
            },
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

fn status_schema() -> Value {
    json!({
        "name": "index_status",
        "title": "Index status",
        "annotations": read_only("Index status"),
        "outputSchema": {
            "type": "object",
            "properties": {
                "schema": { "type": "string" },
                "updating": { "type": "boolean" }
            },
            "required": ["schema", "updating"]
        },
        "description": "Report whether this repository's search index exists, how many files and chunks it holds, how many files were added, changed or removed since it was built, and whether it is being updated right now. Call it when `search` says there is no index or finds nothing you expect.",
        "inputSchema": {
            "type": "object",
            "properties": {},
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
    let name = match request.get("params").and_then(|p| p.get("name")) {
        None | Some(Value::Null) => return error(id, -32602, "missing tool name"),
        Some(Value::String(name)) => name.as_str(),
        Some(other) => {
            let message = format!("'name' must be a string, not {other}");
            return error(id, -32602, &message);
        }
    };
    match name {
        "search" => {}
        "find_similar" => {
            return find_similar(config, session, id, &call_arguments(request));
        }
        "outline" => return outline(config, id, &call_arguments(request)),
        "index_status" => return index_status(config, id, &call_arguments(request)),
        other => {
            let message = format!(
                "unknown tool '{other}'; the tools are 'search', 'find_similar', 'outline' and 'index_status'"
            );
            return error(id, -32602, &message);
        }
    }

    let args = call_arguments(request);
    let Call {
        query,
        limit,
        offset,
        min_score,
        scope,
        text,
    } = match parse_call(&args, &config.root) {
        Ok(call) => call,
        Err(e) => return failed(id, &e),
    };

    let result = match session {
        Some(s) => pipeline::search_answer(
            config,
            s,
            query,
            limit,
            offset,
            scope.as_ref(),
            text.as_ref(),
        ),
        None if crate::watch::indexing() => Err(anyhow!(
            "the index is being built in the background; search again shortly"
        )),
        None => pipeline::open(config).and_then(|s| {
            pipeline::search_answer(
                config,
                &s,
                query,
                limit,
                offset,
                scope.as_ref(),
                text.as_ref(),
            )
        }),
    };

    match result {
        Ok(page) => ok(id, search_result(page, min_score, offset)),
        Err(e) => failed(id, &e),
    }
}

/// What one page of places answers: its text, where the next page starts, and
/// the same hits as data for a revision that has structured output.
fn search_result(page: pipeline::Page, min_score: Option<f32>, offset: usize) -> Value {
    let more = page.more(min_score);
    let found = page.hits.len();
    let hits = apply_min_score(page.hits, min_score);
    let mut result = match min_score {
        Some(min) if hits.is_empty() && found > 0 => below_threshold(min, found),
        _ if hits.is_empty() && offset > 0 => nothing_further(offset),
        _ => tool_result(&hits),
    };
    let next_offset = more.then_some(offset + hits.len());
    if let Some(next) = next_offset {
        say_more(&mut result, next);
    }
    if let Some(object) = result.as_object_mut() {
        object.insert("structuredContent".into(), structured(&hits, next_offset));
    }
    result
}

/// `find_similar`: the places nearest to a place already in the index. The
/// searcher answers it through the same function the CLI's `similar` uses.
fn find_similar(
    config: &Config,
    session: Option<&pipeline::Searcher>,
    id: Value,
    args: &Value,
) -> Value {
    let similar = match parse_similar(args, &config.root) {
        Ok(similar) => similar,
        Err(e) => return tool_error(id, &e),
    };
    let Similar {
        target,
        filters:
            Filters {
                limit,
                offset,
                min_score,
                text,
                ..
            },
        scope,
    } = similar;
    let found = match session {
        Some(s) => pipeline::similar_answer(
            config,
            s,
            &target,
            limit,
            offset,
            scope.as_ref(),
            text.as_ref(),
        ),
        None if crate::watch::indexing() => Err(anyhow!(
            "the index is being built in the background; search again shortly"
        )),
        None => pipeline::open(config).and_then(|s| {
            pipeline::similar_answer(
                config,
                &s,
                &target,
                limit,
                offset,
                scope.as_ref(),
                text.as_ref(),
            )
        }),
    };
    match found {
        Ok(page) => ok(id, search_result(page, min_score, offset)),
        Err(e) => tool_error(id, &e),
    }
}

/// A failure the model should read as its own words, as the CLI prints them.
fn tool_error(id: Value, e: &anyhow::Error) -> Value {
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": format!("{e}") }],
            "isError": true
        }),
    )
}

/// The arguments of `find_similar`: the place to look from, and the filters
/// `search` takes. `scope` holds the language and glob filters.
struct Similar {
    target: pipeline::Target,
    filters: Filters,
    scope: Option<Scope>,
}

const SIMILAR_ARGUMENTS: [&str; 10] = [
    "path",
    "line",
    "end_line",
    "lang",
    "glob",
    "regex",
    "limit",
    "offset",
    "min_score",
    "since",
];

fn parse_similar(args: &Value, root: &Path) -> Result<Similar> {
    let args = args
        .as_object()
        .ok_or_else(|| anyhow!("the arguments must be an object with a 'path'"))?;
    if let Some(name) = args
        .keys()
        .find(|k| !SIMILAR_ARGUMENTS.contains(&k.as_str()))
    {
        bail!(
            "unknown argument '{name}'; the arguments are path, line, end_line, lang, glob, regex, limit, offset, min_score and since"
        );
    }
    let given = |name: &str| args.get(name).filter(|v| !v.is_null());

    let raw = match given("path") {
        None => bail!("missing 'path'"),
        Some(Value::String(p)) => p.as_str(),
        Some(other) => bail!("'path' must be a string, not {other}"),
    };
    let path = scope_prefix(raw, root)?.ok_or_else(|| anyhow!("'{raw}' names no file"))?;
    let line = |name: &str| -> Result<Option<usize>> {
        match given(name) {
            None => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n >= 1)
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or_else(|| anyhow!("'{name}' must be a whole number from 1, not {v}")),
        }
    };
    let lines = match (line("line")?, line("end_line")?) {
        (None, None) => None,
        (None, Some(_)) => bail!("'end_line' needs 'line'"),
        (Some(first), last) => {
            let last = last.unwrap_or(first);
            let raw = format!("{raw}:{first}-{last}");
            Some(crate::cli::line_range(&raw, first, last)?)
        }
    };
    let filters = filters(&given)?;
    let scope = scope_of(
        root,
        None,
        &filters.langs,
        &filters.globs,
        filters.since.as_deref(),
    )?;
    Ok(Similar {
        target: pipeline::Target { path, lines },
        filters,
        scope,
    })
}

/// An offset past the last hit the search ranks is an answer, not a mistake:
/// an agent that pages until it runs out should be told that it has.
fn nothing_further(offset: usize) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": format!("No further results: offset {offset} is past the last hit this search ranks.")
        }],
        "isError": false
    })
}

/// Ends the text of a result with where the next page starts.
fn say_more(result: &mut Value, next_offset: usize) {
    if let Some(Value::String(text)) = result.pointer_mut("/content/0/text") {
        text.push_str(&format!(
            "\n\nMore results below this; pass offset={next_offset} to see them."
        ));
    }
}

/// The `arguments` of a `tools/call`; a missing or `null` one is no arguments.
fn call_arguments(request: &Value) -> Value {
    request
        .get("params")
        .and_then(|p| p.get("arguments"))
        .filter(|a| !a.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// `outline`: the declarations of one file the index holds, as `ripsem outline`
/// lists them.
fn outline(config: &Config, id: Value, args: &Value) -> Value {
    let path = match parse_outline(args, &config.root) {
        Ok(path) => path,
        Err(e) => return tool_error(id, &e),
    };
    if crate::watch::indexing() {
        let e = anyhow!("the index is being built in the background; search again shortly");
        return tool_error(id, &e);
    }
    match pipeline::outline(config, &path) {
        Ok(declarations) => {
            let text = if declarations.is_empty() {
                format!("no declarations in {path}")
            } else {
                declarations
                    .iter()
                    .map(|d| format!("{}-{}  {}", d.start_line, d.end_line, d.symbol.label()))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let rows: Vec<Value> = declarations
                .iter()
                .map(crate::cli::declaration_row)
                .collect();
            ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                    "structuredContent": { "declarations": rows }
                }),
            )
        }
        Err(e) => tool_error(id, &e),
    }
}

/// The file named by the arguments of `outline`, as an index path.
fn parse_outline(args: &Value, root: &Path) -> Result<String> {
    let args = args
        .as_object()
        .ok_or_else(|| anyhow!("the arguments must be an object with a 'path'"))?;
    if let Some(name) = args.keys().find(|k| k.as_str() != "path") {
        bail!("unknown argument '{name}'; the only argument is path");
    }
    let raw = match args.get("path").filter(|v| !v.is_null()) {
        None => bail!("missing 'path'"),
        Some(Value::String(p)) => p.as_str(),
        Some(other) => bail!("'path' must be a string, not {other}"),
    };
    let path = scope_prefix(raw, root)?.ok_or_else(|| anyhow!("'{raw}' names no file"))?;
    if root.join(&path).is_dir() {
        bail!("{raw} is a directory; `outline` takes a file");
    }
    Ok(path)
}

/// What `ripsem status --json` prints, plus whether the server is indexing
/// right now. It answers an agent that was told there is no index, or that
/// finds nothing it expects: whether the index exists, how much it holds and
/// how many files have changed since.
fn index_status(config: &Config, id: Value, args: &Value) -> Value {
    let text_result = |text: String, is_error: bool| {
        ok(
            id.clone(),
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
        )
    };
    let Some(args) = args.as_object() else {
        return text_result("the arguments must be an object".into(), true);
    };
    if let Some(name) = args.keys().next() {
        let message = format!("unknown argument '{name}'; this tool takes none");
        return text_result(message, true);
    }
    let mut report = crate::cli::status_report(config);
    report["updating"] = json!(crate::watch::indexing());
    let text = serde_json::to_string_pretty(&report).unwrap_or_default();
    ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
            "structuredContent": report
        }),
    )
}

const ARGUMENTS: [&str; 9] = [
    "query",
    "path",
    "lang",
    "glob",
    "regex",
    "limit",
    "offset",
    "min_score",
    "since",
];
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 50;

struct Call<'a> {
    query: &'a str,
    limit: usize,
    offset: usize,
    min_score: Option<f32>,
    scope: Option<Scope>,
    text: Option<regex::Regex>,
}

/// Arguments that do not fit the input schema are a tool error, which the
/// model sees and can correct, rather than a protocol error. A `null`
/// argument is one left out.
fn parse_call<'a>(args: &'a Value, root: &Path) -> Result<Call<'a>> {
    let args = args
        .as_object()
        .ok_or_else(|| anyhow!("the arguments must be an object with a 'query'"))?;
    if let Some(name) = args.keys().find(|k| !ARGUMENTS.contains(&k.as_str())) {
        bail!(
            "unknown argument '{name}'; the arguments are query, path, lang, glob, regex, limit, offset, min_score and since"
        );
    }
    let given = |name: &str| args.get(name).filter(|v| !v.is_null());

    let query = match given("query") {
        None => bail!("missing 'query'"),
        Some(Value::String(q)) if q.trim().is_empty() => bail!("'query' is empty"),
        Some(Value::String(q)) => q.as_str(),
        Some(other) => bail!("'query' must be a string, not {other}"),
    };
    let Filters {
        limit,
        offset,
        min_score,
        langs,
        globs,
        text,
        since,
    } = filters(&given)?;
    let prefix = match given("path") {
        None => None,
        Some(Value::String(raw)) => scope_prefix(raw, root)?,
        Some(other) => bail!("'path' must be a string, not {other}"),
    };
    Ok(Call {
        query,
        limit,
        offset,
        min_score,
        scope: scope_of(root, prefix.as_deref(), &langs, &globs, since.as_deref())?,
        text,
    })
}

/// The arguments every search-shaped tool takes besides its own: how many
/// places, where to start, which score to keep, and the filters.
struct Filters {
    limit: usize,
    offset: usize,
    min_score: Option<f32>,
    langs: Vec<String>,
    globs: Vec<String>,
    text: Option<regex::Regex>,
    since: Option<String>,
}

/// The scope a call's path, language and glob arguments make, limited further
/// to the files changed since `since` when one is given.
fn scope_of(
    root: &Path,
    prefix: Option<&str>,
    langs: &[String],
    globs: &[String],
    since: Option<&str>,
) -> Result<Option<Scope>> {
    let scope = Scope::new(prefix, langs, globs)?;
    match since {
        Some(rev) => Scope::since_rev(scope, root, rev),
        None => Ok(scope),
    }
}

fn filters<'a>(given: &dyn Fn(&str) -> Option<&'a Value>) -> Result<Filters> {
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
    let offset = match given("offset") {
        None => 0,
        Some(v) => v
            .as_u64()
            .or_else(|| {
                v.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                    .map(|f| f as u64)
            })
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| anyhow!("'offset' must be a whole number, 0 or more, not {v}"))?,
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
    let text = match given("regex") {
        None => None,
        Some(Value::String(pattern)) => Some(crate::scope::text_pattern(pattern)?),
        Some(other) => bail!("'regex' must be a string, not {other}"),
    };
    let since = match given("since") {
        None => None,
        Some(Value::String(rev)) => Some(rev.clone()),
        Some(other) => bail!("'since' must be a string, not {other}"),
    };
    Ok(Filters {
        limit,
        offset,
        min_score,
        langs: strings("lang", given("lang"))?,
        globs: strings("glob", given("glob"))?,
        text,
        since,
    })
}

/// A value the schema allows to be one string or a list of them.
fn strings(name: &str, value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(one)) => Ok(vec![one.clone()]),
        Some(Value::Array(many)) => many
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                other => bail!("'{name}' must hold strings, not {other}"),
            })
            .collect(),
        Some(other) => bail!("'{name}' must be a string or an array of strings, not {other}"),
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

/// Every hit scored under `min_score`. Saying only "No matches." would read as
/// the code not being there.
fn below_threshold(min: f32, found: usize) -> Value {
    let text = format!(
        "No matches scoring {min} or higher; {} scored lower and {} left out. Lower or leave out `min_score` to see {}.",
        crate::cli::plural(found, "hit"),
        if found == 1 { "was" } else { "were" },
        if found == 1 { "it" } else { "them" }
    );
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

/// The hits as data, for a revision with structured output: the fields the text
/// shows, and where the next page starts, `null` when there is none.
fn structured(hits: &[pipeline::SearchHit], next_offset: Option<usize>) -> Value {
    let hits: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "path": h.path,
                "start_line": h.start_line,
                "end_line": h.end_line,
                "score": h.score,
                "symbol": h.symbol.as_ref().map(|s| s.label()),
                "snippet": h.snippet,
            })
        })
        .collect();
    json!({ "hits": hits, "next_offset": next_offset })
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
    fn an_id_that_is_not_a_string_or_integer_is_an_invalid_request() {
        let cfg = Config::default();
        for id in [json!(null), json!(1.5), json!([1]), json!({}), json!(true)] {
            let request = json!({ "jsonrpc": "2.0", "id": id, "method": "ping" });
            let resp = handle(&cfg, None, &request).unwrap();
            assert_eq!(resp["error"]["code"], -32600, "{id}");
            assert_eq!(resp["id"], Value::Null);
        }
        for id in [json!(7), json!("a")] {
            let request = json!({ "jsonrpc": "2.0", "id": id, "method": "ping" });
            assert_eq!(handle(&cfg, None, &request).unwrap()["id"], id);
        }
    }

    #[test]
    fn another_jsonrpc_version_is_an_invalid_request() {
        let cfg = Config::default();
        let request = json!({ "jsonrpc": "1.0", "id": 3, "method": "ping" });
        let resp = handle(&cfg, None, &request).unwrap();
        assert_eq!(resp["error"]["code"], -32600);
        assert_eq!(resp["id"], 3);
    }

    /// The JSON-RPC 2.0 spec requires the version member; a message without it
    /// is not the protocol and is refused like one with another version.
    #[test]
    fn a_request_without_the_jsonrpc_version_is_an_invalid_request() {
        let cfg = Config::default();
        let request = json!({ "id": 4, "method": "ping" });
        let resp = handle(&cfg, None, &request).unwrap();
        assert_eq!(resp["error"]["code"], -32600);
        assert_eq!(resp["id"], 4);
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
    fn tools_list_exposes_search_and_index_status() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&Value> = tools.iter().map(|t| &t["name"]).collect();
        assert_eq!(names, ["search", "find_similar", "outline", "index_status"]);
        let status = &tools[3]["inputSchema"];
        assert_eq!(status["type"], "object");
        assert_eq!(status["additionalProperties"], false);
        assert!(status.get("required").is_none());
    }

    /// An agent that has one hit in hand must be told which tool finds its
    /// copies, and an agent that has a question must be told it is not the
    /// other one's job.
    #[test]
    fn the_two_search_tools_say_when_to_reach_for_each_other() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let description = |name: &str| -> &str {
            tools.iter().find(|t| t["name"] == name).unwrap()["description"]
                .as_str()
                .unwrap()
        };
        assert!(
            description("search").contains("`find_similar`"),
            "{}",
            description("search")
        );
        assert!(
            description("find_similar").contains("`search`"),
            "{}",
            description("find_similar")
        );
    }

    /// Both tools only read, so a client need not ask before each call. The
    /// annotations are hints in fields a 2024-11-05 client does not know and
    /// ignores: what that revision requires of a tool is untouched.
    #[test]
    fn both_tools_are_marked_as_changing_nothing_the_user_owns() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("tools/list", json!({}))).unwrap();
        for tool in resp["result"]["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let hints = &tool["annotations"];
            assert_eq!(hints["readOnlyHint"], true, "{name}");
            assert_eq!(hints["destructiveHint"], false, "{name}");
            assert_eq!(hints["idempotentHint"], true, "{name}");
            assert_eq!(hints["openWorldHint"], false, "{name}");
            let title = tool["title"].as_str().unwrap();
            assert!(!title.is_empty() && title != name, "{name}");
            assert_eq!(hints["title"], tool["title"], "{name}");
            let mut fields: Vec<&str> = tool
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            fields.sort_unstable();
            let mut expected = vec![
                "annotations",
                "description",
                "inputSchema",
                "name",
                "outputSchema",
                "title",
            ];
            expected.sort_unstable();
            assert_eq!(fields, expected, "{name}");
        }
    }

    fn call_tool(config: &Config, name: &str, arguments: Value) -> Value {
        let request = req(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        handle(config, None, &request).unwrap()
    }

    /// An agent that is told there is no index, or finds nothing it expects,
    /// can ask what the index holds and whether it is behind the tree.
    #[test]
    fn index_status_reports_what_the_index_holds_and_how_far_behind_it_is() {
        let dir = crate::testutil::TempDir::new();
        dir.write("auth.rs", "fn authenticate(user) { verify password }");
        let config = Config {
            backend: crate::config::Backend::Hash,
            root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        config.save().unwrap();

        let report = |config: &Config| -> Value {
            let resp = call_tool(config, "index_status", json!({}));
            assert_eq!(resp["result"]["isError"], false, "{resp}");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            serde_json::from_str(text).unwrap()
        };
        let before = report(&config);
        assert_eq!(before["schema"], "ripsem/status/v1");
        assert_eq!(before["index"]["present"], false);

        pipeline::index_repo(&config, false).unwrap();
        let built = report(&config);
        assert_eq!(built["index"]["present"], true);
        assert_eq!(built["index"]["files"], 1);
        assert_eq!(built["index"]["drift"]["added"], 0);

        dir.write("new.rs", "fn added() {}");
        let behind = report(&config);
        assert_eq!(behind["index"]["drift"]["added"], 1);
        assert!(behind["updating"].is_boolean(), "{behind}");
    }

    /// A client that has picked a file lists its declarations before reading
    /// any of its lines, and the same declarations come back as text and as
    /// structured content.
    #[cfg(feature = "treesitter")]
    #[test]
    fn outline_lists_the_declarations_of_a_file_in_order() {
        let dir = crate::testutil::TempDir::new();
        dir.write(
            "auth.rs",
            "pub struct Session {\n    token: String,\n}\n\nimpl Session {\n    pub fn refresh(&mut self) {}\n}\n",
        );
        let config = Config {
            backend: crate::config::Backend::Hash,
            root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        config.save().unwrap();
        pipeline::index_repo(&config, false).unwrap();

        let resp = call_tool(&config, "outline", json!({ "path": "auth.rs" }));
        assert_eq!(resp["result"]["isError"], false, "{resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("1-3  struct Session"), "{text}");
        assert!(text.contains("fn Session::refresh"), "{text}");
        let rows = &resp["result"]["structuredContent"]["declarations"];
        assert_eq!(rows[0]["kind"], "struct");
        assert_eq!(rows[0]["symbol"], "Session");
        assert_eq!(rows[0]["start_line"], 1);
        assert_eq!(rows[0]["end_line"], 3);
        assert_eq!(rows.as_array().unwrap().len(), 3, "{rows}");
    }

    /// A directory, a path the index leaves out and an unknown argument are
    /// tool errors the client can correct, not protocol errors.
    #[test]
    fn outline_refuses_a_directory_a_missing_file_and_unknown_arguments() {
        let dir = crate::testutil::TempDir::new();
        dir.write("src/auth.rs", "fn authenticate(user) {}");
        let config = Config {
            backend: crate::config::Backend::Hash,
            root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        config.save().unwrap();
        pipeline::index_repo(&config, false).unwrap();

        let failure = |arguments: Value| -> String {
            let resp = call_tool(&config, "outline", arguments);
            assert_eq!(resp["result"]["isError"], true, "{resp}");
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert!(
            failure(json!({ "path": "src" })).contains("is a directory"),
            "directory"
        );
        assert!(
            failure(json!({ "path": "src/nope.rs" })).contains("not in the index"),
            "missing"
        );
        assert!(
            failure(json!({ "path": "src/auth.rs", "line": 1 }))
                .contains("unknown argument 'line'"),
            "unknown"
        );
        assert!(failure(json!({})).contains("missing 'path'"), "absent");
    }

    /// The drift an agent is told is what a search would update: a file whose
    /// size and time are what the index stored is not read, unless the server
    /// was started to hash every file.
    #[test]
    fn index_status_trusts_a_stat_as_a_search_does() {
        let dir = crate::testutil::TempDir::new();
        dir.write("auth.rs", "alpha bravo charlie");
        let path = dir.path().join("auth.rs");
        let set_modified = |time| {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(time)
                .unwrap();
        };
        let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
        set_modified(aged);
        let config = Config {
            backend: crate::config::Backend::Hash,
            root: dir.path().canonicalize().unwrap(),
            ..Default::default()
        };
        config.save().unwrap();
        pipeline::index_repo(&config, false).unwrap();

        dir.write("auth.rs", "omega bravo charlie");
        set_modified(aged);
        let modified = |config: &Config| -> u64 {
            let resp = call_tool(config, "index_status", json!({}));
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            let report: Value = serde_json::from_str(text).unwrap();
            report["index"]["drift"]["modified"].as_u64().unwrap()
        };
        assert_eq!(modified(&config), 0);
        let hashing = Config {
            hash_all: true,
            ..config
        };
        assert_eq!(modified(&hashing), 1);
    }

    #[test]
    fn index_status_takes_no_arguments() {
        let cfg = Config::default();
        let resp = call_tool(&cfg, "index_status", json!({ "verbose": true }));
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("unknown argument 'verbose'"), "got: {text}");
        let resp = call_tool(&cfg, "index_status", Value::Null);
        assert_eq!(resp["result"]["isError"], false, "{resp}");
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

    #[test]
    fn lang_and_glob_take_a_string_or_an_array() {
        let root = Path::new("/repo");
        let scope = |args: Value| parse_call(&args, root).unwrap().scope.unwrap();

        let one = scope(json!({ "query": "x", "lang": "rust" }));
        assert!(one.admits("src/a.rs") && !one.admits("src/a.py"));
        let many = scope(json!({ "query": "x", "lang": ["py", "toml"] }));
        assert!(many.admits("a.py") && many.admits("Cargo.toml") && !many.admits("a.rs"));

        let one = scope(json!({ "query": "x", "glob": "*.rs" }));
        assert!(one.admits("src/a.rs") && !one.admits("src/a.md"));
        let many = scope(json!({ "query": "x", "glob": ["*.rs", "!*_test.rs"] }));
        assert!(many.admits("a.rs") && !many.admits("a_test.rs"));

        let all = scope(json!({ "query": "x", "path": "src", "lang": "rs", "glob": "!gen/" }));
        assert!(all.admits("src/a.rs") && !all.admits("src/gen/a.rs") && !all.admits("lib/a.rs"));
        let args = json!({ "query": "x", "lang": null, "glob": [] });
        assert!(parse_call(&args, root).unwrap().scope.is_none());
    }

    #[test]
    fn a_lang_or_glob_of_the_wrong_shape_is_a_tool_error() {
        for name in ["lang", "glob"] {
            for bad in [json!(7), json!(true), json!({ "a": 1 }), json!(["rust", 1])] {
                let text = tool_error(json!({ "query": "x", name: bad }));
                assert!(text.contains(&format!("'{name}' must")), "{bad}: {text}");
            }
        }
        let text = tool_error(json!({ "query": "x", "lang": "" }));
        assert!(text.contains("names no language"), "{text}");
        let text = tool_error(json!({ "query": "x", "glob": ["*.rs", "src/[a"] }));
        assert!(text.contains("invalid glob 'src/[a'"), "{text}");
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
        let default = schema["properties"]["limit"]["default"].clone();
        assert_eq!(default, DEFAULT_LIMIT);
        let args = json!({ "query": "x" });
        assert_eq!(
            parse_call(&args, Path::new("/repo")).unwrap().limit,
            DEFAULT_LIMIT
        );
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

    /// A `name` of the wrong type is named as the bad field. Calling it missing
    /// sends the client looking for a field it did send.
    #[test]
    fn a_tool_name_that_is_not_a_string_is_named_as_the_bad_field() {
        let cfg = Config::default();
        let resp = handle(&cfg, None, &req("tools/call", json!({ "name": 7 }))).unwrap();
        assert_eq!(resp["error"]["code"], -32602);
        let message = resp["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("'name' must be a string, not 7"),
            "got: {message}"
        );

        let resp = handle(&cfg, None, &req("tools/call", json!({}))).unwrap();
        assert_eq!(resp["error"]["message"], "missing tool name");
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

    /// Framing: CRLF lines are read as the lines they are, a last message with no
    /// line break is answered, and a cancellation or a response from the client
    /// gets no reply. A message cut off by the end of the input is a parse error.
    #[test]
    fn framing_follows_line_endings_and_the_end_of_input() {
        let replies_to = |input: &[u8]| -> Vec<Value> {
            let dir = crate::testutil::TempDir::new();
            let config = Config {
                root: dir.path().to_path_buf(),
                ..Default::default()
            };
            let mut out = Vec::new();
            serve_on(config, false, |_| {}, input, &mut out).unwrap();
            out.split(|&b| b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).unwrap())
                .collect()
        };
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\r\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}",
        );
        let replies = replies_to(input.as_bytes());
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["result"], json!({}));
        assert_eq!(replies[1]["id"], 3);
        assert_eq!(replies[1]["result"], json!({}));

        let replies = replies_to(b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"pi");
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0]["error"]["code"], -32700);
        assert_eq!(replies[0]["id"], Value::Null);
    }

    /// A request line is held whole only up to a size limit. A longer one is
    /// refused as a request, and the lines after it are still answered.
    #[test]
    fn an_over_long_request_is_refused_and_the_next_one_is_served() {
        let dir = crate::testutil::TempDir::new();
        let config = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let padding = "x".repeat(4 * 1024 * 1024);
        let input = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":\"{padding}\"}}\n\
             {{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}}\n"
        );
        let mut out = Vec::new();
        serve_on(config, false, |_| {}, input.as_bytes(), &mut out).unwrap();
        let replies: Vec<Value> = out
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 2, "got {} replies", replies.len());
        assert_eq!(replies[0]["error"]["code"], -32600);
        assert_eq!(replies[0]["id"], Value::Null);
        assert_eq!(replies[1]["id"], 2);
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
        // Saying only "No matches." would read as the code not existing.
        let text = unrelated["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("No matches scoring 0.5 or higher"),
            "{text}"
        );
        assert!(text.contains("scored lower"), "{text}");
        assert!(text.contains("min_score"), "{text}");
        assert_eq!(unrelated["result"]["isError"], false);
    }

    /// Files that all fit the query: `auth` ones hold every term, `note` ones
    /// only some, so the ranking has two plainly different levels.
    fn paged_project() -> (crate::testutil::TempDir, Config, pipeline::Searcher) {
        use crate::config::Backend;

        let dir = crate::testutil::TempDir::new();
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
        let config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        pipeline::index_repo(&config, false).unwrap();
        let searcher = pipeline::open(&config).unwrap();
        (dir, config, searcher)
    }

    fn search_text(config: &Config, searcher: &pipeline::Searcher, arguments: Value) -> String {
        let request = req(
            "tools/call",
            json!({ "name": "search", "arguments": arguments }),
        );
        let resp = handle(config, Some(searcher), &request).unwrap();
        assert_eq!(resp["result"]["isError"], false, "{arguments}: {resp}");
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The `path:start-end` heads of a result text, in order.
    fn heads(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|l| {
                l.split_once(':')
                    .is_some_and(|(path, _)| path.ends_with(".rs") || path.ends_with(".md"))
            })
            .map(|l| l.split(' ').next().unwrap())
            .collect()
    }

    #[test]
    fn an_offset_pages_through_the_ranking() {
        let (_dir, config, searcher) = paged_project();
        let query = "verify password create session token";
        let all = search_text(&config, &searcher, json!({ "query": query, "limit": 50 }));
        assert_eq!(heads(&all).len(), 10, "{all}");
        assert!(
            !all.contains("offset="),
            "nothing follows the last hit: {all}"
        );

        let mut joined = Vec::new();
        for offset in [0, 4, 8] {
            let page = search_text(
                &config,
                &searcher,
                json!({ "query": query, "limit": 4, "offset": offset }),
            );
            let shown = heads(&page);
            assert_eq!(shown.len(), (10 - offset).min(4), "{page}");
            let more = format!("pass offset={} to see them", offset + 4);
            assert_eq!(page.contains(&more), offset + 4 < 10, "{page}");
            if offset + 4 < 10 {
                assert!(page.ends_with(&format!("{more}.")), "{page}");
            }
            joined.extend(shown.into_iter().map(str::to_string));
        }
        assert_eq!(joined, heads(&all));
    }

    #[test]
    fn an_offset_past_the_last_hit_says_there_is_nothing_further() {
        let (_dir, config, searcher) = paged_project();
        let query = "verify password create session token";
        for offset in [10, 11, 5000] {
            let text = search_text(
                &config,
                &searcher,
                json!({ "query": query, "offset": offset }),
            );
            assert!(
                text.starts_with("No further results")
                    && text.contains(&format!("offset {offset}")),
                "{text}"
            );
        }
        let none = search_text(&config, &searcher, json!({ "query": query }));
        assert!(!none.contains("No further"), "{none}");
    }

    #[test]
    fn an_offset_outside_the_schema_is_a_tool_error() {
        for offset in [json!(-1), json!(1.5), json!("2"), json!(true)] {
            let text = tool_error(json!({ "query": "x", "offset": offset }));
            assert!(
                text.contains("'offset' must be a whole number, 0 or more"),
                "{offset}: {text}"
            );
        }
        let root = Path::new("/repo");
        let offset = |args: Value| parse_call(&args, root).unwrap().offset;
        assert_eq!(offset(json!({ "query": "x" })), 0);
        assert_eq!(offset(json!({ "query": "x", "offset": null })), 0);
        assert_eq!(offset(json!({ "query": "x", "offset": 7 })), 7);
        assert_eq!(offset(json!({ "query": "x", "offset": 7.0 })), 7);
    }

    #[test]
    fn paging_follows_lang_glob_and_path() {
        let (_dir, config, searcher) = paged_project();
        let query = "verify password create session token";
        for filter in [
            json!({ "lang": "rust" }),
            json!({ "glob": ["*.rs"] }),
            json!({ "path": "src" }),
        ] {
            let ask = |limit: u64, offset: u64| {
                let mut args = filter.clone();
                args["query"] = json!(query);
                args["limit"] = json!(limit);
                args["offset"] = json!(offset);
                search_text(&config, &searcher, args)
            };
            let all = ask(50, 0);
            assert_eq!(heads(&all).len(), 5, "{filter}: {all}");
            let (first, second) = (ask(3, 0), ask(3, 3));
            assert!(first.ends_with("pass offset=3 to see them."), "{first}");
            assert!(!second.contains("offset="), "{second}");
            let joined: Vec<&str> = heads(&first).into_iter().chain(heads(&second)).collect();
            assert_eq!(joined, heads(&all), "{filter}");
            assert!(
                joined
                    .iter()
                    .all(|h| h.ends_with(".rs") || h.contains(".rs:"))
            );
        }
    }

    #[test]
    fn paging_follows_min_score() {
        let (_dir, config, searcher) = paged_project();
        let query = "verify password create session token";
        let certain = |limit: u64, offset: u64| {
            let args =
                json!({ "query": query, "min_score": 1.0, "limit": limit, "offset": offset });
            search_text(&config, &searcher, args)
        };
        let first = certain(3, 0);
        assert_eq!(heads(&first).len(), 3, "{first}");
        assert!(first.ends_with("pass offset=3 to see them."), "{first}");
        // Two hits hold every term and are left; the notes score lower, so
        // nothing is promised after them.
        let second = certain(3, 3);
        assert_eq!(heads(&second).len(), 2, "{second}");
        assert!(!second.contains("offset="), "{second}");
        // A page of exactly the hits above the threshold has no more to say.
        let exact = certain(5, 0);
        assert_eq!(heads(&exact).len(), 5, "{exact}");
        assert!(!exact.contains("offset="), "{exact}");
        // Everything on the page scores below the threshold.
        let below = certain(3, 5);
        assert!(
            below.starts_with("No matches scoring 1 or higher"),
            "{below}"
        );
    }

    #[test]
    fn a_regex_keeps_the_hits_whose_chunk_matches_in_rank_order() {
        let (_dir, config, searcher) = paged_project();
        let query = "verify password create session token";
        let all = search_text(&config, &searcher, json!({ "query": query, "limit": 50 }));
        let wanted: Vec<&str> = heads(&all)
            .into_iter()
            .filter(|h| h.contains("auth2.rs") || h.contains("auth4.rs"))
            .collect();
        assert_eq!(wanted.len(), 2);

        let kept = search_text(
            &config,
            &searcher,
            json!({ "query": query, "regex": r"auth[24]\(" }),
        );
        assert_eq!(heads(&kept), wanted, "{kept}");
        assert!(!kept.contains("offset="), "{kept}");

        let folded = search_text(
            &config,
            &searcher,
            json!({ "query": query, "regex": "(?i)NOTE [0-9]:" }),
        );
        assert_eq!(heads(&folded).len(), 5, "{folded}");
        assert!(
            heads(&folded).iter().all(|h| h.contains(".md:")),
            "{folded}"
        );

        let none = search_text(
            &config,
            &searcher,
            json!({ "query": query, "regex": "no such text" }),
        );
        assert_eq!(none, "No matches.");
    }

    #[test]
    fn a_regex_combines_with_the_other_filters_and_with_paging() {
        let (_dir, config, searcher) = paged_project();
        let query = "verify password create session token";
        let ask = |limit: u64, offset: u64, filter: Value| {
            let mut args = filter;
            args["query"] = json!(query);
            args["regex"] = json!("(?i)auth|note");
            args["limit"] = json!(limit);
            args["offset"] = json!(offset);
            search_text(&config, &searcher, args)
        };
        let all = ask(50, 0, json!({}));
        assert_eq!(heads(&all).len(), 10, "{all}");
        let (first, second) = (ask(6, 0, json!({})), ask(6, 6, json!({})));
        assert!(first.ends_with("pass offset=6 to see them."), "{first}");
        assert!(!second.contains("offset="), "{second}");
        let joined: Vec<&str> = heads(&first).into_iter().chain(heads(&second)).collect();
        assert_eq!(joined, heads(&all));

        let rust = ask(50, 0, json!({ "lang": "rust" }));
        assert_eq!(heads(&rust).len(), 5, "{rust}");
        let docs = ask(50, 0, json!({ "path": "docs", "glob": "*.md" }));
        assert_eq!(heads(&docs).len(), 5, "{docs}");
    }

    #[test]
    fn a_regex_that_does_not_parse_is_a_tool_error() {
        let text = tool_error(json!({ "query": "x", "regex": "foo(" }));
        assert_eq!(text, "search failed: invalid regex 'foo(': unclosed group");
        let text = tool_error(json!({ "query": "x", "regex": 7 }));
        assert!(text.contains("'regex' must be a string"), "{text}");
        let text = tool_error(json!({ "query": "x", "regex": "((a{100}){100}){100}" }));
        assert!(text.contains("too large"), "{text}");
    }

    /// Checks `value` against the part of JSON Schema the tool schemas use: `type`
    /// (one name or several), `properties`, `required`, `items`, `minimum`,
    /// `maximum` and `additionalProperties: false`. Says where the first
    /// violation is.
    fn conforms(value: &Value, schema: &Value, at: &str) -> Result<(), String> {
        if let Some(declared) = schema.get("type") {
            let names: Vec<&str> = match declared {
                Value::String(one) => vec![one.as_str()],
                Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                _ => return Err(format!("{at}: the schema's type is not a name")),
            };
            if !names.iter().any(|name| is_type(value, name)) {
                return Err(format!("{at}: {value} is not {}", names.join(" or ")));
            }
        }
        if let Some(number) = value.as_f64() {
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && number < min
            {
                return Err(format!("{at}: {number} is below {min}"));
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
                && number > max
            {
                return Err(format!("{at}: {number} is above {max}"));
            }
        }
        if let Some(object) = value.as_object() {
            let properties = schema.get("properties").and_then(Value::as_object);
            let required = schema.get("required").and_then(Value::as_array);
            for name in required.into_iter().flatten().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(format!("{at}: '{name}' is required"));
                }
            }
            let closed = schema.get("additionalProperties") == Some(&json!(false));
            for (name, member) in object {
                match properties.and_then(|p| p.get(name)) {
                    Some(sub) => conforms(member, sub, &format!("{at}.{name}"))?,
                    None if closed => return Err(format!("{at}: '{name}' is not allowed")),
                    None => {}
                }
            }
        }
        if let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) {
            for (i, item) in items.iter().enumerate() {
                conforms(item, item_schema, &format!("{at}[{i}]"))?;
            }
        }
        Ok(())
    }

    fn is_type(value: &Value, name: &str) -> bool {
        match name {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => false,
        }
    }

    /// The checker is only worth trusting if it refuses what a schema forbids.
    #[test]
    fn the_schema_checker_refuses_what_the_schema_forbids() {
        let schema = json!({
            "type": "object",
            "properties": {
                "n": { "type": "integer", "minimum": 1 },
                "next": { "type": ["integer", "null"] },
                "tags": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["n", "next"],
            "additionalProperties": false
        });
        let ok = json!({ "n": 1, "next": null, "tags": ["a"] });
        assert!(conforms(&ok, &schema, "t").is_ok());
        for bad in [
            json!({ "next": null }),
            json!({ "n": 0, "next": null }),
            json!({ "n": 1.5, "next": null }),
            json!({ "n": 1, "next": "2" }),
            json!({ "n": 1, "next": null, "tags": [1] }),
            json!({ "n": 1, "next": null, "extra": true }),
        ] {
            assert!(conforms(&bad, &schema, "t").is_err(), "{bad}");
        }
    }

    /// Every result a tool returns with structured content holds to the
    /// outputSchema the server declares for that tool, for paged hits, an empty
    /// page, a threshold that drops everything, similar places and the status.
    #[test]
    fn structured_results_hold_to_the_output_schema_declared_for_them() {
        let (_dir, config, searcher) = paged_project();
        let listed = handle(&config, None, &req("tools/list", json!({}))).unwrap();
        let schema_of = |tool: &str| -> Value {
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == tool)
                .unwrap()["outputSchema"]
                .clone()
        };
        let calls = [
            ("search", json!({ "query": "verify password" })),
            (
                "search",
                json!({ "query": "verify password", "limit": 2, "offset": 2 }),
            ),
            (
                "search",
                json!({ "query": "verify password", "offset": 9999 }),
            ),
            ("search", json!({ "query": "zzqqx", "min_score": 0.9 })),
            ("find_similar", json!({ "path": "src/auth0.rs", "line": 1 })),
            ("index_status", json!({})),
        ];
        for (tool, arguments) in calls {
            let request = req(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            );
            let resp = handle(&config, Some(&searcher), &request).unwrap();
            assert_eq!(
                resp["result"]["isError"], false,
                "{tool} {arguments}: {resp}"
            );
            let structured = &resp["result"]["structuredContent"];
            assert!(structured.is_object(), "{tool} {arguments}: {resp}");
            conforms(structured, &schema_of(tool), tool)
                .unwrap_or_else(|e| panic!("{tool} {arguments}: {e}"));
        }
    }
}
