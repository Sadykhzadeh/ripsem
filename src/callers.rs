//! The call sites of one name across the files the index holds: the matching
//! rule of `ripsem callers` and the MCP `find_callers` tool, and the parallel
//! read of the files behind them.

use std::path::Path;

use anyhow::{Result, bail};
use rayon::prelude::*;

use crate::calls::{self, CallSite};
use crate::walk;

/// The name a caller asks about, with the qualifier it was written with:
/// `load` matches every call to a `load`, `Index::load` only those whose
/// receiver or path ends in `Index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub callee: String,
    pub qualifier: Vec<String>,
}

impl Query {
    /// Splits `Index::load`, `module.load` or `load` into its segments. Each
    /// segment must be a name, so `Index::` or `::load` is refused.
    pub fn parse(raw: &str) -> Result<Query> {
        let mut segments: Vec<&str> = raw
            .split("::")
            .flat_map(|part| part.split('.'))
            .map(str::trim)
            .collect();
        let well_formed = segments
            .iter()
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'));
        if !well_formed {
            bail!("'{raw}' is not a function name; give a name such as load or Index::load");
        }
        let callee = segments.pop().unwrap_or_default().to_string();
        let qualifier = segments.into_iter().map(str::to_string).collect();
        Ok(Query { callee, qualifier })
    }

    /// Whether a call site is one the query names. The site's qualifier must
    /// end with the query's, so `crate::index::Index::load` answers
    /// `Index::load`. A query without a qualifier takes every call of the name.
    pub fn matches(&self, site: &CallSite) -> bool {
        if site.callee != self.callee {
            return false;
        }
        let (have, want) = (&site.qualifier, &self.qualifier);
        have.len() >= want.len() && have[have.len() - want.len()..] == want[..]
    }
}

/// A call site in the files of the index, with the source line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: String,
    pub site: CallSite,
    /// The line of the call, trimmed.
    pub text: String,
}

/// Every call to the query in `files`, which are repository-relative paths in
/// the order given. A file that the query's name does not occur in is not
/// parsed at all, which is most files for most names.
pub fn find(root: &Path, files: &[&str], query: &Query) -> Vec<Found> {
    files
        .par_iter()
        .flat_map_iter(|rel| in_file(root, rel, query))
        .collect()
}

fn in_file(root: &Path, rel: &str, query: &Query) -> Vec<Found> {
    let Some(spec) = crate::lang::detect(Path::new(rel)) else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(root.join(rel)) else {
        return Vec::new();
    };
    let text = walk::decode_text(&bytes);
    if !text.contains(query.callee.as_str()) {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    calls::call_sites(&text, &spec)
        .into_iter()
        .filter(|site| query.matches(site))
        .map(|site| {
            let line = lines.get(site.line - 1).map_or("", |l| l.trim());
            Found {
                path: rel.to_string(),
                text: line.to_string(),
                site,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(callee: &str, qualifier: &[&str]) -> CallSite {
        CallSite {
            callee: callee.into(),
            qualifier: qualifier.iter().map(|q| q.to_string()).collect(),
            line: 1,
            column: 1,
            enclosing: None,
        }
    }

    #[test]
    fn a_name_splits_into_its_qualifier_and_callee() {
        let parsed = |raw: &str| {
            let q = Query::parse(raw).unwrap();
            (q.qualifier, q.callee)
        };
        assert_eq!(parsed("load"), (vec![], "load".to_string()));
        assert_eq!(
            parsed("Index::load"),
            (vec!["Index".to_string()], "load".to_string())
        );
        assert_eq!(
            parsed("crate::db.open"),
            (
                vec!["crate".to_string(), "db".to_string()],
                "open".to_string()
            )
        );
    }

    #[test]
    fn a_name_with_an_empty_or_odd_segment_is_refused() {
        for raw in [
            "",
            "Index::",
            "::load",
            "a..b",
            "load()",
            "a b",
            "Vec<u8>::new",
        ] {
            let err = Query::parse(raw).unwrap_err().to_string();
            assert!(
                err.contains(&format!("'{raw}' is not a function name")),
                "{raw}: {err}"
            );
        }
    }

    #[test]
    fn an_unqualified_name_takes_every_call_of_it() {
        let q = Query::parse("load").unwrap();
        assert!(q.matches(&site("load", &[])));
        assert!(q.matches(&site("load", &["Index"])));
        assert!(!q.matches(&site("loads", &[])));
    }

    #[test]
    fn a_qualified_name_needs_the_qualifier_at_the_end_of_the_call_path() {
        let q = Query::parse("Index::load").unwrap();
        assert!(q.matches(&site("load", &["Index"])));
        assert!(q.matches(&site("load", &["crate", "index", "Index"])));
        assert!(
            !q.matches(&site("load", &[])),
            "a receiver that is not a name is not narrowed in"
        );
        assert!(!q.matches(&site("load", &["Store"])));
        assert!(!q.matches(&site("load", &["Index", "inner"])));
    }
}
