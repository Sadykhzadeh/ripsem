use std::io::Write;
use std::path::{Path, PathBuf};

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
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", show_path(&dir)))?;
        let path = dir.join(".gitignore");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file
                .write_all(DATA_GITIGNORE.as_bytes())
                .with_context(|| format!("writing {}", show_path(&path))),
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
        let path = cfg.config_path();
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", show_path(&path)))?;
            let mut parsed: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", show_path(&path)))?;
            parsed.root = root.to_path_buf();
            parsed
                .check_sizes()
                .with_context(|| format!("in {}", show_path(&path)))?;
            cfg = parsed;
        }
        Ok(cfg)
    }

    /// A size of 0 would not fail anywhere else: a file limit of 0 indexes
    /// nothing, and a window of 0 lines cuts a chunk for every line.
    fn check_sizes(&self) -> Result<()> {
        for (key, value) in [
            ("chunk_lines", self.chunk_lines as u64),
            ("max_chunk_lines", self.max_chunk_lines as u64),
            ("max_file_kb", self.max_file_kb),
        ] {
            anyhow::ensure!(value > 0, "{key} must be 1 or more");
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        self.create_data_dir()?;
        let text = toml::to_string_pretty(self)?;
        std::fs::write(self.config_path(), text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn a_misspelt_key_is_an_error() {
        let dir = TempDir::new();
        dir.write(".ripsem/config.toml", "exlude = [\"vendor/\"]\n");
        let err = format!("{:#}", Config::load(dir.path()).unwrap_err());
        assert!(err.contains("exlude"), "{err}");
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
}
