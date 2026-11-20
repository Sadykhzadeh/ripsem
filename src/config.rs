use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::show_path;
use crate::model::{self, DEFAULT_MODEL};

/// Patterns are anchored to `.ripsem`, so a project checked out somewhere below
/// it, which the walk reads parent ignore files for, keeps its own files.
const DATA_GITIGNORE: &str = "\
# Written by ripsem, which leaves this file alone once it exists.
# `ripsem index` rebuilds the index; config.toml can be committed.
/index.bin
/*.tmp
";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, bitcode::Encode, bitcode::Decode,
)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Hash,
    Fastembed,
}

impl Default for Backend {
    fn default() -> Self {
        if cfg!(feature = "fastembed") {
            Backend::Fastembed
        } else {
            Backend::Hash
        }
    }
}

/// Unknown keys are refused: a misspelt `exclude` would otherwise be dropped
/// without a word and index everything it was meant to keep out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub backend: Backend,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_chunk_lines")]
    pub chunk_lines: usize,
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
    #[serde(default = "default_max_chunk_lines")]
    pub max_chunk_lines: usize,
    #[serde(default = "default_max_file_kb")]
    pub max_file_kb: u64,
    #[serde(default = "enabled")]
    pub skip_generated: bool,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(skip)]
    pub root: PathBuf,
    /// Hash every file instead of trusting the stat the index stored for it.
    /// Set from `RIPSEM_HASH_ALL` by the command line.
    #[serde(skip)]
    pub hash_all: bool,
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}
fn default_chunk_lines() -> usize {
    40
}
fn default_chunk_overlap() -> usize {
    8
}
fn default_max_chunk_lines() -> usize {
    50
}
fn default_max_file_kb() -> u64 {
    1024
}
fn enabled() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: Backend::default(),
            model: default_model(),
            chunk_lines: default_chunk_lines(),
            chunk_overlap: default_chunk_overlap(),
            max_chunk_lines: default_max_chunk_lines(),
            max_file_kb: default_max_file_kb(),
            skip_generated: enabled(),
            exclude: Vec::new(),
            root: PathBuf::from("."),
            hash_all: false,
        }
    }
}

impl Config {
    pub fn dims(&self) -> usize {
        model::find(&self.model).map(|m| m.dims).unwrap_or(384)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join(".ripsem")
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir().join("config.toml")
    }

    pub fn index_path(&self) -> PathBuf {
        self.data_dir().join("index.bin")
    }

    /// Creates the data directory with a `.gitignore` that keeps the index and
    /// its temp file out of git while `config.toml` stays committable. An
    /// existing `.gitignore` is left exactly as it is, so one the user edited
    /// survives every run.
    pub fn create_data_dir(&self) -> Result<()> {
        let dir = self.data_dir();
        check_data_dir(&dir)?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", show_path(&dir)))?;
        let path = dir.join(".gitignore");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let written = file.write_all(DATA_GITIGNORE.as_bytes());
                drop(file);
                written
                    .inspect_err(|_| {
                        let _ = std::fs::remove_file(&path);
                    })
                    .with_context(|| format!("writing {}", show_path(&path)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e).with_context(|| format!("writing {}", show_path(&path))),
        }
    }

    /// The nearest directory at or above `start` that holds a `.ripsem`
    /// directory, or `start` itself when there is none.
    pub fn find_root(start: &Path) -> PathBuf {
        start
            .ancestors()
            .find(|dir| dir.join(".ripsem").is_dir())
            .unwrap_or(start)
            .to_path_buf()
    }

    pub fn load(root: &Path) -> Result<Self> {
        let mut cfg = Config {
            root: root.to_path_buf(),
            ..Default::default()
        };
        check_data_dir(&cfg.data_dir())?;
        let path = cfg.config_path();
        check_config_file(&path)?;
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", show_path(&path)))?;
            let mut parsed: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", show_path(&path)))?;
            let overlap_given = toml::from_str::<toml::Table>(&text)
                .is_ok_and(|table| table.contains_key("chunk_overlap"));
            parsed.root = root.to_path_buf();
            parsed
                .check()
                .and_then(|()| parsed.check_overlap(overlap_given))
                .with_context(|| format!("in {}", show_path(&path)))?;
            cfg = parsed;
        }
        Ok(cfg)
    }

    /// [`Config::load`] again, for a process that keeps running: what it was
    /// started with and the file does not hold, such as `hash_all`, carries
    /// over from `running`.
    pub fn load_again(root: &Path, running: &Config) -> Result<Self> {
        Ok(Config {
            hash_all: running.hash_all,
            ..Self::load(root)?
        })
    }

    /// A size of 0 would not fail anywhere else: a file limit of 0 indexes
    /// nothing, and a window of 0 lines cuts a chunk for every line. A model
    /// outside the catalogue would index at a guessed width under its own name.
    fn check(&self) -> Result<()> {
        for (key, value) in [
            ("chunk_lines", self.chunk_lines as u64),
            ("max_chunk_lines", self.max_chunk_lines as u64),
            ("max_file_kb", self.max_file_kb),
        ] {
            anyhow::ensure!(value > 0, "{key} must be 1 or more");
        }
        anyhow::ensure!(
            model::find(&self.model).is_some(),
            "model '{}' is not in the catalogue; see `ripsem model list`",
            self.model
        );
        Ok(())
    }

    /// An overlap the file names must be below the window. One left at its
    /// default is scaled down to half a small window instead, so a window the
    /// user chose still works.
    fn check_overlap(&mut self, given: bool) -> Result<()> {
        let window = self.chunk_lines.min(self.max_chunk_lines);
        match given {
            true => anyhow::ensure!(
                self.chunk_overlap < window,
                "chunk_overlap must be less than chunk_lines and max_chunk_lines"
            ),
            false => self.chunk_overlap = self.chunk_overlap.min(window / 2),
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        self.create_data_dir()?;
        let text = toml::to_string_pretty(self)?;
        let path = self.config_path();
        check_config_file(&path)?;
        replace_file(&path, &text)?;
        Ok(())
    }

    /// Writes one setting, `key`, into a configuration file that exists, and
    /// leaves the rest of the file as it is: its comments, order and layout.
    /// Writes the whole configuration when there is no file yet.
    pub fn save_setting(&self, key: &str) -> Result<()> {
        let path = self.config_path();
        check_config_file(&path)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return self.save(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", show_path(&path))),
        };
        let fresh: toml::Table = toml::from_str(&toml::to_string(self)?)?;
        let value = fresh
            .get(key)
            .with_context(|| format!("no setting named {key}"))?;
        let updated = set_setting(&text, key, &value.to_string());
        let loaded: toml::Table = toml::from_str(&updated)
            .with_context(|| format!("could not set {key} in {}", show_path(&path)))?;
        anyhow::ensure!(
            loaded.get(key) == Some(value),
            "could not set {key} in {}; edit it by hand",
            show_path(&path)
        );
        replace_file(&path, &updated)?;
        Ok(())
    }
}

/// `text` with the statement that assigns `key` replaced by `key = value`.
/// A byte order mark at the start stays where it is.
fn set_setting(text: &str, key: &str, value: &str) -> String {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => format!("\u{feff}{}", set_statement(rest, key, value)),
        None => set_statement(text, key, value),
    }
}

/// Keeps a comment that ends the line of a one-line statement. A key the file
/// does not name is added at the end. A statement that spans lines ends at the
/// first line where it parses on its own.
fn set_statement(text: &str, key: &str, value: &str) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| assigns(line, key)) else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&format!("{key} = {value}{eol}"));
        return out;
    };
    let end = (start..lines.len())
        .find(|&end| toml::from_str::<toml::Table>(&lines[start..=end].join("\n")).is_ok())
        .unwrap_or(start);
    let comment = match start == end {
        true => lines[start].find('#').map(|at| &lines[start][at..]),
        false => None,
    };
    let mut replaced = format!("{key} = {value}");
    if let Some(comment) = comment {
        replaced = format!("{replaced}  {comment}");
    }
    let mut out: Vec<&str> = lines[..start].to_vec();
    out.push(&replaced);
    out.extend_from_slice(&lines[end + 1..]);
    let mut joined = out.join(eol);
    if text.ends_with('\n') {
        joined.push_str(eol);
    }
    joined
}

/// Whether `line` assigns `key`, written bare or in quotes.
fn assigns(line: &str, key: &str) -> bool {
    let line = line.trim_start();
    ["", "\"", "'"]
        .iter()
        .find_map(|quote| line.strip_prefix(format!("{quote}{key}{quote}").as_str()))
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Refuses a data directory that is a symbolic link or any other non-directory.
/// A link would send the index and the configuration to a directory outside
/// the project.
fn check_data_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => anyhow::bail!(
            "{} is a symbolic link; ripsem keeps its data in a directory of the project itself",
            show_path(path)
        ),
        Ok(meta) if !meta.is_dir() => anyhow::bail!(
            "{} is not a directory; ripsem keeps its data in a directory of the project itself",
            show_path(path)
        ),
        _ => Ok(()),
    }
}

/// Refuses a configuration path that is a symbolic link or any other non-file.
/// A link can name a file outside the project, and a FIFO would block a read.
fn check_config_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => anyhow::bail!(
            "{} is not a regular file; ripsem keeps its configuration in a file of the project itself",
            show_path(path)
        ),
        _ => Ok(()),
    }
}

/// Counts the replacements this process has started, so each one gets a
/// temporary name of its own.
static REPLACEMENTS: AtomicU64 = AtomicU64::new(0);

/// Replaces the file at `path` with `text`. A target that is not writable is
/// refused before anything is written, as writing it in place would be. The
/// text goes to a new file beside the target, named with this process's id and
/// a counter, which is created exclusively, so a file or link already there is
/// never opened. That file is then renamed over the target, so a write that
/// fails part way leaves the target as it was. A link is followed, so the file
/// it names is the one replaced and the link stays.
///
/// An interrupted run (killed, or its machine lost power) leaves the target as
/// it was, but can leave its new file behind as `.<name>.<pid>.<n>.tmp`. The
/// next replacement of the same target removes those once they are old.
pub(crate) fn replace_file(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match std::fs::OpenOptions::new().write(true).open(&target) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let temp = replacement_temp(&target, REPLACEMENTS.fetch_add(1, Ordering::Relaxed));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let staged = file
        .write_all(text.as_bytes())
        .and_then(|()| match std::fs::metadata(&target) {
            Ok(meta) => file.set_permissions(meta.permissions()),
            Err(_) => Ok(()),
        });
    drop(file);
    let written = staged.and_then(|()| std::fs::rename(&temp, &target));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    } else {
        remove_stale_replacements(&target);
    }
    written
}

/// A replacement still running writes its temporary file in one pass, so one
/// that has not changed for this long was left by a replacement that was killed.
const STALE_REPLACEMENT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Removes the temporary files that replacements of `target` left behind when
/// they were killed. Only names of the form `.<name>.<pid>.<n>.tmp` are removed,
/// and only once they are older than [`STALE_REPLACEMENT`].
fn remove_stale_replacements(target: &Path) {
    let (Some(dir), Some(name)) = (target.parent(), target.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(rest) = file
            .to_str()
            .and_then(|f| f.strip_prefix('.'))
            .and_then(|f| f.strip_prefix(name))
            .and_then(|r| r.strip_prefix('.'))
            .and_then(|r| r.strip_suffix(".tmp"))
        else {
            continue;
        };
        let shaped = rest
            .split_once('.')
            .is_some_and(|(pid, n)| pid.parse::<u32>().is_ok() && n.parse::<u64>().is_ok());
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= STALE_REPLACEMENT);
        if shaped && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The name [`replace_file`] stages `target` under for replacement `n`.
fn replacement_temp(target: &Path, n: u64) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn a_config_read_again_keeps_what_the_run_was_started_with() {
        let dir = TempDir::new();
        dir.write(
            ".ripsem/config.toml",
            "chunk_lines = 20
",
        );
        let running = Config {
            hash_all: true,
            ..Config::load(dir.path()).unwrap()
        };
        dir.write(
            ".ripsem/config.toml",
            "chunk_lines = 30
",
        );
        let again = Config::load_again(dir.path(), &running).unwrap();
        assert_eq!((again.chunk_lines, again.hash_all), (30, true));
        assert!(!Config::load(dir.path()).unwrap().hash_all);
    }

    #[test]
    fn a_misspelt_key_is_an_error() {
        let dir = TempDir::new();
        dir.write(".ripsem/config.toml", "exlude = [\"vendor/\"]\n");
        let err = format!("{:#}", Config::load(dir.path()).unwrap_err());
        assert!(err.contains("exlude"), "{err}");
    }

    #[test]
    fn a_model_the_catalogue_does_not_hold_is_an_error() {
        let dir = TempDir::new();
        dir.write(".ripsem/config.toml", "model = \"no-such-model\"\n");
        let err = format!("{:#}", Config::load(dir.path()).unwrap_err());
        assert!(err.contains("config.toml"), "{err}");
        assert!(err.contains("model 'no-such-model'"), "{err}");
    }

    #[test]
    fn a_size_of_zero_is_an_error() {
        for key in ["chunk_lines", "max_chunk_lines", "max_file_kb"] {
            let dir = TempDir::new();
            dir.write(".ripsem/config.toml", &format!("{key} = 0\n"));
            let err = format!("{:#}", Config::load(dir.path()).unwrap_err());
            assert!(err.contains(&format!("{key} must be 1 or more")), "{err}");
        }
    }

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert!(cfg.chunk_lines > cfg.chunk_overlap);
        assert_eq!(cfg.dims(), 384);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = TempDir::new();
        let mut cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.model = "bge-base-en-v1.5".into();
        cfg.chunk_lines = 64;
        cfg.save().unwrap();

        let loaded = Config::load(dir.path()).unwrap();
        assert_eq!(loaded.model, "bge-base-en-v1.5");
        assert_eq!(loaded.chunk_lines, 64);
        assert_eq!(loaded.root, dir.path());
    }

    #[test]
    fn a_setting_saved_alone_keeps_the_comments_and_layout_around_it() {
        let dir = TempDir::new();
        let mut cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.save().unwrap();
        std::fs::write(
            cfg.config_path(),
            "# Keep the vendored code out.\nbackend = \"fastembed\"  # local only\nexclude = [\n    \"vendor\", # generated\n]\n",
        )
        .unwrap();

        cfg = Config::load(dir.path()).unwrap();
        cfg.backend = Backend::Hash;
        cfg.save_setting("backend").unwrap();
        assert_eq!(
            std::fs::read_to_string(cfg.config_path()).unwrap(),
            "# Keep the vendored code out.\nbackend = \"hash\"  # local only\nexclude = [\n    \"vendor\", # generated\n]\n"
        );
    }

    #[test]
    fn a_byte_order_mark_stays_at_the_start_of_the_file() {
        let dir = TempDir::new();
        let mut cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.save().unwrap();
        std::fs::write(
            cfg.config_path(),
            "\u{feff}backend = \"hash\"\nchunk_lines = 40\n",
        )
        .unwrap();

        cfg = Config::load(dir.path()).unwrap();
        cfg.backend = Backend::Fastembed;
        cfg.save_setting("backend").unwrap();
        assert_eq!(
            std::fs::read_to_string(cfg.config_path()).unwrap(),
            "\u{feff}backend = \"fastembed\"\nchunk_lines = 40\n"
        );
    }

    #[test]
    fn a_quoted_key_is_replaced_in_place() {
        let dir = TempDir::new();
        let mut cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.save().unwrap();
        std::fs::write(
            cfg.config_path(),
            "\"model\" = \"bge-small-en-v1.5\"  # keep\nchunk_lines = 40\n",
        )
        .unwrap();

        cfg = Config::load(dir.path()).unwrap();
        cfg.model = "bge-base-en-v1.5".into();
        cfg.save_setting("model").unwrap();
        assert_eq!(
            std::fs::read_to_string(cfg.config_path()).unwrap(),
            "model = \"bge-base-en-v1.5\"  # keep\nchunk_lines = 40\n"
        );
    }

    #[test]
    fn a_setting_the_file_does_not_name_is_added_at_the_end() {
        let dir = TempDir::new();
        let mut cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.save().unwrap();
        std::fs::write(cfg.config_path(), "chunk_lines = 40").unwrap();

        cfg = Config::load(dir.path()).unwrap();
        cfg.model = "bge-base-en-v1.5".into();
        cfg.save_setting("model").unwrap();
        assert_eq!(
            std::fs::read_to_string(cfg.config_path()).unwrap(),
            "chunk_lines = 40\nmodel = \"bge-base-en-v1.5\"\n"
        );
        assert_eq!(Config::load(dir.path()).unwrap().model, "bge-base-en-v1.5");
    }

    #[test]
    fn find_root_climbs_to_the_nearest_ripsem_directory() {
        let dir = TempDir::new();
        dir.write(".ripsem/config.toml", "");
        dir.write("a/b/file.rs", "fn main() {}");
        dir.write("a/.ripsem/config.toml", "");
        let top = dir.path().to_path_buf();

        assert_eq!(Config::find_root(&top.join("a/b")), top.join("a"));
        assert_eq!(Config::find_root(&top.join("a")), top.join("a"));
        assert_eq!(Config::find_root(&top), top);
    }

    #[test]
    fn find_root_falls_back_to_the_start_directory() {
        let dir = TempDir::new();
        dir.write("a/b/file.rs", "fn main() {}");
        let start = dir.path().join("a/b");

        assert_eq!(Config::find_root(&start), start);
    }

    #[test]
    fn missing_config_yields_defaults() {
        let dir = TempDir::new();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.model, DEFAULT_MODEL);
    }

    /// An overlap the user wrote is refused when it is not below the window. At
    /// or above it the walk advances a line at a time and joins a window for
    /// each, so a config file a repository ships could make indexing quadratic.
    #[test]
    fn an_explicit_overlap_that_is_not_below_the_window_is_refused() {
        let dir = TempDir::new();
        for text in [
            "chunk_lines = 10\nchunk_overlap = 10\n",
            "chunk_lines = 100\nchunk_overlap = 99\nmax_chunk_lines = 10\n",
            "max_chunk_lines = 5\nchunk_overlap = 8\n",
        ] {
            dir.write(".ripsem/config.toml", text);
            assert!(Config::load(dir.path()).is_err(), "{text}");
        }
        dir.write(
            ".ripsem/config.toml",
            "chunk_lines = 10\nchunk_overlap = 9\n",
        );
        assert_eq!(Config::load(dir.path()).unwrap().chunk_overlap, 9);
    }

    /// The default overlap is scaled down to fit a small window the user chose,
    /// so such a window still indexes, and linearly: each step moves by at least
    /// half a window.
    #[test]
    fn a_default_overlap_is_scaled_down_to_a_small_window() {
        let dir = TempDir::new();
        dir.write(
            ".ripsem/config.toml",
            "chunk_lines = 5\nmax_chunk_lines = 5\n",
        );
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.chunk_overlap, 2);
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let chunks = crate::chunk::chunk_source(Path::new("notes.txt"), &text, &config);
        assert!(chunks.len() <= 1000 / 3 + 1, "{}", chunks.len());
    }

    #[test]
    fn the_default_overlap_is_kept_for_the_default_window() {
        let dir = TempDir::new();
        assert_eq!(Config::load(dir.path()).unwrap().chunk_overlap, 8);
        dir.write(".ripsem/config.toml", "chunk_lines = 40\n");
        assert_eq!(Config::load(dir.path()).unwrap().chunk_overlap, 8);
    }

    #[test]
    fn save_ignores_the_index_but_not_the_config() {
        let dir = TempDir::new();
        let cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.save().unwrap();

        let text = std::fs::read_to_string(dir.path().join(".ripsem/.gitignore")).unwrap();
        let patterns: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(patterns, ["/index.bin", "/*.tmp"]);
        assert!(cfg.index_path().ends_with("index.bin"));
    }

    #[test]
    fn an_existing_data_gitignore_is_left_as_it_is() {
        let dir = TempDir::new();
        dir.write(".ripsem/.gitignore", "# commit the index too\n");
        let cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.save().unwrap();
        cfg.create_data_dir().unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join(".ripsem/.gitignore")).unwrap(),
            "# commit the index too\n"
        );
    }

    #[test]
    fn a_replaced_file_keeps_its_name_and_leaves_no_temporary_file() {
        let dir = TempDir::new();
        dir.write("notes.md", "old\n");
        replace_file(&dir.path().join("notes.md"), "new\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "new\n"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// The names the next replacements of `target` in this process will try.
    fn next_temps(target: &Path) -> Vec<PathBuf> {
        let next = REPLACEMENTS.load(Ordering::Relaxed);
        (next..next + 64)
            .map(|n| replacement_temp(target, n))
            .collect()
    }

    #[test]
    fn a_replacement_removes_the_temporary_files_a_killed_one_left() {
        let dir = TempDir::new();
        dir.write("notes.md", "old\n");
        let target = dir.path().join("notes.md");
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 60 * 60);
        let killed = dir.path().join(".notes.md.4242.7.tmp");
        let other = dir.path().join(".notes.md.notes.tmp");
        let running = dir.path().join(".notes.md.4243.0.tmp");
        for file in [&killed, &other] {
            std::fs::write(file, "partial").unwrap();
            std::fs::File::options()
                .write(true)
                .open(file)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        std::fs::write(&running, "partial").unwrap();

        replace_file(&target, "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
        assert!(!killed.exists());
        assert!(other.exists());
        assert!(running.exists());
    }

    #[test]
    fn a_replacement_that_cannot_be_written_leaves_the_file_as_it_was() {
        let dir = TempDir::new();
        dir.write("notes.md", "old\n");
        let target = dir.path().join("notes.md");
        for temp in next_temps(&target) {
            std::fs::create_dir(temp).unwrap();
        }
        assert!(replace_file(&target, "new\n").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_a_temporary_name_is_never_followed() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        dir.write("victim", "keep\n");
        dir.write("CLAUDE.md", "old\n");
        let target = dir.path().join("CLAUDE.md");
        for temp in next_temps(&target) {
            symlink(dir.path().join("victim"), temp).unwrap();
        }
        assert!(replace_file(&target, "new\n").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("victim")).unwrap(),
            "keep\n"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_is_refused_and_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        dir.write("notes.md", "old\n");
        let target = dir.path().join("notes.md");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        // A process that may write any file, such as root, has no read-only file.
        if std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .is_ok()
        {
            return;
        }
        assert!(replace_file(&target, "new\n").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_replacement_writes_through_a_link_and_keeps_the_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = TempDir::new();
        dir.write("AGENTS.md", "old\n");
        std::fs::set_permissions(
            dir.path().join("AGENTS.md"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        symlink("AGENTS.md", dir.path().join("CLAUDE.md")).unwrap();
        replace_file(&dir.path().join("CLAUDE.md"), "new\n").unwrap();
        assert!(
            std::fs::symlink_metadata(dir.path().join("CLAUDE.md"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
            "new\n"
        );
        let mode = std::fs::metadata(dir.path().join("AGENTS.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
    }
}
