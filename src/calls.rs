//! Call sites in source text, read from the tree-sitter grammars the chunker
//! already links. A call is a node the grammar gives call syntax: `f(x)`,
//! `a.b()`, `pkg::f()`, `new X()`, a Rust macro invocation. A name inside a
//! string or a comment is no node of that kind, so it is never a call.

use std::rc::Rc;

use tree_sitter::{Node, Parser};

use crate::lang::LangSpec;
use crate::symbol::{self, Symbol};

/// One place the code calls a function or a method, named by its callee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// The last segment of the callee's name: `load` for `Index::load`.
    pub callee: String,
    /// The segments written before the name: `["Index"]` for `Index::load`,
    /// `["self"]` for `self.load()`. Empty when the receiver is not a name,
    /// as in `a.b().c()`, or when there is none.
    pub qualifier: Vec<String>,
    /// 1-based line and byte column of the callee's name.
    pub line: usize,
    pub column: usize,
    /// The innermost declaration the call sits in, qualified by the ones around
    /// it (`fn Index::load`).
    pub enclosing: Option<Symbol>,
}

/// Every call site in `text`, in source order. A language without call syntax
/// here, such as JSON, has none.
pub fn call_sites(text: &str, spec: &LangSpec) -> Vec<CallSite> {
    let mut parser = Parser::new();
    if parser.set_language(&spec.language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text.as_bytes(), None) else {
        return Vec::new();
    };
    let mut sites = Vec::new();
    // An explicit stack rather than recursion, so a deeply nested file cannot
    // overflow the stack of a worker thread.
    let mut pending: Vec<(Node, Option<Rc<Symbol>>)> = vec![(tree.root_node(), None)];
    while let Some((node, scope)) = pending.pop() {
        if let Some((name, qualifier)) = call_of(&node, spec.name, text) {
            sites.push(CallSite {
                callee: name_text(&name, text),
                qualifier,
                line: name.start_position().row + 1,
                column: name.start_position().column + 1,
                enclosing: scope.as_deref().cloned(),
            });
        }
        let inner = match symbol::raw_declaration(&node) {
            Some((kind, (start, end))) => Some(Rc::new(symbol::named(
                kind,
                &text[start..end],
                scope.as_deref(),
            ))),
            None => scope,
        };
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            pending.push((child, inner.clone()));
        }
    }
    sites.sort_by_key(|site| (site.line, site.column));
    sites
}

/// The name node a call node calls, and the qualifier written before it.
fn call_of<'t>(node: &Node<'t>, lang: &str, text: &str) -> Option<(Node<'t>, Vec<String>)> {
    let parent = node.parent().map(|p| p.kind());
    let kind = node.kind();
    match (lang, kind) {
        ("rust", "call_expression") => callee(node.child_by_field_name("function")?, text),
        ("rust", "macro_invocation") => callee(node.child_by_field_name("macro")?, text),
        ("rust", "identifier") => token_call(node, text),
        // A decorator's own call names the decorator, not a call made in the
        // body; the calls in its arguments are real calls and are kept.
        ("python", "call") if parent != Some("decorator") => {
            callee(node.child_by_field_name("function")?, text)
        }
        ("javascript" | "typescript" | "tsx", "call_expression") if parent != Some("decorator") => {
            callee(node.child_by_field_name("function")?, text)
        }
        ("javascript" | "typescript" | "tsx", "new_expression") => {
            callee(node.child_by_field_name("constructor")?, text)
        }
        ("go", "call_expression") => callee(node.child_by_field_name("function")?, text),
        ("java", "method_invocation") => named_call(node, "name", "object", text),
        ("java", "object_creation_expression") => callee(node.child_by_field_name("type")?, text),
        ("csharp", "invocation_expression") => callee(node.child_by_field_name("function")?, text),
        ("csharp", "object_creation_expression") => callee(node.child_by_field_name("type")?, text),
        ("c" | "cpp", "call_expression") => callee(node.child_by_field_name("function")?, text),
        ("ruby", "call") => named_call(node, "method", "receiver", text),
        _ => None,
    }
}

/// A call whose name and receiver are fields of the call node itself, as Java's
/// `object.name(...)`.
fn named_call<'t>(
    node: &Node<'t>,
    name: &str,
    receiver: &str,
    text: &str,
) -> Option<(Node<'t>, Vec<String>)> {
    let name_node = unwrap(node.child_by_field_name(name)?);
    if !is_name(&name_node) {
        return None;
    }
    Some((
        name_node,
        qualifier(node.child_by_field_name(receiver), text),
    ))
}

/// A Rust call inside a macro's arguments, which the grammar leaves as tokens:
/// an identifier followed by a parenthesised group, as in `assert_eq!(load(x), 1)`.
/// A name after `fn` is a declaration inside the macro, not a call.
fn token_call<'t>(node: &Node<'t>, text: &str) -> Option<(Node<'t>, Vec<String>)> {
    if node.parent()?.kind() != "token_tree" {
        return None;
    }
    let group = node.next_sibling()?;
    let opens =
        group.kind() == "token_tree" && text.as_bytes().get(group.start_byte()) == Some(&b'(');
    if !opens || node.prev_sibling().is_some_and(|p| p.kind() == "fn") {
        return None;
    }
    let mut qualifier = Vec::new();
    let mut at = *node;
    while let Some(sep) = at.prev_sibling().filter(|s| matches!(s.kind(), "::" | ".")) {
        let Some(part) = sep.prev_sibling().filter(is_name) else {
            break;
        };
        qualifier.push(name_text(&part, text));
        at = part;
    }
    qualifier.reverse();
    Some((*node, qualifier))
}

/// The name a callee expression ends in, and the qualifier before it. `None`
/// when the callee is no name: `(f)()`, `handlers[i]()`, a closure in a field.
fn callee<'t>(node: Node<'t>, text: &str) -> Option<(Node<'t>, Vec<String>)> {
    let node = unwrap(node);
    if is_name(&node) {
        return Some((node, Vec::new()));
    }
    let (receiver, name) = member(&node)?;
    if !is_name(&name) {
        return None;
    }
    Some((name, qualifier(receiver, text)))
}

/// The segments a receiver names, as in `a::b` or `a.b.c`. Empty when the
/// receiver is anything else, such as a call, so the match cannot narrow on it.
fn qualifier(receiver: Option<Node>, text: &str) -> Vec<String> {
    receiver.and_then(|r| chain(&r, text)).unwrap_or_default()
}

fn chain(node: &Node, text: &str) -> Option<Vec<String>> {
    let node = unwrap(*node);
    if is_name(&node) {
        return Some(vec![name_text(&node, text)]);
    }
    let (receiver, name) = member(&node)?;
    if !is_name(&name) {
        return None;
    }
    let mut segments = chain(&receiver?, text)?;
    segments.push(name_text(&name, text));
    Some(segments)
}

/// Wrappers around a callee that name it in a field: `f::<T>` is a generic
/// function, `Vec<T>` a generic type.
fn unwrap(node: Node) -> Node {
    const WRAPPERS: [&str; 4] = [
        "generic_function",
        "template_function",
        "generic_type",
        "generic_name",
    ];
    if !WRAPPERS.contains(&node.kind()) {
        return node;
    }
    let inner = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.named_child(0));
    match inner {
        Some(inner) => unwrap(inner),
        None => node,
    }
}

/// A node that names something, as a bare name, `self`, or a type's name.
fn is_name(node: &Node) -> bool {
    let kind = node.kind();
    let scoped = kind.starts_with("scoped") || kind.starts_with("qualified");
    let leaf = kind.ends_with("identifier") && !scoped;
    leaf || matches!(
        kind,
        "self" | "this" | "super" | "crate" | "name" | "constant" | "variable_name"
    )
}

fn name_text(node: &Node, text: &str) -> String {
    text[node.start_byte()..node.end_byte()]
        .trim_start_matches('$')
        .to_string()
}

/// The receiver and the name of a member access, by the grammar's kind and
/// field names. A kind can appear with different fields in different grammars
/// (C's `field_expression` names its receiver `argument`, Rust's `value`).
fn member<'t>(node: &Node<'t>) -> Option<(Option<Node<'t>>, Node<'t>)> {
    const MEMBERS: [(&str, &str, &str); 13] = [
        ("scoped_identifier", "path", "name"),
        ("field_expression", "value", "field"),
        ("field_expression", "argument", "field"),
        ("attribute", "object", "attribute"),
        ("member_expression", "object", "property"),
        ("selector_expression", "operand", "field"),
        ("field_access", "object", "field"),
        ("member_access_expression", "expression", "name"),
        ("member_access_expression", "object", "name"),
        ("member_call_expression", "object", "name"),
        ("scoped_call_expression", "scope", "name"),
        ("qualified_identifier", "scope", "name"),
        ("call", "receiver", "method"),
    ];
    // A Ruby call with arguments is a call made, not a property to read a
    // chain through: in `File.read(path).strip` the receiver names no path.
    if node.kind() == "call" && node.child_by_field_name("arguments").is_some() {
        return None;
    }
    let mut bare = None;
    for (kind, receiver, name) in MEMBERS {
        if kind != node.kind() {
            continue;
        }
        let Some(name_node) = node.child_by_field_name(name).map(unwrap) else {
            continue;
        };
        match node.child_by_field_name(receiver) {
            Some(receiver_node) => return Some((Some(receiver_node), name_node)),
            None => {
                bare.get_or_insert((None, name_node));
            }
        }
    }
    bare
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Each call as `qualifier::callee line:column in enclosing`, in source order.
    fn sites(file: &str, src: &str) -> Vec<String> {
        let spec = crate::lang::detect(Path::new(file)).unwrap();
        call_sites(src, &spec)
            .iter()
            .map(|s| {
                let qualified: String = s.qualifier.iter().map(|q| format!("{q}::")).collect();
                let within = s.enclosing.as_ref().map_or("-".to_string(), Symbol::label);
                format!(
                    "{qualified}{} {}:{} in {within}",
                    s.callee, s.line, s.column
                )
            })
            .collect()
    }

    #[test]
    fn rust_method_chains_and_qualified_paths() {
        let src = "fn run(a: &Vec<u8>) -> usize {\n    a.iter().count() + helper::pick(3)\n}\n";
        assert_eq!(
            sites("a.rs", src),
            [
                "a::iter 2:7 in fn run",
                "count 2:14 in fn run",
                "helper::pick 2:32 in fn run",
            ]
        );
    }

    #[test]
    fn rust_self_turbofish_and_module_paths_keep_their_qualifier() {
        let src = "impl Index {\n\
                   \x20   fn load(path: &str) -> Index {\n\
                   \x20       let raw = std::fs::read(path);\n\
                   \x20       Self::decode(raw.unwrap_or_default())\n\
                   \x20   }\n\
                   \x20   fn reload(&self) {\n\
                   \x20       self.load(\"x\");\n\
                   \x20       let v = parse::<u8>(b\"1\");\n\
                   \x20       let w = Vec::<u8>::with_capacity(2);\n\
                   \x20       super::shared::touch(&w, v);\n\
                   \x20   }\n\
                   }\n";
        assert_eq!(
            sites("a.rs", src),
            [
                "std::fs::read 3:28 in fn Index::load",
                "Self::decode 4:15 in fn Index::load",
                "raw::unwrap_or_default 4:26 in fn Index::load",
                "self::load 7:14 in fn Index::reload",
                "parse 8:17 in fn Index::reload",
                "Vec::with_capacity 9:28 in fn Index::reload",
                "super::shared::touch 10:24 in fn Index::reload",
            ]
        );
    }

    #[test]
    fn rust_strings_and_comments_hold_no_calls_but_macro_arguments_do() {
        let src = "fn main() {\n\
                   \x20   // open(path) is mentioned here\n\
                   \x20   let msg = \"call load(x) now\";\n\
                   \x20   println!(\"{}\", load(msg));\n\
                   \x20   /* save(x) */ let _ = r#\"fn q() { drop(1) }\"#;\n\
                   }\n";
        assert_eq!(
            sites("a.rs", src),
            ["println 4:5 in fn main", "load 4:20 in fn main"]
        );
    }

    #[test]
    fn rust_calls_split_across_lines_and_awaits_keep_their_names() {
        let src = "async fn fetch(client: &Client) -> Result<Body> {\n\
                   \x20   let body = client\n\
                   \x20       .get(url())\n\
                   \x20       .send()\n\
                   \x20       .await?\n\
                   \x20       .text()\n\
                   \x20       .await?;\n\
                   \x20   let pick = client.get;\n\
                   \x20   Ok(body)\n\
                   }\n";
        assert_eq!(
            sites("a.rs", src),
            [
                "client::get 3:10 in fn fetch",
                "url 3:14 in fn fetch",
                "send 4:10 in fn fetch",
                "text 6:10 in fn fetch",
                "Ok 9:5 in fn fetch",
            ]
        );
    }

    #[test]
    fn python_decorators_are_not_calls_but_their_arguments_are() {
        let src = "@register(make_key())\ndef handler():\n    pass\n\nshared = Store()\n";
        assert_eq!(
            sites("a.py", src),
            ["make_key 1:11 in -", "Store 5:10 in -"]
        );
    }

    #[test]
    fn python_super_module_paths_chains_and_f_strings() {
        let src = "import pkg.mod as m\n\
                   \n\
                   def run(items):\n\
                   \x20   # close(items) is only a comment\n\
                   \x20   label = f\"{render(items)} of {len(items)}\"\n\
                   \x20   return m.build(sorted(items)).finish(text=\"stop()\")\n";
        assert_eq!(
            sites("a.py", src),
            [
                "render 5:16 in fn run",
                "len 5:35 in fn run",
                "m::build 6:14 in fn run",
                "sorted 6:20 in fn run",
                "finish 6:35 in fn run",
            ]
        );
    }

    #[test]
    fn python_methods_in_a_class_and_super_calls() {
        let src = "import functools\n\n\
                   class Store(Base):\n\
                   \x20   \"\"\"Holds rows; mentions save(x).\"\"\"\n\n\
                   \x20   @functools.lru_cache(maxsize=4)\n\
                   \x20   @cache\n\
                   \x20   def get(self, key):\n\
                   \x20       return super().get(key) or Row(key).load(self.db.conn())\n";
        assert_eq!(
            sites("a.py", src),
            [
                "super 9:16 in fn Store::get",
                "get 9:24 in fn Store::get",
                "Row 9:36 in fn Store::get",
                "load 9:45 in fn Store::get",
                "self::db::conn 9:58 in fn Store::get",
            ]
        );
    }

    #[test]
    fn javascript_optional_chaining_new_and_template_substitutions() {
        let src = "import { load } from './store';\n\n\
                   export async function sync(db) {\n\
                   \x20 const rows = await db?.fetch?.(url(db));\n\
                   \x20 const obj = new Client(opts).connect();\n\
                   \x20 return `${render(rows)} rows`;\n\
                   }\n";
        assert_eq!(
            sites("a.js", src),
            [
                "db::fetch 4:26 in fn sync",
                "url 4:34 in fn sync",
                "Client 5:19 in fn sync",
                "connect 5:32 in fn sync",
                "render 6:13 in fn sync",
            ]
        );
    }

    #[test]
    fn typescript_class_members_subscripts_and_decorators() {
        let src = "@Component({ selector: 'x' })\n\
                   export class View {\n\
                   \x20 // run() is a comment\n\
                   \x20 handle = (e: Event) => this.emit(e);\n\
                   \x20 go() { const f = this.handle; handlers[key](); return \"go()\"; }\n\
                   }\n";
        assert_eq!(sites("a.ts", src), ["this::emit 4:31 in fn View::handle"]);
    }

    #[test]
    fn go_package_calls_chains_and_method_values() {
        let src = "package main\n\n\
                   import \"strings\"\n\n\
                   func (s *Server) Name() string {\n\
                   \treturn strings.ToUpper(s.name)\n\
                   }\n\n\
                   func main() {\n\
                   \tb := NewBuilder().\n\
                   \t\tAdd(1)\n\
                   \tf := s.Name\n\
                   \tdefer b.Close()\n\
                   \tgo run()\n\
                   \tvar t = []byte(strings.TrimSpace(\" x \"))\n\
                   }\n";
        assert_eq!(
            sites("a.go", src),
            [
                "strings::ToUpper 6:17 in fn Name",
                "NewBuilder 10:7 in fn main",
                "Add 11:3 in fn main",
                "b::Close 13:10 in fn main",
                "run 14:5 in fn main",
                "strings::TrimSpace 15:25 in fn main",
            ]
        );
    }

    #[test]
    fn java_method_calls_and_constructors_keep_their_receivers() {
        let src = "import java.util.List;\n\n\
                   class Store {\n\
                   \x20   // load(x) in a comment\n\
                   \x20   List<String> load(String path) {\n\
                   \x20       String s = Files.readString(path);\n\
                   \x20       return new ArrayList<String>(this.cache.get(path));\n\
                   \x20   }\n\
                   }\n";
        assert_eq!(
            sites("Store.java", src),
            [
                "Files::readString 6:26 in fn Store::load",
                "ArrayList 7:20 in fn Store::load",
                "this::cache::get 7:49 in fn Store::load",
            ]
        );
    }

    #[test]
    fn csharp_member_calls_and_constructors_chain_without_names() {
        let src = "using System.IO;\n\nclass Store\n{\n\
                   \x20   // Load() in a comment\n\
                   \x20   public string Load(string path)\n    {\n\
                   \x20       var text = File.ReadAllText(path);\n\
                   \x20       return new StringBuilder().Append(text).ToString();\n    }\n}\n";
        assert_eq!(
            sites("Store.cs", src),
            [
                "File::ReadAllText 8:25 in fn Store::Load",
                "StringBuilder 9:20 in fn Store::Load",
                "Append 9:36 in fn Store::Load",
                "ToString 9:49 in fn Store::Load",
            ]
        );
    }

    #[test]
    fn c_calls_through_a_pointer_and_c_plus_plus_qualified_templates() {
        let c = "#include <stdio.h>\n\nstatic int add(int a, int b) {\n\
                 \x20   // sum(a) in a comment\n\
                 \x20   int r = sum(a, b) + ops->scale(b);\n\
                 \x20   return r;\n}\n";
        assert_eq!(
            sites("a.c", c),
            ["sum 5:13 in fn add", "ops::scale 5:30 in fn add"]
        );
        let cpp = "#include <memory>\n\nint open(int n) {\n\
                   \x20 auto p = std::make_shared<int>(n);\n\
                   \x20 return Store::build(p->size()).finish(n);\n}\n";
        assert_eq!(
            sites("a.cpp", cpp),
            [
                "std::make_shared 4:17 in fn open",
                "Store::build 5:17 in fn open",
                "p::size 5:26 in fn open",
                "finish 5:34 in fn open",
            ]
        );
    }

    #[test]
    fn ruby_calls_with_and_without_parentheses() {
        let src = "class Store\n  def load(path)\n\
                   \x20   # save(path) in a comment\n\
                   \x20   File.read(path).strip\n  end\n\n\
                   \x20 def open\n    load \"x\"\n  end\nend\n";
        assert_eq!(
            sites("store.rb", src),
            [
                "File::read 4:10 in fn Store::load",
                "strip 4:21 in fn Store::load",
                "load 8:5 in fn Store::open",
            ]
        );
    }

    #[test]
    fn a_language_without_call_syntax_here_has_none() {
        assert!(sites("data.json", "{\"f()\": [1]}\n").is_empty());
    }
}
