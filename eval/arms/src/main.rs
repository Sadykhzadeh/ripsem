//! Reads an index that `ripsem index` built and ranks queries with the vector
//! arm alone, the lexical arm alone and the merge of both. The product has no
//! switch for this, and none is added: the arms are public library calls.
//!
//! ripsem-arms <tree> records
//!     one JSON line: every record of the index as [path, start, end, label, is_test]
//! ripsem-arms <tree> rank <queries.jsonl> <depth>
//!     one JSON line per query: {"id", "dense", "lexical", "fused"}, each a list
//!     of record numbers, best first
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use ripsem::config::Config;
use ripsem::index::Index;
use serde_json::json;

/// The CLI asks the merge for this many hits more than it prints.
const SPARE_HITS: usize = 5;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [tree, "records"] => records(Path::new(tree)),
        [tree, "rank", queries, depth] => rank(Path::new(tree), Path::new(queries), depth.parse()?),
        _ => bail!("usage: ripsem-arms <tree> records | <tree> rank <queries.jsonl> <depth>"),
    }
}

fn open(tree: &Path) -> Result<(Config, Index)> {
    let config = Config::load(tree)?;
    let index = Index::load(&config.index_path()).context("loading the index")?;
    Ok((config, index))
}

fn records(tree: &Path) -> Result<()> {
    let (_, index) = open(tree)?;
    let rows: Vec<_> = index
        .records
        .iter()
        .map(|r| {
            json!([
                r.path.as_str(),
                r.start_line,
                r.end_line,
                r.symbol.as_ref().map(|s| s.label()),
                r.is_test()
            ])
        })
        .collect();
    println!("{}", serde_json::to_string(&rows)?);
    Ok(())
}

fn rank(tree: &Path, queries: &Path, depth: usize) -> Result<()> {
    let (config, index) = open(tree)?;
    let embedder = ripsem::embed::build(&config)?;
    let file = std::fs::File::open(queries)?;
    let mut out = BufWriter::new(std::io::stdout().lock());
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line)?;
        let id = row["id"].clone();
        let text = row["query"]
            .as_str()
            .context("a query line has no `query`")?;
        let qv = embedder.embed_query(text)?;
        let terms = ripsem::lexical::query_terms(text);
        let names = ripsem::lexical::query_names(text);
        let dense = index.dense_ranking(&qv, depth, None);
        let lexical = index.lexical_ranking(&terms, depth, None);
        let k = depth + SPARE_HITS;
        let fused = index.search_fused(&qv, &terms, &names, k, None);
        let numbers = |hits: &[ripsem::index::Hit]| -> Vec<usize> {
            hits.iter().map(|h| h.record_idx).collect()
        };
        writeln!(
            out,
            "{}",
            json!({
                "id": id,
                "dense": numbers(&dense),
                "lexical": numbers(lexical.hits()),
                "fused": numbers(&fused),
            })
        )?;
    }
    Ok(())
}
