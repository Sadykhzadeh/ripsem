use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub kind: String,
    pub name: String,
}

impl Symbol {
    pub fn label(&self) -> String {
        format!("{} {}", self.kind, self.name)
    }
}

/// Grammars name their nodes differently per language, but they agree on two
/// conventions: a declaration's node kind spells out what is being declared, and
/// the identifier sits in a `name`, `type` or `declarator` field. Reading both
/// keeps this generic — there is no per-language table to keep in step with
/// eighteen grammars, and a language that breaks the convention degrades to an
/// unnamed chunk rather than a wrong one.
#[cfg(feature = "treesitter")]
pub fn declaration(
    node: &tree_sitter::Node,
    text: &str,
    enclosing: Option<&Symbol>,
) -> Option<Symbol> {
    let kind = declared_kind(node.kind())?;
    let name = declared_name(node, text)?;
    Some(Symbol {
        kind: kind.to_string(),
        name: match enclosing {
            Some(outer) => format!("{}::{name}", outer.name),
            None => name,
        },
    })
}

/// Longer needles come first so `module` is not read as `mod`. A kind match
/// alone proves nothing — Java's `modifiers` node matches `mod` — which is why
/// a declaration also has to yield a name.
#[cfg(feature = "treesitter")]
fn declared_kind(node_kind: &str) -> Option<&'static str> {
    const KINDS: &[(&str, &str)] = &[
        ("interface", "interface"),
        ("namespace", "mod"),
        ("constructor", "fn"),
        ("function", "fn"),
        ("method", "fn"),
        ("module", "mod"),
        ("struct", "struct"),
        ("class", "class"),
        ("macro", "macro"),
        ("union", "union"),
        ("const", "const"),
        ("static", "static"),
        ("trait", "trait"),
        ("enum", "enum"),
        ("impl", "impl"),
        ("type", "type"),
        ("mod", "mod"),
        ("def", "fn"),
    ];
    KINDS
        .iter()
        .find(|(needle, _)| node_kind.contains(needle))
        .map(|&(_, kind)| kind)
}

#[cfg(feature = "treesitter")]
fn declared_name(node: &tree_sitter::Node, text: &str) -> Option<String> {
    ["name", "type", "declarator"]
        .into_iter()
        .filter_map(|field| node.child_by_field_name(field))
        .find_map(|child| identifier_text(&child, text))
}

/// C-like grammars wrap the identifier in nested declarators, so the field node
/// is a starting point rather than the answer.
#[cfg(feature = "treesitter")]
fn identifier_text(node: &tree_sitter::Node, text: &str) -> Option<String> {
    if is_name_leaf(node.kind()) {
        return Some(text[node.start_byte()..node.end_byte()].to_string());
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| identifier_text(&child, text))
}

/// Most grammars call the leaf holding a declared name some flavour of
/// `identifier`; bash calls it `word` and PHP calls it `name`.
#[cfg(feature = "treesitter")]
fn is_name_leaf(kind: &str) -> bool {
    kind.ends_with("identifier") || matches!(kind, "word" | "name")
}
