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

    let session = pipeline::open(&config).ok();

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
        "description": "Semantic code search over the local index. Returns the most relevant code chunks for a natural-language query.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Natural-language description of what to find." },
                "path": { "type": "string", "description": "Optional directory to search; defaults to the server's working directory." },
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

    let result = match args.get("path").and_then(|p| p.as_str()) {
        Some(p) => Config::load(std::path::Path::new(p))
            .and_then(|c| pipeline::search(&c, query, limit)),
        None => match session {
            Some(s) => s.query(query, limit),
            None => pipeline::search(config, query, limit),
        },
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
