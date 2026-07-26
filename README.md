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
fixed line windows, which keeps each indexed snippet aligned to a logical unit,
and names each chunk after the declaration it belongs to — results arrive as
`src/pipeline.rs:258-293 fn Searcher::query_scoped`, so a caller can tell a
definition from a call site without opening the file. A function too large to be
one chunk passes its name to every piece it is cut into. It covers Rust, Python,
JavaScript, TypeScript, Go, Java, C, C++, C#, Ruby, PHP, Scala, Kotlin, Bash,
HTML, CSS, and JSON; other files fall back to line-window chunking without
names.

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
found literally, whichever is higher. Order comes from how strongly both
retrievers back a chunk, so confidence does not have to fall down the list — a
hit ranked lower can still carry every word you typed. Add `--json` for
machine-readable output,
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
| bge-small-en-v1.5 | 384 | ~133 MB | default, and the best of the three measured |
| all-MiniLM-L6-v2 | 384 | ~90 MB | fastest, measurably lower recall |
| bge-base-en-v1.5 | 768 | ~436 MB | better alone, worse merged — see Quality |
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
skip_generated = true
exclude = []
```

`chunk_lines`/`chunk_overlap` size the line windows used for plain-text files and
for languages without a parser. `max_chunk_lines` caps a single semantic chunk;
larger functions are split. `exclude` takes gitignore-style globs.

`skip_generated` leaves out files that are committed but written by a machine —
lock files, `*.min.js`, `*.min.css` and source maps. `.gitignore` does not cover
them because they are committed on purpose, and one of them can outweigh
everything written by hand: in this repository `Cargo.lock` alone accounted for
124 of 291 chunks. Set it to `false` to index them anyway.

## Quality

The benchmark asks 73 labelled questions across four real projects in four
languages, in four shapes — a description in the reader's own words, an identifier
typed from memory, a message seen in output, and a name remembered wrong — and
scores each retriever against ground-truth files:

```
cargo test --release --features fastembed,treesitter \
  --test retrieval_quality -- --ignored --nocapture
```

Every corpus is pinned: ripsem's own source at the `corpus/v1` tag, and three
projects at release tags, cloned once into `target/corpora`. Nothing moves as the
code moves, so two runs differ only by what changed in retrieval. The first run
needs network and the embedding model, so it is `#[ignore]`d out of the normal
run.

| corpus | language | chunks | vector only | lexical only | merged |
| --- | --- | --- | --- | --- | --- |
| ripsem | Rust | 109 | 0.587 | 0.718 | **0.754** |
| requests v2.34.2 | Python | 612 | **0.747** | 0.531 | 0.713 |
| mux v1.8.1 | Go | 257 | 0.326 | 0.590 | **0.681** |
| got v14.4.5 | TypeScript | 757 | 0.438 | 0.458 | **0.567** |
| all | | 1735 | 0.547 | 0.575 | **0.679** |

| query shape | vector only | lexical only | merged |
| --- | --- | --- | --- |
| description (28) | 0.429 | 0.354 | **0.551** |
| identifier (22) | 0.731 | 0.714 | **0.811** |
| message (9) | 0.519 | **1.000** | 0.870 |
| recalled name (14) | 0.514 | 0.524 | **0.607** |

The per-corpus column is why more than one project is measured. Which arm is
stronger *reverses* between them: BM25 wins on Go and on Rust, the embedding model
wins decisively on Python, and on TypeScript they are level. An earlier version of
this file claimed BM25 beats the model on real code — that held for the one
repository being measured and is false in general.

So the case for merging is not that it wins every corpus; on `requests` it costs
0.034 against the vector arm alone. The case is that nothing tells you in advance
which arm a given repository and a given question will need, and merging is ahead
of both wherever it is not ahead of one of them: +0.10 on the aggregate, and ahead
on three shapes out of four.

The default model is a measurement, not a guess. Three models over the same 73
questions:

| model | dims | disk | vector only | merged |
| --- | --- | --- | --- | --- |
| all-MiniLM-L6-v2 | 384 | ~90 MB | 0.537 | 0.647 |
| bge-small-en-v1.5 | 384 | ~133 MB | 0.547 | **0.679** |
| bge-base-en-v1.5 | 768 | ~436 MB | **0.564** | 0.628 |

The larger model is the better retriever on its own and the worse one merged, and
it is worst exactly on descriptions — the shape a stronger language model should
help most. Its advantage shows up only on the Rust corpus (0.774 against 0.754)
and it loses on the other three. Why a model that ranks better alone merges worse
is not explained here, only measured; three times the disk for less accuracy is
reason enough to keep the smaller default and not to invent a story about it.

The same run measures what reaching the answer costs to read. Counting the
snippets up to and including the first correct hit, a caller takes in **587 bytes
to reach the answer, against 14 887 bytes for the whole file that answer lives in
— 25× less**. Read that narrowly: it is the 61 of 73 questions answered inside the
top five, and the comparison is against reading one file end to end, which is what
happens when a search can only name a filename. It is not a model of what an agent
really does, and it says nothing about the 12 questions no arm answered.

A second benchmark compares what the two `skip_generated` settings return.
Indexing the lock file put a machine-written chunk in 6 of 105 result slots, and
cost 291 chunks against 167, 386 KB against 237 KB, and 16.4 s against 10.0 s to
build. Ranking is deliberately not part of that claim: `crates.io-index` occurs
409 times in `Cargo.lock`, so indexing it hands the word `index` the document
frequency of a stopword, and one corpus cannot separate that accident from the
change being measured.

The honest gaps. A message seen in output is still served better by the lexical
arm alone, 1.000 against 0.870: when several chunks contain every word of the
query the merge falls back to how much the two arms agree, which gives the vector
arm a vote it has not earned. Letting BM25 order that group instead was tried — it
lifts messages to 0.926 and drops identifiers from 0.811 to 0.714, because a
one-word query reaches full coverage in dozens of chunks and the tier stops
meaning anything. Aggregate 0.652 against 0.679, so it is not in the code, and the
gap stays open rather than being closed with a threshold tuned on nine queries.
A name remembered wrong is the weakest shape for every retriever. `models` does not find `ModelInfo` because nothing folds plurals —
a trailing-`s` rule was tried and measured worse. Weighting the declared name
above the rest of a chunk moved a single query in and out of first place and was
dropped as a bump rather than a trend.

## License

Apache-2.0
