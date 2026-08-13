use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::Config;
use crate::pipeline;

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn serve(config: Config) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut session = None;

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
        hold_the_index(&mut session, &config);
        if let Some(response) = handle(&config, session.as_ref(), &request) {
            writeln!(writer, "{}", serde_json::to_string(&response)?)?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Hold an open searcher across requests, and take one as soon as there is an
/// index to take. The server usually starts before the first `ripsem index`
/// finishes — the README wires the agent up first — and a session that stayed
/// empty because the index was missing at startup made every later search open
/// the index and load the embedding model again, a second or more of work per
/// call. Once held, the searcher only re-reads the file when it changes.
fn hold_the_index(session: &mut Option<pipeline::Searcher>, config: &Config) {
    match session {
        Some(searcher) => {
            let _ = searcher.refresh();
        }
        None => *session = pipeline::open(config).ok(),
    }
}

fn handle(config: &Config, session: Option<&pipeline::Searcher>, request: &Value) -> Option<Value> {
    let method = request.get("method")?.as_str()?;
    let id = request.get("id").cloned();

    match method {
        "initialize" => Some(ok(
            id?,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
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

    let query = match args.get("query").and_then(|q| q.as_str()) {
        Some(q) => q,
        None => return error(id, -32602, "missing 'query'"),
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

    let prefix = args
        .get("path")
        .and_then(|p| p.as_str())
        .and_then(|p| scope_prefix(p, &config.root));

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
        Err(e) => ok(
            id,
            json!({
                "content": [{ "type": "text", "text": format!("search failed: {e}") }],
                "isError": true
            }),
        ),
    }
}

/// Normalize a user-supplied subdirectory into a repo-relative, forward-slash
/// prefix usable for scoping search results. `.`/empty/whitespace ⇒ no scope.
/// Shared by the MCP `search` tool and the CLI `search --in` flag.
pub(crate) fn scope_prefix(raw: &str, root: &std::path::Path) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "." {
        return None;
    }
    let candidate = std::path::Path::new(raw);
    let rel = if candidate.is_absolute() {
        candidate.strip_prefix(root).ok()?
    } else {
        candidate
    };
    let normalized = rel.to_string_lossy().replace('\\', "/");
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
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

    #[test]
    fn call_without_query_is_rejected() {
        let cfg = Config::default();
        let resp = handle(
            &cfg,
            None,
            &req("tools/call", json!({ "name": "search", "arguments": {} })),
        )
        .unwrap();
        assert_eq!(resp["error"]["code"], -32602);
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
        let root = std::path::Path::new("/repo");
        assert_eq!(scope_prefix("src/embed", root), Some("src/embed".into()));
        assert_eq!(scope_prefix("src\\embed\\", root), Some("src/embed".into()));
        assert_eq!(scope_prefix(".", root), None);
        assert_eq!(scope_prefix("  ", root), None);
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
        // Score must not appear in the MCP output.
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

    /// The documented order is `install claude-code` and then `index`, so the
    /// server is expected to start without one. It has to pick the index up
    /// when it appears instead of reopening it for every search from then on.
    #[test]
    fn the_index_is_held_as_soon_as_there_is_one() {
        use crate::config::Backend;
        use crate::testutil::TempDir;

        let dir = TempDir::new();
        dir.write("auth.rs", "fn authenticate(user) { verify password }");
        let config = Config {
            backend: Backend::Hash,
            root: dir.path().to_path_buf(),
            ..Default::default()
        };

        let mut session = None;
        hold_the_index(&mut session, &config);
        assert!(session.is_none(), "there is no index to hold yet");

        pipeline::index_repo(&config, false).unwrap();
        hold_the_index(&mut session, &config);
        assert!(
            session.is_some(),
            "the index exists now and must be held instead of reopened per call"
        );
    }

    #[test]
    fn mcp_apply_min_score_none_keeps_all() {
        let hits = vec![make_hit("a.rs", 1, 2, 0.1), make_hit("b.rs", 1, 2, 0.9)];
        assert_eq!(apply_min_score(hits, None).len(), 2);
    }

    // Build a tiny Hash-backend index and drive a real tools/call through handle().
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

        // Query that should match auth.rs and not math.rs.
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

        // Must be a successful result, not a JSON-RPC error.
        assert!(resp.get("error").is_none(), "unexpected error: {resp}");
        let result = &resp["result"];
        assert_eq!(result["isError"], false);

        let text = result["content"][0]["text"].as_str().unwrap();
        // The top hit should be auth.rs.
        assert!(
            text.contains("auth.rs"),
            "expected auth.rs in result, got: {text}"
        );
        // Lean format: no score field exposed.
        assert!(!text.contains("score"), "score should not appear: {text}");

        // Verify limit is honored: at most 3 results.
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
