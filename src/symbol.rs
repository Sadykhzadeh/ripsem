use bitcode::{Decode, Encode};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct Symbol {
    pub kind: String,
    pub name: String,
}

impl Symbol {
    pub fn heading(name: String) -> Self {
        Self {
            kind: "heading".into(),
            name,
        }
    }

    /// A section of a document, not a declaration of code.
    pub fn is_heading(&self) -> bool {
        self.kind == "heading"
    }

    /// A block whose members are declarations searched for on their own, as
    /// a class's methods are.
    pub fn is_container(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "impl" | "class" | "trait" | "interface" | "mod"
        )
    }

    pub fn label(&self) -> String {
        format!("{} {}", self.kind, self.name)
    }

    /// The name without the declarations it sits in: `refresh` for
    /// `Searcher::refresh`.
    pub fn leaf(&self) -> &str {
        self.name.rsplit("::").next().unwrap_or(&self.name)
    }
}

/// One of the declarations a chunk holds, with the lines it spans. A chunk
/// groups small neighbours, so the one a search asks for is often not the first.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Declaration {
    pub symbol: Symbol,
    pub start_line: usize,
    pub end_line: usize,
}

impl Declaration {
    /// An `impl` block adds to a type declared elsewhere.
    pub fn is_extension(&self) -> bool {
        self.symbol.kind == "impl"
    }

    /// A block whose members are declarations searched for on their own, as
    /// a class's methods are.
    pub fn is_container(&self) -> bool {
        self.symbol.is_container()
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
    let (kind, (start, end)) = raw_declaration(node)?;
    Some(named(kind, &text[start..end], enclosing))
}

/// The kind a node declares and the bytes of its name, before the name of an
/// enclosing declaration is put in front of it. [`named`] completes it.
#[cfg(feature = "treesitter")]
pub fn raw_declaration(node: &tree_sitter::Node) -> Option<(&'static str, (usize, usize))> {
    match is_binding(node.kind()) {
        true => Some(("fn", bound_function_name(node)?)),
        false => Some((declared_kind(node.kind())?, declared_name(node)?)),
    }
}

#[cfg(feature = "treesitter")]
pub fn named(kind: &str, name: &str, enclosing: Option<&Symbol>) -> Symbol {
    Symbol {
        kind: kind.to_string(),
        name: match enclosing {
            Some(outer) => format!("{}::{name}", outer.name),
            None => name.to_string(),
        },
    }
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
    if is_use(node_kind) {
        return None;
    }
    KINDS
        .iter()
        .find(|(needle, _)| kind_contains(node_kind, needle))
        .map(|&(_, kind)| kind)
}

/// `str::contains` for a needle of a few bytes. Every std call first builds its
/// search tables, and this runs for each node of a file, so the scan is cheaper.
#[cfg(feature = "treesitter")]
fn kind_contains(node_kind: &str, needle: &str) -> bool {
    let (kind, needle) = (node_kind.as_bytes(), needle.as_bytes());
    kind.windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(a, b)| a == b))
}

/// Types, expressions, patterns and parameters name something without declaring
/// it, yet their kinds carry the same needles: Rust's `Some(x)` is a
/// `tuple_struct_pattern`, Go's `testing.B` a `qualified_type` and Python's
/// `timeout=0.5` a `default_parameter`.
#[cfg(feature = "treesitter")]
fn is_use(node_kind: &str) -> bool {
    const USES: &[&str] = &[
        "_type",
        "_expression",
        "_pattern",
        "_parameter",
        "_identifier",
        "_clause",
        "_predicate",
        "_binding",
        "_invocation",
    ];
    USES.iter().any(|suffix| node_kind.ends_with(suffix))
}

/// JavaScript and TypeScript mostly declare a function by binding one to a
/// name, `const load = async () => {}` or a class field `handle = () => {}`.
/// Such a variable or field is named like the function it holds. One holding
/// anything else is not a declaration, even where its node kind, such as
/// TypeScript's `public_field_definition`, contains a needle like `def`.
#[cfg(feature = "treesitter")]
fn is_binding(node_kind: &str) -> bool {
    node_kind == "variable_declarator" || node_kind.ends_with("field_definition")
}

/// The name has to be a plain identifier: a destructuring pattern, a string or
/// a computed key does not name one function. A variable inside a function
/// body holds a closure, which belongs to the function around it as a Rust
/// closure or a Python lambda does.
#[cfg(feature = "treesitter")]
fn bound_function_name(node: &tree_sitter::Node) -> Option<(usize, usize)> {
    let value = node.child_by_field_name("value")?;
    if !value.kind().contains("function") {
        return None;
    }
    let statement = node.parent()?;
    if statement
        .parent()
        .is_some_and(|block| block.kind() == "statement_block")
    {
        return None;
    }
    ["name", "property"]
        .into_iter()
        .filter_map(|field| node.child_by_field_name(field))
        .find(|name| is_name_leaf(name.kind()))
        .map(|name| (name.start_byte(), name.end_byte()))
}

#[cfg(feature = "treesitter")]
fn declared_name(node: &tree_sitter::Node) -> Option<(usize, usize)> {
    // A C or C++ function's `type` is its return type, so its declarator comes
    // first, and a typedef's `type` is what it aliases.
    ["name", "declarator", "type"]
        .into_iter()
        .filter_map(|field| node.child_by_field_name(field))
        .find_map(|child| identifier_span(&child))
}

/// C-like grammars wrap the identifier in nested declarators, so the field node
/// is a starting point rather than the answer.
#[cfg(feature = "treesitter")]
fn identifier_span(node: &tree_sitter::Node) -> Option<(usize, usize)> {
    if is_name_leaf(node.kind()) {
        return Some((node.start_byte(), node.end_byte()));
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| identifier_span(&child))
}

/// Most grammars call the leaf holding a declared name some flavour of
/// `identifier`; bash calls it `word` and PHP calls it `name`.
#[cfg(feature = "treesitter")]
fn is_name_leaf(kind: &str) -> bool {
    kind.ends_with("identifier") || matches!(kind, "word" | "name")
}

#[cfg(all(test, feature = "treesitter"))]
mod tests {
    use super::kind_contains;

    #[test]
    fn kind_contains_agrees_with_str_contains() {
        let kinds = [
            "",
            "fn",
            "function_item",
            "modifiers",
            "impl_item",
            "type_identifier",
        ];
        let needles = [
            "", "fn", "mod", "def", "type", "function", "module", "x_item",
        ];
        for kind in kinds {
            for needle in needles.iter().filter(|n| !n.is_empty()) {
                assert_eq!(
                    kind_contains(kind, needle),
                    kind.contains(needle),
                    "{kind} {needle}"
                );
            }
        }
    }
}
