pub mod chunk;
pub mod cli;
pub mod config;
pub mod embed;
pub mod fingerprint;
pub mod index;
#[cfg(feature = "treesitter")]
pub mod lang;
pub mod mcp;
pub mod model;
pub mod pipeline;
pub mod quant;
#[cfg(feature = "treesitter")]
pub mod semantic;
pub mod walk;
pub mod watch;

#[cfg(test)]
pub mod testutil;
