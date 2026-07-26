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
`path:start-end` with a snippet, ordered by the merged ranking. Two records can
cover the same lines — a declaration and the documentation above it, or the
overlap between adjacent windows — and only the better-ranked one is returned, so
`-k 5` means five distinct places rather than five records. The number shown
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
| ripsem | Rust | 146 | 0.611 | 0.675 | **0.778** |
| requests v2.34.2 | Python | 909 | 0.700 | 0.610 | **0.796** |
| mux v1.8.1 | Go | 398 | 0.576 | 0.736 | **0.875** |
| got v14.4.5 | TypeScript | 851 | 0.600 | 0.542 | **0.729** |
| all | | 2304 | 0.627 | 0.631 | **0.785** |

| query shape | vector only | lexical only | merged |
| --- | --- | --- | --- |
| description (28) | 0.438 | 0.373 | **0.595** |
| identifier (22) | 0.826 | 0.856 | **0.924** |
| message (9) | 0.537 | 1.000 | **1.000** |
| recalled name (14) | 0.750 | 0.554 | **0.810** |

The per-corpus column is why more than one project is measured. Which arm is
stronger *reverses* between them: BM25 wins on Go and on Rust, the embedding model
wins on Python and TypeScript. An earlier version of this file claimed BM25 beats
the model on real code — that held for the one repository being measured and is
false in general.

Merging is ahead on every corpus and on every query shape. Nothing tells you in
advance which arm a given repository and a given question will need, which is the
whole case for not having to choose.

Results that belong to a declaration are ranked above results that do not, and
that one rule is worth 0.05 of the aggregate on its own. It came out of reading
the questions the merge got wrong rather than from a theory: on those, more than
half the returned results were changelogs, documentation, manifests and tests.
Both retrievers prefer them honestly — a paragraph of prose answers a prose
question more closely than the function that implements it — and free prose is not
what a code search is for. Documentation attached to a declaration carries that
declaration's symbol and is unaffected; a repository the parser cannot read has no
declarations at all and still gets its results. The caveat to hold onto: every
label in this benchmark names a source file, so a rule preferring source is
rewarded partly by construction, and the number overstates how general it is.

The default model is a measurement, not a guess. Three models over the same 73
questions:

| model | dims | disk | vector only | merged |
| --- | --- | --- | --- | --- |
| all-MiniLM-L6-v2 | 384 | ~90 MB | 0.604 | 0.718 |
| bge-small-en-v1.5 | 384 | ~133 MB | 0.627 | **0.733** |
| bge-base-en-v1.5 | 768 | ~436 MB | **0.637** | 0.689 |

The larger model is the better retriever on its own and the worse one merged. The
ordering held before doc records were added and after, so it is not an accident of
one configuration. Why a model that ranks better alone merges worse is not
explained here, only measured; three times the disk for less accuracy settles the
default without an explanation.

Documentation is indexed as records of its own. A doc comment or docstring is
turned into a second record for the same declaration — the same file, the same
line range, so a hit still points at code — carrying the author's prose instead of
the code. It is the only change measured here that improved every corpus at once:
merged MRR 0.679 to **0.733**, and 0.681 to 0.799 on Go, the largest single move
in this table.

It did not improve what it was aimed at. The idea was that a description-shaped
query would find the sentence explaining a function; descriptions did not move
(0.551 to 0.545). What moved was identifiers (0.811 to 0.913), messages (0.870 to
0.944) and remembered names (0.607 to 0.690).

The reason is an interaction, and it took a two-by-two to see it:

| | length normalisation on | off |
| --- | --- | --- |
| without doc records | 0.679 | 0.670 |
| with doc records | **0.733** | 0.679 |

Neither on its own is worth anything — doc records add 0.009 without length
normalisation, and length normalisation adds 0.009 without doc records. Together
they add 0.054. A doc record is a *short* document that names the declaration and
says what it is for, and BM25's length term exists precisely to say that a term in
a short document is stronger evidence than the same term in a long one. The gain
is that rule finally having something to work with, not the prose being read for
its meaning.

Giving the model more context to embed was tried twice and is not in the code.
Prepending each chunk's path and declaration to the text sent for embedding —
standard advice for retrieval over code — measured 0.644 raw and 0.661 with the
identifiers spelled out as words (`url-to-options` as `url to options`), against
0.679 for the bare body. Both forms lift identifiers, to 0.835 at best; both push
descriptions down, to 0.487 at best from 0.551. The header helps the shape the
lexical arm already answers well and hurts the shape where the model is the only
arm that can answer at all, and descriptions are 28 of the 73 questions.

The same run measures what reaching the answer costs to read. Counting the
snippets up to and including the first correct hit, a caller takes in **439 bytes
to reach the answer, against 14 410 bytes for the whole file that answer lives in
— 33× less**. Read that narrowly: it is the 65 of 73 questions answered inside the
top five, and the comparison is against reading one file end to end, which is what
happens when a search can only name a filename. It is not a model of what an agent
really does, and it says nothing about the 8 questions no arm answered.

A second benchmark compares what the two `skip_generated` settings return.
Indexing the lock file put a machine-written chunk in 6 of 105 result slots, and
cost 380 chunks against 256, 484 KB against 335 KB, and 21.5 s against 14.6 s to
build. Ranking is deliberately not part of that claim: `crates.io-index` occurs
409 times in `Cargo.lock`, so indexing it hands the word `index` the document
frequency of a stopword, and one corpus cannot separate that accident from the
change being measured.

The honest gaps. A description in the reader's own words remains the weakest shape
at 0.595, and eight questions are still unanswered inside the top five. Only one
of those is a retrieval failure — a chunk no arm surfaces at any depth; the rest
are ranked somewhere below five.

Two attempts at the ranking are recorded because they failed instructively.
Ordering the complete-coverage group by BM25 lifts messages and drops identifiers
from 0.811 to 0.714, because a one-word query reaches full coverage in dozens of
chunks and the group stops meaning anything. Weighting the lexical arm by how much
of the query it could anchor did exactly what it was predicted to do — identifiers
and messages unchanged, remembered names up from 0.702 to 0.726 — and lost the same
amount on descriptions, 0.545 to 0.524. Partial coverage is evidence rather than
ignorance: a description whose words half-match is still being helped by the
lexical arm, and no measure taken from one arm alone separates that from a
paraphrase the arm cannot follow. Both were reverted; what fixed the shapes they
were aimed at was reading the failures and finding prose in the results. `models` does not find `ModelInfo` because nothing folds plurals —
a trailing-`s` rule was tried and measured worse. Weighting the declared name
above the rest of a chunk moved a single query in and out of first place and was
dropped as a bump rather than a trend.

## License

Apache-2.0
