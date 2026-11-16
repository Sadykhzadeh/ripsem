use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::{Node, Parser};

use crate::chunk::{Chunk, chunk_file};
use crate::lang::LangSpec;
use crate::symbol::{self, Declaration, Symbol};

pub fn chunk(text: &str, spec: &LangSpec, max_lines: usize, overlap: usize) -> Option<Vec<Chunk>> {
    let mut parser = Parser::new();
    parser.set_language(&spec.language).ok()?;
    let tree = parser.parse(text.as_bytes(), None)?;
    if deeper_than(tree.root_node(), MAX_DEPTH) {
        return None;
    }
    let mut emitter = Emitter {
        text,
        max_lines: max_lines.max(1),
        overlap,
        out: Vec::new(),
        last: None,
        prefix: None,
        raw: RefCell::new(HashMap::new()),
    };
    emitter.walk(tree.root_node(), None);
    emitter.documentation(tree.root_node(), None);
    Some(emitter.out)
}

/// The walk below recurses once per level of the tree, and a few thousand
/// levels overflow the stack. Only generated text nests that deep (a chain of
/// thousands of `+`, a JSON document nested as deep), and line windows serve
/// it as well as anything.
const MAX_DEPTH: usize = 512;

fn deeper_than(root: Node, limit: usize) -> bool {
    let mut cursor = root.walk();
    let mut depth = 0;
    loop {
        if cursor.goto_first_child() {
            depth += 1;
            if depth > limit {
                return true;
            }
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
            depth -= 1;
        }
    }
}

/// A node starts after its indentation, so a chunk cut at the node would lose
/// the first line's indent and keep every other line's.
fn line_start(text: &str, byte: usize) -> usize {
    let start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
    match text[start..byte].trim().is_empty() {
        true => start,
        false => byte,
    }
}

fn node_lines(node: &Node, text: &str) -> usize {
    last_row(node, text) - node.start_position().row + 1
}

/// The row of a node's last line. A node that takes in the newline after its
/// last line, as a whole file does, ends at the start of the row below it.
fn last_row(node: &Node, text: &str) -> usize {
    let body = &text[node.start_byte()..node.end_byte()];
    let newlines = body[body.trim_end().len()..].matches('\n').count();
    node.end_position()
        .row
        .saturating_sub(newlines)
        .max(node.start_position().row)
}

struct Emitter<'a> {
    text: &'a str,
    max_lines: usize,
    overlap: usize,
    out: Vec<Chunk>,
    /// Where the text of the last record cut from nodes starts, and the row
    /// it ends on.
    last: Option<(usize, usize)>,
    /// The start of a few lines that open the next record.
    prefix: Option<(usize, usize)>,
    /// What each node declares, by node id, for the passes that ask about the same
    /// nodes again.
    raw: RefCell<HashMap<usize, RawName>>,
}

/// What a node declares: its kind and the bytes of its name, or nothing.
type RawName = Option<(&'static str, (usize, usize))>;

/// A declaration that can be a record of its own, with the node that declares
/// it. The node a chunk is cut at can be a wrapper around it: `export` around a
/// TypeScript function, a decorator list around a Python class.
struct Unit<'t> {
    node: Node<'t>,
    symbol: Symbol,
}

/// A container whose members are cut out one by one. The lines from its start
/// to its first member are the record that opens it.
struct Opening<'t> {
    /// Where the record starts: the comments and attributes above the
    /// container, or the container itself.
    start: Node<'t>,
    unit: Unit<'t>,
}

/// Declarations other than functions are cut out as records of their own only
/// from this many lines on. Below it, a constant or an alias says too little to
/// be a place worth finding, and it stays with its neighbours.
const STANDALONE_FLOOR: usize = 3;

/// A header of at most this many lines (`impl Road {`) is no record of its own:
/// it opens the record of the first member.
const FRAGMENT_LINES: usize = 2;

/// The node holding a container's members: its body, or the container itself
/// for a grammar that names no body.
fn members_parent<'t>(node: &Node<'t>) -> Node<'t> {
    node.child_by_field_name("body").unwrap_or(*node)
}

/// A function with a body is code to read; a signature in a trait, an interface
/// or an overload list only names one.
fn has_body(node: &Node) -> bool {
    node.child_by_field_name("body").is_some()
        || node
            .child_by_field_name("value")
            .is_some_and(|value| value.child_by_field_name("body").is_some())
}

/// Comments, attributes and decorators belong to the declaration below them.
fn is_lead(node: &Node) -> bool {
    let kind = node.kind();
    ["comment", "attribute", "decorator"]
        .iter()
        .any(|k| kind.contains(k))
}

impl Emitter<'_> {
    fn declared(&self, node: &Node, enclosing: Option<&Symbol>) -> Option<Symbol> {
        let (kind, (start, end)) = (*self
            .raw
            .borrow_mut()
            .entry(node.id())
            .or_insert_with(|| symbol::raw_declaration(node)))?;
        Some(symbol::named(kind, &self.text[start..end], enclosing))
    }

    /// `enclosing` is the declaration being walked, so every piece of a function
    /// too large for one chunk keeps its name. The piece holding its first line
    /// also declares it, which is where a search for the name should land.
    fn walk(&mut self, node: Node, enclosing: Option<&Symbol>) {
        let root = node.parent().is_none();
        if node_lines(&node, self.text) <= self.max_lines && !(root && self.has_units(&node)) {
            self.push(&node, &node, enclosing, None, None);
            return;
        }

        let declared = self.declared(&node, enclosing);
        let scope = declared.as_ref().or(enclosing);
        let first = self.out.len();
        self.walk_children(node, scope, None);
        if let Some(symbol) = declared
            && let Some(head) = self.out.get_mut(first)
        {
            head.declarations.insert(
                0,
                Declaration {
                    symbol,
                    start_line: node.start_position().row + 1,
                    end_line: last_row(&node, self.text) + 1,
                },
            );
        }
    }

    /// The declaration a child of a file or a container stands for, looking
    /// through wrappers that hold nothing else: `export`, a decorator list, the
    /// statement around a bound function.
    fn unit<'t>(&self, child: Node<'t>, scope: Option<&Symbol>) -> Option<Unit<'t>> {
        let mut node = child;
        for _ in 0..4 {
            if let Some(symbol) = self.declared(&node, scope) {
                return Some(Unit { node, symbol });
            }
            let mut cursor = node.walk();
            let mut inside = node.named_children(&mut cursor).filter(|n| !is_lead(n));
            let only = inside.next()?;
            if inside.next().is_some() || child.end_byte() > only.end_byte() + 1 {
                return None;
            }
            node = only;
        }
        None
    }

    /// Whether a declaration is a record of its own however few lines it has: a
    /// function with a body is what a question is about. A signature, a
    /// constant or an alias is one only from a few lines on.
    fn stands_alone(&self, unit: &Unit) -> bool {
        match unit.symbol.kind.as_str() {
            "fn" if has_body(&unit.node) => true,
            _ => node_lines(&unit.node, self.text) >= STANDALONE_FLOOR,
        }
    }

    /// Whether a file has declarations to cut out of it.
    fn has_units(&self, node: &Node) -> bool {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        children
            .into_iter()
            .filter_map(|child| self.unit(child, None))
            .any(|unit| self.carves(&unit) || self.stands_alone(&unit))
    }

    /// A container is cut up when one of its members is a function with a body,
    /// or a container that is itself cut up, and that member starts on a line
    /// below the container's first.
    fn carves(&self, unit: &Unit) -> bool {
        if !unit.symbol.is_container() {
            return false;
        }
        let mut cursor = members_parent(&unit.node).walk();
        let members: Vec<Node> = members_parent(&unit.node)
            .named_children(&mut cursor)
            .collect();
        members
            .into_iter()
            .filter_map(|member| self.unit(member, Some(&unit.symbol)))
            .find(|u| match u.symbol.is_container() {
                true => self.carves(u),
                false => u.symbol.kind == "fn" && has_body(&u.node),
            })
            .is_some_and(|u| u.node.start_position().row > unit.node.start_position().row)
    }

    fn walk_children(&mut self, node: Node, scope: Option<&Symbol>, mut opening: Option<Opening>) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        if children.is_empty() || node.is_error() {
            self.push_windows(&node, scope);
            return;
        }

        let splits = scope.is_none_or(Symbol::is_container);
        let mut group: Vec<Node> = Vec::new();
        let mut lines = 0usize;
        for child in children.iter().copied() {
            let child_lines = node_lines(&child, self.text);
            if splits && let Some(unit) = self.unit(child, scope) {
                let carves = self.carves(&unit);
                let lead = self.lead_in(&children[0], &child);
                let begin = lead.unwrap_or(child);
                let size = last_row(&child, self.text) - begin.start_position().row + 1;
                if carves || (size <= self.max_lines && self.stands_alone(&unit)) {
                    while group
                        .last()
                        .is_some_and(|n| n.start_byte() >= begin.start_byte())
                    {
                        group.pop();
                    }
                    let begin_row = begin.start_position().row;
                    let joins = !carves
                        && match (group.first().zip(group.last()), opening.as_ref()) {
                            (Some((first, last)), open) => {
                                let from = open.map_or(first, |o| &o.start).start_position().row;
                                let end = last_row(last, self.text);
                                end - from < FRAGMENT_LINES && begin_row - end <= FRAGMENT_LINES
                            }
                            (None, Some(open)) => {
                                begin_row - open.start.start_position().row <= FRAGMENT_LINES
                            }
                            (None, None) => false,
                        };
                    let detached = joins
                        && opening.is_none()
                        && group.iter().all(is_lead)
                        && group
                            .last()
                            .is_some_and(|l| begin_row - last_row(l, self.text) > 1)
                        && self.extend_last(&group);
                    if detached {
                        group.clear();
                    }
                    let (start, joined) = match joins && !detached {
                        true => {
                            let start = group.first().copied().unwrap_or(begin);
                            group.clear();
                            (start, opening.take())
                        }
                        false => {
                            self.flush(&mut group, &mut opening, scope, Some(&begin));
                            (begin, None)
                        }
                    };
                    lines = 0;
                    if carves {
                        let inner = unit.symbol.clone();
                        let parent = members_parent(&unit.node);
                        let open = Opening { start: begin, unit };
                        self.walk_children(parent, Some(&inner), Some(open));
                    } else {
                        let symbol = Some(unit.symbol.clone());
                        let open = joined.as_ref().map(|o| (o, true));
                        self.push(&start, &child, scope, open, symbol);
                    }
                    continue;
                }
            }
            if child_lines > self.max_lines {
                match self.is_fragment(&group) && opening.is_none() {
                    true => {
                        self.prefix = self.prefix.or(group
                            .first()
                            .map(|n| (n.start_byte(), n.start_position().row)));
                        group.clear();
                    }
                    false => self.flush(&mut group, &mut opening, scope, Some(&child)),
                }
                lines = 0;
                self.walk(child, scope);
            } else if lines + child_lines > self.max_lines {
                let lead = group.first().and_then(|s| self.lead_in(s, &child));
                let keep = match lead {
                    Some(first) => group
                        .iter()
                        .position(|n| n.start_byte() >= first.start_byte())
                        .unwrap_or(group.len()),
                    None => group.len(),
                };
                let rest = group.split_off(keep);
                self.flush(
                    &mut group,
                    &mut opening,
                    scope,
                    rest.first().or(Some(&child)),
                );
                group = rest;
                group.push(child);
                lines = last_row(&child, self.text) - group[0].start_position().row + 1;
            } else {
                group.push(child);
                lines += child_lines;
            }
        }
        self.flush(&mut group, &mut opening, scope, None);
    }

    /// Whether the nodes grouped so far are too few lines to be a record.
    fn is_fragment(&self, group: &[Node]) -> bool {
        group
            .first()
            .zip(group.last())
            .is_some_and(|(first, last)| {
                last_row(last, self.text) - first.start_position().row < FRAGMENT_LINES
            })
    }

    /// The nodes grouped so far become one record, and so does the opening of a
    /// container when nothing was grouped after it, up to `next`. A few lines
    /// left over at the end of a block join the record before them.
    fn flush(
        &mut self,
        group: &mut Vec<Node>,
        opening: &mut Option<Opening>,
        scope: Option<&Symbol>,
        next: Option<&Node>,
    ) {
        let open = opening.take();
        if next.is_none() && open.is_none() && self.is_fragment(group) && self.extend_last(group) {
            group.clear();
            return;
        }
        match (group.first().zip(group.last()), open) {
            (Some((first, last)), open) => {
                self.push(first, last, scope, open.as_ref().map(|o| (o, false)), None);
            }
            (None, Some(open)) => {
                let Some(next) = next else { return };
                let from = line_start(self.text, open.start.start_byte());
                let to = line_start(self.text, next.start_byte());
                let body = self.text[from..to.max(from)].trim_end();
                if body.trim().is_empty() {
                    return;
                }
                let start_row = open.start.start_position().row;
                self.out.push(Chunk {
                    start_line: start_row + 1,
                    end_line: start_row + body.lines().count(),
                    text: body.to_string(),
                    declarations: vec![self.whole(&open.unit)],
                    symbol: Some(open.unit.symbol),
                });
            }
            (None, None) => {}
        }
        group.clear();
    }

    /// Adds `group` to the record cut just before it, when that ends within a
    /// line or two of it.
    fn extend_last(&mut self, group: &[Node]) -> bool {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            return false;
        };
        let Some((from, row)) = self.last else {
            return false;
        };
        if first.start_byte() < from || first.start_position().row > row + FRAGMENT_LINES + 1 {
            return false;
        }
        let mut declarations = Vec::new();
        for node in group {
            self.declarations_within(*node, None, &mut declarations);
        }
        let text = self.text[from..last.end_byte()].to_string();
        let end_line = last_row(last, self.text) + 1;
        let Some(chunk) = self.out.last_mut() else {
            return false;
        };
        chunk.text = text;
        chunk.end_line = end_line;
        chunk.declarations.append(&mut declarations);
        self.last = Some((from, end_line - 1));
        true
    }

    fn whole(&self, unit: &Unit) -> Declaration {
        Declaration {
            symbol: unit.symbol.clone(),
            start_line: unit.node.start_position().row + 1,
            end_line: last_row(&unit.node, self.text) + 1,
        }
    }

    /// The comments and attributes right above `next`, back to `start` at most,
    /// that open a chunk with it within the size limit: the first of them. A
    /// doc comment or a `#[test]` belongs with the declaration below it, not at
    /// the end of the chunk before. One after code on the same line, or with a
    /// blank line below it, is about something else.
    fn lead_in<'t>(&self, start: &Node<'t>, next: &Node<'t>) -> Option<Node<'t>> {
        if is_lead(next) {
            return None;
        }
        let end = last_row(next, self.text);
        let mut first = None;
        let mut below = *next;
        while let Some(node) = below.prev_named_sibling() {
            let trailing = node
                .prev_named_sibling()
                .is_some_and(|before| last_row(&before, self.text) == node.start_position().row);
            if !is_lead(&node)
                || trailing
                || node.start_byte() < start.start_byte()
                || last_row(&node, self.text) + 1 < below.start_position().row
                || end - node.start_position().row + 1 > self.max_lines
            {
                break;
            }
            first = Some(node);
            below = node;
        }
        first
    }

    /// A chunk grouping several small declarations is named after the first of
    /// them, not the enclosing block (which would label every test `mod tests`).
    /// A record that opens a container starts where the container does and
    /// lists it first among its declarations. It is named after the container,
    /// unless it joins the first member's record (`true`), which keeps the
    /// member's name. A declaration's own record is given its symbol, so a
    /// constant joined to it does not name it.
    fn push(
        &mut self,
        start: &Node,
        end: &Node,
        enclosing: Option<&Symbol>,
        opening: Option<(&Opening, bool)>,
        named: Option<Symbol>,
    ) {
        let from = opening.map_or(*start, |(o, _)| o.start);
        let (from_byte, from_row) = match self.prefix.take() {
            Some(prefix) if opening.is_none() => prefix,
            _ => (from.start_byte(), from.start_position().row),
        };
        let body = &self.text[line_start(self.text, from_byte)..end.end_byte()];
        if body.trim().is_empty() {
            return;
        }
        self.last = Some((line_start(self.text, from_byte), last_row(end, self.text)));
        let mut declarations = Vec::new();
        if let Some((open, _)) = opening {
            declarations.push(self.whole(&open.unit));
        }
        let mut node = Some(*start);
        while let Some(current) = node.filter(|n| n.start_byte() <= end.start_byte()) {
            self.declarations_within(current, enclosing, &mut declarations);
            node = current.next_named_sibling();
        }
        let named = match opening {
            Some((open, false)) => Some(open.unit.symbol.clone()),
            _ => named.or_else(|| self.first_declaration(start, end, enclosing)),
        };
        self.out.push(Chunk {
            start_line: from_row + 1,
            end_line: last_row(end, self.text) + 1,
            text: body.to_string(),
            symbol: named.or_else(|| enclosing.cloned()),
            declarations,
        });
    }

    fn declarations_within(
        &self,
        node: Node,
        enclosing: Option<&Symbol>,
        out: &mut Vec<Declaration>,
    ) {
        let declared = self.declared(&node, enclosing);
        if let Some(symbol) = &declared {
            out.push(Declaration {
                symbol: symbol.clone(),
                start_line: node.start_position().row + 1,
                end_line: last_row(&node, self.text) + 1,
            });
        }
        let scope = declared.as_ref().or(enclosing);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.declarations_within(child, scope, out);
        }
    }

    /// Attributes, decorators, export wrappers and imports can sit before the
    /// declaration a chunk is about, so this scans forward and downward rather
    /// than asking only the first node.
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
        if let Some(found) = self.declared(node, enclosing) {
            return Some(found);
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find_map(|child| self.declaration_within(&child, enclosing))
    }

    /// Indexes a declaration's documentation as a record of its own. It spans the
    /// declaration, not the comment, so a hit still points at code. A function
    /// body rarely uses the words its author would use to explain it.
    fn documentation(&mut self, node: Node, enclosing: Option<&Symbol>) {
        let declared = self.declared(&node, enclosing);
        if let Some(symbol) = &declared
            && (symbol.kind == "fn" || node_lines(&node, self.text) >= STANDALONE_FLOOR)
            && let Some(prose) = self.prose_for(&node)
        {
            let start_line = node.start_position().row + 1;
            let end_line = last_row(&node, self.text) + 1;
            self.out.push(Chunk {
                start_line,
                end_line,
                text: prose,
                symbol: Some(symbol.clone()),
                declarations: vec![Declaration {
                    symbol: symbol.clone(),
                    start_line,
                    end_line,
                }],
            });
        }
        let scope = declared.as_ref().or(enclosing);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.documentation(child, scope);
        }
    }

    fn prose_for(&self, node: &Node) -> Option<String> {
        let raw = comment_above(node, self.text).or_else(|| leading_string(node, self.text))?;
        let prose = as_prose(&raw);
        (!prose.is_empty()).then_some(prose)
    }

    fn push_windows(&mut self, node: &Node, enclosing: Option<&Symbol>) {
        let (from, base) = self
            .prefix
            .take()
            .unwrap_or((node.start_byte(), node.start_position().row));
        let slice = &self.text[line_start(self.text, from)..node.end_byte()];
        for c in chunk_file(slice, self.max_lines, self.overlap) {
            self.out.push(Chunk {
                start_line: base + c.start_line,
                end_line: base + c.end_line,
                text: c.text,
                symbol: enclosing.cloned(),
                declarations: Vec::new(),
            });
        }
    }
}

/// Gathers the doc comment in front of a node by walking backwards. A comment
/// separated by a blank line (licence header, section divider) is not one.
fn comment_above(node: &Node, text: &str) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();
    let mut below = past_prefixes(node, text);
    while let Some(previous) = below.prev_named_sibling() {
        if previous.end_position().row + 1 < below.start_position().row {
            break;
        }
        let kind = previous.kind();
        if kind.contains("attribute") || kind.contains("decorator") || kind.contains("modifier") {
            below = previous;
            continue;
        }
        if !kind.contains("comment") {
            break;
        }
        lines.push(&text[previous.start_byte()..previous.end_byte()]);
        below = previous;
    }
    (!lines.is_empty()).then(|| {
        lines.reverse();
        lines.join("\n")
    })
}

/// `export`, `public` and similar wrap a declaration on the same line, so the
/// comment sits beside the wrapper. Climbs only through wrappers that start on
/// that line, which keeps it from reaching the file node. A wrapper may end in
/// a semicolon, as `const load = () => {};` wraps the declarator `load`, but
/// in `let a = f, b = g;` the comment is not about `b`, so a node that follows
/// another of its kind stays where it is.
fn past_prefixes<'tree>(node: &Node<'tree>, text: &str) -> Node<'tree> {
    let mut current = *node;
    while let Some(parent) = current.parent() {
        let tail = &text[current.end_byte()..parent.end_byte()];
        let same_row = parent.start_position().row == current.start_position().row;
        let follows_its_kind = current
            .prev_named_sibling()
            .is_some_and(|previous| previous.kind() == current.kind());
        if !same_row || !matches!(tail, "" | ";") || follows_its_kind {
            break;
        }
        current = parent;
    }
    current
}

/// Python and similar languages put the docstring first in the body, where the
/// comment search does not look.
fn leading_string(node: &Node, text: &str) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let first = body.named_children(&mut cursor).next()?;
    let string = match first.kind().contains("string") {
        true => first,
        false => {
            let mut inner = first.walk();
            first
                .named_children(&mut inner)
                .next()
                .filter(|n| n.kind().contains("string"))?
        }
    };
    Some(text[string.start_byte()..string.end_byte()].to_string())
}

/// Strips comment and string markers and joins the lines into one run of prose.
fn as_prose(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let cleaned = line
            .trim()
            .trim_start_matches(['/', '*', '#', '!', '"', '\'', '-'])
            .trim_end_matches(['/', '*', '"', '\''])
            .trim();
        if cleaned.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(cleaned);
    }
    out
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
    fn small_file_without_declarations_to_cut_out_is_a_single_chunk() {
        let src = "const A: u8 = 1;\nconst B: u8 = 2;\n";
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
    fn a_doc_comment_and_attribute_stay_with_the_function_below() {
        let mut src = String::new();
        for (name, value) in [("first", 1), ("second", 2), ("third", 3)] {
            src.push_str(&format!("/// Opens the {name} door.\n#[inline]\n"));
            src.push_str(&make_fn(name, 12, value));
        }
        let chunks = chunk_rust(&src, 20);
        for chunk in chunks.iter().filter(|c| c.text.contains("let _")) {
            let first = chunk.text.lines().next().unwrap();
            assert!(first.starts_with("/// Opens"), "{}", chunk.text);
            let last = chunk.text.trim_end().lines().last().unwrap();
            assert_eq!(last, "}", "{}", chunk.text);
        }
        let starts: Vec<usize> = chunks
            .iter()
            .filter(|c| c.text.contains("let _"))
            .map(|c| c.start_line)
            .collect();
        assert_eq!(starts, [1, 17, 33]);
    }

    #[test]
    fn a_comment_after_code_or_a_blank_line_stays_where_it_is() {
        let mut src = make_fn("first", 12, 1);
        src.push_str("const LIMIT: u32 = 3; // about LIMIT\n");
        src.push_str(&make_fn("second", 12, 2));
        src.push_str("// about the file\n\n");
        src.push_str(&make_fn("third", 12, 3));
        let chunks = chunk_rust(&src, 20);
        let holding = |needle: &str| chunks.iter().find(|c| c.text.contains(needle)).unwrap();
        assert!(holding("about LIMIT").text.contains("const LIMIT"));
        assert!(!holding("about the file").text.contains("fn third"));
        assert!(holding("fn third").text.starts_with("fn third"));
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

    /// A function larger than the budget is cut into pieces, and each piece
    /// keeps the function's name.
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
            let declared: Vec<String> = chunks
                .iter()
                .flat_map(|c| c.declarations.iter().map(|d| d.symbol.label()))
                .chain(labels(&chunks))
                .collect();
            assert!(
                declared.iter().any(|l| l == expected),
                "{file}: expected {expected:?}, got {declared:?}"
            );
        }
    }

    #[test]
    fn names_functions_bound_to_a_name() {
        let cases = [
            (
                "a.js",
                "const load = (path) => {\n  return path;\n};\n",
                "fn load",
            ),
            ("b.js", "export const run = async () => {}\n", "fn run"),
            ("c.js", "const parse = function (text) {}\n", "fn parse"),
            ("d.mjs", "const ids = function* () {}\n", "fn ids"),
            (
                "e.js",
                "class Store {\n  handle = () => {}\n}\n",
                "fn Store::handle",
            ),
            (
                "f.ts",
                "export const render = (rows: Row[]): string => rows.join()\n",
                "fn render",
            ),
            (
                "g.ts",
                "class Store {\n  private handle = (): void => {}\n}\n",
                "fn Store::handle",
            ),
            ("h.tsx", "export const App = () => <div />;\n", "fn App"),
        ];
        for (file, src, expected) in cases {
            let chunks = chunk_source(file, src, 1);
            assert!(
                labels(&chunks).iter().any(|l| l == expected),
                "{file}: expected {expected:?}, got {:?}",
                labels(&chunks)
            );
        }
    }

    #[test]
    fn a_variable_or_field_holding_a_value_is_not_a_declaration() {
        let cases = [
            ("a.js", "const limit = 5;\nlet { a, b } = () => {};\n"),
            ("b.js", "const rows = items.map((item) => item.id);\n"),
            ("c.ts", "class Store {\n  count: number = 0\n}\n"),
            ("d.js", "class Store {\n  count = 0\n}\n"),
            (
                "e.js",
                "class Store {\n  load() {\n    const done = () => {};\n  }\n}\n",
            ),
        ];
        for (file, src) in cases {
            let found = labels(&chunk_source(file, src, 1));
            assert!(
                found
                    .iter()
                    .all(|l| l == "class Store" || l == "fn Store::load"),
                "{file}: got {found:?}"
            );
        }
    }

    #[test]
    fn a_chunk_lists_every_declaration_it_holds() {
        let src = "impl Road {\n    fn preview() {}\n\n    pub fn quote_cost() -> u32 {\n        3\n    }\n}\n";
        let chunks = chunk_rust(src, 40);
        let held = |chunk: &Chunk| -> Vec<(String, usize, usize)> {
            chunk
                .declarations
                .iter()
                .map(|d| (d.symbol.label(), d.start_line, d.end_line))
                .collect()
        };
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            held(&chunks[0]),
            [
                ("impl Road".to_string(), 1, 7),
                ("fn Road::preview".to_string(), 2, 2),
            ]
        );
        assert_eq!(
            held(&chunks[1]),
            [("fn Road::quote_cost".to_string(), 4, 6)]
        );
    }

    #[test]
    fn a_chunk_keeps_the_indentation_of_its_first_line() {
        let mut src = String::from("class Store:\n");
        for name in ["load", "save"] {
            src.push_str(&format!("    def {name}(self):\n"));
            for _ in 0..12 {
                src.push_str("        pass\n");
            }
        }
        let chunks = chunk_source("s.py", &src, 20);
        let save = chunks.iter().find(|c| c.text.contains("def save")).unwrap();
        assert!(save.text.starts_with("    def save"), "{:?}", save.text);
    }

    #[test]
    fn nesting_too_deep_to_walk_is_left_to_line_windows() {
        let terms: Vec<String> = (0..5_000).map(|i| format!("a{i}")).collect();
        let chain = format!("const x = {};\n", terms.join(" +\n"));
        let nested = format!("{}{}\n", "[".repeat(5_000), "]".repeat(5_000));
        for (file, src) in [("a.js", chain.as_str()), ("b.json", nested.as_str())] {
            let spec = crate::lang::detect(Path::new(file)).unwrap();
            assert!(chunk(src, &spec, 50, 4).is_none(), "{file}");
        }
        let shallow = format!("const x = {}1{};\n", "[".repeat(100), "]".repeat(100));
        assert!(!chunk_source("c.js", &shallow, 50).is_empty());
    }

    #[test]
    fn a_name_that_is_only_used_is_not_a_declaration() {
        let cases = [
            (
                "a.rs",
                "fn run() {\n    let Some(x) = y else { return };\n    let v: Vec<u8> = Vec::new();\n    let c = Config { x };\n    let r: &Path = p as &Path;\n}\n",
            ),
            (
                "b.py",
                "def run(\n    timeout=0.5,\n    stream: bool = False,\n    rows: Any,\n):\n    pass\n",
            ),
            (
                "c.go",
                "package main\nfunc run() {\n    b := []byte(\"a\")\n    r := sr.(*Router)\n    var t testing.B\n}\n",
            ),
            (
                "d.ts",
                "function run(\n  m: Map<string, T>,\n  e: NodeJS.ErrnoException,\n): m is Request {\n  return true\n}\n",
            ),
        ];
        for (file, src) in cases {
            let found = labels(&chunk_source(file, src, 1));
            assert!(found.iter().all(|l| l == "fn run"), "{file}: got {found:?}");
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

    fn holds_prose(chunks: &[Chunk], expected: &str) -> bool {
        chunks.iter().any(|c| c.text == expected)
    }

    #[test]
    fn a_doc_comment_becomes_a_record_pointing_at_its_declaration() {
        let src = "/// Decides whether a file is text rather than binary.\nfn is_indexable() {}\n";
        let chunks = chunk_rust(src, 1);
        let doc = chunks
            .iter()
            .find(|c| c.text.starts_with("Decides"))
            .expect("no documentation chunk");
        assert_eq!(
            doc.text,
            "Decides whether a file is text rather than binary."
        );
        assert_eq!(
            doc.symbol.as_ref().map(Symbol::label),
            Some("fn is_indexable".to_string())
        );
        assert_eq!(doc.start_line, 2, "the range must point at the declaration");
    }

    #[test]
    fn several_comment_lines_join_into_one_run_of_prose() {
        let src = "// First sentence.\n// Second sentence.\nfn thing() {}\n";
        assert!(holds_prose(
            &chunk_rust(src, 1),
            "First sentence. Second sentence."
        ));
    }

    #[test]
    fn a_comment_separated_by_a_blank_line_belongs_to_nobody() {
        let src = "// Copyright somebody.\n\nfn thing() {}\n";
        assert!(!holds_prose(&chunk_rust(src, 1), "Copyright somebody."));
    }

    #[test]
    fn a_python_docstring_is_found_inside_the_declaration() {
        let src = "def load(path):\n    \"\"\"Read the settings from disk.\"\"\"\n    return 1\n";
        let chunks = chunk_source("m.py", src, 1);
        assert!(
            chunks
                .iter()
                .any(|c| c.text == "Read the settings from disk."),
            "got {:?}",
            chunks.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn documentation_is_found_across_comment_styles() {
        let cases = [
            (
                "t.ts",
                "/** Joins a base url and a path. */\nexport function combineURLs() {}\n",
                "Joins a base url and a path.",
            ),
            (
                "g.go",
                "// Match reports whether the request matches.\nfunc Match() bool { return true }\n",
                "Match reports whether the request matches.",
            ),
            (
                "r.rb",
                "# Computes the total.\ndef compute\n  1\nend\n",
                "Computes the total.",
            ),
            (
                "a.js",
                "/** Loads the settings. */\nexport const load = () => {};\n",
                "Loads the settings.",
            ),
            (
                "p.tsx",
                "// Renders the page.\nconst Page = () => <main />;\n",
                "Renders the page.",
            ),
            (
                "m.h",
                "// Adds two numbers.\nint add(int a, int b);\n",
                "Adds two numbers.",
            ),
        ];
        for (file, src, expected) in cases {
            let chunks = chunk_source(file, src, 1);
            assert!(
                chunks.iter().any(|c| c.text == expected),
                "{file}: expected {expected:?}, got {:?}",
                chunks.iter().map(|c| &c.text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_comment_above_several_declarators_is_not_about_the_last() {
        for src in [
            "// Both handlers.\nlet open = () => {}, close = () => {};\n",
            "// Both handlers.\nlet open = () => {}, close = () => {}\n",
        ] {
            let chunks = chunk_source("a.js", src, 1);
            let close = Some("fn close".to_string());
            assert!(
                !chunks
                    .iter()
                    .any(|c| c.text == "Both handlers."
                        && c.symbol.as_ref().map(Symbol::label) == close),
                "{src:?}"
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

    /// A file ends with the newline after its last line, which its node takes
    /// in, so the end of a whole-file chunk is not the row that newline opens.
    #[test]
    fn a_whole_file_chunk_ends_on_the_last_line() {
        for (file, src) in [
            ("x.rs", "fn main() {\n    run();\n}\n"),
            ("x.py", "def main():\n    run()\n\n\n"),
            ("x.go", "package main\n\nfunc main() {}\n"),
            ("x.json", "{\n  \"k\": 1\n}\n"),
            ("x.rs", "fn main() {\n    run();\n}"),
        ] {
            let chunks = chunk_source(file, src, 40);
            let last = src.trim_end().lines().count();
            assert_eq!(
                (chunks[0].start_line, chunks.last().unwrap().end_line),
                (1, last),
                "{file}: {src:?}"
            );
        }
    }

    #[test]
    fn every_line_lands_in_a_chunk() {
        let many = |f: &dyn Fn(usize) -> String| (0..30).map(f).collect::<String>();
        let cases: Vec<(&str, String)> = vec![
            (
                "a.rs",
                many(&|n| format!("use a::b{n};\nconst X{n}: u8 = 1;\nstatic Y{n}: u8 = 2;\n")),
            ),
            (
                "a.rs",
                many(&|n| format!("fn f{n}() {{\n    let x = 1;\n}}\n;\n")),
            ),
            (
                "a.py",
                many(&|n| format!("import m{n}\nx{n} = 1\nprint(x{n})\n")),
            ),
            (
                "a.js",
                many(&|n| format!("var a{n} = 1;\nconsole.log(a{n});\nif (a{n}) {{ run(); }}\n")),
            ),
            (
                "a.ts",
                many(&|n| format!("type T{n} = number;\nlet a{n}: T{n} = 1;\nrun(a{n});\n")),
            ),
            (
                "a.go",
                format!(
                    "package main\nimport \"fmt\"\n{}",
                    many(&|n| format!("var a{n} = 1\nfunc f{n}() {{\n fmt.Println(a{n})\n}}\n"))
                ),
            ),
            (
                "a.sh",
                many(&|n| format!("A{n}=1\necho $A{n}\nif [ -z $A{n} ]; then\n  exit 1\nfi\n")),
            ),
            (
                "a.c",
                many(&|n| format!("#include <a{n}.h>\n#define X{n} 1\nint v{n} = X{n};\n")),
            ),
            (
                "a.css",
                many(&|n| format!("@import 'a{n}.css';\n.a{n} {{\n  color: red;\n}}\n")),
            ),
            (
                "a.html",
                many(&|n| format!("<!-- c{n} -->\n<div>\n  <p>t{n}</p>\n</div>\n")),
            ),
            (
                "a.json",
                format!("[\n{}0\n]\n", many(&|n| format!("  {{\"a\": {n}}},\n"))),
            ),
            (
                "a.rb",
                many(&|n| format!("require 'a{n}'\nx{n} = 1\nputs x{n}\n")),
            ),
            (
                "a.php",
                format!("<?php\n{}", many(&|n| format!("$a{n} = 1;\necho $a{n};\n"))),
            ),
            (
                "a.kt",
                format!(
                    "{}{}",
                    many(&|n| format!("import a.b{n}\n")),
                    many(&|n| format!("val x{n} = 1\nfun f{n}() = x{n}\n"))
                ),
            ),
            (
                "a.scala",
                many(&|n| format!("import a.b{n}\nval x{n} = 1\nprintln(x{n})\n")),
            ),
            (
                "build.gradle.kts",
                many(&|n| {
                    format!("println({n})\ndependencies {{\n    implementation(\"x{n}\")\n}}\n")
                }),
            ),
            (
                "a.java",
                format!("package p;\n{}", many(&|n| format!("import a.B{n};\n"))),
            ),
            ("a.cs", many(&|n| format!("using A{n};\n"))),
            (
                "a.cpp",
                many(&|n| format!("#include <a{n}>\nusing namespace n{n};\nint v{n} = 1;\n")),
            ),
        ];
        for (file, src) in cases {
            let chunks = chunk_source(file, &src, 8);
            let kept: std::collections::HashSet<&str> = chunks
                .iter()
                .flat_map(|c| c.text.lines())
                .map(|l| l.trim().trim_end_matches([',', ';']))
                .collect();
            let lost: Vec<&str> = src
                .lines()
                .map(str::trim)
                .map(|l| l.trim_end_matches([',', ';']))
                .filter(|l| l.contains(char::is_alphanumeric) && !kept.contains(l))
                .take(5)
                .collect();
            assert!(
                lost.is_empty(),
                "{file}: lost {lost:?} of {:?}",
                chunks
                    .iter()
                    .map(|c| (c.start_line, c.end_line, c.text.lines().count()))
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Text the parser could not place sits in an error node, whose few named
    /// children are not all of it.
    #[test]
    fn a_stretch_the_parser_gave_up_on_is_still_cut() {
        let src: String = (0..30)
            .map(|n| format!("import a.b{n}\nval x{n} = 1\nprintln(x{n})\n"))
            .collect();
        let chunks = chunk_source("a.kt", &src, 8);
        let text: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(text.contains("println(x29)"));
    }
}
