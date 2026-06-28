use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::config::{Backend, Config};
use crate::index::Index;
use crate::{mcp, model, pipeline, watch};

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
    },
    /// Search the index with a natural-language query.
    Search {
        query: String,
        #[arg(short = 'k', long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Index and keep the index in sync as files change.
    Watch,
    /// Show index health and configuration for this directory.
    Status,
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
        Command::Index { force } => cmd_index(&root, force),
        Command::Search { query, limit, json } => cmd_search(&root, &query, limit, json),
        Command::Watch => watch::run(Config::load(&root)?),
        Command::Status => cmd_status(&root),
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

fn cmd_status(root: &Path) -> Result<()> {
    let config = Config::load(root)?;
    println!(
        "model:  {} ({:?} backend, {} dims)",
        config.model,
        config.backend,
        config.dims()
    );

    let path = config.index_path();
    if !path.exists() {
        println!("index:  missing — run `ripsem index`");
        return Ok(());
    }
    let index = match Index::load(&path) {
        Ok(i) => i,
        Err(e) => {
            println!("index:  unreadable ({e})");
            return Ok(());
        }
    };

    let files: std::collections::HashSet<&str> =
        index.records.iter().map(|r| r.path.as_str()).collect();
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("index:  {} ({})", show_path(&path), human_size(bytes));
    println!(
        "chunks: {} across {} files",
        index.records.len(),
        files.len()
    );
    if index.ann.is_some() {
        println!("ann:    present (approximate search)");
    }
    if index.model != config.model {
        println!(
            "note:   index built with {}, config wants {}; run `ripsem index --force`",
            index.model, config.model
        );
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

fn cmd_index(root: &Path, force: bool) -> Result<()> {
    let config = Config::load(root)?;
    let stats = pipeline::index_repo(&config, force)?;
    println!(
        "{} files: {} embedded, {} unchanged -> {} chunks",
        stats.files, stats.embedded, stats.reused, stats.chunks
    );
    Ok(())
}

fn cmd_search(root: &Path, query: &str, limit: usize, as_json: bool) -> Result<()> {
    let config = Config::load(root)?;
    let hits = pipeline::search(&config, query, limit)?;

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
        for l in h.snippet.lines().take(3) {
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
