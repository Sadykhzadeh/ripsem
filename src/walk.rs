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
        if path.components().any(|c| c.as_os_str() == ".ripsem") {
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
    match std::fs::read(path) {
        Ok(bytes) => content_inspector::inspect(&bytes).is_text(),
        Err(_) => false,
    }
}
