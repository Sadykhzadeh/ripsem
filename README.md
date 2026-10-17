# ripsem

[![ci](https://github.com/Sadykhzadeh/ripsem/actions/workflows/ci.yml/badge.svg)](https://github.com/Sadykhzadeh/ripsem/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Local, offline code search that answers one question: where is the code that
does this? Ask with an identifier, an error string, or a plain description of
the behaviour. One query, one ranked answer. No account, no API key, and
nothing leaves the machine. Embeddings run on a model stored on your disk, and
the index is a single file inside your repo.

Every query runs through two independent retrievers and their rankings are
merged: BM25 over code-aware tokens, and vector similarity from the embedding
model. A literal identifier match cannot be lost to semantic distance, and a
question with no shared words still finds the right function.

## Quick start

```
cd your-repo
ripsem index
ripsem search "strip the auth header when redirecting to another host"
```

On [requests](https://github.com/psf/requests) v2.34.2 that prints:

```
src/requests/sessions.py:154-184  fn SessionRedirectMixin::should_strip_auth  (0.816)
    def should_strip_auth(self, old_url: str, new_url: str) -> bool:
        """Decide whether Authorization header should be removed when redirecting"""

src/requests/sessions.py:309-332  fn SessionRedirectMixin::rebuild_auth  (0.766)
    def rebuild_auth(
        self, prepared_request: PreparedRequest, response: Response

tests/test_requests.py:1924-1960  fn TestRequests::test_should_strip_auth_https_upgrade  (0.745)
    def test_should_strip_auth_https_upgrade(self):
        s = requests.Session()
```

(`-k 3 --context 2`; the default is ten hits with three lines each.)

The first `index` downloads the model (~133 MB) and embeds the repository;
later runs re-embed only files that changed. `ripsem install claude-code` hands
the same search to a coding agent (see [below](#use-with-coding-agents)).

### Why not grep?

grep answers "where does this string occur". That is the right question when you
know the string. ripsem is for when you know what the code does but not what it
is called: a description finds the function, and an identifier or an error
message still finds its exact match, because the lexical arm sees it
literally. Each hit is a whole function or type with its name, not a matching
line, so a reader or an agent can tell a definition from a call site without
opening the file. Everything runs locally: no hosted service, no account,
nothing leaves the machine.

## Install

Every [release](https://github.com/Sadykhzadeh/ripsem/releases) has a prebuilt
binary for each platform below, built with the embedding runtime and
tree-sitter (the model itself downloads on first use), and a `SHA256SUMS` file
to check the archives against. Unpack one and put
`ripsem` on your `PATH`; on Windows, keep `DirectML.dll` next to `ripsem.exe`.

| archive | runs on |
| --- | --- |
| `x86_64-unknown-linux-gnu` | glibc 2.35, libstdc++ 12 and OpenSSL 3 or newer: Ubuntu 22.04, Debian 12 and later |
| `aarch64-apple-darwin` | macOS 13.4 or newer on Apple silicon |
| `x86_64-pc-windows-msvc` | Windows 10 or newer |

The embedding runtime in the x86-64 builds needs a CPU with AVX2 (Intel
Haswell, AMD Excavator or newer).

To build from source instead, with Rust 1.90 or newer:

```
cargo install --locked --git https://github.com/Sadykhzadeh/ripsem --features fastembed,treesitter
```

The build downloads a prebuilt ONNX Runtime and takes several minutes. It works
on the systems the release binaries run on, and needs a C and C++ compiler, plus
pkg-config and the OpenSSL headers on Linux (`build-essential pkg-config
libssl-dev` on Debian and Ubuntu).

To build from a checkout:

```
git clone https://github.com/Sadykhzadeh/ripsem
cd ripsem
cargo install --locked --path . --features fastembed,treesitter
```

The `fastembed` feature pulls in the local embedding runtime. Without it the
binary builds and runs on a lightweight hashing backend (useful for tests and
CI), but real semantic search needs the model.

The `treesitter` feature chunks code on function and type boundaries instead of
fixed line windows, and names each chunk after the declaration it belongs to.
Results look like `src/pipeline.rs:501-544 fn Searcher::query_scoped`, so a
caller can tell a definition from a call site without opening the file. A
function too large for one chunk passes its name to every piece. It covers Rust,
Python, JavaScript, TypeScript, Go, Java, C, C++, C#, Ruby, PHP, Scala, Kotlin,
Bash, HTML, CSS and JSON; other files fall back to line windows without names.

The `hnsw` feature builds an approximate-nearest-neighbour graph, but only above
500 000 chunks; below that every result is exact. The graph changes only how the
vector arm finds candidates. The lexical arm still scans every chunk and the two
are merged the same way, so ranking and `--min-score` behave identically at any
size. It rarely pays: it can only save the vector half of a query, and it is
rebuilt on every reindex that changes anything. On a 3 864-file monorepo
(51 612 chunks) a query took 2.1 ms with the graph and 2.1 ms without it, while
the graph cost 13 s on every reindex and 96 MB of a 172 MB index. On synthetic
384-dimension vectors the graph's half of a query took 2.5 ms against 0.9 ms for
the scan at 50 000 chunks, and was still no faster at 200 000 (3.5 ms against
3.2 ms), where it took 191 s to build.

## Usage

```
ripsem init                       # write .ripsem/config.toml
ripsem index                      # build the index (incremental on re-runs)
ripsem search "where is auth set up?"
ripsem status                     # index size, counts, model, drift from the tree
ripsem status --json              # the same as JSON (schema ripsem/status/v1)
ripsem watch                      # index and keep it in sync as files change
```

Like git, every command except `init` works from any subdirectory: it uses the
nearest parent that holds a `.ripsem` directory. `--path <dir>` overrides that.

The index stays out of git: `.ripsem/.gitignore` lists it, so `git status`
shows only `config.toml` (which can be committed) and that `.gitignore`. ripsem
writes the file when it is missing and never changes an existing one.

`status` also reports how far the index has drifted from the files. Hits carry
the line range their file had when it was indexed, so `tree: out of date: 3
changed, 1 new` tells you when a result may no longer point at the right lines.
Drift is measured with the same content hash the incremental index uses, so it
counts exactly what a rebuild would touch. The JSON carries it as `index.drift`
(`added`/`modified`/`removed`).

Search respects `.gitignore` and an optional `.ripsemignore`. Results print as
`path:start-end` with a snippet, in merged-ranking order. Two records can cover
the same lines (a declaration and the documentation above it, or the overlap
between adjacent windows); only the better-ranked one is returned, so `-k 5`
means five distinct places. The number shown is confidence in 0-1: semantic
similarity, or the share of the query's terms found literally, whichever is
higher. Order comes from how strongly both retrievers back a chunk, so
confidence does not have to fall down the list.

- `-k <N>` sets the number of results (default 10).
- `--json` prints machine-readable output.
- `--in <subdir>` limits results to a subdirectory (`./src/auth` and an absolute
  path into the repository mean the same; a path outside it is an error).
- `--context <N>` sets how many snippet lines print per hit (default 3; `0`
  prints just the locations).
- `--min-score <SCORE>` keeps only strong matches, in both human and JSON output.
- `--count-by-file` prints hit counts per file instead of the hits.

## Models

```
ripsem model list                 # show available models
ripsem model set bge-base-en-v1.5
ripsem index                      # rebuilds the index for the new model
```

| Model | Dims | Disk | Notes |
| --- | --- | --- | --- |
| bge-small-en-v1.5 | 384 | ~133 MB | default, and the best of the five measured |
| all-MiniLM-L6-v2 | 384 | ~90 MB | fastest, measurably lower recall |
| bge-base-en-v1.5 | 768 | ~436 MB | better alone, worse merged (see Quality) |
| multilingual-e5-small | 384 | ~470 MB | multilingual queries and comments |
| nomic-embed-text-v1.5 | 768 | ~550 MB | lowest measured, and the most memory to index |

The model downloads once on first use, then everything is offline.

## Use with coding agents

```
ripsem index
ripsem install claude-code        # writes ripsem into .mcp.json
ripsem install cursor             # writes ripsem into .cursor/mcp.json
```

`ripsem mcp` speaks MCP over stdio and exposes a single `search` tool: a
`query`, and optionally a `path` to stay inside, a `limit` (default 5) and a
`min_score`. After `install`, restart the agent in the directory and it can
call `search` directly. A running server picks up a rebuilt index by itself.
Any other MCP client takes the same entry in its own config:

```json
{
  "mcpServers": {
    "ripsem": { "command": "ripsem", "args": ["mcp"] }
  }
}
```

The server finds the index from the directory it starts in. If the client
starts it somewhere else, add `"--path", "/path/to/repo"` to `args`; `install
cursor` writes `"--path", "${workspaceFolder}"`, which Cursor expands to the
project.

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

`chunk_lines` and `chunk_overlap` size the line windows used for plain-text
files and languages without a parser. `max_chunk_lines` caps a single semantic
chunk; larger functions are split. After changing any of the three, the next
`ripsem index` chunks and embeds every file again. Files larger than
`max_file_kb` are skipped. `exclude` takes gitignore-style globs. `backend =
"hash"` matches words instead of meaning; builds without the `fastembed`
feature start with it. A key ripsem does not know is an error rather than
silently ignored.

`skip_generated` leaves out files that are committed but written by a machine:
lock files, `go.sum`, `*.min.js`, `*.min.css` and source maps. `.gitignore`
does not cover them because they are committed on purpose, and one can outweigh
everything written by hand: in this repository `Cargo.lock` alone accounted for
124 of 297 chunks. Set it to `false` to index them anyway.

## Quality

The benchmark asks 73 labelled questions across four real projects in four
languages, in four shapes: a description in the reader's own words, an
identifier typed from memory, a message seen in output, and a name remembered
wrong. Each retriever is scored against ground-truth files (MRR, top five).

```
cargo test --release --features fastembed,treesitter \
  --test retrieval_quality -- --ignored --nocapture fusion_beats_either_retriever_alone
```

Every corpus is pinned: ripsem's own source at the `corpus/v1` tag, and three
projects at release tags, cloned once into `target/corpora`. Two runs differ only
by what changed in retrieval. The first run needs network and the embedding
model, so the test is `#[ignore]`d.

| corpus | language | chunks | vector only | lexical only | merged |
| --- | --- | --- | --- | --- | --- |
| ripsem | Rust | 144 | 0.611 | 0.666 | **0.778** |
| requests v2.34.2 | Python | 909 | 0.700 | 0.610 | **0.796** |
| mux v1.8.1 | Go | 389 | 0.576 | 0.736 | **0.875** |
| got v14.4.5 | TypeScript | 842 | 0.608 | 0.542 | **0.721** |
| all | | 2284 | 0.629 | 0.628 | **0.783** |

| query shape | vector only | lexical only | merged |
| --- | --- | --- | --- |
| description (28) | 0.438 | 0.367 | **0.595** |
| identifier (22) | 0.833 | 0.856 | **0.924** |
| message (9) | 0.537 | 1.000 | **1.000** |
| recalled name (14) | 0.750 | 0.554 | **0.798** |

Which arm is stronger reverses between corpora: BM25 wins on Go and Rust, the
embedding model on Python and TypeScript. Merging is ahead on every corpus and
never behind on any query shape (messages tie with BM25 at 1.000), so nobody has
to know in advance which arm a question needs.

### Ranking code above prose

Results that belong to a declaration rank above results that do not, with one
exception: a chunk holding every informative term of the query comes first
wherever it is, so a literal string found only in a CI workflow, a config file
or a language without a parser is never lost. The rule is worth 0.05 of the
aggregate. On the questions the merge got wrong, more
than half the returned results were changelogs, documentation, manifests and
tests: both retrievers prefer prose for a prose question, and free prose is not
what a code search is for. Documentation attached to a declaration carries that
declaration's symbol and is unaffected, and a repository the parser cannot read
has no declarations and still gets its results. Every label in the benchmark
names a source file, so a rule preferring source is rewarded partly by
construction, and the number overstates how general it is.

### Choice of model

Every model in the catalogue over the same 73 questions, measured on the same
code as the tables above:

| model | dims | disk | vector only | merged |
| --- | --- | --- | --- | --- |
| all-MiniLM-L6-v2 | 384 | ~90 MB | 0.616 | 0.761 |
| bge-small-en-v1.5 | 384 | ~133 MB | 0.629 | **0.783** |
| bge-base-en-v1.5 | 768 | ~436 MB | **0.645** | 0.753 |
| multilingual-e5-small | 384 | ~470 MB | 0.609 | 0.761 |
| nomic-embed-text-v1.5 | 768 | ~550 MB | 0.580 | 0.725 |

bge-base-en-v1.5 is the better retriever alone and the worse one merged. The
ordering held before and after doc records were added, and again once
declarations ranked above prose. Why is not explained here, only measured;
three times the disk for less accuracy settles the default. Both 768-dimension
models need more memory to index: building an index of ripsem's own `src/`
peaked at 0.6 GB with bge-small-en-v1.5, 0.9 GB with bge-base-en-v1.5 and
1.8 GB with nomic-embed-text-v1.5. Every chunk is cut to 512 tokens before it is
embedded, so nomic-embed-text-v1.5's longer context goes unused.

### Documentation records

A doc comment or docstring becomes a second record for the same declaration (same
file, same line range, so a hit still points at code) holding the author's prose
instead of the code. Measured before declarations ranked above prose, it
improved every corpus: merged MRR 0.679 to **0.733**, and 0.681 to 0.799 on Go.

It did not improve what it was aimed at. Descriptions did not move (0.551 to
0.545); identifiers (0.811 to 0.913), messages (0.870 to 0.944) and recalled
names (0.607 to 0.690) did. The cause is an interaction with BM25 length
normalisation:

| | length normalisation on | off |
| --- | --- | --- |
| without doc records | 0.679 | 0.670 |
| with doc records | **0.733** | 0.679 |

Doc records add 0.009 without length normalisation, and length normalisation adds
0.009 without doc records; together they add 0.063. A doc record is a short
document that names the declaration, and BM25's length term says a term in a
short document is stronger evidence than the same term in a long one.

### Reading cost

Counting the snippets up to and including the first correct hit, a caller takes
in **446 bytes to reach the answer, against 14 410 bytes for the whole file it
lives in, 32x less**. This covers the 65 of 73 questions answered inside the top
five, and compares against reading one file end to end, which is what happens
when a search can only name a filename. It is not a model of what an agent does,
and says nothing about the 8 questions the merge does not answer inside the
top five.

A second benchmark compares the two `skip_generated` settings. Indexing the lock
file cost 297 chunks against 173. Before declarations ranked above prose it also
put a machine-written chunk in 6 of 105 result slots; now none gets through
either way, and skipping is about the size of the index. Ranking is
deliberately not part of that claim: `crates.io-index` occurs 409 times in
`Cargo.lock`, so indexing it gives the word `index` the document frequency of a
stopword, and one corpus cannot separate that from the change being measured.

### Known gaps

A description in the reader's own words is the weakest shape at 0.595, and eight
questions are still unanswered inside the top five. Every one of them is a
ranking failure: at least one arm has the answer within its first 200, and the
merge places it lower. The worst is the vector arm's fourth result for "path to
the bundled certificate authority file", which the merge drops below 200.
`models` does not find `ModelInfo` because nothing folds plurals.

### Tried and not kept

Each was measured against the code of its day, so the numbers compare within an
entry, not across entries.

- multilingual-e5-small and nomic-embed-text-v1.5 were trained with a marker in
  front of every document (`passage:`, `search_document:`) as well as every
  query. Adding the document marker to multilingual-e5-small lifted its vector
  arm from 0.605 to 0.614 and lowered the merge from 0.761 to 0.737, so neither
  model gets one.
- Prepending each chunk's path and declaration to the text sent for embedding
  measured 0.644 raw and 0.661 with identifiers spelled out as words
  (`url-to-options` as `url to options`), against 0.679 for the bare body. Both
  lift identifiers (0.835 at best) and push descriptions down (0.487 at best,
  from 0.551). Descriptions are 28 of the 73 questions.
- Ordering the chunks that hold every query term by BM25 lifts messages and
  drops identifiers from 0.811 to 0.714, because a one-word query reaches full
  coverage in dozens of chunks.
- Weighting the lexical arm by how much of the query it could anchor moved
  recalled names from 0.702 to 0.726 and lost the same amount on descriptions,
  0.545 to 0.524.
- A trailing-`s` plural rule measured worse.
- Weighting the declared name above the rest of a chunk moved a single query in
  and out of first place.

## License

Apache-2.0
