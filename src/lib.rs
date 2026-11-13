#[cfg(feature = "hnsw")]
pub mod ann;
pub mod chunk;
pub mod cli;
pub mod config;
pub mod embed;
pub mod fingerprint;
pub mod index;
pub mod lang;
pub mod lexical;
pub mod mcp;
pub mod model;
pub mod pipeline;
pub mod quant;
pub mod scope;
#[cfg(feature = "treesitter")]
pub mod semantic;
pub mod symbol;
pub mod walk;
pub mod watch;

#[cfg(test)]
pub mod testutil;
