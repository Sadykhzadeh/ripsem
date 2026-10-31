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
const REVISION: u32 = 6;

/// A line longer than this is cut into pieces of at most this many bytes. The
/// models read about this much of a text, so a minified bundle on one line
/// would otherwise be embedded from its first few hundred characters only.
pub(crate) const MAX_CHUNK_BYTES: usize = 2048;

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
    let chunks = cut(path, text, config);
    match chunks.iter().any(has_long_line) {
        true => chunks.into_iter().flat_map(split_long_lines).collect(),
        false => chunks,
    }
}

fn cut(path: &Path, text: &str, config: &Config) -> Vec<Chunk> {
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

fn has_long_line(chunk: &Chunk) -> bool {
    chunk.text.len() > MAX_CHUNK_BYTES && chunk.text.split('\n').any(|l| l.len() > MAX_CHUNK_BYTES)
}

/// Lines of a chunk that fit stay together; a longer line becomes pieces of its
/// own, all reporting that line. The symbol is kept on every piece and the
/// declarations on the first.
fn split_long_lines(chunk: Chunk) -> Vec<Chunk> {
    if !has_long_line(&chunk) {
        return vec![chunk];
    }
    let mut out: Vec<Chunk> = Vec::new();
    let mut group: Vec<&str> = Vec::new();
    let mut group_start = chunk.start_line;
    let push = |out: &mut Vec<Chunk>, text: String, start: usize, end: usize| {
        if is_blank(&text) {
            return;
        }
        out.push(Chunk {
            start_line: start,
            end_line: end,
            text,
            symbol: chunk.symbol.clone(),
            declarations: match out.is_empty() {
                true => chunk.declarations.clone(),
                false => Vec::new(),
            },
        });
    };
    for (i, line) in chunk.text.split('\n').enumerate() {
        let number = (chunk.start_line + i).min(chunk.end_line);
        if line.len() <= MAX_CHUNK_BYTES {
            if group.is_empty() {
                group_start = number;
            }
            group.push(line);
            continue;
        }
        if !group.is_empty() {
            push(&mut out, group.join("\n"), group_start, number - 1);
            group.clear();
        }
        let mut rest = line;
        while !rest.is_empty() {
            let (piece, tail) = rest.split_at(cut_at(rest));
            push(&mut out, piece.to_string(), number, number);
            rest = tail;
        }
    }
    if !group.is_empty() {
        push(&mut out, group.join("\n"), group_start, chunk.end_line);
    }
    out
}

/// Where to cut a line for a piece of at most [`MAX_CHUNK_BYTES`]: on a
/// character boundary, after punctuation or a space when one is near, so a
/// word is not split between two pieces.
fn cut_at(line: &str) -> usize {
    if line.len() <= MAX_CHUNK_BYTES {
        return line.len();
    }
    let mut end = MAX_CHUNK_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let near = end.saturating_sub(MAX_CHUNK_BYTES / 8);
    match line.as_bytes()[near..end]
        .iter()
        .rposition(|b| !b.is_ascii_alphanumeric() && *b != b'_' && b.is_ascii())
    {
        Some(i) => near + i + 1,
        None => end,
    }
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
                for d in &c.declarations {
                    seen.push_str(&format!(
                        "  declares {} {}-{}\n",
                        d.symbol.label(),
                        d.start_line,
                        d.end_line
                    ));
                }
            }
        }
        let expected = match cfg!(feature = "treesitter") {
            true => 0xa4a9_1e7c_13de_e71c,
            false => 0x23ab_773d_1824_385f,
        };
        assert_eq!(
            (REVISION, crate::fingerprint::content_hash(seen.as_bytes())),
            (6, expected),
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

    #[test]
    fn a_single_long_line_is_cut_so_all_of_it_is_searchable() {
        let line: String = (0..30_000).map(|n| format!("w{n};")).collect();
        for file in ["min.js", "data.txt"] {
            let chunks = chunk_source(Path::new(file), &line, &Config::default());
            assert!(chunks.len() > 1, "{file}: {} chunk", chunks.len());
            assert!(chunks.iter().all(|c| c.text.len() <= MAX_CHUNK_BYTES));
            assert!(chunks.iter().all(|c| (c.start_line, c.end_line) == (1, 1)));
            assert_eq!(
                chunks.iter().map(|c| c.text.as_str()).collect::<String>(),
                line
            );
        }
    }

    #[test]
    fn a_long_line_among_short_ones_keeps_its_line_number() {
        let long = "\u{e9}".repeat(3000);
        let text = format!("first\nsecond\n{long}\nlast\n");
        let chunks = chunk_source(Path::new("a.txt"), &text, &Config::default());
        let spans: Vec<(usize, usize)> =
            chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
        assert_eq!(spans, [(1, 2), (3, 3), (3, 3), (3, 3), (4, 4)]);
        assert!(chunks.iter().all(|c| c.text.len() <= MAX_CHUNK_BYTES));
        let pieces: String = chunks[1..4].iter().map(|c| c.text.as_str()).collect();
        assert_eq!(pieces, long);
    }
}
