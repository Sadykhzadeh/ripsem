//! The index as it sits on disk: one column per field of a record, so
//! decoding builds a handful of big buffers instead of a few allocations per
//! record, and a loaded record is a set of ranges into them.

use std::ops::Range;
use std::sync::Arc;

use anyhow::{Result, anyhow, ensure};
use bitcode::{Decode, Encode};

use super::{Index, Record};
use crate::chunk::Chunking;
use crate::config::Backend;
use crate::lexical::TermVector;
use crate::quant::QuantizedVector;
use crate::shared::{Shared, SharedStr};
use crate::symbol::{Declaration, Symbol};

#[derive(Encode, Decode)]
pub(super) struct Stored {
    dims: usize,
    backend: Backend,
    model: String,
    chunking: Chunking,
    ann: Option<Vec<u8>>,
    noise_floor: Option<f32>,
    embedded_since_floor: usize,
    /// Runs of records that share a path, in record order.
    files: Vec<StoredFile>,
    start_lines: Vec<u32>,
    end_lines: Vec<u32>,
    text_hashes: Vec<u64>,
    /// Where each snippet ends in `snippets`; the same shape follows for the
    /// terms and the declarations.
    snippet_ends: Vec<u32>,
    snippets: String,
    scales: Vec<f32>,
    /// `dims` values per record.
    vectors: Vec<i8>,
    term_ends: Vec<u32>,
    term_lengths: Vec<u32>,
    term_hashes: Vec<u64>,
    term_counts: Vec<u8>,
    symbols: Vec<Option<Symbol>>,
    declaration_ends: Vec<u32>,
    declarations: Vec<Declaration>,
}

#[derive(Encode, Decode)]
struct StoredFile {
    path: String,
    file_hash: u64,
    records: u32,
}

fn narrow(value: usize, what: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| anyhow!("{what} does not fit in 32 bits"))
}

/// The span the `i`th of a column's cumulative ends covers.
fn span(ends: &[u32], i: usize) -> Range<usize> {
    let start = match i {
        0 => 0,
        _ => ends[i - 1] as usize,
    };
    start..ends[i] as usize
}

impl Stored {
    pub(super) fn from_index(index: &Index) -> Result<Self> {
        let count = index.records.len();
        narrow(count, "the number of records")?;
        let mut stored = Self {
            dims: index.dims,
            backend: index.backend,
            model: index.model.clone(),
            chunking: index.chunking,
            ann: index.ann.clone(),
            noise_floor: index.noise_floor,
            embedded_since_floor: index.embedded_since_floor,
            files: Vec::new(),
            start_lines: Vec::with_capacity(count),
            end_lines: Vec::with_capacity(count),
            text_hashes: Vec::with_capacity(count),
            snippet_ends: Vec::with_capacity(count),
            snippets: String::new(),
            scales: Vec::with_capacity(count),
            vectors: Vec::with_capacity(count * index.dims),
            term_ends: Vec::with_capacity(count),
            term_lengths: Vec::with_capacity(count),
            term_hashes: Vec::new(),
            term_counts: Vec::new(),
            symbols: Vec::with_capacity(count),
            declaration_ends: Vec::with_capacity(count),
            declarations: Vec::new(),
        };
        for r in &index.records {
            match stored.files.last_mut() {
                Some(file) if file.path == r.path.as_str() && file.file_hash == r.file_hash => {
                    file.records += 1;
                }
                _ => stored.files.push(StoredFile {
                    path: r.path.to_string(),
                    file_hash: r.file_hash,
                    records: 1,
                }),
            }
            ensure!(
                r.vector.len() == index.dims,
                "a record holds {} vector values in an index of {} dimensions",
                r.vector.len(),
                index.dims
            );
            stored
                .start_lines
                .push(narrow(r.start_line, "a line number")?);
            stored.end_lines.push(narrow(r.end_line, "a line number")?);
            stored.text_hashes.push(r.text_hash);
            stored.snippets.push_str(&r.snippet);
            stored
                .snippet_ends
                .push(narrow(stored.snippets.len(), "the snippets")?);
            stored.scales.push(r.vector.scale);
            stored.vectors.extend_from_slice(&r.vector.data);
            stored.term_hashes.extend_from_slice(r.terms.hashes());
            stored.term_counts.extend_from_slice(r.terms.counts());
            stored
                .term_ends
                .push(narrow(stored.term_hashes.len(), "the terms")?);
            stored.term_lengths.push(r.terms.length());
            stored.symbols.push(r.symbol.clone());
            stored.declarations.extend_from_slice(&r.declarations);
            stored
                .declaration_ends
                .push(narrow(stored.declarations.len(), "the declarations")?);
        }
        Ok(stored)
    }

    pub(super) fn into_index(self) -> Result<Index> {
        let Self {
            dims,
            backend,
            model,
            chunking,
            ann,
            noise_floor,
            embedded_since_floor,
            files,
            start_lines,
            end_lines,
            text_hashes,
            snippet_ends,
            snippets,
            scales,
            vectors,
            term_ends,
            term_lengths,
            term_hashes,
            term_counts,
            symbols,
            declaration_ends,
            declarations,
        } = self;
        let count = start_lines.len();
        let in_files: u64 = files.iter().map(|f| u64::from(f.records)).sum();
        ensure!(
            in_files == count as u64
                && [
                    end_lines.len(),
                    text_hashes.len(),
                    snippet_ends.len(),
                    scales.len(),
                    term_ends.len(),
                    term_lengths.len(),
                    symbols.len(),
                    declaration_ends.len(),
                ]
                .iter()
                .all(|&len| len == count)
                && count.checked_mul(dims) == Some(vectors.len())
                && term_hashes.len() == term_counts.len(),
            "its columns disagree about how many records there are"
        );

        let snippets: Arc<str> = snippets.into();
        let vectors: Arc<[i8]> = vectors.into();
        let term_hashes: Arc<[u64]> = term_hashes.into();
        let term_counts: Arc<[u8]> = term_counts.into();
        let declarations: Arc<[Declaration]> = declarations.into();
        let outside = || anyhow!("a record points outside its column");

        let mut records = Vec::with_capacity(count);
        let mut symbols = symbols.into_iter();
        for file in files {
            let path = SharedStr::from(file.path);
            for _ in 0..file.records {
                let i = records.len();
                let terms = span(&term_ends, i);
                records.push(Record {
                    path: path.clone(),
                    start_line: start_lines[i] as usize,
                    end_line: end_lines[i] as usize,
                    snippet: SharedStr::slice_of(&snippets, span(&snippet_ends, i))
                        .ok_or_else(outside)?,
                    file_hash: file.file_hash,
                    text_hash: text_hashes[i],
                    vector: QuantizedVector {
                        scale: scales[i],
                        data: Shared::slice_of(&vectors, i * dims..(i + 1) * dims)
                            .ok_or_else(outside)?,
                    },
                    terms: TermVector::from_columns(
                        Shared::slice_of(&term_hashes, terms.clone()).ok_or_else(outside)?,
                        Shared::slice_of(&term_counts, terms).ok_or_else(outside)?,
                        term_lengths[i],
                    ),
                    symbol: symbols.next().flatten(),
                    declarations: Shared::slice_of(&declarations, span(&declaration_ends, i))
                        .ok_or_else(outside)?,
                });
            }
        }
        Ok(Index {
            dims,
            backend,
            model,
            chunking,
            records,
            ann,
            noise_floor,
            embedded_since_floor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> Stored {
        let mut index = Index::new(2, Backend::Hash, "test".into());
        for (path, snippet) in [("a.rs", "one"), ("a.rs", "two")] {
            index.records.push(Record {
                path: path.into(),
                start_line: 1,
                end_line: 2,
                snippet: snippet.into(),
                file_hash: 5,
                text_hash: 6,
                vector: crate::quant::quantize(&[1.0, 0.5]),
                terms: TermVector::new(snippet, path, None),
                symbol: None,
                declarations: Shared::default(),
            });
        }
        Stored::from_index(&index).unwrap()
    }

    #[test]
    fn records_of_one_file_are_one_run() {
        let stored = stored();
        assert_eq!(stored.files.len(), 1);
        assert_eq!(stored.files[0].records, 2);
        assert!(stored.into_index().is_ok());
    }

    /// An index file is data on disk, so a column that disagrees with the rest
    /// is an error, not an out-of-range slice.
    #[test]
    fn columns_that_disagree_are_corrupt_not_a_panic() {
        let mut short = stored();
        short.scales.pop();
        assert!(short.into_index().is_err());

        let mut past_the_end = stored();
        past_the_end.snippet_ends[1] += 100;
        assert!(past_the_end.into_index().is_err());

        let mut backwards = stored();
        backwards.term_ends[1] = 0;
        backwards.term_ends[0] = 1;
        assert!(backwards.into_index().is_err());

        let mut more_files = stored();
        more_files.files[0].records = 3;
        assert!(more_files.into_index().is_err());
    }

    #[test]
    fn a_snippet_cut_inside_a_character_is_corrupt() {
        let mut cut = stored();
        cut.snippets = "\u{e9}\u{e9}".into();
        cut.snippet_ends = vec![1, 4];
        assert!(cut.into_index().is_err());
    }
}
