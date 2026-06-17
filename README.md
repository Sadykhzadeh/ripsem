# ripsem

Local, offline semantic code search. Ask your codebase questions in plain
language and get back the most relevant chunks — no account, no API key, no
network. Embeddings run on a model that lives on your disk, and the index is a
single file inside your repo.

Pairs with `grep`/`ripgrep`: use those for exact patterns, use `ripsem` when you
know *what* you want but not the exact words.

## Install

```
cargo install --path . --features fastembed,treesitter
```

The `fastembed` feature pulls in the local embedding runtime. Without it the
binary still builds and runs on a lightweight lexical backend (useful for tests
and CI), but real semantic search needs the model.

The `treesitter` feature chunks code on function and type boundaries instead of
fixed line windows, which keeps each indexed snippet aligned to a logical unit.
It covers Rust, Python, JavaScript, TypeScript, Go, Java, C, C++, C#, Ruby, PHP,
Scala, Kotlin, Bash, HTML, CSS, and JSON; other files fall back to line-window
chunking.

The `hnsw` feature builds an approximate-nearest-neighbour index for large
repositories. Below ~20k chunks the exact flat scan is already sub-10 ms, so the
graph is only built above that size; smaller repos keep exact results.

## Usage

```
ripsem init                       # write .ripsem/config.toml
ripsem index                      # build the index (incremental on re-runs)
ripsem search "where is auth set up?"
ripsem status                     # show index size, chunk/file counts, model
ripsem watch                      # index and keep it in sync as files change
```

Search respects `.gitignore` and an optional `.ripsemignore`. Results are
ranked by cosine similarity and print as `path:start-end` with a snippet. Add
`--json` for machine-readable output.

## Models

```
ripsem model list                 # show available models
ripsem model set bge-base-en-v1.5
ripsem index --force              # rebuild after switching
```

| Model | Dims | Disk | Notes |
| --- | --- | --- | --- |
| bge-small-en-v1.5 | 384 | ~133 MB | default, strong quality/size balance |
| all-MiniLM-L6-v2 | 384 | ~90 MB | fastest, lower recall |
| bge-base-en-v1.5 | 768 | ~436 MB | higher quality, heavier |
| multilingual-e5-small | 384 | ~470 MB | multilingual |
| nomic-embed-text-v1.5 | 768 | ~550 MB | long context |

The model downloads once on first use, then everything is offline.

## Use with Claude Code

```
ripsem index
ripsem install claude-code        # writes ripsem into .mcp.json
```

`ripsem mcp` speaks MCP over stdio and exposes a single `search` tool. After
`install claude-code`, restart Claude Code in the directory and it can call
`search` directly.

## Configuration

`.ripsem/config.toml`:

```toml
backend = "fastembed"
model = "bge-small-en-v1.5"
chunk_lines = 40
chunk_overlap = 8
max_chunk_lines = 50
lexical_boost = 0.1
max_file_kb = 1024
exclude = []
```

`chunk_lines`/`chunk_overlap` size the line windows used for plain-text files and
for languages without a parser. `max_chunk_lines` caps a single semantic chunk;
larger functions are split. `lexical_boost` nudges results whose text contains the
query's literal terms above otherwise-equal matches (set it to `0.0` for pure
semantic ranking). `exclude` takes gitignore-style globs.

## Quality

Retrieval quality is measured by a labelled benchmark over a small multi-domain
corpus, scored by recall@k and MRR against ground-truth files:

```
cargo test --release --features fastembed,treesitter \
  --test retrieval_quality -- --ignored --nocapture
```

It needs the embedding model, so it is `#[ignore]`d out of the normal run.

## License

Apache-2.0
