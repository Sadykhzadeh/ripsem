# Retrieval benchmark

Measures how often `ripsem search` puts the right place in its first results,
against four real projects, and compares it with its own halves and with
ripgrep. It drives the real binary through `search --json`, so the numbers
include everything a user or an agent pays: process start, the update of the
index, loading the model, ranking, snippets.

It needs Python 3.13 (standard library only), git, a release build of ripsem,
and ripgrep on `PATH` (or `--rg`) for the keyword floor. It runs on Windows and
Linux. The ripgrep timings include whatever launches `rg`, so they only say
that a grep-style search starts and finishes in about that time on your machine.

## Run it

```
cargo build --release
cargo build --release --manifest-path eval/arms/Cargo.toml

python eval/bench.py fetch    --work ~/ripsem-eval
python eval/bench.py prepare  --work ~/ripsem-eval
python eval/bench.py validate --work ~/ripsem-eval
python eval/bench.py index    --work ~/ripsem-eval
python eval/bench.py run      --work ~/ripsem-eval
python eval/bench.py report   --work ~/ripsem-eval
```

`--work` is where corpora, indexes and results go; keep it outside the
repository. `--ripsem` and `--arms` name the two binaries when they are not
where the defaults look, `--repo axum` limits a step to one corpus, and
`--systems`, `--sets` and `--backends` take comma-separated lists. Indexing
the four corpora with the model takes a few minutes on a GPU (ripsem uses
DirectML on Windows) and much longer on a CPU; `RIPSEM_NO_GPU=1` keeps the
build on the CPU if the vectors have to match another machine's. The model
downloads once, as for any use of ripsem.

`report` ends with a digest of every quality number. Two runs of the same
commands on the same machine print the same digest; timings are left out of it.

```
python eval/bench.py failures --system fastembed --set curated
python eval/bench.py failures --system fastembed --set auto --sample 10
```

lists the misses of one system with what is known about each, for sorting them
into classes by hand.

```
python eval/bench.py ranks --system fastembed --set curated
```

writes, for every query of a set and not only for the misses, the position of
the answer in the vector ranking, the lexical ranking and the merged ranking
(`results/ranks-<backend>-<set>.json`), and prints how the queries the merge
misses are spread: answer at merged 11 to 50, further down, or in the top
three of exactly one arm. The merge there is the one a search for 200 results
makes, which draws its candidates deeper than a search for 10, so its
positions can differ a little from the ones in `report`.

## Corpora

`corpora.json` pins four projects by commit: axum (Rust), scrapy (Python),
nest (TypeScript) and caddy (Go). `fetch` clones them shallowly at those
commits and never builds or runs them. The copy that is indexed has no `.git`.

## Query set 1: automatic

For every documented function or method outside tests, examples and vendored
code, the query is the first sentence of its doc comment (a docstring, `///`,
`/** */` or a `//` block) and the answer is that function's lines. Comments
are read from the formatted layout of each language; Python goes through `ast`.

- Skipped: sentences under 5 words or over 40, sentences that only spell out
  the function's own name, and any sentence that occurs twice.
- Up to 300 per corpus, drawn with a fixed seed (`SEED` in `bench.py`).
- The doc comments of the chosen functions are cut out of the copy that gets
  indexed (the `strip` tree), so the query text is not in the code. Comments
  of all other functions stay, as in a real repository, and the answer's line
  range is given in the stripped file's numbering.
- Some sentences still contain the function's name (in Go the first word of a
  doc comment is the name). `report` splits the table by that.

The file `queries/auto-<name>.jsonl` is written to the work directory; its
hash is printed by `prepare`.

## Query set 2: curated

`queries/<name>.jsonl` holds about 25 queries per corpus, written by hand
against the pinned commit by reading the code, before any search was run. They
ask the way someone asks who does not know the identifiers, and name no
identifier of the answer. A few use ordinary words that also occur in the
answer's code (`header`, `path`); the check is by eye, not by machine. Each has
the file and the line range of the code that answers it. They are searched in
the original, unmodified tree. `validate` checks every range against the tree.

## Systems

| name | what runs |
|---|---|
| `hash`, `fastembed` | `ripsem search --json -k 10` with that backend (default model for fastembed) |
| `*-lexical`, `*-dense` | one arm of the index on its own, through `eval/arms`, which calls the same library functions the search calls |
| `ripgrep` | the query's content words (stop words dropped) OR-ed, case-insensitive; files ranked by number of matching lines, restricted to the files ripsem indexed |

`eval/arms` is a separate crate (its own `Cargo.toml` and lock, outside the
ripsem package) that links ripsem as a library. It adds nothing to the product.
The arms are ranked by record, with records of identical lines counted once.

## Metrics

For each query the result list is the first 10 hits as the system returns
them (ripsem returns distinct places, ripgrep distinct files).

- region level: a hit counts when it is in the answer's file and its line
  range overlaps the answer's. For ripgrep a file counts when one of its
  matching lines is inside the answer.
- file level: a hit counts when it is in the answer's file.
- MRR@10 and recall at 1, 5 and 10, from the first hit that counts.
- Latency is the wall time of one `search` process, start to exit, over every
  query of both sets, run one after the other on a warm file cache. Peak RSS is
  read from the operating system for each process. Index time is the median of
  repeated cold builds (`--repeats`, default 5).

## Failure analysis

`failures` writes, for each miss of a system, where the answer's records sit in
the vector, lexical and merged rankings (down to 200), how many records cover
the answer and how much of it the best one covers, what ranked first, and a few
text features of the answer. The classes are assigned from that and checked by
reading the miss.
