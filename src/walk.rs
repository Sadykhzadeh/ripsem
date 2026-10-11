use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use rayon::prelude::*;

use crate::config::Config;

/// Committed, but written by a machine. Nobody asking where the code that does
/// something lives means a lock file or a minified bundle, and a single lock file
/// can outnumber every hand-written file in the repository put together.
/// `.gitignore` does not cover these because they are deliberately committed.
const GENERATED: &[&str] = &[
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "packages.lock.json",
    "go.sum",
    "*.min.js",
    "*.min.css",
    "*.map",
];

pub fn collect(config: &Config) -> Result<Vec<PathBuf>> {
    let max_bytes = config.max_file_kb.saturating_mul(1024);
    let mut builder = WalkBuilder::new(&config.root);
    builder
        .hidden(false)
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".ripsemignore");

    let defaults = match config.skip_generated {
        true => GENERATED,
        false => &[],
    };
    if !config.exclude.is_empty() || !defaults.is_empty() {
        let mut overrides = OverrideBuilder::new(&config.root);
        for pattern in config
            .exclude
            .iter()
            .map(String::as_str)
            .chain(defaults.iter().copied())
        {
            overrides
                .add(&format!("!{pattern}"))
                .with_context(|| format!("invalid exclude pattern '{pattern}'"))?;
        }
        builder.overrides(overrides.build().context("building exclude overrides")?);
    }

    // Pruned here rather than filtered afterwards, so the walk never descends
    // into a `.git` object store at all.
    let root = config.root.clone();
    builder.filter_entry(move |entry| !is_internal(entry.path(), &root));
    let mut candidates = Vec::new();
    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().is_some_and(|t| t.is_file()) {
            candidates.push(entry.into_path());
        }
    }
    // Telling text from binary opens every file, which is most of what a
    // walk costs, and no file's answer depends on another's.
    Ok(candidates
        .into_par_iter()
        .filter(|path| is_indexable(path, max_bytes))
        .collect())
}

/// ripsem's own index and git's object store are never source. The check is
/// against the path *below the root*: an absolute-path check would refuse to
/// index a repository that merely happens to live inside a directory called
/// `.ripsem` or `.git`.
fn is_internal(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(".ripsem") | Some(".git")))
}

fn is_indexable(path: &Path, max_bytes: u64) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if meta.len() == 0 || meta.len() > max_bytes {
        return false;
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut prefix = [0u8; 1024];
    match file.read(&mut prefix) {
        Ok(n) => content_inspector::inspect(&prefix[..n]).is_text(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn config_at(root: &Path) -> Config {
        Config {
            root: root.to_path_buf(),
            ..Default::default()
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn collects_text_and_skips_binary() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("blob.bin", "head\0\0\0\u{1}\u{2}tail");
        dir.write("empty.txt", "");

        let names = names(&collect(&config_at(dir.path())).unwrap());
        assert!(names.contains(&"keep.rs".to_string()));
        assert!(!names.contains(&"blob.bin".to_string()));
        assert!(!names.contains(&"empty.txt".to_string()));
    }

    #[test]
    fn skips_git_internals() {
        let dir = TempDir::new();
        dir.write("main.rs", "fn main() {}\n");
        dir.write(".git/config", "[core]\n\trepositoryformatversion = 0\n");
        dir.write(".git/hooks/pre-commit.sample", "#!/bin/sh\nexit 0\n");

        let collected = collect(&config_at(dir.path())).unwrap();
        assert!(
            collected
                .iter()
                .all(|p| !p.components().any(|c| c.as_os_str() == ".git"))
        );
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn indexes_a_repository_that_lives_under_a_dot_ripsem_directory() {
        let dir = TempDir::new();
        dir.write(".ripsem/corpora/project/main.rs", "fn main() {}\n");
        let root = dir.path().join(".ripsem/corpora/project");

        assert_eq!(names(&collect(&config_at(&root)).unwrap()), ["main.rs"]);
    }

    #[test]
    fn skips_the_index_directory_inside_the_root() {
        let dir = TempDir::new();
        dir.write("main.rs", "fn main() {}\n");
        dir.write(".ripsem/config.toml", "model = \"x\"\n");

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["main.rs"]
        );
    }

    #[test]
    fn applies_config_exclude_globs() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("Cargo.lock", "version = 3\n");
        let mut config = config_at(dir.path());
        config.exclude = vec!["*.lock".into()];

        let names = names(&collect(&config).unwrap());
        assert!(names.contains(&"keep.rs".to_string()));
        assert!(!names.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn skips_generated_files_without_configuration() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("Cargo.lock", "version = 3\n");
        dir.write("package-lock.json", "{\"lockfileVersion\": 3}\n");
        dir.write("go.sum", "example.com/mod v1.0.0 h1:abc=\n");
        dir.write("app.min.js", "function a(){return 1}\n");
        dir.write("app.js.map", "{\"version\":3}\n");

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["keep.rs"]
        );
    }

    #[test]
    fn generated_files_are_indexed_when_the_default_is_turned_off() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("Cargo.lock", "version = 3\n");
        let mut config = config_at(dir.path());
        config.skip_generated = false;

        assert!(names(&collect(&config).unwrap()).contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn invalid_exclude_glob_is_an_error() {
        let dir = TempDir::new();
        let mut config = config_at(dir.path());
        config.exclude = vec!["[".into()];
        assert!(collect(&config).is_err());
    }

    #[test]
    fn skips_files_over_the_size_limit() {
        let dir = TempDir::new();
        dir.write("big.txt", &"x".repeat(2048));
        let mut config = config_at(dir.path());
        config.max_file_kb = 1;

        assert!(!names(&collect(&config).unwrap()).contains(&"big.txt".to_string()));
    }
}
