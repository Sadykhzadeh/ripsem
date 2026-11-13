//! The index as format 11 stored it: every record a value of its own. It is
//! only read, so an index built before the columnar format loads without
//! embedding the repository again, and the next save writes the new one.

use bitcode::Decode;

use super::{Index as Current, Record as CurrentRecord};
use crate::chunk::Chunking;
use crate::config::Backend;
use crate::lexical::TermVector as CurrentTerms;
use crate::quant::QuantizedVector as CurrentVector;
use crate::symbol::{Declaration, Symbol};

#[derive(Decode, bitcode::Encode)]
pub(super) struct Index {
    dims: usize,
    backend: Backend,
    model: String,
    chunking: Chunking,
    records: Vec<Record>,
    ann: Option<Vec<u8>>,
    noise_floor: Option<f32>,
    embedded_since_floor: usize,
}

#[derive(Decode, bitcode::Encode)]
struct Record {
    path: String,
    start_line: usize,
    end_line: usize,
    snippet: String,
    file_hash: u64,
    text_hash: u64,
    vector: QuantizedVector,
    terms: TermVector,
    symbol: Option<Symbol>,
    declarations: Vec<Declaration>,
}

#[derive(Decode, bitcode::Encode)]
struct QuantizedVector {
    scale: f32,
    data: Vec<i8>,
}

#[derive(Decode, bitcode::Encode)]
struct TermVector {
    terms: Vec<(u64, u8)>,
    length: u32,
}

impl From<Index> for Current {
    fn from(old: Index) -> Self {
        Self {
            dims: old.dims,
            backend: old.backend,
            model: old.model,
            chunking: old.chunking,
            records: old.records.into_iter().map(Into::into).collect(),
            ann: old.ann,
            noise_floor: old.noise_floor,
            embedded_since_floor: old.embedded_since_floor,
        }
    }
}

impl From<Record> for CurrentRecord {
    fn from(old: Record) -> Self {
        let (hashes, counts): (Vec<u64>, Vec<u8>) = old.terms.terms.into_iter().unzip();
        Self {
            path: old.path.into(),
            start_line: old.start_line,
            end_line: old.end_line,
            snippet: old.snippet.into(),
            file_hash: old.file_hash,
            text_hash: old.text_hash,
            vector: CurrentVector {
                scale: old.vector.scale,
                data: old.vector.data.into(),
            },
            terms: CurrentTerms::from_columns(hashes.into(), counts.into(), old.terms.length),
            symbol: old.symbol,
            declarations: old.declarations.into(),
        }
    }
}

#[cfg(test)]
impl From<&Current> for Index {
    fn from(index: &Current) -> Self {
        Self {
            dims: index.dims,
            backend: index.backend,
            model: index.model.clone(),
            chunking: index.chunking,
            records: index.records.iter().map(Into::into).collect(),
            ann: index.ann.clone(),
            noise_floor: index.noise_floor,
            embedded_since_floor: index.embedded_since_floor,
        }
    }
}

#[cfg(test)]
impl From<&CurrentRecord> for Record {
    fn from(record: &CurrentRecord) -> Self {
        Self {
            path: record.path.to_string(),
            start_line: record.start_line,
            end_line: record.end_line,
            snippet: record.snippet.to_string(),
            file_hash: record.file_hash,
            text_hash: record.text_hash,
            vector: QuantizedVector {
                scale: record.vector.scale,
                data: record.vector.data.to_vec(),
            },
            terms: TermVector {
                terms: record
                    .terms
                    .hashes()
                    .iter()
                    .copied()
                    .zip(record.terms.counts().iter().copied())
                    .collect(),
                length: record.terms.length(),
            },
            symbol: record.symbol.clone(),
            declarations: record.declarations.to_vec(),
        }
    }
}
