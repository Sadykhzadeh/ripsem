use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::config::{Backend, Config};
use crate::index::Index;
use crate::{mcp, model, pipeline, walk, watch};

#[derive(Parser)]
#[command(name = "ripsem", version, about = "Local semantic code search")]
pub struct Cli {
    #[arg(long, global = true, default_value = ".")]
    pub path: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create the local config for this directory.
    Init,
    /// Build or update the index for this directory.
    Index {
        #[arg(long)]
        force: bool,
        /// List the files that would be indexed and exit, without building
        /// embeddings or writing the index. Useful for checking which
        /// ignore/exclude/config patterns are in effect.
        #[arg(long)]
        dry_run: bool,
    },
    /// Search the index with a natural-language query.
    Search {
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// Limit results to a subdirectory (relative to the repo root), e.g.
        /// `--in src/embed`. `.` or empty searches the whole index.
        #[arg(long = "in", value_name = "PREFIX")]
        within: Option<String>,
        /// Number of snippet lines to print per hit in the human output. `0`
        /// prints just the location header with no snippet body. Ignored by
        /// `--json`, which always returns the full snippet.
        #[arg(long, value_name = "LINES", default_value_t = 3)]
        context: usize,
        /// Drop hits whose cosine score is below this threshold, keeping only
        /// confident matches. Absent means no filtering. Applies to both the
        /// human and `--json` output.
        #[arg(long, value_name = "SCORE")]
        min_score: Option<f32>,
    },
    /// Index and keep the index in sync as files change.
    Watch,
    /// Show index health and configuration for this directory.
    Status {
        /// Emit machine-readable JSON (schema `ripsem/status/v1`) instead of the
        /// human summary — for tooling or an agent checking whether the index
        /// exists, is stale, or needs rebuilding.
        #[arg(long)]
        json: bool,
    },
    /// Run the MCP server over stdio.
    Mcp,
    /// Inspect and choose the embedding model and backend.
    Model {
        #[command(subcommand)]
        action: ModelCmd,
    },
    /// Wire ripsem into a coding agent.
    Install {
        #[command(subcommand)]
        target: InstallCmd,
    },
}

#[derive(Subcommand)]
pub enum ModelCmd {
    /// List the available embedding models.
    List,
    /// Set the embedding model for this directory.
    Set { id: String },
    /// Set the embedding backend (hash or fastembed).
    Backend { name: String },
}

#[derive(Subcommand)]
pub enum InstallCmd {
    /// Add ripsem to .mcp.json for Claude Code.
    ClaudeCode,
    /// Add ripsem to .cursor/mcp.json for Cursor.
    Cursor,
}

pub fn run(cli: Cli) -> Result<()> {
    let root = cli.path.canonicalize().unwrap_or(cli.path);
    match cli.command {
        Command::Init => cmd_init(&root),
        Command::Index { force, dry_run } => cmd_index(&root, force, dry_run),
        Command::Search {
            query,
            limit,
            json,
            within,
            context,
            min_score,
        } => cmd_search(
            &root,
            &query,
            limit,
            json,
            within.as_deref(),
            context,
            min_score,
        ),
        Command::Watch => watch::run(Config::load(&root)?),
        Command::Status { json } => cmd_status(&root, json),
        Command::Mcp => mcp::serve(Config::load(&root)?),
        Command::Model { action } => cmd_model(&root, action),
        Command::Install { target } => cmd_install(&root, target),
    }
}

fn cmd_init(root: &Path) -> Result<()> {
    let config = Config {
        root: root.to_path_buf(),
        ..Default::default()
    };
    config.save()?;
    println!("wrote {}", show_path(&config.config_path()));
    println!("model: {} ({:?} backend)", config.model, config.backend);
    Ok(())
}

/// Index-health facts gathered for the `status` output (both human and JSON).
struct IndexFacts {
    path: String,
    size_bytes: u64,
    chunks: usize,
    files: usize,
    ann: bool,
    index_model: String,
    stale: bool,
}

/// Render `status` as machine-readable JSON (schema `ripsem/status/v1`). Pure:
/// takes the already-gathered facts so it is unit-testable without a real index.
/// `index` is `None` (with an optional `error`) when the index is missing or
/// unreadable; the `error` is `null` for a plain "missing" index.
fn status_json(
    model: &str,
    backend: Backend,
    dims: usize,
    index: Option<&IndexFacts>,
    error: Option<&str>,
) -> String {
    let index_json = match index {
        Some(f) => json!({
            "present": true,
            "path": f.path,
            "size_bytes": f.size_bytes,
            "chunks": f.chunks,
            "files": f.files,
            "ann": f.ann,
            "model": f.index_model,
            "stale": f.stale,
        }),
        None => json!({ "present": false, "error": error }),
    };
    let doc = json!({
        "schema": "ripsem/status/v1",
        "model": model,
        "backend": backend,
        "dims": dims,
        "index": index_json,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

fn cmd_status(root: &Path, as_json: bool) -> Result<()> {
    let config = Config::load(root)?;
    let path = config.index_path();

    // Gather the index facts once, then render as JSON or human text. A missing
    // file and an unreadable index both yield `None`; the latter carries an error.
    let (facts, error) = if !path.exists() {
        (None, None)
    } else {
        match Index::load(&path) {
            Ok(index) => {
                let files: std::collections::HashSet<&str> =
                    index.records.iter().map(|r| r.path.as_str()).collect();
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let facts = IndexFacts {
                    path: show_path(&path),
                    size_bytes: size,
                    chunks: index.records.len(),
                    files: files.len(),
                    ann: index.ann.is_some(),
                    index_model: index.model.clone(),
                    stale: index.model != config.model,
                };
                (Some(facts), None)
            }
            Err(e) => (None, Some(e.to_string())),
        }
    };

    if as_json {
        println!(
            "{}",
            status_json(
                &config.model,
                config.backend,
                config.dims(),
                facts.as_ref(),
                error.as_deref()
            )
        );
        return Ok(());
    }

    println!(
        "model:  {} ({:?} backend, {} dims)",
        config.model,
        config.backend,
        config.dims()
    );
    match (&facts, &error) {
        (None, None) => println!("index:  missing — run `ripsem index`"),
        (None, Some(e)) => println!("index:  unreadable ({e})"),
        (Some(f), _) => {
            println!("index:  {} ({})", f.path, human_size(f.size_bytes));
            println!("chunks: {} across {} files", f.chunks, f.files);
            if f.ann {
                println!("ann:    present (approximate search)");
            }
            if f.stale {
                println!(
                    "note:   index built with {}, config wants {}; run `ripsem index --force`",
                    f.index_model, config.model
                );
            }
        }
    }
    Ok(())
}

fn show_path(path: &Path) -> String {
    let shown = path.display().to_string();
    shown
        .strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or(shown)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn cmd_index(root: &Path, force: bool, dry_run: bool) -> Result<()> {
    let config = Config::load(root)?;
    if dry_run {
        let files = walk::collect(&config)?;
        for path in &files {
            println!("{}", pipeline::relative(&config.root, path));
        }
        println!("{} files would be indexed", files.len());
        return Ok(());
    }
    let stats = pipeline::index_repo(&config, force)?;
    println!(
        "{} files: {} embedded, {} unchanged -> {} chunks",
        stats.files, stats.embedded, stats.reused, stats.chunks
    );
    Ok(())
}

/// First `context` lines of a snippet for the human output. `0` yields an empty
/// slice (just the location header), and the count is capped at the lines
/// available, so a short snippet never pads with blanks.
fn snippet_lines(snippet: &str, context: usize) -> Vec<&str> {
    snippet.lines().take(context).collect()
}

/// Drop hits scoring below `min`. `None` keeps everything; a set threshold
/// retains only hits with `score >= min`. This is a result filter, so callers
/// apply it before both the JSON and human branches.
fn apply_min_score(hits: Vec<pipeline::SearchHit>, min: Option<f32>) -> Vec<pipeline::SearchHit> {
    match min {
        Some(min) => hits.into_iter().filter(|h| h.score >= min).collect(),
        None => hits,
    }
}

fn cmd_search(
    root: &Path,
    query: &str,
    limit: usize,
    as_json: bool,
    within: Option<&str>,
    context: usize,
    min_score: Option<f32>,
) -> Result<()> {
    let config = Config::load(root)?;
    // Normalize the optional --in scope the same way the MCP `search` tool does,
    // so a relative subdir, an absolute path under the repo, or trailing slashes
    // all resolve to the same repo-relative prefix.
    let prefix = within.and_then(|raw| mcp::scope_prefix(raw, &config.root));
    let hits = pipeline::search_scoped(&config, query, limit, prefix.as_deref())?;
    let hits = apply_min_score(hits, min_score);

    if as_json {
        let rows: Vec<Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "path": h.path,
                    "start_line": h.start_line,
                    "end_line": h.end_line,
                    "score": h.score,
                    "snippet": h.snippet,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if hits.is_empty() {
        println!("no matches");
        return Ok(());
    }
    for h in &hits {
        println!(
            "{}:{}-{}  ({:.3})",
            h.path, h.start_line, h.end_line, h.score
        );
        for l in snippet_lines(&h.snippet, context) {
            println!("    {l}");
        }
        println!();
    }
    Ok(())
}

fn cmd_model(root: &Path, action: ModelCmd) -> Result<()> {
    match action {
        ModelCmd::List => {
            let current = Config::load(root).map(|c| c.model).unwrap_or_default();
            for m in model::catalog() {
                let mark = if m.id == current { "*" } else { " " };
                let lang = if m.multilingual { "multi" } else { "en" };
                println!(
                    "{mark} {:<24} {}d  ~{}MB  {:<5}  {}",
                    m.id, m.dims, m.disk_mb, lang, m.note
                );
            }
            Ok(())
        }
        ModelCmd::Set { id } => {
            if model::find(&id).is_none() {
                anyhow::bail!("unknown model '{id}'; see `ripsem model list`");
            }
            let mut config = Config::load(root)?;
            config.model = id.clone();
            config.save()?;
            println!("model set to {id} (run `ripsem index --force` to rebuild)");
            Ok(())
        }
        ModelCmd::Backend { name } => {
            let backend = match name.as_str() {
                "hash" => Backend::Hash,
                "fastembed" => Backend::Fastembed,
                other => anyhow::bail!("unknown backend '{other}' (hash | fastembed)"),
            };
            let mut config = Config::load(root)?;
            config.backend = backend;
            config.save()?;
            println!("backend set to {name}");
            Ok(())
        }
    }
}

fn cmd_install(root: &Path, target: InstallCmd) -> Result<()> {
    let (rel, name) = match target {
        InstallCmd::ClaudeCode => (".mcp.json", "Claude Code"),
        InstallCmd::Cursor => (".cursor/mcp.json", "Cursor"),
    };
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc: Value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    if !doc
        .get("mcpServers")
        .map(|v| v.is_object())
        .unwrap_or(false)
    {
        doc["mcpServers"] = json!({});
    }
    doc["mcpServers"]["ripsem"] = json!({
        "command": "ripsem",
        "args": ["mcp"]
    });

    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    println!("added ripsem to {}", show_path(&path));
    println!("restart {name} in this directory to pick it up");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_by_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn snippet_lines_respects_the_context_count() {
        let snippet = "one\ntwo\nthree\nfour";
        // N>0 returns the first N lines.
        assert_eq!(snippet_lines(snippet, 2), vec!["one", "two"]);
        // 0 returns nothing — just the location header prints.
        assert!(snippet_lines(snippet, 0).is_empty());
        // A request beyond the available lines is capped, never padded.
        assert_eq!(
            snippet_lines(snippet, 10),
            vec!["one", "two", "three", "four"]
        );
    }

    fn hit(score: f32) -> pipeline::SearchHit {
        pipeline::SearchHit {
            path: "a.rs".into(),
            start_line: 1,
            end_line: 2,
            score,
            snippet: "fn main() {}".into(),
        }
    }

    #[test]
    fn min_score_filters_by_threshold() {
        // No threshold keeps everything in order.
        let kept = apply_min_score(vec![hit(0.9), hit(0.5), hit(0.2)], None);
        assert_eq!(kept.len(), 3);
        // A threshold keeps hits at or above it and drops the rest.
        let kept = apply_min_score(vec![hit(0.9), hit(0.5), hit(0.2)], Some(0.5));
        let scores: Vec<f32> = kept.iter().map(|h| h.score).collect();
        assert_eq!(scores, vec![0.9, 0.5]);
        // An empty input stays empty whether or not a threshold is set.
        assert!(apply_min_score(vec![], Some(0.5)).is_empty());
    }

    #[test]
    fn strips_the_windows_verbatim_prefix() {
        assert_eq!(show_path(Path::new(r"\\?\C:\repo")), r"C:\repo");
        assert_eq!(show_path(Path::new("/home/user/repo")), "/home/user/repo");
    }

    #[test]
    fn install_writes_cursor_config_in_a_subdir() {
        let dir = crate::testutil::TempDir::new();
        cmd_install(dir.path(), InstallCmd::Cursor).unwrap();
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".cursor/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem");
        assert_eq!(doc["mcpServers"]["ripsem"]["args"][0], "mcp");
    }

    #[test]
    fn status_json_reports_a_loaded_index() {
        // A present index renders the versioned schema, the lowercase backend, and
        // the health facts (chunks/files/ann/stale) a tool or agent checks.
        let facts = IndexFacts {
            path: "/repo/.ripsem/index.bin".into(),
            size_bytes: 2048,
            chunks: 12,
            files: 3,
            ann: true,
            index_model: "bge-small-en-v1.5".into(),
            stale: false,
        };
        let out = status_json("bge-small-en-v1.5", Backend::Hash, 384, Some(&facts), None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["schema"], "ripsem/status/v1");
        assert_eq!(v["model"], "bge-small-en-v1.5");
        assert_eq!(v["backend"], "hash");
        assert_eq!(v["dims"], 384);
        assert_eq!(v["index"]["present"], true);
        assert_eq!(v["index"]["chunks"], 12);
        assert_eq!(v["index"]["files"], 3);
        assert_eq!(v["index"]["ann"], true);
        assert_eq!(v["index"]["stale"], false);
    }

    #[test]
    fn status_json_reports_a_missing_index_with_null_error() {
        // No index file ⇒ present:false and a null error (distinct from unreadable).
        let out = status_json("m", Backend::Fastembed, 768, None, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["backend"], "fastembed");
        assert_eq!(v["index"]["present"], false);
        assert!(v["index"]["error"].is_null());
    }

    #[test]
    fn status_json_reports_an_unreadable_index_with_the_error() {
        // A corrupt/unreadable index ⇒ present:false carrying the error string, so
        // a caller can tell "never built" from "broke".
        let out = status_json("m", Backend::Hash, 384, None, Some("bad magic"));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["index"]["present"], false);
        assert_eq!(v["index"]["error"], "bad magic");
    }

    #[test]
    fn install_preserves_existing_servers() {
        let dir = crate::testutil::TempDir::new();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(doc["mcpServers"]["other"]["command"], "x");
        assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem");
    }
}
