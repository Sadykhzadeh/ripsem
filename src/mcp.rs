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

    let mut session = pipeline::open(&config).ok();

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
        if let Some(s) = session.as_mut() {
            let _ = s.refresh();
        }
        if let Some(response) = handle(&config, session.as_ref(), &request) {
            writeln!(writer, "{}", serde_json::to_string(&response)?)?;
            writer.flush()?;
        }
    }
    Ok(())
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
        "description": "Search this codebase by meaning rather than exact text. Reach for this first on any 'where/how/what' question about the code: describe what you are looking for in plain language (e.g. 'where is authentication configured', 'how are payments validated') and it returns the most relevant functions and files as ranked snippets with line ranges. Faster and far cheaper on context than scanning with grep, which you should fall back to only for exact strings or symbols.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Plain-language description of the code or behavior you want to find." },
                "path": { "type": "string", "description": "Optional subdirectory (relative to the repo) to limit results to, e.g. 'src/embed'." },
                "limit": { "type": "integer", "description": "Maximum number of results (default 10)." }
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
        .unwrap_or(10);

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
        Ok(hits) => ok(id, tool_result(&hits)),
        Err(e) => ok(
            id,
            json!({
                "content": [{ "type": "text", "text": format!("search failed: {e}") }],
                "isError": true
            }),
        ),
    }
}

fn scope_prefix(raw: &str, root: &std::path::Path) -> Option<String> {
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

fn tool_result(hits: &[pipeline::SearchHit]) -> Value {
    if hits.is_empty() {
        return json!({
            "content": [{ "type": "text", "text": "No matches." }],
            "isError": false
        });
    }
    let mut text = String::new();
    for h in hits {
        text.push_str(&format!(
            "{}:{}-{}  (score {:.3})\n{}\n\n",
            h.path, h.start_line, h.end_line, h.score, h.snippet
        ));
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
        }];
        let value = tool_result(&hits);
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("a.rs:1-4"));
        assert_eq!(value["isError"], false);
    }
}
