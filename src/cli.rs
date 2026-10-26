use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::config::{Backend, Config};
use crate::index::{self, Index};
use crate::{embed, mcp, model, pipeline, walk, watch};

/// Prints a line to stdout. A reader that has gone away, as `head` does once it
/// has what it wants, ends the program quietly instead of panicking.
macro_rules! out {
    () => {
        write_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        write_line(format_args!($($arg)*))
    };
}

fn write_line(line: std::fmt::Arguments) {
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout
        .write_fmt(line)
        .and_then(|()| stdout.write_all(b"\n"))
    {
        stdout_failed(&e);
    }
}

fn stdout_failed(e: &std::io::Error) -> ! {
    if e.kind() != std::io::ErrorKind::BrokenPipe {
        eprintln!("Error: failed printing to stdout: {e}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

fn limit(text: &str) -> Result<usize, String> {
    match text.parse::<usize>() {
        Ok(0) | Err(_) => Err("must be a whole number, 1 or more".to_string()),
        Ok(n) => Ok(n),
    }
}

fn score(text: &str) -> Result<f32, String> {
    match text.parse::<f32>() {
        Ok(score) if (0.0..=1.0).contains(&score) => Ok(score),
        _ => Err("must be a number from 0 to 1".to_string()),
    }
}

#[derive(Parser)]
#[command(name = "ripsem", version, about = "Local semantic code search")]
pub struct Cli {
    /// Project directory. Defaults to the nearest parent of the current
    /// directory that holds a `.ripsem` directory, or the current directory.
    #[arg(long, global = true)]
    pub path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create the local config for this directory.
    Init,
    /// Build or update the index for this directory.
    Index {
        /// Embed every file again instead of reusing the unchanged ones.
        #[arg(long)]
        force: bool,
        /// List the files that would be indexed and exit, without building
        /// embeddings or writing the index. Useful for checking which
        /// ignore/exclude/config patterns are in effect.
        #[arg(long)]
        dry_run: bool,
    },
    /// Find code by an identifier, an error message or a description.
    Search {
        /// What to look for, in any of those shapes.
        query: String,
        /// Number of results.
        #[arg(short = 'k', long, default_value_t = 10, value_parser = limit)]
        limit: usize,
        /// Print the results as JSON.
        #[arg(long)]
        json: bool,
        /// Limit results to a subdirectory (relative to the repo root), e.g.
        /// `--in src/embed`. `.` or empty searches the whole index; a path
        /// outside the repository is an error.
        #[arg(long = "in", value_name = "PREFIX")]
        within: Option<String>,
        /// Number of snippet lines to print per hit in the human output: 3 in
        /// a terminal, and the whole snippet when the output goes to another
        /// program, such as an agent's shell tool. `0` prints just the
        /// location header with no snippet body. Ignored by `--json`, which
        /// always returns the whole snippet.
        #[arg(long, value_name = "LINES")]
        context: Option<usize>,
        /// Drop hits scoring below this, keeping only strong matches. A score
        /// runs from 0, no closer than unrelated text in this repository, to
        /// 1, every term of the query found literally or an identical text,
        /// and falls with rank, so this keeps the top of the ranking. Absent
        /// means no filtering. Applies to both the human and `--json` output.
        #[arg(long, value_name = "SCORE", value_parser = score)]
        min_score: Option<f32>,
        /// Summarise as one line per file with its hit count (sorted by count),
        /// instead of listing each hit. Aggregates the top-`k` matches, so pair
        /// it with a larger `-k` for a wider view. Honoured by `--json` too.
        #[arg(long)]
        count_by_file: bool,
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
    /// Print the completion script for a shell.
    Completions {
        /// bash, zsh, fish, powershell or elvish.
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
pub enum ModelCmd {
    /// List the available embedding models.
    List,
    /// Set the embedding model for this directory.
    Set {
        /// A model id from `ripsem model list`.
        id: String,
    },
    /// Set the embedding backend (hash or fastembed).
    Backend {
        /// `hash` or `fastembed`.
        name: String,
    },
}

#[derive(Subcommand)]
pub enum InstallCmd {
    /// Add ripsem to .mcp.json for Claude Code.
    ClaudeCode,
    /// Add ripsem to .cursor/mcp.json for Cursor.
    Cursor,
}

pub fn run(cli: Cli) -> Result<()> {
    let root = match &cli.path {
        Some(path) => {
            let root = path
                .canonicalize()
                .map_err(|_| anyhow::anyhow!("no such directory: {}", path.display()))?;
            anyhow::ensure!(root.is_dir(), "not a directory: {}", path.display());
            root
        }
        None => {
            let here = Path::new(".").canonicalize().unwrap_or_else(|_| ".".into());
            match cli.command {
                Command::Init => here,
                _ => Config::find_root(&here),
            }
        }
    };
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
            count_by_file,
        } => cmd_search(
            &root,
            &query,
            limit,
            json,
            within.as_deref(),
            context,
            min_score,
            count_by_file,
        ),
        Command::Watch => watch::run(Config::load(&root)?),
        Command::Status { json } => cmd_status(&root, json),
        Command::Mcp => mcp::serve(Config::load(&root)?, cli.path.is_some()),
        Command::Model { action } => cmd_model(&root, action),
        Command::Install { target } => cmd_install(&root, target),
        Command::Completions { shell } => {
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "ripsem", &mut script);
            if let Err(e) = std::io::stdout().write_all(&script) {
                stdout_failed(&e);
            }
            Ok(())
        }
    }
}

/// Writes the default config, or keeps the one already there: it holds the
/// model the index was built with and the user's excludes.
fn cmd_init(root: &Path) -> Result<()> {
    let config = Config::load(root)?;
    let path = show_path(&config.config_path());
    if config.config_path().exists() {
        config.create_data_dir()?;
        out!("kept {path}, which already exists");
    } else {
        config.save()?;
        out!("wrote {path}");
    }
    out!("model: {} ({:?} backend)", config.model, config.backend);
    warn_about_backend(&config);
    Ok(())
}

/// Hashing-backend results look like any others, so every command that builds
/// or reads vectors says on stderr that they match words, not meaning.
fn warn_about_backend(config: &Config) {
    if let Some(warning) = embed::backend_warning(config.backend) {
        eprintln!("warning: {warning}");
    }
}

/// Index-health facts gathered for the `status` output (both human and JSON).
struct IndexFacts {
    path: String,
    size_bytes: u64,
    chunks: usize,
    files: usize,
    named: usize,
    ann: bool,
    index_backend: Backend,
    index_model: String,
    stale: bool,
    /// Built under other chunk settings, so `ripsem index` cuts every file
    /// again.
    rechunk: bool,
    /// `None` when the tree could not be walked to compare against.
    drift: Option<pipeline::Drift>,
}

/// The `tree:` line of `status`: what rebuilding would touch, or that it would
/// touch nothing. Counts are files, and only the non-zero kinds are named.
fn drift_line(drift: &pipeline::Drift) -> String {
    if drift.is_current() {
        return "matches the index".to_string();
    }
    let counted: Vec<String> = [
        (drift.modified, "changed"),
        (drift.added, "new"),
        (drift.removed, "gone"),
    ]
    .into_iter()
    .filter(|&(count, _)| count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect();
    format!("out of date: {} — run `ripsem index`", counted.join(", "))
}

/// Renders `status` as JSON (schema `ripsem/status/v1`). `index` is `None` when
/// the index is missing or unreadable; `error` is set only for unreadable.
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
            "named": f.named,
            "ann": f.ann,
            "backend": f.index_backend,
            "model": f.index_model,
            "stale": f.stale,
            "drift": match f.drift {
                Some(d) => json!({
                    "added": d.added,
                    "modified": d.modified,
                    "removed": d.removed,
                }),
                None => Value::Null,
            },
        }),
        None => json!({ "present": false, "error": error }),
    };
    let doc = json!({
        "schema": "ripsem/status/v1",
        "model": model,
        "backend": backend,
        "dims": dims,
        "warning": embed::backend_warning(backend),
        "index": index_json,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

fn cmd_status(root: &Path, as_json: bool) -> Result<()> {
    let config = Config::load(root)?;
    let path = config.index_path();

    // A missing file and an unreadable index both yield no facts; only the
    // latter carries an error.
    let (facts, error) = if !path.exists() {
        (None, None)
    } else {
        match Index::load(&path) {
            Ok(index) => {
                let files: std::collections::HashSet<&str> =
                    index.records.iter().map(|r| r.path.as_str()).collect();
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let drift = pipeline::drift(&config, &index).ok();
                let facts = IndexFacts {
                    path: show_path(&path),
                    size_bytes: size,
                    chunks: index.records.len(),
                    files: files.len(),
                    named: index.records.iter().filter(|r| r.symbol.is_some()).count(),
                    ann: index.ann.is_some(),
                    index_backend: index.backend,
                    index_model: index.model.clone(),
                    stale: !index.built_for(config.backend, &config.model),
                    rechunk: index.chunking != crate::chunk::Chunking::of(&config),
                    drift,
                };
                (Some(facts), None)
            }
            Err(e) => (None, Some(e.to_string())),
        }
    };

    if as_json {
        out!(
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

    out!(
        "model:  {} ({:?} backend, {} dims)",
        config.model,
        config.backend,
        config.dims()
    );
    match (&facts, &error) {
        (None, None) => out!("index:  missing — run `ripsem index`"),
        (None, Some(e)) => out!("index:  unreadable ({e})"),
        (Some(f), _) => {
            out!("index:  {} ({})", f.path, human_size(f.size_bytes));
            out!("chunks: {} across {}", f.chunks, plural(f.files, "file"));
            out!("named:  {} with a declaration", plural(f.named, "chunk"));
            if let Some(drift) = &f.drift {
                out!("tree:   {}", drift_line(drift));
            }
            if f.ann {
                out!("ann:    present (approximate search)");
            }
            if f.rechunk {
                out!(
                    "note:   chunk settings differ from the ones the index was built with; \
                     `ripsem index` chunks every file again"
                );
            }
            if f.stale {
                out!(
                    "note:   index built with {}, config wants {}; run `ripsem index --force`",
                    index::embedded_by(f.index_backend, &f.index_model),
                    index::embedded_by(config.backend, &config.model)
                );
            }
        }
    }
    warn_about_backend(&config);
    Ok(())
}

/// `n` with its noun, in the plural unless `n` is 1: "1 file", "3 files".
pub(crate) fn plural(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        _ => format!("{n} {noun}s"),
    }
}

pub(crate) fn show_path(path: &Path) -> String {
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
            out!("{}", pipeline::relative(&config.root, path));
        }
        out!("{} would be indexed", plural(files.len(), "file"));
        return Ok(());
    }
    let stats = pipeline::index_repo(&config, force)?;
    out!(
        "{}: {} changed, {} unchanged -> {}, {} embedded",
        plural(stats.files, "file"),
        stats.embedded,
        stats.reused,
        plural(stats.chunks, "chunk"),
        stats.vectors
    );
    warn_about_backend(&config);
    Ok(())
}

/// A score to three places for the human output. One that rounds to zero
/// prints as `0.000`, never `-0.000`.
fn show_score(score: f32) -> String {
    let shown = format!("{score:.3}");
    match shown.strip_prefix('-') {
        Some(digits) if digits.bytes().all(|b| b == b'0' || b == b'.') => digits.to_string(),
        _ => shown,
    }
}

/// The first `context` lines of a snippet for the human output.
fn snippet_lines(snippet: &str, context: usize) -> Vec<&str> {
    snippet.lines().take(context).collect()
}

/// Width a snippet line is broken at in the human output. A doc record is one
/// run of prose, which a terminal would otherwise split in the middle of a word.
const WRAP: usize = 96;

/// Breaks a line longer than [`WRAP`] at spaces, keeping its indentation on
/// every piece. Shorter lines are left exactly as they are.
fn wrap(line: &str) -> Vec<String> {
    if line.chars().count() <= WRAP {
        return vec![line.to_string()];
    }
    let indent = &line[..line.len() - line.trim_start().len()];
    let mut pieces = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let width = indent.len() + current.chars().count() + 1 + word.chars().count();
        if !current.is_empty() && width > WRAP {
            pieces.push(format!("{indent}{current}"));
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    pieces.push(format!("{indent}{current}"));
    pieces
}

/// Drops hits scoring below `min`; `None` keeps everything.
fn apply_min_score(hits: Vec<pipeline::SearchHit>, min: Option<f32>) -> Vec<pipeline::SearchHit> {
    match min {
        Some(min) => hits.into_iter().filter(|h| h.score >= min).collect(),
        None => hits,
    }
}

/// Counts hits per file for `--count-by-file`, most hits first, then by path.
fn file_hit_counts(hits: &[pipeline::SearchHit]) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for h in hits {
        *counts.entry(h.path.as_str()).or_insert(0) += 1;
    }
    let mut pairs: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(p, c)| (p.to_string(), c))
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

#[allow(clippy::too_many_arguments)]
fn cmd_search(
    root: &Path,
    query: &str,
    limit: usize,
    as_json: bool,
    within: Option<&str>,
    context: Option<usize>,
    min_score: Option<f32>,
    count_by_file: bool,
) -> Result<()> {
    let context = context.unwrap_or(match std::io::stdout().is_terminal() {
        true => 3,
        false => usize::MAX,
    });
    let config = Config::load(root)?;
    // Same normalisation as the MCP `search` tool.
    let prefix = match within {
        Some(raw) => mcp::scope_prefix(raw, &config.root)?,
        None => None,
    };
    let hits = pipeline::search_scoped(&config, query, limit, prefix.as_deref())?;
    let hits = apply_min_score(hits, min_score);
    print_hits(&hits, as_json, context, count_by_file)?;
    warn_about_backend(&config);
    Ok(())
}

/// Prints search results in the shape the flags ask for.
fn print_hits(
    hits: &[pipeline::SearchHit],
    as_json: bool,
    context: usize,
    count_by_file: bool,
) -> Result<()> {
    if count_by_file {
        let counts = file_hit_counts(hits);
        if as_json {
            let rows: Vec<Value> = counts
                .into_iter()
                .map(|(path, count)| json!({ "path": path, "count": count }))
                .collect();
            out!("{}", serde_json::to_string_pretty(&rows)?);
        } else if counts.is_empty() {
            out!("no matches");
        } else {
            for (path, count) in counts {
                out!("{count:>5}  {path}");
            }
        }
        return Ok(());
    }

    if as_json {
        let rows: Vec<Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "path": h.path,
                    "start_line": h.start_line,
                    "end_line": h.end_line,
                    "score": h.score,
                    "kind": h.symbol.as_ref().map(|s| &s.kind),
                    "symbol": h.symbol.as_ref().map(|s| &s.name),
                    "snippet": h.snippet,
                })
            })
            .collect();
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if hits.is_empty() {
        out!("no matches");
        return Ok(());
    }
    for h in hits {
        match &h.symbol {
            Some(s) => out!(
                "{}:{}-{}  {}  ({})",
                h.path,
                h.start_line,
                h.end_line,
                s.label(),
                show_score(h.score)
            ),
            None => out!(
                "{}:{}-{}  ({})",
                h.path,
                h.start_line,
                h.end_line,
                show_score(h.score)
            ),
        }
        for l in snippet_lines(&h.snippet, context) {
            for piece in wrap(l) {
                out!("    {piece}");
            }
        }
        out!();
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
                let disk = format!("~{}MB", m.disk_mb);
                out!(
                    "{mark} {:<24} {}d  {disk:<6}  {lang:<5}  {}",
                    m.id,
                    m.dims,
                    m.note
                );
            }
            Ok(())
        }
        ModelCmd::Set { id } => {
            if model::find(&id).is_none() {
                anyhow::bail!("unknown model '{id}'; see `ripsem model list`");
            }
            let mut config = Config::load(root)?;
            if config.model == id && config.config_path().exists() {
                out!("model is already {id}; the index needs no rebuild");
                return Ok(());
            }
            config.model = id.clone();
            config.save()?;
            out!("model set to {id} (run `ripsem index --force` to rebuild)");
            Ok(())
        }
        ModelCmd::Backend { name } => {
            let backend = match name.as_str() {
                "hash" => Backend::Hash,
                "fastembed" => Backend::Fastembed,
                other => anyhow::bail!("unknown backend '{other}' (hash | fastembed)"),
            };
            let mut config = Config::load(root)?;
            if config.backend == backend && config.config_path().exists() {
                out!("backend is already {name}; the index needs no rebuild");
                return Ok(());
            }
            config.backend = backend;
            config.save()?;
            out!("backend set to {name} (run `ripsem index --force` to rebuild)");
            Ok(())
        }
    }
}

/// Writes the config as `init` does, because a `.ripsem` directory is what lets
/// the server keep the index current, then the server entry and the note.
fn cmd_install(root: &Path, target: InstallCmd) -> Result<()> {
    let config = Config::load(root)?;
    if !config.config_path().exists() {
        config.save()?;
    }
    let name = match target {
        InstallCmd::ClaudeCode => "Claude Code",
        InstallCmd::Cursor => "Cursor",
    };
    add_server(root, &target)?;
    add_note(root, &target)?;
    out!("restart {name} in this directory to pick it up");
    Ok(())
}

fn add_note(root: &Path, target: &InstallCmd) -> Result<()> {
    let (path, note) = match target {
        InstallCmd::ClaudeCode => (
            root.join("CLAUDE.md"),
            format!("## Code search\n\n{}\n", mcp::INSTRUCTIONS),
        ),
        InstallCmd::Cursor => (
            root.join(".cursor/rules/ripsem.mdc"),
            format!(
                "---\ndescription: Finding code in this repository\nalwaysApply: true\n---\n\n{}\n",
                mcp::INSTRUCTIONS
            ),
        ),
    };
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", show_path(&path))),
    };
    if existing.contains("ripsem") {
        out!(
            "{} already mentions ripsem; left as it is",
            show_path(&path)
        );
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = match existing.trim().is_empty() {
        true => note,
        false => format!("{}\n\n{note}", existing.trim_end()),
    };
    std::fs::write(&path, text)?;
    out!("added a note on when to use it to {}", show_path(&path));
    Ok(())
}

fn add_server(root: &Path, target: &InstallCmd) -> Result<()> {
    let rel = match target {
        InstallCmd::ClaudeCode => ".mcp.json",
        InstallCmd::Cursor => ".cursor/mcp.json",
    };
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // The file usually lists other servers. If it is not a readable JSON object
    // it is left alone, since a fresh file would drop them.
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", show_path(&path))),
    };
    // Windows PowerShell writes a byte-order mark that JSON does not allow.
    let text = text.trim_start_matches('\u{feff}');
    let doc: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).with_context(|| {
            format!(
                "{} is not valid JSON; fix it and run this again",
                show_path(&path)
            )
        })?
    };
    if !doc.is_object() {
        anyhow::bail!(
            "{} does not hold a JSON object; fix it and run this again",
            show_path(&path)
        );
    }

    match doc.get("mcpServers") {
        None => {}
        Some(servers) if !servers.is_object() => anyhow::bail!(
            "mcpServers in {} is not a JSON object; fix it and run this again",
            show_path(&path)
        ),
        // An entry already there may carry a --path or env the user added.
        Some(servers) if servers.get("ripsem").is_some() => {
            out!("ripsem is already in {}; left as it is", show_path(&path));
            return Ok(());
        }
        Some(_) => {}
    }
    // Claude Code starts a project's server in the project. Cursor does not
    // say where it starts one, but expands `${workspaceFolder}`, which keeps
    // the file free of this machine's paths.
    let args = match target {
        InstallCmd::ClaudeCode => json!(["mcp"]),
        InstallCmd::Cursor => json!(["mcp", "--path", "${workspaceFolder}"]),
    };
    let entry = json!({ "command": "ripsem", "args": args });

    let written = if text.trim().is_empty() {
        serde_json::to_string_pretty(&json!({ "mcpServers": { "ripsem": entry } }))? + "\n"
    } else {
        // Only text is added, so the other servers keep their order and layout.
        let top: HashMap<&str, &RawValue> = serde_json::from_str(text)?;
        match top.get("mcpServers") {
            Some(servers) => add_member(text, servers.get(), "ripsem", &entry),
            None => add_member(text, text.trim(), "mcpServers", &json!({ "ripsem": entry })),
        }
    };
    std::fs::write(&path, written)?;
    out!("added ripsem to {}", show_path(&path));
    Ok(())
}

/// `text` with `key: value` added as the last member of `object`, a JSON
/// object that is a slice of `text`. When the object spans several lines the
/// member goes on a line of its own, indented like its neighbours.
fn add_member(text: &str, object: &str, key: &str, value: &Value) -> String {
    let start = object.as_ptr() as usize - text.as_ptr() as usize;
    let inner = &object[1..object.len() - 1];
    let kept = inner.trim_end().len();
    let key = Value::from(key);
    let member = if object.contains('\n') {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let close = inner[kept..].rsplit('\n').next().unwrap_or("");
        let indent = match kept {
            0 => format!("{close}  "),
            _ => {
                let first = inner.split_once('\n').map_or("", |(_, rest)| rest);
                first[..first.len() - first.trim_start().len()].to_string()
            }
        };
        let unit = indent.strip_prefix(close).filter(|u| !u.is_empty());
        let mut pretty = Vec::new();
        let format = serde_json::ser::PrettyFormatter::with_indent(unit.unwrap_or("  ").as_bytes());
        let mut serializer = serde_json::Serializer::with_formatter(&mut pretty, format);
        serde::Serialize::serialize(value, &mut serializer).expect("a JSON value serializes");
        let pretty = String::from_utf8_lossy(&pretty).replace('\n', &format!("{newline}{indent}"));
        let comma = if kept == 0 { "" } else { "," };
        let tail = if kept == 0 {
            format!("{newline}{close}")
        } else {
            String::new()
        };
        format!("{comma}{newline}{indent}{key}: {pretty}{tail}")
    } else {
        let comma = if kept == 0 { "" } else { ", " };
        format!("{comma}{key}: {value}")
    };
    // Whitespace inside an empty object is replaced by the member's own.
    let skipped = if kept == 0 { inner.len() } else { 0 };
    let at = start + 1 + kept;
    let mut out = String::with_capacity(text.len() + member.len());
    out.push_str(&text[..at]);
    out.push_str(&member);
    out.push_str(&text[at + skipped..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_one_in_the_singular() {
        assert_eq!(plural(1, "chunk"), "1 chunk");
        assert_eq!(plural(0, "file"), "0 files");
        assert_eq!(plural(3, "file"), "3 files");
    }

    #[test]
    fn formats_sizes_by_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_score_that_rounds_to_zero_has_no_sign() {
        assert_eq!(show_score(-0.0), "0.000");
        assert_eq!(show_score(-0.0004), "0.000");
        assert_eq!(show_score(-0.25), "-0.250");
        assert_eq!(show_score(0.46884), "0.469");
    }

    #[test]
    fn a_long_line_is_broken_at_spaces_and_a_short_one_is_kept() {
        let prose = "Walk walks the router and all its sub-routers, calling walkFn for each route in the tree. The routes are walked in the order they were added.";
        let pieces = wrap(prose);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|p| p.chars().count() <= WRAP));
        assert_eq!(pieces.join(" "), prose);
        assert_eq!(wrap("    let  x = 1;"), vec!["    let  x = 1;"]);
    }

    #[test]
    fn snippet_lines_respects_the_context_count() {
        let snippet = "one\ntwo\nthree\nfour";
        assert_eq!(snippet_lines(snippet, 2), vec!["one", "two"]);
        assert!(snippet_lines(snippet, 0).is_empty());
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
            symbol: None,
        }
    }

    #[test]
    fn min_score_filters_by_threshold() {
        let kept = apply_min_score(vec![hit(0.9), hit(0.5), hit(0.2)], None);
        assert_eq!(kept.len(), 3);
        let kept = apply_min_score(vec![hit(0.9), hit(0.5), hit(0.2)], Some(0.5));
        let scores: Vec<f32> = kept.iter().map(|h| h.score).collect();
        assert_eq!(scores, vec![0.9, 0.5]);
        assert!(apply_min_score(vec![], Some(0.5)).is_empty());
    }

    #[test]
    fn file_hit_counts_aggregates_and_sorts() {
        let hit_at = |path: &str, score: f32| pipeline::SearchHit {
            path: path.into(),
            start_line: 1,
            end_line: 2,
            score,
            snippet: String::new(),
            symbol: None,
        };
        let hits = vec![
            hit_at("src/a.rs", 0.9),
            hit_at("src/b.rs", 0.8),
            hit_at("src/a.rs", 0.7),
            hit_at("src/c.rs", 0.6),
            hit_at("src/b.rs", 0.5),
        ];
        assert_eq!(
            file_hit_counts(&hits),
            vec![
                ("src/a.rs".to_string(), 2),
                ("src/b.rs".to_string(), 2),
                ("src/c.rs".to_string(), 1),
            ]
        );
        assert!(file_hit_counts(&[]).is_empty());
    }

    #[test]
    fn strips_the_windows_verbatim_prefix() {
        assert_eq!(show_path(Path::new(r"\\?\C:\repo")), r"C:\repo");
        assert_eq!(show_path(Path::new("/home/user/repo")), "/home/user/repo");
    }

    #[test]
    fn init_keeps_an_existing_config() {
        let dir = crate::testutil::TempDir::new();
        cmd_init(dir.path()).unwrap();
        assert!(dir.path().join(".ripsem/.gitignore").exists());

        let edited = "model = \"all-MiniLM-L6-v2\"\nexclude = [\"vendor/\"]\n";
        dir.write(".ripsem/config.toml", edited);
        std::fs::remove_file(dir.path().join(".ripsem/.gitignore")).unwrap();
        cmd_init(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".ripsem/config.toml")).unwrap(),
            edited
        );
        assert!(dir.path().join(".ripsem/.gitignore").exists());

        dir.write(".ripsem/config.toml", "model = [");
        assert!(cmd_init(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".ripsem/config.toml")).unwrap(),
            "model = ["
        );
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
        assert_eq!(
            doc["mcpServers"]["ripsem"]["args"],
            json!(["mcp", "--path", "${workspaceFolder}"])
        );
    }

    #[test]
    fn drift_line_names_only_what_differs() {
        let none = pipeline::Drift::default();
        assert_eq!(drift_line(&none), "matches the index");

        let some = pipeline::Drift {
            added: 2,
            modified: 1,
            removed: 0,
        };
        assert_eq!(
            drift_line(&some),
            "out of date: 1 changed, 2 new — run `ripsem index`"
        );

        let gone = pipeline::Drift {
            removed: 3,
            ..Default::default()
        };
        assert_eq!(
            drift_line(&gone),
            "out of date: 3 gone — run `ripsem index`"
        );
    }

    #[test]
    fn status_json_reports_a_loaded_index() {
        let facts = IndexFacts {
            path: "/repo/.ripsem/index.bin".into(),
            size_bytes: 2048,
            chunks: 12,
            files: 3,
            named: 9,
            ann: true,
            index_backend: Backend::Hash,
            index_model: "bge-small-en-v1.5".into(),
            stale: false,
            rechunk: false,
            drift: Some(pipeline::Drift {
                added: 1,
                modified: 2,
                removed: 0,
            }),
        };
        let out = status_json("bge-small-en-v1.5", Backend::Hash, 384, Some(&facts), None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["schema"], "ripsem/status/v1");
        assert_eq!(v["model"], "bge-small-en-v1.5");
        assert_eq!(v["backend"], "hash");
        assert_eq!(v["dims"], 384);
        assert!(
            v["warning"]
                .as_str()
                .is_some_and(|w| w.starts_with("hashing backend")),
            "{out}"
        );
        assert_eq!(v["index"]["present"], true);
        assert_eq!(v["index"]["chunks"], 12);
        assert_eq!(v["index"]["files"], 3);
        assert_eq!(v["index"]["named"], 9);
        assert_eq!(v["index"]["ann"], true);
        assert_eq!(v["index"]["backend"], "hash");
        assert_eq!(v["index"]["stale"], false);
        assert_eq!(v["index"]["drift"]["added"], 1);
        assert_eq!(v["index"]["drift"]["modified"], 2);
        assert_eq!(v["index"]["drift"]["removed"], 0);
    }

    #[test]
    fn status_json_reports_a_missing_index_with_null_error() {
        let out = status_json("m", Backend::Fastembed, 768, None, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["backend"], "fastembed");
        assert!(v["warning"].is_null());
        assert_eq!(v["index"]["present"], false);
        assert!(v["index"]["error"].is_null());
    }

    #[test]
    fn status_json_reports_an_unreadable_index_with_the_error() {
        let out = status_json("m", Backend::Hash, 384, None, Some("bad magic"));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["index"]["present"], false);
        assert_eq!(v["index"]["error"], "bad magic");
    }

    #[test]
    fn install_leaves_a_config_it_cannot_read_untouched() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join(".mcp.json");
        let broken = r#"{"mcpServers":{"other":{"command":"x"},}}"#;
        std::fs::write(&path, broken).unwrap();
        let err = cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);

        for unreadable in ["[1, 2]", r#"{"mcpServers": null}"#] {
            std::fs::write(&path, unreadable).unwrap();
            let err = cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap_err();
            assert!(err.to_string().contains("JSON object"), "got: {err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), unreadable);
        }
    }

    #[test]
    fn install_keeps_a_ripsem_entry_already_there() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join(".mcp.json");
        let mine =
            r#"{"mcpServers":{"ripsem":{"command":"ripsem","args":["mcp","--path","/repo"]}}}"#;
        std::fs::write(&path, mine).unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
    }

    #[test]
    fn install_opts_the_directory_in_and_tells_the_agent_once() {
        let dir = crate::testutil::TempDir::new();
        let claude = dir.path().join("CLAUDE.md");
        std::fs::write(&claude, "# Project\n\nBuild with make.\n").unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();

        assert!(dir.path().join(".ripsem/config.toml").exists());
        let text = std::fs::read_to_string(&claude).unwrap();
        assert!(text.starts_with("# Project\n\nBuild with make.\n\n## Code search\n"));
        assert_eq!(text.matches("indexed by ripsem").count(), 1);
    }

    #[test]
    fn install_cursor_writes_an_always_applied_rule() {
        let dir = crate::testutil::TempDir::new();
        cmd_install(dir.path(), InstallCmd::Cursor).unwrap();
        let rule = std::fs::read_to_string(dir.path().join(".cursor/rules/ripsem.mdc")).unwrap();
        assert!(rule.starts_with("---\ndescription: "), "{rule}");
        assert!(rule.contains("alwaysApply: true") && rule.contains("`search` tool"));
    }

    #[test]
    fn install_keeps_the_servers_of_a_config_with_a_byte_order_mark() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join(".mcp.json");
        std::fs::write(
            &path,
            "\u{feff}{\"mcpServers\":{\"other\":{\"command\":\"x\"}}}",
        )
        .unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["mcpServers"]["other"]["command"], "x");
        assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem");
    }

    #[test]
    fn install_fills_an_empty_config() {
        let dir = crate::testutil::TempDir::new();
        std::fs::write(dir.path().join(".mcp.json"), "\n").unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        let text = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert!(text.ends_with("}\n"), "{text:?}");
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem");
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

    #[test]
    fn install_adds_its_entry_without_rewriting_the_rest() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join(".mcp.json");
        let before = concat!(
            "{\n",
            "    \"version\": 2,\n",
            "    \"mcpServers\": {\n",
            "        \"zeta\": { \"command\": \"z\" },\n",
            "        \"alpha\": {\"command\":\"a\"}\n",
            "    },\n",
            "    \"inputs\": []\n",
            "}\n",
        );
        std::fs::write(&path, before).unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "    \"version\": 2,\n    \"mcpServers\": {\n",
            "        \"zeta\": { \"command\": \"z\" },\n",
            "        \"alpha\": {\"command\":\"a\"},\n",
            "    },\n    \"inputs\": []\n}\n",
        ] {
            assert!(text.contains(kept), "{kept:?} in {text}");
        }
        assert!(
            text.contains("\n        \"ripsem\": {\n            \"args\""),
            "{text}"
        );
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem");

        std::fs::write(&path, "{\"b\":1,\"a\":[2]}").unwrap();
        cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\"b\":1,\"a\":[2],"), "{text}");
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem");
    }

    #[test]
    fn install_adds_its_entry_to_any_layout() {
        let dir = crate::testutil::TempDir::new();
        let path = dir.path().join(".mcp.json");
        for before in [
            "{}",
            "{\n}\n",
            "{\"mcpServers\":{}}",
            "{\n\t\"mcpServers\": {\n\t}\n}\n",
            "{\r\n  \"mcpServers\": {\r\n    \"a\": {}\r\n  }\r\n}\r\n",
            "{ \"x\": 1 }",
            "\u{feff}{\"mcpServers\": {\"a\": 1}}",
        ] {
            std::fs::write(&path, before).unwrap();
            cmd_install(dir.path(), InstallCmd::ClaudeCode).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let doc: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
            assert_eq!(doc["mcpServers"]["ripsem"]["command"], "ripsem", "{text}");
            if before.contains("\r\n") {
                assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
            }
        }
    }
}
