use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use rayon::prelude::*;

use crate::config::Config;

/// Machine-written files that are committed on purpose, so `.gitignore` does not
/// cover them. A lock file can outweigh all the hand-written source in a repo.
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
    let mut builder = builder(config)?;
    // Pruned during the walk so it never descends into `.git`.
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
    // The text/binary check opens every file and is most of the cost, so it
    // runs in parallel.
    Ok(candidates
        .into_par_iter()
        .filter(|path| is_indexable(path, max_bytes))
        .collect())
}

fn builder(config: &Config) -> Result<WalkBuilder> {
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

    Ok(builder)
}

/// Whether a change at any of these paths can alter what [`collect`] returns:
/// the walk reaches the path, or it is gone from a directory the walk reaches.
/// Reads only the directories on the way to each path, so thousands of writes
/// under an ignored `target/` cost a top-level listing. A path outside the root
/// cannot be judged and counts as a change.
pub fn reaches_any(config: &Config, paths: &[PathBuf]) -> bool {
    let root = &config.root;
    if paths.iter().any(|p| !p.starts_with(root)) {
        return true;
    }
    // The one ignore source that lives inside `.git`.
    let exclude = root.join(".git").join("info").join("exclude");
    if paths.contains(&exclude) {
        return true;
    }
    let targets: Vec<PathBuf> = paths
        .iter()
        .filter(|p| !is_internal(p, root))
        .cloned()
        .collect();
    if targets.is_empty() {
        return false;
    }
    let Ok(mut builder) = builder(config) else {
        return true;
    };
    let on_the_way = targets.clone();
    let filter_root = root.clone();
    builder.filter_entry(move |entry| {
        !is_internal(entry.path(), &filter_root)
            && on_the_way.iter().any(|t| t.starts_with(entry.path()))
    });
    let reached: std::collections::HashSet<PathBuf> = builder
        .build()
        .flatten()
        .map(ignore::DirEntry::into_path)
        .collect();
    targets.iter().any(|path| {
        reached.contains(path)
            || (!path.exists() && path.parent().is_some_and(|dir| reached.contains(dir)))
    })
}

/// ripsem's own data and `.git` are never source. Judged on the path below the
/// root, so a repository that lives inside a directory named `.git` or
/// `.ripsem` still indexes.
fn is_internal(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(".ripsem") | Some(".git")))
}

/// The text a file holds. UTF-16 with its byte order mark, as Windows tools
/// save it, is decoded and the mark dropped; anything else is read as UTF-8 with
/// bad bytes replaced. Every reader of a file's text goes through here, so a
/// line number means the same line in the index and in the snippets.
pub fn decode_text(bytes: &[u8]) -> Cow<'_, str> {
    let units = |order: fn([u8; 2]) -> u16, rest: &[u8]| {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| order([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => Cow::Owned(units(u16::from_le_bytes, rest)),
        [0xFE, 0xFF, rest @ ..] => Cow::Owned(units(u16::from_be_bytes, rest)),
        _ => String::from_utf8_lossy(bytes),
    }
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
        config_at(dir.path()).create_data_dir().unwrap();
        dir.write(".ripsem/corpora/project/main.rs", "fn main() {}\n");
        dir.write(".ripsem/corpora/project/index.bin", "not ripsem's index\n");
        dir.write(".ripsem/corpora/project/notes.tmp", "scratch\n");
        let root = dir.path().join(".ripsem/corpora/project");

        let mut collected = names(&collect(&config_at(&root)).unwrap());
        collected.sort();
        assert_eq!(collected, ["index.bin", "main.rs", "notes.tmp"]);
    }

    #[test]
    fn skips_the_index_directory_inside_the_root() {
        let dir = TempDir::new();
        dir.write("main.rs", "fn main() {}\n");
        dir.write(".ripsem/config.toml", "model = \"x\"\n");
        dir.write(".ripsem/.gitignore", "/index.bin\n");

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
    fn a_change_the_walk_would_see_reaches_the_index() {
        let dir = TempDir::new();
        dir.write(".gitignore", "target/\n*.log\n");
        dir.write("src/deep/lib.rs", "fn main() {}\n");
        dir.write("target/debug/build.rs", "fn main() {}\n");
        dir.write("debug.log", "noise\n");
        dir.write("Cargo.lock", "version = 3\n");
        dir.write(".git/index", "binary\n");
        let config = config_at(dir.path());
        let reaches = |rel: &str| reaches_any(&config, &[dir.path().join(rel)]);

        assert!(reaches("src/deep/lib.rs"));
        assert!(reaches(".gitignore"), "a new ignore rule changes the walk");
        assert!(
            reaches("src/deep/gone.rs"),
            "removed from a walked directory"
        );
        assert!(reaches("src/new_dir"), "removed or renamed directory");
        assert!(
            reaches(".git/info/exclude"),
            "git's own ignore file changes the walk too"
        );

        assert!(!reaches("target/debug/build.rs"));
        assert!(!reaches("target/debug/gone.rs"));
        assert!(!reaches("debug.log"));
        assert!(!reaches("Cargo.lock"), "generated files are skipped too");
        assert!(!reaches(".git/index"));
        assert!(!reaches(".ripsem/index.bin"));
    }

    #[test]
    fn one_relevant_path_in_a_burst_is_enough() {
        let dir = TempDir::new();
        dir.write(".gitignore", "target/\n");
        dir.write("src/lib.rs", "fn main() {}\n");
        dir.write("target/a.o", "x\n");
        let config = config_at(dir.path());
        let noise = dir.path().join("target/a.o");
        let source = dir.path().join("src/lib.rs");

        assert!(!reaches_any(&config, std::slice::from_ref(&noise)));
        assert!(reaches_any(&config, &[noise, source]));
        assert!(!reaches_any(&config, &[]));
    }

    /// A path that does not start with the root cannot be judged. A needless
    /// reindex is cheaper than a missed change.
    #[test]
    fn a_path_outside_the_root_counts_as_a_change() {
        let dir = TempDir::new();
        let config = config_at(&dir.path().join("repo"));
        assert!(reaches_any(&config, &[dir.path().join("elsewhere.rs")]));
    }

    #[test]
    fn skips_files_over_the_size_limit() {
        let dir = TempDir::new();
        dir.write("big.txt", &"x".repeat(2048));
        let mut config = config_at(dir.path());
        config.max_file_kb = 1;

        assert!(!names(&collect(&config).unwrap()).contains(&"big.txt".to_string()));
    }

    #[test]
    fn decodes_utf16_with_its_byte_order_mark() {
        let text = "fn a() {}\r\nfn b() {}";
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode_text(&le), text);
        assert_eq!(decode_text(&be), text);
        assert_eq!(decode_text(b"plain\n"), "plain\n");
    }
}
