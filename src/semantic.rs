use tree_sitter::{Node, Parser};

use crate::chunk::{Chunk, chunk_file};
use crate::lang::LangSpec;

pub fn chunk(text: &str, spec: &LangSpec, max_lines: usize, overlap: usize) -> Option<Vec<Chunk>> {
    let mut parser = Parser::new();
    parser.set_language(&spec.language).ok()?;
    let tree = parser.parse(text.as_bytes(), None)?;
    let mut out = Vec::new();
    emit(tree.root_node(), text, max_lines.max(1), overlap, &mut out);
    Some(out)
}

fn node_lines(node: &Node) -> usize {
    node.end_position().row - node.start_position().row + 1
}

fn push(start: &Node, end: &Node, text: &str, out: &mut Vec<Chunk>) {
    let body = &text[start.start_byte()..end.end_byte()];
    if body.trim().is_empty() {
        return;
    }
    out.push(Chunk {
        start_line: start.start_position().row + 1,
        end_line: end.end_position().row + 1,
        text: body.to_string(),
    });
}

fn emit(node: Node, text: &str, max_lines: usize, overlap: usize, out: &mut Vec<Chunk>) {
    if node_lines(&node) <= max_lines {
        push(&node, &node, text, out);
        return;
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    if children.is_empty() {
        let base = node.start_position().row;
        let slice = &text[node.start_byte()..node.end_byte()];
        for c in chunk_file(slice, max_lines, overlap) {
            out.push(Chunk {
                start_line: base + c.start_line,
                end_line: base + c.end_line,
                text: c.text,
            });
        }
        return;
    }

    let mut group: Option<(Node, Node)> = None;
    let mut lines = 0usize;
    for child in children {
        let child_lines = node_lines(&child);
        if child_lines > max_lines {
            if let Some((s, e)) = group.take() {
                push(&s, &e, text, out);
            }
            lines = 0;
            emit(child, text, max_lines, overlap, out);
        } else if lines + child_lines > max_lines {
            if let Some((s, e)) = group.take() {
                push(&s, &e, text, out);
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
        push(&s, &e, text, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn chunk_rust(src: &str, max: usize) -> Vec<Chunk> {
        let spec = crate::lang::detect(Path::new("x.rs")).unwrap();
        chunk(src, &spec, max, 4).unwrap()
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
            let spec = crate::lang::detect(Path::new(file)).unwrap();
            let chunks = chunk(src, &spec, 40, 4).unwrap();
            assert!(!chunks.is_empty(), "no chunks for {file}");
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
