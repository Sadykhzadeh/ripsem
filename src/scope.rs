use std::path::Path;

use anyhow::{Result, anyhow, bail};
use ignore::overrides::{Override, OverrideBuilder};

use crate::index::path_in_scope;
use crate::walk::slashed;

/// What a search is limited to: a path prefix, the languages of the files and
/// globs on the path, all of which a hit's file must satisfy. Built once per
/// query, so a hit is kept or dropped before the ranking is cut to its depth.
#[derive(Debug)]
pub struct Scope {
    prefix: Option<String>,
    /// Lowercase extensions, with the languages as given for messages.
    langs: Option<(Vec<String>, Vec<String>)>,
    globs: Option<Globs>,
}

/// A glob without `!` includes, one with it excludes. A path counts when it
/// or a directory above it matches, as `vendor/` takes in everything inside.
/// Exclusion wins over inclusion.
#[derive(Debug)]
struct Globs {
    include: Option<Override>,
    exclude: Option<Override>,
    given: Vec<String>,
}

impl Globs {
    fn new(raw: &[String]) -> Result<Option<Globs>> {
        if raw.is_empty() {
            return Ok(None);
        }
        // Both sets take their patterns as inclusions; which set a pattern is
        // in says what a match means.
        let mut include = (OverrideBuilder::new(""), false);
        let mut exclude = (OverrideBuilder::new(""), false);
        for glob in raw {
            let glob = glob.trim();
            let (set, pattern) = match glob.strip_prefix('!') {
                Some(rest) => (&mut exclude, rest),
                None => (&mut include, glob),
            };
            if pattern.is_empty() {
                bail!("'{glob}' is not a glob");
            }
            set.0
                .add(&slashed(pattern))
                .map_err(|e| anyhow!("invalid glob '{glob}': {e}"))?;
            set.1 = true;
        }
        let build = |(builder, used): (OverrideBuilder, bool)| match used {
            true => builder.build().map(Some).map_err(anyhow::Error::from),
            false => Ok(None),
        };
        Ok(Some(Globs {
            include: build(include)?,
            exclude: build(exclude)?,
            given: raw.iter().map(|g| g.trim().to_string()).collect(),
        }))
    }

    fn admits(&self, path: &str) -> bool {
        self.include.as_ref().is_none_or(|set| reaches(set, path))
            && !self.exclude.as_ref().is_some_and(|set| reaches(set, path))
    }
}

fn reaches(set: &Override, path: &str) -> bool {
    set.matched(path, false).is_whitelist()
        || Path::new(path)
            .ancestors()
            .skip(1)
            .filter(|dir| !dir.as_os_str().is_empty())
            .any(|dir| set.matched(dir, true).is_whitelist())
}

impl Scope {
    /// `None` when nothing limits the search. A `lang` may hold several values
    /// separated by commas.
    pub fn new(prefix: Option<&str>, langs: &[String], globs: &[String]) -> Result<Option<Scope>> {
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
        let globs = Globs::new(globs)?;
        if prefix.is_none() && langs.is_none() && globs.is_none() {
            return Ok(None);
        }
        Ok(Some(Scope {
            prefix: prefix.map(str::to_string),
            langs,
            globs,
        }))
    }

    pub fn under(prefix: &str) -> Scope {
        Scope {
            prefix: Some(prefix.to_string()),
            langs: None,
            globs: None,
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
        self.globs.as_ref().is_none_or(|globs| globs.admits(path))
    }

    /// The report for a scope no indexed file satisfies.
    pub fn nothing_matches(&self) -> String {
        match (&self.prefix, &self.langs, &self.globs) {
            (Some(prefix), None, None) => format!("nothing is indexed under '{prefix}'"),
            _ => {
                let mut parts = Vec::new();
                if let Some(prefix) = &self.prefix {
                    parts.push(format!("path '{prefix}'"));
                }
                if let Some((_, langs)) = &self.langs {
                    parts.push(format!("lang {}", langs.join(",")));
                }
                if let Some(globs) = &self.globs {
                    parts.push(format!("glob {}", globs.given.join(",")));
                }
                format!("no indexed file matches {}", parts.join(" and "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn scope(prefix: Option<&str>, langs: &[&str], globs: &[&str]) -> Scope {
        Scope::new(prefix, &strings(langs), &strings(globs))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn no_limit_is_no_scope() {
        assert!(Scope::new(None, &[], &[]).unwrap().is_none());
    }

    #[test]
    fn a_language_keeps_only_its_files() {
        let rust = scope(None, &["rust"], &[]);
        assert!(rust.admits("src/a.rs"));
        assert!(rust.admits("SRC/A.RS"));
        assert!(!rust.admits("src/a.py"));
        assert!(!rust.admits("Makefile"));
    }

    #[test]
    fn languages_are_alternatives_and_may_be_comma_separated() {
        let both = scope(None, &["rs,py", "toml"], &[]);
        for path in ["a.rs", "b.pyi", "Cargo.toml"] {
            assert!(both.admits(path), "{path}");
        }
        assert!(!both.admits("a.go"));
    }

    #[test]
    fn prefix_and_language_must_both_hold() {
        let both = scope(Some("src"), &["rust"], &[]);
        assert!(both.admits("src/a.rs"));
        assert!(!both.admits("src/a.md"));
        assert!(!both.admits("tests/a.rs"));
    }

    #[test]
    fn a_glob_matches_as_in_a_gitignore() {
        let rs = scope(None, &[], &["*.rs"]);
        assert!(rs.admits("a.rs"));
        assert!(rs.admits("src/deep/a.rs"));
        assert!(!rs.admits("src/a.md"));

        let rooted = scope(None, &[], &["src/*.rs"]);
        assert!(rooted.admits("src/a.rs"));
        assert!(!rooted.admits("src/deep/a.rs"));
        assert!(!rooted.admits("tests/a.rs"));

        let any = scope(None, &[], &["src/**/*.rs"]);
        assert!(any.admits("src/deep/er/a.rs"));
        let braces = scope(None, &[], &["*.{rs,py}"]);
        assert!(braces.admits("a.py") && !braces.admits("a.go"));
    }

    #[test]
    fn globs_are_alternatives_and_a_negation_excludes() {
        let some = scope(None, &[], &["*.rs", "*.py"]);
        assert!(some.admits("a.rs") && some.admits("a.py") && !some.admits("a.go"));

        let not_tests = scope(None, &[], &["*.rs", "!*_test.rs"]);
        assert!(not_tests.admits("a.rs"));
        assert!(!not_tests.admits("a_test.rs"));

        let only_not = scope(None, &[], &["!*.md"]);
        assert!(only_not.admits("a.rs"));
        assert!(!only_not.admits("docs/a.md"));
    }

    #[test]
    fn a_directory_glob_takes_in_what_is_inside() {
        let skip = scope(None, &[], &["!vendor/"]);
        assert!(!skip.admits("vendor/a.rs"));
        assert!(!skip.admits("src/vendor/deep/a.rs"));
        assert!(skip.admits("src/a.rs"));

        let only = scope(None, &[], &["src/", "!src/gen.rs"]);
        assert!(only.admits("src/deep/a.rs"));
        assert!(!only.admits("src/gen.rs"));
        assert!(!only.admits("docs/a.rs"));
    }

    #[cfg(windows)]
    #[test]
    fn a_glob_written_with_backslashes_matches_as_with_slashes() {
        let skip = scope(None, &[], &[r"!vendor\"]);
        assert!(!skip.admits("vendor/a.rs"));
        assert!(skip.admits("src/a.rs"));

        let inside = scope(None, &[], &[r"src\*.rs"]);
        assert!(inside.admits("src/a.rs"));
        assert!(!inside.admits("tests/a.rs"));
    }

    #[test]
    fn prefix_language_and_glob_must_all_hold() {
        let all = scope(Some("src"), &["rust"], &["!*_test.rs"]);
        assert!(all.admits("src/a.rs"));
        assert!(!all.admits("src/a_test.rs"));
        assert!(!all.admits("src/a.py"));
        assert!(!all.admits("lib/a.rs"));
    }

    #[test]
    fn a_value_naming_no_language_is_an_error() {
        for bad in ["", "  ", "rust,", ".", ","] {
            let err = Scope::new(None, &[bad.to_string()], &[]).unwrap_err();
            assert!(err.to_string().contains(&format!("'{bad}'")), "{err}");
        }
    }

    #[test]
    fn a_bad_glob_is_an_error_naming_it() {
        for bad in ["", "!", "src/[a", "{a,b"] {
            let err = Scope::new(None, &[], &[bad.to_string()]).unwrap_err();
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
            scope(Some("src"), &["go"], &[]).nothing_matches(),
            "no indexed file matches path 'src' and lang go"
        );
        assert_eq!(
            scope(None, &["go"], &["!a/**"]).nothing_matches(),
            "no indexed file matches lang go and glob !a/**"
        );
    }
}
