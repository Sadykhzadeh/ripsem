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
            "impl" | "class" | "record" | "object" | "trait" | "interface" | "mod"
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

    /// Whether `name` is the whole name or its last parts, cut between two
    /// declarations: `Session::refresh` and `refresh` both name
    /// `auth::Session::refresh`.
    pub fn is_named(&self, name: &str) -> bool {
        self.name == name
            || self
                .name
                .strip_suffix(name)
                .is_some_and(|rest| rest.ends_with("::"))
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
    if is_reference(node) {
        return None;
    }
    if is_prototype(node) {
        return Some(("fn", declared_name(node)?));
    }
    if let Some(span) = kotlin_property(node) {
        return Some(("val", span));
    }
    match is_binding(node.kind()) {
        true => Some(("fn", bound_function_name(node)?)),
        false => {
            let (start, end) = declared_name(node)?;
            let start = kotlin_receiver(node).unwrap_or(start);
            Some((declared_kind(node.kind())?, (start, end)))
        }
    }
}

/// A Kotlin property names what it declares in a `variable_declaration` child,
/// and only a property of a file or a class is a declaration. A local `val`
/// inside a function is not, as a Rust `let` is not.
#[cfg(feature = "treesitter")]
fn kotlin_property(node: &tree_sitter::Node) -> Option<(usize, usize)> {
    if node.kind() != "property_declaration" {
        return None;
    }
    let parent = node.parent()?;
    if !matches!(parent.kind(), "source_file" | "class_body") {
        return None;
    }
    let mut cursor = node.walk();
    let variable = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "variable_declaration")?;
    identifier_span(&variable)
}

/// A Kotlin extension function is named with its receiver type, as in
/// `fun String.shout()`, so the name runs from the receiver to the function's
/// own name. The receiver is the type that comes before the name; a return type
/// comes after it.
#[cfg(feature = "treesitter")]
fn kotlin_receiver(node: &tree_sitter::Node) -> Option<usize> {
    if node.kind() != "function_declaration" {
        return None;
    }
    let name = node.child_by_field_name("name")?;
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .take_while(|child| child.start_byte() < name.start_byte())
        .find(|child| matches!(child.kind(), "user_type" | "nullable_type"))
        .map(|receiver| receiver.start_byte())
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

/// `struct point *p` in C and C++ names a type in a variable or a parameter,
/// where its specifier has no body, and defines nothing.
#[cfg(feature = "treesitter")]
fn is_reference(node: &tree_sitter::Node) -> bool {
    node.kind().ends_with("_specifier") && node.child_by_field_name("body").is_none()
}

/// A C or C++ prototype is a declaration of a function that has no body, and
/// its declarator is the only place its name sits. A declarator inside a
/// definition or a parameter names the same function, or nothing, and is left
/// out by [`is_use`].
#[cfg(feature = "treesitter")]
fn is_prototype(node: &tree_sitter::Node) -> bool {
    node.kind() == "function_declarator"
        && node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), "declaration" | "field_declaration"))
}

/// Longer needles come first so `module` is not read as `mod`. A kind match
/// alone proves nothing — Java's `modifiers` node matches `mod` — which is why
/// a declaration also has to yield a name.
#[cfg(feature = "treesitter")]
fn declared_kind(node_kind: &str) -> Option<&'static str> {
    if is_use(node_kind) {
        return None;
    }
    // A grammar has a few hundred node kinds and a file has thousands of nodes,
    // so each kind is matched against the needles once per thread.
    KIND_CACHE.with(|cache| {
        if let Some(&found) = cache.borrow().get(node_kind) {
            return found;
        }
        let found = KINDS
            .iter()
            .find(|(needle, _)| kind_contains(node_kind, needle))
            .map(|&(_, kind)| kind);
        cache.borrow_mut().insert(node_kind.to_string(), found);
        found
    })
}

#[cfg(feature = "treesitter")]
thread_local! {
    static KIND_CACHE: std::cell::RefCell<std::collections::HashMap<String, Option<&'static str>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(feature = "treesitter")]
const KINDS: &[(&str, &str)] = &[
    ("preproc_function_def", "macro"),
    ("preproc_def", "macro"),
    ("object_definition", "object"),
    ("object_declaration", "object"),
    ("interface", "interface"),
    ("namespace", "mod"),
    ("constructor", "fn"),
    ("destructor", "fn"),
    ("function", "fn"),
    ("method", "fn"),
    ("module", "mod"),
    ("struct", "struct"),
    ("record", "record"),
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
/// `timeout=0.5` a `default_parameter`. A C declarator (`function_declarator`)
/// repeats the name of the function or the variable its declaration starts,
/// and a C++ `static_cast<U>(x)` is a call, so both are left out as well. The
/// body of a TypeScript enum holds its members under the field `name`, so it
/// would be declared under the name of its first one. A Scala `val` or `var`
/// holds a value, and its kind only carries the needle `def`; its `type` field
/// is the annotation, so it would be declared under the name of its type. A C
/// or C++ `type_descriptor`, the type in a cast or a `sizeof`, is a use of its
/// type. A C or C++ `#ifdef` or `#ifndef` carries the needle `def` and the name
/// of its macro, so an include guard would declare a function that holds the
/// whole header. In Rust a bound written `for<'a> FnOnce(&'a str)` is a
/// `higher_ranked_trait_bound` and `Vec::<u8>::new()` holds a
/// `generic_type_with_turbofish`: both name a trait or a type they do not
/// declare. A Go `case Foo:` in a type switch is a `type_case`, the types in a
/// Scala function type `(Int, String) => Unit` are `parameter_types`, and a
/// TypeScript `new (x: number): Foo` is a `construct_signature`: each holds a
/// type it only names.
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
        "_declarator",
        "_parameter_declaration",
        "_parameters",
        "template_function",
        "enum_body",
        "val_definition",
        "var_definition",
        "type_descriptor",
        "preproc_ifdef",
        "preproc_elifdef",
        "higher_ranked_trait_bound",
        "generic_type_with_turbofish",
        "type_case",
        "parameter_types",
        "construct_signature",
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
    matches!(
        node_kind,
        "variable_declarator" | "assignment_expression" | "assignment"
    ) || node_kind.ends_with("field_definition")
}

/// The type a Go method is declared on: `Set` for `func (s *Set[T]) Add`. Its
/// methods are named as a class's are, `Set::Add`.
#[cfg(feature = "treesitter")]
pub fn receiver_type(node: &tree_sitter::Node, text: &str) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let parameter = receiver.named_children(&mut cursor).next()?;
    let (start, end) = identifier_span(&parameter.child_by_field_name("type")?)?;
    Some(text[start..end].to_string())
}

/// The name has to be a plain identifier: a destructuring pattern, a string or
/// a computed key does not name one function. A variable inside a function
/// body holds a closure, which belongs to the function around it as a Rust
/// closure or a Python lambda does.
#[cfg(feature = "treesitter")]
fn bound_function_name(node: &tree_sitter::Node) -> Option<(usize, usize)> {
    let value = node
        .child_by_field_name("value")
        .or_else(|| node.child_by_field_name("right"))?;
    if !(value.kind().contains("function") || value.kind() == "lambda") {
        return None;
    }
    let statement = node.parent()?;
    if statement
        .parent()
        .is_some_and(|block| matches!(block.kind(), "statement_block" | "block"))
    {
        return None;
    }
    // `module.exports.parse = ...` is named by its last property, `parse`.
    let names: Vec<tree_sitter::Node> = match node.child_by_field_name("left") {
        Some(target) => ["property", "attribute"]
            .into_iter()
            .filter_map(|field| target.child_by_field_name(field))
            .chain(std::iter::once(target))
            .collect(),
        None => ["name", "property"]
            .into_iter()
            .filter_map(|field| node.child_by_field_name(field))
            .collect(),
    };
    names
        .into_iter()
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
/// is a starting point rather than the answer. A dotted or scoped name is the
/// answer whole, as its first part alone would name another declaration.
#[cfg(feature = "treesitter")]
fn identifier_span(node: &tree_sitter::Node) -> Option<(usize, usize)> {
    if is_name_leaf(node.kind()) || is_qualified_name(node.kind()) {
        return Some((node.start_byte(), node.end_byte()));
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find_map(|child| identifier_span(&child))
}

/// Most grammars call the leaf holding a declared name some flavour of
/// `identifier`; bash calls it `word`, PHP calls it `name`, Ruby names a
/// module or a class by its `constant` and C++ writes a destructor `~Res`.
#[cfg(feature = "treesitter")]
fn is_name_leaf(kind: &str) -> bool {
    kind.ends_with("identifier")
        || matches!(
            kind,
            "word" | "name" | "constant" | "operator_name" | "destructor_name"
        )
}

/// What C# and PHP call `Foo.Bar` and `App\Http`, and C++ `net::http`, in a
/// namespace.
#[cfg(feature = "treesitter")]
fn is_qualified_name(kind: &str) -> bool {
    matches!(
        kind,
        "qualified_name" | "namespace_name" | "nested_namespace_specifier"
    )
}

#[cfg(all(test, feature = "treesitter"))]
mod tests {
    use super::{KINDS, declared_kind, kind_contains};

    /// A kind answered from the cache gives the answer a fresh scan gives.
    #[test]
    fn a_cached_kind_answers_as_a_fresh_scan_does() {
        let kinds = [
            "function_item",
            "modifiers",
            "impl_item",
            "identifier",
            "module",
            "class_declaration",
            "identifier",
        ];
        for kind in kinds.iter().chain(kinds.iter()) {
            let fresh = KINDS
                .iter()
                .find(|(needle, _)| kind_contains(kind, needle))
                .map(|&(_, k)| k);
            let expected = if super::is_use(kind) { None } else { fresh };
            assert_eq!(declared_kind(kind), expected, "{kind}");
        }
    }

    #[test]
    fn kind_contains_agrees_with_str_contains() {
        let extra = [
            "",
            "fn",
            "function_item",
            "modifiers",
            "impl_item",
            "type_identifier",
        ];
        for (needle, _) in KINDS {
            let wrapped = format!("x_{needle}_y");
            let mut kinds = vec![needle.to_string(), wrapped];
            kinds.extend(extra.iter().map(|k| k.to_string()));
            for kind in &kinds {
                assert_eq!(
                    kind_contains(kind, needle),
                    kind.contains(needle),
                    "{kind} {needle}"
                );
            }
        }
    }
}
