use std::path::Path;

use crate::config::Config;

use crate::symbol::Symbol;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub symbol: Option<Symbol>,
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

    #[test]
    fn overlap_at_least_window_does_not_stall() {
        let body: String = (1..=6).map(|n| format!("line{n}\n")).collect();
        let chunks = chunk_file(&body, 3, 9);
        assert_eq!(chunks.last().unwrap().end_line, 6);
        assert!(chunks.len() <= body.lines().count());
    }
}
