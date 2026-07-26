# ripsem

Local, offline code search that answers one question well: *where is the code
that does this?* Ask with an identifier, an error string, or a plain-language
description of the behaviour — one query, one ranked answer. No account, no API
key, no network. Embeddings run on a model that lives on your disk, and the
index is a single file inside your repo.

Every query runs through two independent retrievers and their rankings are
merged: BM25 over code-aware tokens, and vector similarity from the embedding
model. A literal identifier match cannot be lost to semantic distance, and a
question with no shared words still finds the right function.

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
graph is only built above that size; smaller repos keep exact results. The graph
only changes how the vector arm finds its candidates — the lexical arm still
scans exactly, and the two are merged the same way, so ranking and `--min-score`
behave identically at any repo size.

## Usage

```
ripsem init                       # write .ripsem/config.toml
ripsem index                      # build the index (incremental on re-runs)
ripsem search "where is auth set up?"
ripsem status                     # show index size, chunk/file counts, model
ripsem status --json              # the same as JSON (schema ripsem/status/v1)
ripsem watch                      # index and keep it in sync as files change
```

Search respects `.gitignore` and an optional `.ripsemignore`. Results print as
`path:start-end` with a snippet, ordered by the merged ranking. The number shown
is confidence in 0–1: semantic similarity, or the share of the query's terms
found literally, whichever is higher. Add `--json` for machine-readable output,
or `--in <subdir>` to limit results to a subdirectory (e.g. `ripsem search
"where is auth set up?" --in src/auth`). Use `--context <N>` to control how many
snippet lines print per hit (default 3; `0` prints just the locations). Pass
`--min-score <SCORE>` to keep only strong matches (applies to both the human and
`--json` output).

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
`search` directly. `ripsem install cursor` does the same via `.cursor/mcp.json`.

## Configuration

`.ripsem/config.toml`:

```toml
backend = "fastembed"
model = "bge-small-en-v1.5"
chunk_lines = 40
chunk_overlap = 8
max_chunk_lines = 50
max_file_kb = 1024
exclude = []
```

`chunk_lines`/`chunk_overlap` size the line windows used for plain-text files and
for languages without a parser. `max_chunk_lines` caps a single semantic chunk;
larger functions are split. `exclude` takes gitignore-style globs.

## Quality

The benchmark asks the same corpus 21 labelled questions in four shapes — a
description in the reader's own words, an identifier typed from memory, a message
seen in output, and a name remembered wrong — and scores each retriever against
ground-truth files:

```
cargo test --release --features fastembed,treesitter \
  --test retrieval_quality -- --ignored --nocapture
```

The corpus is ripsem's own source at the last commit, snapshotted with `git
archive` so a run is not moved by uncommitted edits. It needs the embedding
model, so it is `#[ignore]`d out of the normal run.

| retriever | MRR | description | identifier | message | recalled name |
| --- | --- | --- | --- | --- | --- |
| vector only | 0.587 | 0.573 | 0.792 | 0.444 | 0.417 |
| lexical only | 0.718 | 0.469 | 1.000 | 1.000 | 0.583 |
| merged | **0.754** | **0.688** | 1.000 | 0.778 | 0.500 |

Two things are worth reading off this table. BM25 alone beats the embedding model
alone on real code, so a search built only on vectors is leaving the stronger
signal on the floor. And neither arm is enough: vectors win where the words don't
match, lexical wins where they do, and only merging them is good at both.

The honest gaps: a name remembered wrong is still served better by the lexical
arm alone, and `models` does not find `ModelInfo` because nothing folds plurals.
A trailing-`s` rule was tried and measured slightly worse (MRR 0.746), so it is
not in the code.

## License

Apache-2.0
