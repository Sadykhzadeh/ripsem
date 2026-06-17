use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{self, DEFAULT_MODEL};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    120
}
fn default_max_file_kb() -> u64 {
    1024
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

    pub fn load(root: &Path) -> Result<Self> {
        let mut cfg = Config {
            root: root.to_path_buf(),
            ..Default::default()
        };
        let path = cfg.config_path();
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let mut parsed: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            parsed.root = root.to_path_buf();
            cfg = parsed;
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(self.data_dir())?;
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
    fn missing_config_yields_defaults() {
        let dir = TempDir::new();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.model, DEFAULT_MODEL);
    }
}
