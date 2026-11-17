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
const REVISION: u32 = 18;

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
    if crate::markdown::is_markdown(path)
        && let Some(chunks) = crate::markdown::chunk(text, config.max_chunk_lines)
        && !chunks.is_empty()
    {
        return chunks;
    }
    #[cfg(feature = "treesitter")]
    if let Some(spec) = crate::lang::detect(path)
        && let Some(chunks) =
            crate::semantic::chunk(text, &spec, config.max_chunk_lines, config.chunk_overlap)
        && !chunks.is_empty()
    {
        return chunks;
    }
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
pub(crate) fn cut_at(line: &str) -> usize {
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
        let docs = "# Setup\n\n## Install\nrun it\nthen this\n\nSetext\n------\n\
                    ```\n# kept\n```\nafter\nthe\nfence\n";
        let mut seen = String::new();
        for (file, src) in [
            ("a.rs", rust),
            ("b.py", python),
            ("c.txt", text.as_str()),
            ("d.md", docs),
        ] {
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
            true => 0x229c_0c8b_1945_d7f1,
            false => 0x6725_02a7_559a_6bae,
        };
        assert_eq!(
            (REVISION, crate::fingerprint::content_hash(seen.as_bytes())),
            (18, expected),
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

/// How a source is cut into records: a declaration that can stand alone is a
/// record of its own however small, a container is cut into its members, and
/// what is no declaration stays grouped.
#[cfg(all(test, feature = "treesitter"))]
mod cutting {
    use super::*;

    /// First line, last line and label of every record cut from the code,
    /// leaving out the records that hold a declaration's prose.
    fn cut_code(file: &str, src: &str) -> Vec<(usize, usize, String)> {
        let lines: Vec<&str> = src.lines().collect();
        chunk_source(Path::new(file), src, &Config::default())
            .into_iter()
            .filter(|c| {
                lines[c.start_line - 1..c.end_line]
                    .join("\n")
                    .starts_with(c.text.trim_end())
            })
            .map(|c| {
                let label = c.symbol.as_ref().map(Symbol::label).unwrap_or_default();
                (c.start_line, c.end_line, label)
            })
            .collect()
    }

    fn has(cut: &[(usize, usize, String)], start: usize, end: usize, label: &str) -> bool {
        cut.iter()
            .any(|(s, e, l)| (*s, *e) == (start, end) && l == label)
    }

    fn spans(cut: &[(usize, usize, String)], start: usize, end: usize) -> bool {
        cut.iter().any(|(s, e, _)| (*s, *e) == (start, end))
    }

    fn assert_apart(cut: &[(usize, usize, String)]) {
        for (i, (s, e, _)) in cut.iter().enumerate() {
            for (s2, e2, _) in &cut[i + 1..] {
                assert!(e < s2 || e2 < s, "{cut:?}: {s}-{e} overlaps {s2}-{e2}");
            }
        }
    }

    #[test]
    fn rust_functions_are_records_of_their_own_with_their_doc_comment() {
        let src = "use std::fmt;\n\nconst LIMIT: u32 = 3;\n\n/// Adds two numbers.\n#[inline]\n\
                   fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n\
                   fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n";
        let cut = cut_code("a.rs", src);
        assert_eq!(cut.len(), 3, "{cut:?}");
        assert!(has(&cut, 5, 9, "fn add"), "{cut:?}");
        assert!(has(&cut, 11, 13, "fn sub"), "{cut:?}");
        assert!(spans(&cut, 1, 3), "{cut:?}");
        assert_apart(&cut);
    }

    #[test]
    fn a_one_line_function_is_a_record_and_a_one_line_constant_is_not() {
        let src = "fn first() -> u8 {\n    1\n}\nfn id(x: u8) -> u8 { x }\n\
                   const A: u8 = 1;\nconst B: u8 = 2;\ntype Pair = (u8, u8);\n\
                   fn last() -> u8 {\n    2\n}\n";
        let cut = cut_code("a.rs", src);
        assert!(has(&cut, 4, 4, "fn id"), "{cut:?}");
        assert!(has(&cut, 8, 10, "fn last"), "{cut:?}");
        assert!(
            spans(&cut, 5, 7),
            "the constants and the alias are one: {cut:?}"
        );
        assert_eq!(cut.len(), 4, "{cut:?}");
    }

    #[test]
    fn rust_methods_are_records_and_a_bare_header_joins_the_first() {
        let src = "struct Counter {\n    n: u32,\n}\n\nimpl Counter {\n    fn new() -> Self {\n\
                   \x20       Counter { n: 0 }\n    }\n\n    fn bump(&mut self) {\n\
                   \x20       self.n += 1;\n    }\n}\n";
        let cut = cut_code("a.rs", src);
        assert!(has(&cut, 1, 3, "struct Counter"), "{cut:?}");
        assert!(has(&cut, 5, 8, "fn Counter::new"), "{cut:?}");
        assert!(has(&cut, 10, 12, "fn Counter::bump"), "{cut:?}");
        assert_eq!(cut.len(), 3, "{cut:?}");
        let chunks = chunk_source(Path::new("a.rs"), src, &Config::default());
        let first = chunks.iter().find(|c| c.start_line == 5).unwrap();
        let held: Vec<(String, usize, usize)> = first
            .declarations
            .iter()
            .map(|d| (d.symbol.label(), d.start_line, d.end_line))
            .collect();
        assert_eq!(
            held,
            [
                ("impl Counter".to_string(), 5, 13),
                ("fn Counter::new".to_string(), 6, 8)
            ]
        );
    }

    #[test]
    fn a_trait_keeps_its_signatures_together_and_cuts_out_a_default_method() {
        let signatures =
            "trait Shape {\n    fn area(&self) -> f64;\n    fn name(&self) -> String;\n}\n";
        assert_eq!(cut_code("a.rs", signatures), [(1, 4, "trait Shape".into())]);
        let src = "trait Shape {\n    fn area(&self) -> f64;\n\n    fn describe(&self) -> String {\n\
                   \x20       format!(\"{}\", self.area())\n    }\n}\n";
        let cut = cut_code("a.rs", src);
        assert!(
            has(&cut, 1, 6, "fn Shape::describe"),
            "a header of two lines opens the first member: {cut:?}"
        );
        let src = "trait Shape {\n    fn area(&self) -> f64;\n    fn name(&self) -> String;\n\
                   \x20   fn id(&self) -> u8;\n\n    fn describe(&self) -> String {\n\
                   \x20       format!(\"{}\", self.area())\n    }\n}\n";
        let cut = cut_code("a.rs", src);
        assert!(has(&cut, 1, 4, "trait Shape"), "{cut:?}");
        assert!(has(&cut, 6, 8, "fn Shape::describe"), "{cut:?}");
    }

    #[test]
    fn python_functions_and_methods_are_records_and_the_docstring_opens_the_class() {
        let src = "import os\n\nLIMIT = 3\n\ndef load(path):\n    return open(path).read()\n\n\
                   class Store:\n    \"\"\"Keeps things.\"\"\"\n\n    def get(self, key):\n\
                   \x20       return self.items[key]\n\n    def put(self, key, value):\n\
                   \x20       self.items[key] = value\n";
        let cut = cut_code("a.py", src);
        assert!(has(&cut, 5, 6, "fn load"), "{cut:?}");
        assert!(has(&cut, 8, 12, "fn Store::get"), "{cut:?}");
        assert!(has(&cut, 14, 15, "fn Store::put"), "{cut:?}");
        assert!(spans(&cut, 1, 3), "{cut:?}");
        assert_apart(&cut);

        let src = "class Store:\n    \"\"\"Keeps things.\n\n    Nothing else.\n    \"\"\"\n\n\
                   \x20   def get(self, key):\n        return self.items[key]\n";
        let cut = cut_code("a.py", src);
        assert!(has(&cut, 1, 5, "class Store"), "{cut:?}");
        assert!(has(&cut, 7, 8, "fn Store::get"), "{cut:?}");
    }

    #[test]
    fn a_decorated_python_function_keeps_its_decorators() {
        let src = "@cache\n@wraps(f)\ndef slow(x):\n    return x\n\ndef fast(x):\n    return x\n";
        let cut = cut_code("a.py", src);
        assert!(has(&cut, 1, 4, "fn slow"), "{cut:?}");
        assert!(has(&cut, 6, 7, "fn fast"), "{cut:?}");
    }

    #[test]
    fn typescript_functions_methods_and_bound_functions_are_records() {
        let src = "import { a } from './a';\n\nexport interface Options {\n  limit: number;\n}\n\n\
                   export function parse(text: string): number {\n  return text.length;\n}\n\n\
                   export class Reader {\n  private a = 0;\n  private b = 1;\n  private c = 2;\n\n  read(): number {\n\
                   \x20   return this.a++;\n  }\n\n  reset(): void {\n    this.a = 0;\n  }\n}\n\n\
                   export const double = (x: number) => x * 2;\n";
        let cut = cut_code("a.ts", src);
        assert!(
            has(&cut, 1, 5, "interface Options"),
            "the import line joins the interface below it: {cut:?}"
        );
        assert!(has(&cut, 7, 9, "fn parse"), "{cut:?}");
        assert!(has(&cut, 11, 14, "class Reader"), "{cut:?}");
        assert!(has(&cut, 16, 18, "fn Reader::read"), "{cut:?}");
        assert!(has(&cut, 20, 22, "fn Reader::reset"), "{cut:?}");
        assert!(has(&cut, 25, 25, "fn double"), "{cut:?}");
        assert_apart(&cut);
    }

    #[test]
    fn go_functions_methods_and_types_are_records() {
        let src = "package main\n\nimport \"fmt\"\n\ntype Server struct {\n\tname string\n}\n\n\
                   func (s *Server) Name() string {\n\treturn s.name\n}\n\n\
                   func run() {\n\tfmt.Println(\"hi\")\n}\n";
        let cut = cut_code("a.go", src);
        assert!(has(&cut, 5, 7, "type Server"), "{cut:?}");
        assert!(has(&cut, 9, 11, "fn Server::Name"), "{cut:?}");
        assert!(has(&cut, 13, 15, "fn run"), "{cut:?}");
        assert_apart(&cut);
    }

    #[test]
    fn a_function_inside_a_function_stays_with_it() {
        let rust = "fn outer() {\n    fn inner() -> u8 {\n        1\n    }\n    inner();\n}\n";
        assert_eq!(cut_code("a.rs", rust), [(1, 6, "fn outer".into())]);
        let js =
            "function outer() {\n  function inner() {\n    return 1;\n  }\n  return inner();\n}\n";
        assert_eq!(cut_code("a.js", js), [(1, 6, "fn outer".into())]);
    }

    #[test]
    fn a_container_on_one_line_is_not_cut() {
        let src = "impl Road { fn preview() {} }\n";
        assert_eq!(cut_code("a.rs", src), [(1, 1, "impl Road".into())]);
    }

    #[test]
    fn a_file_without_declarations_to_cut_out_is_one_record() {
        let src = "use a::b;\nuse a::c;\nconst X: u8 = 1;\n";
        assert_eq!(cut_code("a.rs", src).len(), 1);
    }

    #[test]
    fn a_long_run_of_comment_lines_is_cut_by_size_not_line_by_line() {
        let mut src: String = (0..120).map(|n| format!("/// line {n}\n")).collect();
        src.push_str("pub struct S(u8);\n\nfn a() {\n    1;\n}\n");
        let cut = cut_code("a.rs", &src);
        assert!(cut.iter().all(|(s, e, _)| e - s + 1 >= 3), "{cut:?}");
        assert!(cut.len() <= 4, "{cut:?}");
    }

    #[test]
    fn a_few_lines_before_a_block_too_long_for_one_record_open_its_first_piece() {
        let mut src = String::from("describe('suite', () => {\n");
        for n in 0..60 {
            src.push_str(&format!(
                "  it('case {n}', () => {{\n    run({n});\n  }});\n"
            ));
        }
        src.push_str("});\n");
        let chunks = chunk_source(Path::new("a.test.js"), &src, &Config::default());
        assert!(
            chunks.iter().all(|c| c.end_line - c.start_line >= 2),
            "{chunks:?}"
        );
        assert!(chunks[0].text.starts_with("describe('suite'"));
    }

    #[test]
    fn a_function_too_long_for_one_record_has_no_fragment_among_its_pieces() {
        let mut src = String::from("fn long(a: u8) -> u8 {\n");
        for n in 0..105 {
            src.push_str(&format!("    let v{n} = a + {n};\n"));
        }
        src.push_str("    a\n}\n");
        let cut = cut_code("a.rs", &src);
        assert!(cut.iter().all(|(s, e, _)| e - s + 1 >= 3), "{cut:?}");
        assert!(cut.iter().all(|(_, _, l)| l == "fn long"), "{cut:?}");
    }

    #[test]
    fn a_comment_cut_off_by_a_blank_line_stays_with_the_function_above() {
        let src = "fn first() {\n    1;\n}\n// about the file\n\nfn second() {\n    2;\n}\n";
        let cut = cut_code("a.rs", src);
        assert!(has(&cut, 1, 4, "fn first"), "{cut:?}");
        assert!(has(&cut, 6, 8, "fn second"), "{cut:?}");
    }

    #[test]
    fn a_small_declaration_that_is_not_a_function_has_no_prose_record() {
        let src = "/// Where it starts.\nconst START: u8 = 1;\n\n\
                   /// Doubles a byte.\nfn double(x: u8) -> u8 { x * 2 }\n";
        let prose: Vec<String> = chunk_source(Path::new("a.rs"), src, &Config::default())
            .into_iter()
            .map(|c| c.text)
            .filter(|t| t.ends_with("byte.") || t.ends_with("starts."))
            .collect();
        assert_eq!(prose, ["Doubles a byte."]);
    }

    #[test]
    fn a_c_function_is_named_by_its_declarator_not_its_return_type() {
        let make = "Point make(int x) {\n  return Point{x};\n}\n";
        assert!(has(&cut_code("a.cpp", make), 1, 3, "fn make"));
        let twice = "template <typename T>\nT twice(T x) {\n  return x + x;\n}\n";
        let cut = cut_code("a.cpp", twice);
        assert!(has(&cut, 1, 4, "fn twice"), "{cut:?}");
    }

    /// Every declaration a file's chunks hold, as labels, in order.
    fn declared_labels(file: &str, src: &str) -> Vec<String> {
        chunk_source(Path::new(file), src, &Config::default())
            .iter()
            .flat_map(|c| c.declarations.iter().map(|d| d.symbol.label()))
            .collect()
    }

    #[test]
    fn a_c_declarator_is_not_a_second_declaration_of_its_function() {
        let c = "int apply(int (*f)(int), int x) {\n    return f(x);\n}\n";
        assert_eq!(declared_labels("a.c", c), ["fn apply"]);
        let cpp = "int geo::helper(int a) {\n    return a;\n}\n";
        assert_eq!(declared_labels("a.cpp", cpp), ["fn geo::helper"]);
        let call = "class Box {\n  int as() const { return static_cast<int>(v); }\n};\n";
        assert_eq!(declared_labels("a.cpp", call), ["class Box", "fn Box::as"]);
    }

    #[test]
    fn a_type_parameter_is_not_a_declaration() {
        let go = "type Set[T comparable] struct {\n\titems map[T]struct{}\n}\n\n\
                  func Map[T, U any](xs []T) []U {\n\treturn nil\n}\n";
        assert_eq!(declared_labels("a.go", go), ["type Set", "fn Map"]);
        let cpp = "class Box {\n  template <typename U>\n  U as() const { return U(v); }\n};\n";
        assert_eq!(declared_labels("a.cpp", cpp), ["class Box", "fn Box::as"]);
        let scala = "sealed trait Tree[+A]\n\ntype Pair[A] = (A, A)\n";
        assert_eq!(
            declared_labels("a.scala", scala),
            ["trait Tree", "type Pair"]
        );
    }

    #[test]
    fn a_record_is_a_declaration_with_its_members() {
        let java = "public record Point(int x, int y) {\n    public int sum() {\n        return x + y;\n    }\n}\n";
        assert_eq!(
            declared_labels("Point.java", java),
            ["record Point", "fn Point::sum"]
        );
        let cs =
            "public record Item(string Name, int Qty)\n{\n    public int Total() => Qty * 2;\n}\n";
        assert_eq!(
            declared_labels("a.cs", cs),
            ["record Item", "fn Item::Total"]
        );
    }

    #[test]
    fn a_go_method_is_named_by_its_receiver_type() {
        let go = "package store\n\ntype Set struct{}\n\nfunc (s *Set) Add(v int) {\n\t_ = v\n}\n\n\
                  func (Set) Len() int {\n\treturn 0\n}\n";
        assert_eq!(
            declared_labels("a.go", go),
            ["type Set", "fn Set::Add", "fn Set::Len"]
        );
    }

    #[test]
    fn a_function_assigned_to_a_property_or_a_name_is_named_by_it() {
        let js = "module.exports.parse = function (text) {\n  return text.trim();\n};\n";
        assert_eq!(declared_labels("a.js", js), ["fn parse"]);
        let py = "double = lambda x: x * 2\n\ndef run():\n    helper = lambda x: x\n    return helper(1)\n";
        assert_eq!(declared_labels("a.py", py), ["fn double", "fn run"]);
    }

    #[test]
    fn files_with_the_extensions_of_a_language_are_cut_as_that_language() {
        let ruby = "def build(x)\n  x + 1\nend\n";
        assert!(has(&cut_code("Tasks.rake", ruby), 1, 3, "fn build"));
        assert!(has(&cut_code("ripsem.gemspec", ruby), 1, 3, "fn build"));
        let python = "def load(path):\n    return path\n";
        assert!(has(&cut_code("setup.pyw", python), 1, 2, "fn load"));
        let cpp = "template <typename T>\nT twice(T x) {\n  return x + x;\n}\n";
        assert!(has(&cut_code("util.ipp", cpp), 1, 4, "fn twice"));
        assert!(has(&cut_code("util.tpp", cpp), 1, 4, "fn twice"));
    }
}
