use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::config::{Backend, Config};
use crate::index::{self, Index};
use crate::scope::Scope;
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
    #[command(after_long_help = "Examples:
  ripsem init
  ripsem --path ../other-repo init")]
    Init,
    /// Build or update the index for this directory.
    #[command(after_long_help = "Examples:
  ripsem index
  ripsem index --force                re-embed every file
  ripsem index --dry-run              list what would be indexed, and why files are left out")]
    Index {
        /// Embed every file again instead of reusing the unchanged ones.
        #[arg(long)]
        force: bool,
        /// List the files that would be indexed, then count the files left out
        /// and why, and exit, without building embeddings or writing the index.
        /// Useful for checking which ignore/exclude/config patterns are in
        /// effect.
        #[arg(long)]
        dry_run: bool,
    },
    /// Find code by an identifier, an error message or a description.
    #[command(after_long_help = "Examples:
  ripsem search where is auth set up
  ripsem search retry on failure -k 5 --lang rust
  ripsem search verify_password -l -0 | xargs -0 wc -l
  ripsem search -e 'backoff|jitter' retry on failure
  ripsem search --since main 'retry on failure'
  echo 'parse_header' | ripsem search - --context 0")]
    Search {
        /// What to look for, in any of those shapes. Several words need no
        /// quotes: `ripsem search where is auth set up` is one query. `-`, or
        /// no words with stdin piped, reads the query from stdin instead, so a
        /// long question or an editor selection can be piped in.
        #[arg(num_args = 0..)]
        query: Vec<String>,
        #[command(flatten)]
        opts: SearchOpts,
    },
    /// Find the code nearest to a location already in the index.
    ///
    /// Takes `src/auth.rs:40-72`, a single line such as `src/auth.rs:40`, or a
    /// bare path for the whole file. It uses the stored vectors, so no model runs.
    #[command(after_long_help = "Examples:
  ripsem similar src/auth.rs:40-72
  ripsem similar src/auth.rs:40
  ripsem similar src/auth.rs --lang rust -k 5")]
    Similar {
        /// `path`, `path:LINE` or `path:START-END`, relative to the current
        /// directory. The lines are 1-based.
        target: String,
        #[command(flatten)]
        opts: SearchOpts,
    },
    /// List the declarations the index holds for a file, in order.
    #[command(after_long_help = "Examples:
  ripsem outline src/auth.rs
  ripsem outline src/auth.rs --json | jq -r '.[].symbol'")]
    Outline {
        /// The file, relative to the current directory.
        file: String,
        /// Print the declarations as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Index and keep the index in sync as files change.
    #[command(after_long_help = "Examples:
  ripsem watch
  ripsem watch --path ../other-repo")]
    Watch,
    /// Show index health and configuration for this directory.
    #[command(after_long_help = "Examples:
  ripsem status
  ripsem status --json | jq .index.drift")]
    Status {
        /// Emit machine-readable JSON (schema `ripsem/status/v1`) instead of the
        /// human summary — for tooling or an agent checking whether the index
        /// exists, is stale, or needs rebuilding.
        #[arg(long)]
        json: bool,
        /// Read every indexed file and list the ones whose content no longer
        /// matches the index, including a rewrite that kept its size and time.
        #[arg(long)]
        verify: bool,
    },
    /// Run the MCP server over stdio.
    #[command(after_long_help = "Examples:
  ripsem install claude-code      register this server with the agent
  ripsem mcp                      run by the agent; reads and writes stdio")]
    Mcp,
    /// Inspect and choose the embedding model and backend.
    #[command(after_long_help = "Examples:
  ripsem model list
  ripsem model set bge-base-en-v1.5
  ripsem model backend hash")]
    Model {
        #[command(subcommand)]
        action: ModelCmd,
    },
    /// Wire ripsem into a coding agent.
    #[command(after_long_help = "Examples:
  ripsem install claude-code
  ripsem install opencode")]
    Install {
        #[command(subcommand)]
        target: InstallCmd,
    },
    /// Print the completion script for a shell.
    #[command(after_long_help = "Examples:
  ripsem completions bash > ~/.local/share/bash-completion/completions/ripsem
  ripsem completions powershell >> $PROFILE")]
    Completions {
        /// bash, zsh, fish, powershell or elvish.
        shell: clap_complete::Shell,
    },
}

/// The options `search` and `similar` share: how many places, which filters
/// and how they are printed.
#[derive(clap::Args)]
pub struct SearchOpts {
    /// Number of results.
    #[arg(short = 'k', long, default_value_t = 10, value_parser = limit)]
    limit: usize,
    /// Skip this many results, taken from the ranking after every filter,
    /// to read the next page: the output says when more follow and what
    /// to pass.
    #[arg(long, default_value_t = 0, value_name = "N")]
    offset: usize,
    /// Print the results as JSON. Cannot be combined with `--vimgrep` or `-0`.
    #[arg(long)]
    json: bool,
    /// Colour the path and line numbers of each hit, and the query's words in
    /// its snippet: `auto` colours a terminal unless `NO_COLOR` is set,
    /// `always` and `never` decide it.
    #[arg(long, value_enum, default_value_t = ColorWhen::Auto, value_name = "WHEN")]
    color: ColorWhen,
    /// Limit results to a subdirectory, e.g. `--in src/embed`. A relative
    /// path starts from the current directory, as in the shell, so `.`
    /// is the directory you are in; a path outside the repository is an
    /// error.
    #[arg(long = "in", value_name = "PREFIX")]
    within: Option<String>,
    /// Keep only hits in files of this language, by name or extension:
    /// `--lang rust`, `--lang py`, `--lang toml`. Repeat the flag or
    /// separate values with commas for several. A leading `!` leaves a
    /// language out: `--lang '!json'`.
    #[arg(short = 't', long, alias = "type", value_name = "LANG")]
    lang: Vec<String>,
    /// Keep only hits whose path matches this glob, as with ripgrep's
    /// `-g`; a leading `!` leaves matching paths out. `-g '*.rs' -g
    /// '!*_test.rs'`. Repeat for several; a path counts when it or a
    /// directory above it matches.
    #[arg(short = 'g', long, value_name = "GLOB")]
    glob: Vec<String>,
    /// Keep only hits whose code matches this regular expression, to find
    /// the place that does something and also mentions a name: `-e
    /// 'retry|backoff'`, `-e '(?i)argon2'`. It is matched against each
    /// hit's whole chunk and tried on the best 200 places of the ranking,
    /// so it narrows an answer; to find every occurrence use grep.
    #[arg(short = 'e', long, value_name = "PATTERN")]
    regex: Option<String>,
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
    /// Cannot be combined with `-l` or `--vimgrep`.
    #[arg(short = 'c', long)]
    count_by_file: bool,
    /// Print nothing. The exit status says whether a hit is left after the
    /// filters and `--min-score`: 0 when one is, 1 when none is, so a
    /// script can branch on a match as with `grep -q`. Cannot be combined
    /// with `--vimgrep`.
    #[arg(short = 'q', long)]
    quiet: bool,
    /// List each file with a hit once, in the order of its best hit, instead
    /// of the hits. Like `--count-by-file` it reads the top `-k` places.
    /// With `--json` it prints a JSON array of the paths, as strings, rather
    /// than of hits. Cannot be combined with `--count-by-file` or `--vimgrep`.
    #[arg(short = 'l', long, conflicts_with = "count_by_file")]
    files_with_matches: bool,
    /// With `-l`, end each path with a NUL byte instead of a line break, for
    /// `xargs -0` and other readers of NUL-separated names, so a path with a
    /// space or a newline stays whole. Prints nothing when no file has a hit.
    /// Requires `-l` and cannot be combined with `--json`.
    #[arg(
        short = '0',
        long = "null",
        requires = "files_with_matches",
        conflicts_with = "json"
    )]
    null: bool,
    /// One `path:line:column:text` line per hit, the format editors load into
    /// a quickfix list and grep tools read: column is 1, and text is the
    /// hit's first non-blank line. Vim loads it with
    /// `:cexpr system('ripsem search --vimgrep retry')`. No colour, and no
    /// note about more results. Cannot be combined with `--json`, `-l`, `-q`
    /// or `--count-by-file`.
    #[arg(
        long,
        conflicts_with_all = ["json", "files_with_matches", "quiet", "count_by_file"]
    )]
    vimgrep: bool,
    /// The order of the hits: `score`, the ranking, or `path`, by file and then
    /// by line. `path` orders the hits of the page shown; `--offset` still
    /// reads the ranking by score.
    #[arg(long, value_enum, default_value_t = SortBy::Score, value_name = "ORDER")]
    sort: SortBy,
    /// Keep only hits in files that differ from this git revision, as the
    /// working tree has them: committed, staged and unstaged changes and
    /// untracked files. Useful for reviewing a branch, e.g. `--since main`.
    #[arg(long, value_name = "REV")]
    since: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SortBy {
    Score,
    Path,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

/// Whether hit headers get colour. `auto` colours a terminal, and a
/// `NO_COLOR` that is set to anything but an empty string turns it off.
fn use_color(when: ColorWhen, terminal: bool, no_color: bool) -> bool {
    match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => terminal && !no_color,
    }
}

/// `text` in the SGR code `code`, or `text` as it is when `color` is off.
fn paint(text: &str, code: &str, color: bool) -> String {
    match color {
        true => format!("\x1b[{code}m{text}\x1b[0m"),
        false => text.to_string(),
    }
}

/// `line` with each word of it that is one of the query's `words` painted,
/// compared without case. A word is a run of letters and digits, as the
/// query's words are cut, so `parse_config` holds `parse` and `config`.
fn highlight(line: &str, words: &[String], color: bool) -> String {
    if !color {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(char::is_alphanumeric) {
        let (before, from) = rest.split_at(start);
        let end = from
            .find(|c: char| !c.is_alphanumeric())
            .unwrap_or(from.len());
        let (word, after) = from.split_at(end);
        out.push_str(before);
        match words.contains(&word.to_lowercase()) {
            true => out.push_str(&paint(word, "1;31", true)),
            false => out.push_str(word),
        }
        rest = after;
    }
    out.push_str(rest);
    out
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
    /// Add ripsem to .vscode/mcp.json for GitHub Copilot in VS Code.
    Vscode,
    /// Add ripsem to .gemini/settings.json for Gemini CLI.
    Gemini,
    /// Add ripsem to opencode.json for OpenCode.
    #[command(name = "opencode", alias = "open-code")]
    OpenCode,
    /// Add ripsem to .codex/config.toml for Codex CLI.
    Codex,
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
        Command::Search { query, opts } => cmd_search(&root, &query, &opts),
        Command::Similar { target, opts } => cmd_similar(&root, &target, &opts),
        Command::Outline { file, json } => cmd_outline(&root, &file, json),
        Command::Watch => watch::run(load_config(&root)?),
        Command::Status { json, verify } => cmd_status(&root, json, verify),
        Command::Mcp => mcp::serve(load_config(&root)?, cli.path.is_some()),
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
    if let Some(warning) = shadowed_index_warning(root) {
        eprintln!("{warning}");
    }
    Ok(())
}

/// The warning `init` gives in a directory below one that already has an
/// index. Every command run here then searches the new `.ripsem` and misses
/// the rest of the project, so the warning names both.
fn shadowed_index_warning(root: &Path) -> Option<String> {
    let above = root
        .ancestors()
        .skip(1)
        .find(|dir| dir.join(".ripsem").is_dir())?;
    Some(format!(
        "warning: {} already indexes this directory. Commands run here will use {} instead, which holds only this directory. Remove that .ripsem directory to search the whole project again.",
        show_path(&above.join(".ripsem")),
        show_path(&root.join(".ripsem")),
    ))
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
    /// Seconds since the Unix epoch when the index file was last written.
    written: Option<u64>,
    /// `None` when the tree could not be walked to compare against.
    drift: Option<pipeline::Drift>,
    /// The indexed files whose content differs, read and hashed; `None`
    /// unless `status --verify` asked for them.
    changed: Option<Vec<String>>,
}

/// How long before `now` the index was written, in one unit of time.
fn written_ago(written: u64, now: u64) -> String {
    let secs = now.saturating_sub(written);
    match secs {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} ago", plural((secs / 86_400) as usize, "day")),
    }
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
    let doc = status_value(model, backend, dims, index, error);
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// The document `status --json` prints, for the MCP `index_status` tool.
pub(crate) fn status_report(config: &Config) -> Value {
    let (facts, error) = index_facts(config, false);
    status_value(
        &config.model,
        config.backend,
        config.dims(),
        facts.as_ref(),
        error.as_deref(),
    )
}

/// `index` with the files `status --verify` found changed, when it ran.
fn with_changed(mut index: Value, changed: Option<&[String]>) -> Value {
    if let Some(changed) = changed {
        index["changed_on_disk"] = json!(changed);
    }
    index
}

fn status_value(
    model: &str,
    backend: Backend,
    dims: usize,
    index: Option<&IndexFacts>,
    error: Option<&str>,
) -> Value {
    let index_json = match index {
        Some(f) => with_changed(
            json!({
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
                "written_unix": f.written,
                "drift": match f.drift {
                    Some(d) => json!({
                        "added": d.added,
                        "modified": d.modified,
                        "removed": d.removed,
                    }),
                    None => Value::Null,
                },
            }),
            f.changed.as_deref(),
        ),
        None => json!({ "present": false, "error": error }),
    };
    json!({
        "schema": "ripsem/status/v1",
        "model": model,
        "backend": backend,
        "dims": dims,
        "warning": embed::backend_warning(backend),
        "index": index_json,
    })
}

/// What the index file on disk holds, and how far the tree has drifted from
/// it. A missing file and an unreadable index both yield no facts; only the
/// latter carries an error.
fn index_facts(config: &Config, verify: bool) -> (Option<IndexFacts>, Option<String>) {
    let path = config.index_path();
    if !path.exists() {
        (None, None)
    } else {
        match Index::load(&path) {
            Ok(index) => {
                let files: std::collections::HashSet<&str> =
                    index.records.iter().map(|r| r.path.as_str()).collect();
                let meta = std::fs::metadata(&path).ok();
                let size = meta.as_ref().map_or(0, |m| m.len());
                let written = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let drift = pipeline::drift(config, &index).ok();
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
                    rechunk: index.chunking != crate::chunk::Chunking::of(config),
                    written,
                    drift,
                    changed: verify.then(|| pipeline::changed_on_disk(config, &index)),
                };
                (Some(facts), None)
            }
            Err(e) => (None, Some(e.to_string())),
        }
    }
}

fn cmd_status(root: &Path, as_json: bool, verify: bool) -> Result<()> {
    let config = load_config(root)?;
    let (facts, error) = index_facts(&config, verify);

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
            if let Some(changed) = &f.changed {
                match changed.is_empty() {
                    true => out!("verify: every indexed file matches its content"),
                    false => {
                        out!(
                            "verify: {} changed on disk since indexed; run `ripsem index --force`",
                            plural(changed.len(), "file")
                        );
                        for path in changed {
                            out!("  {path}");
                        }
                    }
                }
            }
            if let Some(written) = f.written {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(written, |d| d.as_secs());
                out!("built:  {}", written_ago(written, now));
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

/// Text from the repository as a terminal shows it. A control character, such
/// as the escape that starts a sequence for the terminal to act on, is written
/// as `\u{1b}`; a tab is left as it is.
pub(crate) fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\t') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c.is_control() && c != '\t' {
            true => out.extend(c.escape_unicode()),
            false => out.push(c),
        }
    }
    Cow::Owned(out)
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

/// [`Config::load`], with the settings the environment can turn on.
fn load_config(root: &Path) -> Result<Config> {
    let mut config = Config::load(root)?;
    config.hash_all = std::env::var_os("RIPSEM_HASH_ALL").is_some_and(|v| v == "1");
    Ok(config)
}

fn cmd_index(root: &Path, force: bool, dry_run: bool) -> Result<()> {
    let config = load_config(root)?;
    if dry_run {
        let (files, counts) = walk::tally(&config)?;
        for path in &files {
            out!("{}", pipeline::relative(&config.root, path));
        }
        out!("{} would be indexed", plural(files.len(), "file"));
        if let Some(left) = left_out_line(&counts) {
            out!("{} seen, {left}", plural(counts.seen(), "file"));
        }
        return Ok(());
    }
    let stats = pipeline::index_repo_once(&config, force)?;
    out!(
        "{}: {} changed, {} unchanged -> {}, {} embedded",
        plural(stats.files, "file"),
        stats.embedded,
        stats.reused,
        plural(stats.chunks, "chunk"),
        stats.vectors
    );
    if !stats.unreadable.is_empty() {
        eprintln!("warning: {}", skipped_line(&stats.unreadable));
    }
    warn_about_backend(&config);
    Ok(())
}

/// The unreadable files a run skipped, as one line naming the first three.
fn skipped_line(paths: &[String]) -> String {
    const SHOWN: usize = 3;
    let shown = paths[..paths.len().min(SHOWN)]
        .iter()
        .map(|path| printable(path).into_owned())
        .collect::<Vec<_>>()
        .join(", ");
    let more = paths.len().saturating_sub(SHOWN);
    let what = plural(paths.len(), "unreadable file");
    match more {
        0 => format!("skipped {what}: {shown}"),
        _ => format!("skipped {what}: {shown}, and {more} more"),
    }
}

/// The files a run left out and why, as one line; `None` when it left none out.
fn left_out_line(counts: &walk::Tally) -> Option<String> {
    let reasons: Vec<String> = [
        (counts.binary, "binary"),
        (counts.minified, "minified"),
        (counts.blank, "blank"),
        (counts.unreadable, "unreadable"),
        (counts.too_large, "over max_file_kb"),
        (counts.excluded, "excluded by pattern"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, why)| format!("{n} {why}"))
    .collect();
    (!reasons.is_empty()).then(|| format!("left out: {}", reasons.join(", ")))
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

/// `context` lines of a snippet for the human output: its first ones, or,
/// when the line naming most of the query's words comes later, the lines
/// around that one.
fn snippet_lines<'a>(snippet: &'a str, context: usize, words: &[String]) -> Vec<&'a str> {
    let lines: Vec<&str> = snippet.lines().collect();
    if context == 0 || lines.len() <= context {
        return lines.into_iter().take(context).collect();
    }
    let best = pipeline::best_line(&lines, words).unwrap_or(0);
    let start = match best < context {
        true => 0,
        false => (best - (context - 1) / 2).min(lines.len() - context),
    };
    lines[start..start + context].to_vec()
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

/// The line of a hit's snippet that holds its first text, with its number in
/// the file and the line trimmed. A snippet with no text names the hit's first
/// line with an empty text.
fn first_text_line(hit: &pipeline::SearchHit) -> (usize, &str) {
    hit.snippet
        .lines()
        .enumerate()
        .find(|(_, line)| !line.trim().is_empty())
        .map_or((hit.start_line, ""), |(i, line)| {
            (hit.start_line + i, line.trim())
        })
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

/// The query as the words given, or as stdin when it is `-` or absent and
/// stdin is not a terminal. One trailing line break is not part of it.
fn query_text(words: &[String]) -> Result<String> {
    let from_stdin = match words {
        [dash] => dash == "-",
        [] => !std::io::stdin().is_terminal(),
        _ => false,
    };
    if from_stdin {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("reading the query from stdin")?;
        if let Some(line) = text.strip_suffix('\n') {
            text = line.strip_suffix('\r').unwrap_or(line).to_string();
        }
        return Ok(text);
    }
    if words.is_empty() {
        anyhow::bail!("no query: give the words after `search`, or pipe one in on stdin");
    }
    Ok(words.join(" "))
}

fn context_lines(opts: &SearchOpts) -> usize {
    opts.context
        .unwrap_or(match std::io::stdout().is_terminal() {
            true => 3,
            false => usize::MAX,
        })
}

fn cmd_search(root: &Path, words: &[String], opts: &SearchOpts) -> Result<()> {
    let query = query_text(words)?;
    let text = opts
        .regex
        .as_deref()
        .map(crate::scope::text_pattern)
        .transpose()?;
    let config = load_config(root)?;
    let scope = scope_for(&config, opts)?;
    let page = pipeline::search_page(
        &config,
        &query,
        opts.limit,
        opts.offset,
        scope.as_ref(),
        text.as_ref(),
    )?;
    show(
        page,
        opts,
        context_lines(opts),
        &pipeline::query_words(&query),
    )?;
    warn_about_backend(&config);
    Ok(())
}

fn cmd_similar(root: &Path, target: &str, opts: &SearchOpts) -> Result<()> {
    let config = load_config(root)?;
    let (raw, lines) = parse_target(target)?;
    anyhow::ensure!(
        !config.root.join(from_here(raw, &config.root)).is_dir(),
        "{raw} is a directory; `similar` takes a file, or a line range in one such as src/auth.rs:40-72"
    );
    let path = mcp::scope_prefix(&from_here(raw, &config.root), &config.root)?
        .ok_or_else(|| anyhow::anyhow!("'{target}' names no file"))?;
    let text = opts
        .regex
        .as_deref()
        .map(crate::scope::text_pattern)
        .transpose()?;
    let scope = scope_for(&config, opts)?;
    let target = pipeline::Target { path, lines };
    let page = pipeline::similar_page(
        &config,
        &target,
        opts.limit,
        opts.offset,
        scope.as_ref(),
        text.as_ref(),
    )
    .map_err(|e| similar_line_hint(raw, e))?;
    show(page, opts, context_lines(opts), &[])?;
    warn_about_backend(&config);
    Ok(())
}

fn cmd_outline(root: &Path, raw: &str, as_json: bool) -> Result<()> {
    let config = load_config(root)?;
    anyhow::ensure!(
        !config.root.join(from_here(raw, &config.root)).is_dir(),
        "{raw} is a directory; `outline` takes a file"
    );
    let path = mcp::scope_prefix(&from_here(raw, &config.root), &config.root)?
        .ok_or_else(|| anyhow::anyhow!("'{raw}' names no file"))?;
    let declarations = pipeline::outline(&config, &path)?;
    if as_json {
        let rows: Vec<Value> = declarations
            .iter()
            .map(|d| {
                json!({
                    "kind": d.symbol.kind,
                    "symbol": d.symbol.name,
                    "start_line": d.start_line,
                    "end_line": d.end_line,
                })
            })
            .collect();
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if declarations.is_empty() {
        out!("no declarations in {path}");
    }
    for d in &declarations {
        out!("{}-{}  {}", d.start_line, d.end_line, d.symbol.label());
    }
    Ok(())
}

/// When the part after the last colon is not a line number and the path is not
/// in the index, says what a line range looks like: a colon in a path is
/// otherwise taken for a path that is simply missing.
fn similar_line_hint(raw: &str, err: anyhow::Error) -> anyhow::Error {
    let asks_for_lines = raw
        .rsplit_once(':')
        .is_some_and(|(_, spec)| !spec.is_empty() && !is_line_spec(spec));
    if asks_for_lines && err.to_string().contains("is not in the index") {
        anyhow::anyhow!(
            "{err}; a line range is PATH:LINE or PATH:LINE-LINE, such as src/auth.rs:40-72"
        )
    } else {
        err
    }
}

/// Splits `path`, `path:LINE` or `path:START-END`. A colon that is not
/// followed by line numbers belongs to the path.
fn parse_target(raw: &str) -> Result<(&str, Option<(usize, usize)>)> {
    let (path, spec) = match raw.rsplit_once(':') {
        Some((path, spec)) if !path.is_empty() && is_line_spec(spec) => (path, spec),
        _ => return Ok((raw, None)),
    };
    let (first, last) = spec.split_once('-').unwrap_or((spec, spec));
    let number = |s: &str| {
        s.parse::<usize>()
            .map_err(|_| anyhow::anyhow!("'{s}' is not a line number"))
    };
    let (first, last) = line_range(raw, number(first)?, number(last)?)?;
    Ok((path, Some((first, last))))
}

/// `LINE` or `START-END`, both made of digits and neither empty.
fn is_line_spec(spec: &str) -> bool {
    let parts: Vec<&str> = spec.split('-').collect();
    parts.len() <= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A 1-based line range as `similar` takes it, from `raw` as the user wrote it.
pub(crate) fn line_range(raw: &str, first: usize, last: usize) -> Result<(usize, usize)> {
    anyhow::ensure!(first >= 1, "line numbers start at 1, not 0");
    anyhow::ensure!(
        last >= first,
        "line {last} comes before line {first} in '{raw}'"
    );
    Ok((first, last))
}

/// The filters the options ask for, as one scope. `None` when nothing limits
/// the search.
fn scope_for(config: &Config, opts: &SearchOpts) -> Result<Option<Scope>> {
    // Same normalisation as the MCP `search` tool.
    let prefix = match &opts.within {
        Some(raw) => mcp::scope_prefix(&from_here(raw, &config.root), &config.root)?,
        None => None,
    };
    let scope = Scope::new(prefix.as_deref(), &opts.lang, &opts.glob)?;
    match &opts.since {
        Some(rev) => Scope::since_rev(scope, &config.root, rev),
        None => Ok(scope),
    }
}

/// Prints one page of results as the options ask, after `-q` and
/// `--min-score` have had their say.
fn show(page: pipeline::Page, opts: &SearchOpts, context: usize, words: &[String]) -> Result<()> {
    let more = page.more(opts.min_score);
    let mut hits = apply_min_score(page.hits, opts.min_score);
    if opts.quiet {
        std::process::exit(i32::from(hits.is_empty()));
    }
    if opts.sort == SortBy::Path {
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.start_line.cmp(&b.start_line)));
    }
    let next = more.then_some(opts.offset + hits.len());
    let color = use_color(
        opts.color,
        std::io::stdout().is_terminal(),
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
    );
    print_hits(
        &hits,
        opts.json,
        context,
        words,
        opts.count_by_file,
        opts.files_with_matches,
        opts.null,
        opts.vimgrep,
        opts.offset,
        next,
        color,
    )
}

/// A relative `--in` read from the current directory, as the shell and git
/// read a path, when that directory is inside the project. Run from `src`,
/// `--in embed` means `src/embed`. From elsewhere, as with a `--path`, it is
/// read from the project root.
fn from_here(raw: &str, root: &Path) -> String {
    let raw = raw.trim();
    let here = Path::new(".").canonicalize().unwrap_or_default();
    match here.strip_prefix(root) {
        Ok(below) if !below.as_os_str().is_empty() && Path::new(raw).is_relative() => {
            format!("{}/{raw}", below.to_string_lossy().replace('\\', "/"))
        }
        _ => raw.to_string(),
    }
}

/// What a search with no hits to show says: a page past the last one has
/// matches behind it, so it is not "no matches".
fn no_hits_message(offset: usize) -> &'static str {
    match offset > 0 {
        true => "no further matches",
        false => "no matches",
    }
}

/// Prints search results in the shape the flags ask for. `next` is the offset
/// of the page that follows, when there is one.
#[allow(clippy::too_many_arguments)]
fn print_hits(
    hits: &[pipeline::SearchHit],
    as_json: bool,
    context: usize,
    words: &[String],
    count_by_file: bool,
    files_with_matches: bool,
    null: bool,
    vimgrep: bool,
    offset: usize,
    next: Option<usize>,
    color: bool,
) -> Result<()> {
    if vimgrep {
        for h in hits {
            let (line, text) = first_text_line(h);
            out!("{}:{line}:1:{}", printable(&h.path), printable(text));
        }
        return Ok(());
    }

    if files_with_matches {
        let mut seen = std::collections::HashSet::new();
        let paths: Vec<&str> = hits
            .iter()
            .map(|h| h.path.as_str())
            .filter(|path| seen.insert(*path))
            .collect();
        if as_json {
            out!("{}", serde_json::to_string_pretty(&paths)?);
        } else if null {
            let bytes: Vec<u8> = paths
                .iter()
                .flat_map(|path| path.bytes().chain(std::iter::once(0)))
                .collect();
            if let Err(e) = std::io::stdout().write_all(&bytes) {
                stdout_failed(&e);
            }
        } else if paths.is_empty() {
            out!("{}", no_hits_message(offset));
        } else {
            for path in paths {
                out!("{}", printable(path));
            }
        }
        return Ok(());
    }

    if count_by_file {
        let counts = file_hit_counts(hits);
        if as_json {
            let rows: Vec<Value> = counts
                .into_iter()
                .map(|(path, count)| json!({ "path": path, "count": count }))
                .collect();
            out!("{}", serde_json::to_string_pretty(&rows)?);
        } else if counts.is_empty() {
            out!("{}", no_hits_message(offset));
        } else {
            for (path, count) in counts {
                out!("{count:>5}  {}", printable(&path));
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
        out!("{}", no_hits_message(offset));
        return Ok(());
    }
    for h in hits {
        let place = format!(
            "{}:{}",
            paint(&printable(&h.path), "1", color),
            paint(&format!("{}-{}", h.start_line, h.end_line), "32", color)
        );
        match &h.symbol {
            Some(s) => out!(
                "{place}  {}  ({})",
                printable(&s.label()),
                show_score(h.score)
            ),
            None => out!("{place}  ({})", show_score(h.score)),
        }
        for l in snippet_lines(&h.snippet, context, words) {
            for piece in wrap(&printable(l)) {
                out!("    {}", highlight(&piece, words, color));
            }
        }
        out!();
    }
    if let Some(next) = next {
        out!("more results below this; pass --offset {next} to see them");
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
            config.save_setting("model")?;
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
            config.save_setting("backend")?;
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
        InstallCmd::Vscode => "VS Code",
        InstallCmd::Gemini => "Gemini CLI",
        InstallCmd::OpenCode => "OpenCode",
        InstallCmd::Codex => "Codex CLI",
    };
    add_server(root, &target)?;
    add_note(root, &target)?;
    out!("restart {name} in this directory to pick it up");
    Ok(())
}

/// Refuses an install target that leaves the project through a link: the file
/// itself, or a directory between it and the root, that points outside the
/// root or nowhere. A link that stays inside the project, such as a `CLAUDE.md`
/// that names `AGENTS.md`, is written through as usual.
fn check_inside_root(root: &Path, path: &Path) -> Result<()> {
    let real_root = root.canonicalize()?;
    for link in path.ancestors().take_while(|p| *p != root) {
        if !std::fs::symlink_metadata(link).is_ok_and(|meta| meta.is_symlink()) {
            continue;
        }
        let inside = link
            .canonicalize()
            .is_ok_and(|real| real.starts_with(&real_root));
        anyhow::ensure!(
            inside,
            "{} is a link out of the project; ripsem writes only inside it",
            show_path(link)
        );
    }
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
        InstallCmd::Vscode => (
            root.join(".github/copilot-instructions.md"),
            format!("## Code search\n\n{}\n", mcp::INSTRUCTIONS),
        ),
        InstallCmd::Gemini => (
            root.join("GEMINI.md"),
            format!("## Code search\n\n{}\n", mcp::INSTRUCTIONS),
        ),
        InstallCmd::OpenCode | InstallCmd::Codex => (
            root.join("AGENTS.md"),
            format!("## Code search\n\n{}\n", mcp::INSTRUCTIONS),
        ),
    };
    check_inside_root(root, &path)?;
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

/// Codex keeps its servers in TOML, so the table is appended as text and the
/// rest of the file stays as it is.
fn add_codex_server(path: &Path) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", show_path(path))),
    };
    let doc: toml::Table =
        toml::from_str(text.trim_start_matches('\u{feff}')).with_context(|| {
            format!(
                "{} is not valid TOML; fix it and run this again",
                show_path(path)
            )
        })?;
    match doc.get("mcp_servers") {
        None => {}
        Some(toml::Value::Table(servers)) if servers.contains_key("ripsem") => {
            out!("ripsem is already in {}; left as it is", show_path(path));
            return Ok(());
        }
        Some(toml::Value::Table(_)) => {}
        Some(_) => anyhow::bail!(
            "mcp_servers in {} is not a table; fix it and run this again",
            show_path(path)
        ),
    }
    let mut written = text.trim_end().to_string();
    if !written.is_empty() {
        written.push_str("\n\n");
    }
    written.push_str("[mcp_servers.ripsem]\ncommand = \"ripsem\"\nargs = [\"mcp\"]\n");
    // A table header cannot add to `mcp_servers = { .. }` written on one line.
    if toml::from_str::<toml::Table>(&written).is_err() {
        anyhow::bail!(
            "could not add a [mcp_servers.ripsem] table to {}; add it by hand",
            show_path(path)
        );
    }
    std::fs::write(path, written)?;
    out!("added ripsem to {}", show_path(path));
    Ok(())
}

fn add_server(root: &Path, target: &InstallCmd) -> Result<()> {
    let (rel, key) = match target {
        InstallCmd::ClaudeCode => (".mcp.json", "mcpServers"),
        InstallCmd::Cursor => (".cursor/mcp.json", "mcpServers"),
        InstallCmd::Vscode => (".vscode/mcp.json", "servers"),
        InstallCmd::Gemini => (".gemini/settings.json", "mcpServers"),
        InstallCmd::OpenCode => ("opencode.json", "mcp"),
        InstallCmd::Codex => (".codex/config.toml", "mcp_servers"),
    };
    let path = root.join(rel);
    check_inside_root(root, &path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let InstallCmd::Codex = target {
        return add_codex_server(&path);
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

    match doc.get(key) {
        None => {}
        Some(servers) if !servers.is_object() => anyhow::bail!(
            "{key} in {} is not a JSON object; fix it and run this again",
            show_path(&path)
        ),
        // An entry already there may carry a --path or env the user added.
        Some(servers) if servers.get("ripsem").is_some() => {
            out!("ripsem is already in {}; left as it is", show_path(&path));
            return Ok(());
        }
        Some(_) => {}
    }
    // Claude Code, Gemini CLI and OpenCode start a project's server in the
    // directory they run in. Cursor and VS Code do not say where they start
    // one, but expand `${workspaceFolder}`, which keeps the file free of this
    // machine's paths.
    let entry = match target {
        InstallCmd::ClaudeCode | InstallCmd::Gemini => {
            json!({ "command": "ripsem", "args": ["mcp"] })
        }
        InstallCmd::OpenCode => json!({
            "type": "local",
            "command": ["ripsem", "mcp"],
            "enabled": true,
        }),
        InstallCmd::Codex => unreachable!("Codex is written as TOML above"),
        InstallCmd::Cursor => json!({
            "command": "ripsem",
            "args": ["mcp", "--path", "${workspaceFolder}"],
        }),
        InstallCmd::Vscode => json!({
            "type": "stdio",
            "command": "ripsem",
            "args": ["mcp", "--path", "${workspaceFolder}"],
        }),
    };

    let written = if text.trim().is_empty() {
        serde_json::to_string_pretty(&json!({ key: { "ripsem": entry } }))? + "\n"
    } else {
        // Only text is added, so the other servers keep their order and layout.
        let top: HashMap<&str, &RawValue> = serde_json::from_str(text)?;
        match top.get(key) {
            Some(servers) => add_member(text, servers.get(), "ripsem", &entry),
            None => add_member(text, text.trim(), key, &json!({ "ripsem": entry })),
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
    fn color_follows_the_flag_and_a_terminal_unless_no_color_is_set() {
        use ColorWhen::*;
        assert!(use_color(Always, false, true));
        assert!(!use_color(Never, true, false));
        assert!(use_color(Auto, true, false));
        assert!(!use_color(Auto, false, false));
        assert!(!use_color(Auto, true, true));
    }

    #[test]
    fn paint_wraps_text_only_when_colour_is_on() {
        assert_eq!(paint("a.rs", "1", true), "\x1b[1ma.rs\x1b[0m");
        assert_eq!(paint("a.rs", "1", false), "a.rs");
    }

    #[test]
    fn highlight_paints_the_query_words_as_whole_words() {
        let words = pipeline::query_words("parse config");
        assert_eq!(
            highlight("fn parse_config(path: &str) {", &words, true),
            "fn \x1b[1;31mparse\x1b[0m_\x1b[1;31mconfig\x1b[0m(path: &str) {"
        );
        assert_eq!(
            highlight("Config::load() reads PARSE", &words, true),
            "\x1b[1;31mConfig\x1b[0m::load() reads \x1b[1;31mPARSE\x1b[0m"
        );
        assert_eq!(highlight("parser configs", &words, true), "parser configs");
        assert_eq!(highlight("parse config", &words, false), "parse config");
    }

    #[test]
    fn snippet_lines_respects_the_context_count() {
        let snippet = "one\ntwo\nthree\nfour";
        assert_eq!(snippet_lines(snippet, 2, &[]), vec!["one", "two"]);
        assert!(snippet_lines(snippet, 0, &[]).is_empty());
        assert_eq!(
            snippet_lines(snippet, 10, &[]),
            vec!["one", "two", "three", "four"]
        );
    }

    #[test]
    fn snippet_lines_move_to_the_line_the_query_names() {
        let snippet = "fn truncate() {}\n\nfn progress(len: u64) {\n    ProgressBar::new(len)\n}\n\nfn style() {}";
        assert_eq!(
            snippet_lines(snippet, 3, &pipeline::query_words("progress bar")),
            vec!["fn progress(len: u64) {", "    ProgressBar::new(len)", "}"]
        );
        assert_eq!(
            snippet_lines(snippet, 3, &pipeline::query_words("truncate")),
            vec!["fn truncate() {}", "", "fn progress(len: u64) {"]
        );
        assert_eq!(
            snippet_lines(snippet, 3, &pipeline::query_words("nothing here")),
            vec!["fn truncate() {}", "", "fn progress(len: u64) {"]
        );
        assert_eq!(
            snippet_lines(snippet, 2, &pipeline::query_words("style")),
            vec!["", "fn style() {}"]
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
    fn init_below_an_index_says_which_index_it_hides() {
        let dir = crate::testutil::TempDir::new();
        cmd_init(dir.path()).unwrap();
        let sub = dir.path().join("src/util");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(shadowed_index_warning(dir.path()), None);
        let warning = shadowed_index_warning(&sub).unwrap();
        assert!(warning.starts_with("warning: "));
        assert!(warning.contains(&dir.path().join(".ripsem").display().to_string()));
        assert!(warning.contains(&sub.join(".ripsem").display().to_string()));
        cmd_init(&sub).unwrap();
        assert!(sub.join(".ripsem/config.toml").exists());
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
            written: Some(1_700_000_000),
            drift: Some(pipeline::Drift {
                added: 1,
                modified: 2,
                removed: 0,
            }),
            changed: None,
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
        assert_eq!(v["index"]["written_unix"], 1_700_000_000);
    }

    #[test]
    fn written_ago_names_one_unit_of_time() {
        assert_eq!(written_ago(100, 50), "just now");
        assert_eq!(written_ago(0, 59), "just now");
        assert_eq!(written_ago(0, 600), "10 min ago");
        assert_eq!(written_ago(0, 7_200), "2 h ago");
        assert_eq!(written_ago(0, 86_400), "1 day ago");
        assert_eq!(written_ago(0, 3 * 86_400 + 5), "3 days ago");
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
    fn install_vscode_lists_the_server_under_servers() {
        let dir = crate::testutil::TempDir::new();
        dir.write(
            ".vscode/mcp.json",
            "{\n  \"servers\": {\n    \"other\": { \"command\": \"x\" }\n  }\n}\n",
        );
        cmd_install(dir.path(), InstallCmd::Vscode).unwrap();
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".vscode/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["servers"]["other"]["command"], "x");
        assert_eq!(doc["servers"]["ripsem"]["type"], "stdio");
        assert_eq!(
            doc["servers"]["ripsem"]["args"],
            json!(["mcp", "--path", "${workspaceFolder}"])
        );
        assert!(doc.get("mcpServers").is_none());
        let note =
            std::fs::read_to_string(dir.path().join(".github/copilot-instructions.md")).unwrap();
        assert!(note.starts_with("## Code search\n") && note.contains("`search` tool"));
    }

    #[test]
    fn install_gemini_keeps_the_other_settings() {
        let dir = crate::testutil::TempDir::new();
        dir.write(".gemini/settings.json", "{\n  \"theme\": \"Dracula\"\n}\n");
        cmd_install(dir.path(), InstallCmd::Gemini).unwrap();
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["theme"], "Dracula");
        assert_eq!(
            doc["mcpServers"]["ripsem"],
            json!({ "command": "ripsem", "args": ["mcp"] })
        );
        let note = std::fs::read_to_string(dir.path().join("GEMINI.md")).unwrap();
        assert!(note.starts_with("## Code search\n") && note.contains("`search` tool"));
    }

    #[test]
    fn install_opencode_keeps_the_other_settings() {
        let dir = crate::testutil::TempDir::new();
        dir.write("opencode.json", "{\n  \"model\": \"x/y\"\n}\n");
        cmd_install(dir.path(), InstallCmd::OpenCode).unwrap();
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("opencode.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["model"], "x/y");
        assert_eq!(
            doc["mcp"]["ripsem"],
            json!({ "type": "local", "command": ["ripsem", "mcp"], "enabled": true })
        );
        let note = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(note.starts_with("## Code search\n") && note.contains("`search` tool"));
    }

    #[test]
    fn install_codex_keeps_the_other_settings() {
        let dir = crate::testutil::TempDir::new();
        dir.write(
            ".codex/config.toml",
            "model = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"x\"\n",
        );
        cmd_install(dir.path(), InstallCmd::Codex).unwrap();
        let doc: toml::Table = toml::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));
        assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("x"));
        assert_eq!(
            doc["mcp_servers"]["ripsem"]["command"].as_str(),
            Some("ripsem")
        );
        assert_eq!(
            doc["mcp_servers"]["ripsem"]["args"],
            toml::Value::from(vec!["mcp"])
        );
        let note = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(note.starts_with("## Code search\n") && note.contains("`search` tool"));
    }

    /// Every file under `dir`, by path, with its bytes.
    fn tree(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(dir, &mut out);
        out
    }

    /// A second install changes no byte of any file, for each agent, in a
    /// directory the first one wrote to.
    #[test]
    fn running_install_twice_changes_no_file_for_any_agent() {
        let targets: [fn() -> InstallCmd; 6] = [
            || InstallCmd::ClaudeCode,
            || InstallCmd::Cursor,
            || InstallCmd::Vscode,
            || InstallCmd::Gemini,
            || InstallCmd::OpenCode,
            || InstallCmd::Codex,
        ];
        for target in targets {
            let dir = crate::testutil::TempDir::new();
            cmd_install(dir.path(), target()).unwrap();
            let first = tree(dir.path());
            cmd_install(dir.path(), target()).unwrap();
            assert_eq!(tree(dir.path()), first, "{:?}", first.keys());
        }
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
