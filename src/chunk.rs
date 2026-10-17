use std::path::Path;

use bitcode::{Decode, Encode};

use crate::config::Config;

use crate::symbol::{Declaration, Symbol};

#[derive(Debug, Clone)]
pub struct Chunk {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub symbol: Option<Symbol>,
    pub declarations: Vec<Declaration>,
}

/// Changes whenever the same file and settings would give other chunks, so an
/// index cut by an older ripsem is cut again. A test pins it to the output.
const REVISION: u32 = 2;

/// Everything that decides how a file is cut into chunks. An index records the
/// one it was built with, and reuses a file's records only under the same one.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct Chunking {
    revision: u32,
    parsed: bool,
    lines: usize,
    overlap: usize,
    max_lines: usize,
}

impl Chunking {
    pub fn of(config: &Config) -> Self {
        Self {
            revision: REVISION,
            parsed: cfg!(feature = "treesitter"),
            lines: config.chunk_lines,
            overlap: config.chunk_overlap,
            max_lines: config.max_chunk_lines,
        }
    }
}

pub fn chunk_source(path: &Path, text: &str, config: &Config) -> Vec<Chunk> {
    #[cfg(feature = "treesitter")]
    if let Some(spec) = crate::lang::detect(path)
        && let Some(chunks) =
            crate::semantic::chunk(text, &spec, config.max_chunk_lines, config.chunk_overlap)
        && !chunks.is_empty()
    {
        return chunks;
    }
    #[cfg(not(feature = "treesitter"))]
    let _ = path;
    chunk_file(text, config.chunk_lines, config.chunk_overlap)
}

/// Text with nothing but whitespace in it yields no chunk, so a file made only
/// of it is never indexed. Anything asking what the indexer would keep asks
/// here rather than restating the rule.
pub fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

pub fn chunk_file(text: &str, chunk_lines: usize, overlap: usize) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let window = chunk_lines.max(1);
    let step = window.saturating_sub(overlap).max(1);

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let end = (start + window).min(lines.len());
        let body = lines[start..end].join("\n");
        if !is_blank(&body) {
            chunks.push(Chunk {
                start_line: start + 1,
                end_line: end,
                text: body,
                symbol: None,
                declarations: Vec::new(),
            });
        }
        if end == lines.len() {
            break;
        }
        start += step;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_file("", 10, 2).is_empty());
        assert!(chunk_file("   \n\t\n", 10, 2).is_empty());
    }

    #[test]
    fn single_window_covers_short_file() {
        let chunks = chunk_file("a\nb\nc", 40, 8);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
        assert_eq!(chunks[0].text, "a\nb\nc");
    }

    #[test]
    fn windows_overlap_by_step() {
        let body: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        let chunks = chunk_file(&body, 4, 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 4);
        assert_eq!(chunks[1].start_line, 4);
        assert_eq!(chunks.last().unwrap().end_line, 10);
    }

    /// An index keeps the chunks of an unchanged file only while [`REVISION`]
    /// stays the same, so a change in what these files give has to come with a
    /// new revision and fails here until it does. Update both together.
    #[test]
    fn the_chunks_are_pinned_to_the_revision() {
        let config = Config::default();
        let rust = "/// Adds two numbers.\nfn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n\
                    struct Point {\n    x: i32,\n}\n";
        let python =
            "def load(path):\n    \"\"\"Read a file.\"\"\"\n    return open(path).read()\n";
        let text: String = (1..=90).map(|n| format!("note {n}\n")).collect();
        let mut seen = String::new();
        for (file, src) in [("a.rs", rust), ("b.py", python), ("c.txt", text.as_str())] {
            for c in chunk_source(Path::new(file), src, &config) {
                let label = c.symbol.as_ref().map(Symbol::label).unwrap_or_default();
                seen.push_str(&format!(
                    "{file}:{}-{} {label}\n{}\n",
                    c.start_line, c.end_line, c.text
                ));
            }
        }
        let expected = match cfg!(feature = "treesitter") {
            true => 0xd329_d7ee_1bd4_a9b7,
            false => 0x23ab_773d_1824_385f,
        };
        assert_eq!(
            (REVISION, crate::fingerprint::content_hash(seen.as_bytes())),
            (2, expected),
            "{seen}"
        );
    }

    #[test]
    fn overlap_at_least_window_does_not_stall() {
        let body: String = (1..=6).map(|n| format!("line{n}\n")).collect();
        let chunks = chunk_file(&body, 3, 9);
        assert_eq!(chunks.last().unwrap().end_line, 6);
        assert!(chunks.len() <= body.lines().count());
    }
}
