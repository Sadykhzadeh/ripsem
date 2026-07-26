use tree_sitter::{Node, Parser};

use crate::chunk::{Chunk, chunk_file};
use crate::lang::LangSpec;
use crate::symbol::{self, Symbol};

pub fn chunk(text: &str, spec: &LangSpec, max_lines: usize, overlap: usize) -> Option<Vec<Chunk>> {
    let mut parser = Parser::new();
    parser.set_language(&spec.language).ok()?;
    let tree = parser.parse(text.as_bytes(), None)?;
    let mut emitter = Emitter {
        text,
        max_lines: max_lines.max(1),
        overlap,
        out: Vec::new(),
    };
    emitter.walk(tree.root_node(), None);
    Some(emitter.out)
}

fn node_lines(node: &Node) -> usize {
    node.end_position().row - node.start_position().row + 1
}

struct Emitter<'a> {
    text: &'a str,
    max_lines: usize,
    overlap: usize,
    out: Vec<Chunk>,
}

impl Emitter<'_> {
    /// `enclosing` carries the declaration the caller is inside, so a function
    /// too large to be one chunk still stamps its name on each of its pieces —
    /// otherwise the fragments of exactly the longest functions, where a reader
    /// most needs to know where they are, arrive anonymous.
    fn walk(&mut self, node: Node, enclosing: Option<&Symbol>) {
        if node_lines(&node) <= self.max_lines {
            self.push(&node, &node, enclosing);
            return;
        }

        let declared = symbol::declaration(&node, self.text, enclosing);
        let scope = declared.as_ref().or(enclosing);

        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        if children.is_empty() {
            self.push_windows(&node, scope);
            return;
        }

        let mut group: Option<(Node, Node)> = None;
        let mut lines = 0usize;
        for child in children {
            let child_lines = node_lines(&child);
            if child_lines > self.max_lines {
                if let Some((s, e)) = group.take() {
                    self.push(&s, &e, scope);
                }
                lines = 0;
                self.walk(child, scope);
            } else if lines + child_lines > self.max_lines {
                if let Some((s, e)) = group.take() {
                    self.push(&s, &e, scope);
                }
                group = Some((child, child));
                lines = child_lines;
            } else {
                group = match group {
                    Some((s, _)) => Some((s, child)),
                    None => Some((child, child)),
                };
                lines += child_lines;
            }
        }
        if let Some((s, e)) = group {
            self.push(&s, &e, scope);
        }
    }

    /// A chunk that groups several small declarations is named after the first
    /// of them, which is where its line range starts. Naming it after the
    /// enclosing block instead would answer `mod tests` to every question about
    /// a test, which is true and useless.
    fn push(&mut self, start: &Node, end: &Node, enclosing: Option<&Symbol>) {
        let body = &self.text[start.start_byte()..end.end_byte()];
        if body.trim().is_empty() {
            return;
        }
        let own = self.first_declaration(start, end, enclosing);
        self.out.push(Chunk {
            start_line: start.start_position().row + 1,
            end_line: end.end_position().row + 1,
            text: body.to_string(),
            symbol: own.or_else(|| enclosing.cloned()),
        });
    }

    /// Attributes, decorators, export wrappers and import lists all sit between
    /// the start of a chunk and the declaration it is really about, so finding
    /// that declaration means scanning forward and downward, not just asking the
    /// first node.
    fn first_declaration(
        &self,
        start: &Node,
        end: &Node,
        enclosing: Option<&Symbol>,
    ) -> Option<Symbol> {
        let mut node = Some(*start);
        while let Some(current) = node {
            if current.start_byte() > end.end_byte() {
                return None;
            }
            if let Some(found) = self.declaration_within(&current, enclosing) {
                return Some(found);
            }
            node = current.next_named_sibling();
        }
        None
    }

    fn declaration_within(&self, node: &Node, enclosing: Option<&Symbol>) -> Option<Symbol> {
        if let Some(found) = symbol::declaration(node, self.text, enclosing) {
            return Some(found);
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        children
            .iter()
            .find_map(|child| self.declaration_within(child, enclosing))
    }

    fn push_windows(&mut self, node: &Node, enclosing: Option<&Symbol>) {
        let base = node.start_position().row;
        let slice = &self.text[node.start_byte()..node.end_byte()];
        for c in chunk_file(slice, self.max_lines, self.overlap) {
            self.out.push(Chunk {
                start_line: base + c.start_line,
                end_line: base + c.end_line,
                text: c.text,
                symbol: enclosing.cloned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn chunk_source(file: &str, src: &str, max: usize) -> Vec<Chunk> {
        let spec = crate::lang::detect(Path::new(file)).unwrap();
        chunk(src, &spec, max, 4).unwrap()
    }

    fn chunk_rust(src: &str, max: usize) -> Vec<Chunk> {
        chunk_source("x.rs", src, max)
    }

    fn labels(chunks: &[Chunk]) -> Vec<String> {
        chunks
            .iter()
            .filter_map(|c| c.symbol.as_ref().map(Symbol::label))
            .collect()
    }

    #[test]
    fn small_file_is_a_single_chunk() {
        let src = "fn a() {}\nfn b() {}\n";
        let chunks = chunk_rust(src, 40);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
    }

    fn make_fn(name: &str, body_lines: usize, value: u32) -> String {
        let mut s = format!("fn {name}() {{\n");
        for _ in 0..body_lines {
            s.push_str(&format!("    let _ = {value};\n"));
        }
        s.push_str("}\n");
        s
    }

    #[test]
    fn splits_on_function_boundaries_when_over_budget() {
        let mut src = make_fn("first", 12, 1);
        src.push('\n');
        src.push_str(&make_fn("second", 12, 2));

        let chunks = chunk_rust(&src, 20);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().any(|c| c.text.contains("fn first")));
        assert!(chunks.iter().any(|c| c.text.contains("fn second")));
    }

    #[test]
    fn each_function_chunk_names_its_declaration() {
        let mut src = make_fn("first", 12, 1);
        src.push_str(&make_fn("second", 12, 2));
        let chunks = chunk_rust(&src, 20);
        assert!(labels(&chunks).contains(&"fn first".to_string()));
        assert!(labels(&chunks).contains(&"fn second".to_string()));
    }

    #[test]
    fn methods_are_qualified_by_their_type() {
        let mut src = String::from("struct Searcher;\nimpl Searcher {\n");
        src.push_str(&make_fn("query_scoped", 12, 1));
        src.push_str(&make_fn("refresh", 12, 2));
        src.push_str("}\n");

        let chunks = chunk_rust(&src, 20);
        let found = labels(&chunks);
        assert!(
            found.contains(&"fn Searcher::query_scoped".to_string()),
            "got {found:?}"
        );
        assert!(found.contains(&"fn Searcher::refresh".to_string()));
    }

    /// The case the fixed-window fallback used to lose: a function larger than
    /// the budget is cut into pieces, and every piece still says where it is.
    #[test]
    fn oversized_function_passes_its_name_to_every_fragment() {
        let src = make_fn("enormous", 200, 7);
        let chunks = chunk_rust(&src, 20);
        assert!(chunks.len() > 2);
        for c in &chunks {
            assert_eq!(
                c.symbol.as_ref().map(Symbol::label),
                Some("fn enormous".to_string()),
                "fragment at {}-{} lost its symbol",
                c.start_line,
                c.end_line
            );
        }
    }

    #[test]
    fn names_declarations_across_languages() {
        let cases = [
            ("m.py", "def first():\n    return 1\n", "fn first"),
            ("c.py", "class Thing:\n    pass\n", "class Thing"),
            (
                "g.go",
                "package main\nfunc first() int { return 1 }\n",
                "fn first",
            ),
            (
                "t.ts",
                "export class Store {\n  load() { return 1 }\n}\n",
                "class Store",
            ),
            (
                "j.java",
                "class Box { int size() { return 1; } }\n",
                "class Box",
            ),
            ("r.rb", "def compute\n  1\nend\n", "fn compute"),
            ("s.sh", "foo() {\n  echo hi\n}\n", "fn foo"),
            ("n.c", "int add(int a, int b) { return a + b; }\n", "fn add"),
            (
                "p.php",
                "<?php\nfunction render($x) { return $x; }\n",
                "fn render",
            ),
            (
                "h.cpp",
                "class Buffer { public: int size() { return 1; } };\n",
                "class Buffer",
            ),
            (
                "d.cs",
                "class Bag { int Count() { return 1; } }\n",
                "class Bag",
            ),
            (
                "k.kt",
                "class Holder {\n    fun load(): Int = 1\n}\n",
                "class Holder",
            ),
            (
                "a.scala",
                "class Repo {\n  def load(): Int = 1\n}\n",
                "class Repo",
            ),
        ];
        for (file, src, expected) in cases {
            let chunks = chunk_source(file, src, 40);
            assert!(
                labels(&chunks).iter().any(|l| l == expected),
                "{file}: expected {expected:?}, got {:?}",
                labels(&chunks)
            );
        }
    }

    #[test]
    fn chunks_a_range_of_languages() {
        let cases = [
            (
                "m.py",
                "def first():\n    return 1\n\ndef second():\n    return 2\n",
            ),
            ("s.sh", "foo() {\n  echo hi\n}\nbar() {\n  echo bye\n}\n"),
            (
                "p.php",
                "<?php\nfunction first() { return 1; }\nfunction second() { return 2; }\n",
            ),
            (
                "g.go",
                "package main\nfunc first() int { return 1 }\nfunc second() int { return 2 }\n",
            ),
        ];
        for (file, src) in cases {
            assert!(
                !chunk_source(file, src, 40).is_empty(),
                "no chunks for {file}"
            );
        }
    }

    #[test]
    fn line_ranges_are_within_the_file() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let chunks = chunk_rust(src, 1);
        let total = src.lines().count();
        for c in &chunks {
            assert!(c.start_line >= 1);
            assert!(c.end_line <= total);
            assert!(c.start_line <= c.end_line);
        }
    }
}
