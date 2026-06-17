use std::path::Path;

use tree_sitter::Language;

pub struct LangSpec {
    pub language: Language,
}

pub fn detect(path: &Path) -> Option<LangSpec> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let language: Language = match ext.as_str() {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" | "pyi" => tree_sitter_python::LANGUAGE.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => tree_sitter_cpp::LANGUAGE.into(),
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
        _ => return None,
    };
    Some(LangSpec { language })
}
