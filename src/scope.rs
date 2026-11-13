use std::path::Path;

use anyhow::{Result, bail};

use crate::index::path_in_scope;

/// What a search is limited to: a path prefix and the languages of the files,
/// all of which a hit's file must satisfy. Built once per query, so a hit is
/// kept or dropped before the ranking is cut to its depth.
#[derive(Debug)]
pub struct Scope {
    prefix: Option<String>,
    /// Lowercase extensions, with the languages as given for messages.
    langs: Option<(Vec<String>, Vec<String>)>,
}

impl Scope {
    /// `None` when nothing limits the search. A `lang` may hold several values
    /// separated by commas.
    pub fn new(prefix: Option<&str>, langs: &[String]) -> Result<Option<Scope>> {
        let mut given = Vec::new();
        let mut exts = Vec::new();
        for raw in langs {
            for piece in raw.split(',') {
                let found = crate::lang::extensions(piece);
                if found.is_empty() {
                    bail!(
                        "'{raw}' names no language; give a name such as rust or an extension such as toml"
                    );
                }
                given.push(piece.trim().to_string());
                exts.extend(found);
            }
        }
        let langs = (!exts.is_empty()).then_some((exts, given));
        if prefix.is_none() && langs.is_none() {
            return Ok(None);
        }
        Ok(Some(Scope {
            prefix: prefix.map(str::to_string),
            langs,
        }))
    }

    pub fn under(prefix: &str) -> Scope {
        Scope {
            prefix: Some(prefix.to_string()),
            langs: None,
        }
    }

    pub fn admits(&self, path: &str) -> bool {
        if let Some(prefix) = &self.prefix
            && !path_in_scope(path, prefix)
        {
            return false;
        }
        if let Some((exts, _)) = &self.langs {
            let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
                return false;
            };
            if !exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                return false;
            }
        }
        true
    }

    /// The report for a scope no indexed file satisfies.
    pub fn nothing_matches(&self) -> String {
        match (&self.prefix, &self.langs) {
            (Some(prefix), None) => format!("nothing is indexed under '{prefix}'"),
            _ => {
                let mut parts = Vec::new();
                if let Some(prefix) = &self.prefix {
                    parts.push(format!("path '{prefix}'"));
                }
                if let Some((_, langs)) = &self.langs {
                    parts.push(format!("lang {}", langs.join(",")));
                }
                format!("no indexed file matches {}", parts.join(" and "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(prefix: Option<&str>, langs: &[&str]) -> Scope {
        let langs: Vec<String> = langs.iter().map(|l| l.to_string()).collect();
        Scope::new(prefix, &langs).unwrap().unwrap()
    }

    #[test]
    fn no_limit_is_no_scope() {
        assert!(Scope::new(None, &[]).unwrap().is_none());
    }

    #[test]
    fn a_language_keeps_only_its_files() {
        let rust = scope(None, &["rust"]);
        assert!(rust.admits("src/a.rs"));
        assert!(rust.admits("SRC/A.RS"));
        assert!(!rust.admits("src/a.py"));
        assert!(!rust.admits("Makefile"));
    }

    #[test]
    fn languages_are_alternatives_and_may_be_comma_separated() {
        let both = scope(None, &["rs,py", "toml"]);
        for path in ["a.rs", "b.pyi", "Cargo.toml"] {
            assert!(both.admits(path), "{path}");
        }
        assert!(!both.admits("a.go"));
    }

    #[test]
    fn prefix_and_language_must_both_hold() {
        let both = scope(Some("src"), &["rust"]);
        assert!(both.admits("src/a.rs"));
        assert!(!both.admits("src/a.md"));
        assert!(!both.admits("tests/a.rs"));
    }

    #[test]
    fn a_value_naming_no_language_is_an_error() {
        for bad in ["", "  ", "rust,", ".", ","] {
            let err = Scope::new(None, &[bad.to_string()]).unwrap_err();
            assert!(err.to_string().contains(&format!("'{bad}'")), "{err}");
        }
    }

    #[test]
    fn the_report_names_what_was_asked() {
        assert_eq!(
            Scope::under("src").nothing_matches(),
            "nothing is indexed under 'src'"
        );
        assert_eq!(
            scope(Some("src"), &["go"]).nothing_matches(),
            "no indexed file matches path 'src' and lang go"
        );
    }
}
