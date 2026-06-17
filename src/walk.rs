use std::io::Read;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::config::Config;

pub fn collect(config: &Config) -> Vec<PathBuf> {
    let max_bytes = config.max_file_kb.saturating_mul(1024);
    let mut builder = WalkBuilder::new(&config.root);
    builder
        .hidden(false)
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".ripsemignore");

    let mut out = Vec::new();
    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.into_path();
        if path
            .components()
            .any(|c| matches!(c.as_os_str().to_str(), Some(".ripsem") | Some(".git")))
        {
            continue;
        }
        if is_indexable(&path, max_bytes) {
            out.push(path);
        }
    }
    out
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

    #[test]
    fn collects_text_and_skips_binary() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("blob.bin", "head\0\0\0\u{1}\u{2}tail");
        dir.write("empty.txt", "");

        let names: Vec<String> = collect(&config_at(dir.path()))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

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

        let collected = collect(&config_at(dir.path()));
        assert!(collected.iter().all(|p| !p
            .components()
            .any(|c| c.as_os_str() == ".git")));
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn skips_files_over_the_size_limit() {
        let dir = TempDir::new();
        dir.write("big.txt", &"x".repeat(2048));
        let mut config = config_at(dir.path());
        config.max_file_kb = 1;

        let names: Vec<String> = collect(&config)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(!names.contains(&"big.txt".to_string()));
    }
}
