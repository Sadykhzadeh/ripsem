#[cfg(feature = "treesitter")]
use std::path::Path;

#[cfg(feature = "treesitter")]
use tree_sitter::Language;

#[cfg(feature = "treesitter")]
pub struct LangSpec {
    pub language: Language,
}

struct Lang {
    name: &'static str,
    aliases: &'static [&'static str],
    exts: &'static [&'static str],
}

const LANGS: &[Lang] = &[
    Lang {
        name: "rust",
        aliases: &["rs"],
        exts: &["rs"],
    },
    Lang {
        name: "python",
        aliases: &["py"],
        exts: &["py", "pyi", "pyw"],
    },
    Lang {
        name: "javascript",
        aliases: &["js"],
        exts: &["js", "jsx", "mjs", "cjs"],
    },
    Lang {
        name: "typescript",
        aliases: &["ts"],
        exts: &["ts", "mts", "cts"],
    },
    Lang {
        name: "tsx",
        aliases: &[],
        exts: &["tsx"],
    },
    Lang {
        name: "go",
        aliases: &[],
        exts: &["go"],
    },
    Lang {
        name: "java",
        aliases: &[],
        exts: &["java"],
    },
    Lang {
        name: "c",
        aliases: &[],
        exts: &["c", "h"],
    },
    Lang {
        name: "cpp",
        aliases: &["c++"],
        exts: &["cc", "cpp", "cxx", "hpp", "hh", "hxx", "ipp", "tpp"],
    },
    Lang {
        name: "ruby",
        aliases: &["rb"],
        exts: &["rb", "rake", "gemspec"],
    },
    Lang {
        name: "csharp",
        aliases: &["c#", "cs"],
        exts: &["cs"],
    },
    Lang {
        name: "php",
        aliases: &[],
        exts: &["php"],
    },
    Lang {
        name: "scala",
        aliases: &[],
        exts: &["scala", "sc"],
    },
    Lang {
        name: "kotlin",
        aliases: &["kt"],
        exts: &["kt", "kts"],
    },
    Lang {
        name: "bash",
        aliases: &["sh", "shell"],
        exts: &["sh", "bash", "zsh"],
    },
    Lang {
        name: "json",
        aliases: &[],
        exts: &["json"],
    },
    Lang {
        name: "css",
        aliases: &[],
        exts: &["css"],
    },
    Lang {
        name: "html",
        aliases: &[],
        exts: &["html", "htm"],
    },
];

/// Languages without a grammar. They name their files for `--lang` only, and
/// their files are chunked by lines, as any text is.
const TEXT_LANGS: &[Lang] = &[
    Lang {
        name: "markdown",
        aliases: &["md"],
        exts: &["md", "markdown", "mdx"],
    },
    Lang {
        name: "yaml",
        aliases: &["yml"],
        exts: &["yaml", "yml"],
    },
];

/// The file extensions a `--lang` value stands for. A language name or alias
/// gives its extensions; a value with a leading dot is that extension alone,
/// and anything else is taken as a bare extension, so `toml` works without
/// being listed. Empty when nothing is named.
pub fn extensions(value: &str) -> Vec<String> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(ext) = value.strip_prefix('.') {
        return match ext {
            "" => Vec::new(),
            ext => vec![ext.to_string()],
        };
    }
    match LANGS
        .iter()
        .chain(TEXT_LANGS)
        .find(|l| l.name == value || l.aliases.contains(&value.as_str()))
    {
        Some(l) => l.exts.iter().map(|e| e.to_string()).collect(),
        None => match value.as_str() {
            "" => Vec::new(),
            ext => vec![ext.to_string()],
        },
    }
}

#[cfg(feature = "treesitter")]
pub fn detect(path: &Path) -> Option<LangSpec> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let lang = LANGS.iter().find(|l| l.exts.contains(&ext.as_str()))?;
    let language: Language = match lang.name {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "json" => tree_sitter_json::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "html" => tree_sitter_html::LANGUAGE.into(),
        _ => return None,
    };
    Some(LangSpec { language })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_or_alias_gives_the_languages_extensions() {
        assert_eq!(extensions("rust"), ["rs"]);
        assert_eq!(extensions("rs"), ["rs"]);
        assert_eq!(extensions("python"), ["py", "pyi", "pyw"]);
        assert_eq!(extensions("ts"), ["ts", "mts", "cts"]);
        assert_eq!(extensions("tsx"), ["tsx"]);
        assert_eq!(extensions("c++"), extensions("cpp"));
        assert_eq!(extensions("c#"), ["cs"]);
        assert_eq!(extensions("shell"), ["sh", "bash", "zsh"]);
        assert_eq!(extensions("kt"), ["kt", "kts"]);
        assert_eq!(extensions("md"), ["md", "markdown", "mdx"]);
        assert_eq!(extensions("markdown"), extensions("md"));
        assert_eq!(extensions("yml"), ["yaml", "yml"]);
    }

    #[test]
    fn a_name_is_read_without_regard_to_case() {
        assert_eq!(extensions("Rust"), ["rs"]);
        assert_eq!(extensions(" CSharp "), ["cs"]);
        assert_eq!(extensions("TOML"), ["toml"]);
    }

    #[test]
    fn anything_else_is_a_bare_extension() {
        assert_eq!(extensions("toml"), ["toml"]);
        assert_eq!(extensions(".md"), ["md"]);
        assert_eq!(extensions(".py"), ["py"]);
        assert_eq!(extensions(".sh"), ["sh"]);
        assert_eq!(extensions("h"), ["h"]);
        assert!(extensions("").is_empty());
        assert!(extensions(" . ").is_empty());
    }

    #[test]
    fn the_readme_counts_the_languages_with_a_grammar() {
        let readme = include_str!("../README.md");
        let grammars = LANGS.len();
        assert!(
            readme.contains(&format!("in {grammars} languages, so a definition")),
            "README should say the chunker covers {grammars} languages"
        );
    }

    #[cfg(feature = "treesitter")]
    #[test]
    fn detect_reads_the_same_table() {
        for lang in LANGS {
            for ext in lang.exts {
                assert!(detect(Path::new(&format!("x.{ext}"))).is_some(), "{ext}");
            }
        }
        assert!(detect(Path::new("x.toml")).is_none());
    }
}
