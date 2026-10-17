# Contributing

Bug reports, missed searches and pull requests are all welcome.

## Building and testing

```
cargo test --features treesitter,hnsw --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all
```

The default build uses the hashing backend, so the tests above run without
downloading a model. Anything that touches the model runtime also needs:

```
cargo test --features fastembed,treesitter --all-targets
cargo test --features fastembed,treesitter --test semantic_search -- --ignored
```

CI runs all of these on Linux and the pure-Rust ones on Windows.

## Changes to ranking

A change to chunking, tokenising or scoring has to show its effect on the
retrieval benchmark (see Quality in the README):

```
cargo test --release --features fastembed,treesitter \
  --test retrieval_quality -- --ignored --nocapture fusion_beats_either_retriever_alone
```

Put the numbers before and after in the pull request, per corpus and per query
shape. A change that lifts one shape and costs another is worth reporting
too, but say so plainly. Name one benchmark per run as above: running them all
takes a long time and downloads every model in the catalogue. Corpora are
pinned, so numbers are comparable only between runs on the same corpora.

`misses` prints every question the merge answers below the top five and
whether the answer was anywhere in either arm. It is the place to start when a
number moves for no obvious reason.

## A search that missed

A search that should have found something and did not is the most useful
report there is. Open an issue with the query, the file and lines you expected,
and what came back. If the repository is public, link it and the commit, so the
query can become a benchmark question.

## Style

- Commits follow `type(scope): summary` (`fix(search): ...`,
  `perf(index): ...`). The body says why, and with measurements where there
  are any.
- Comments are rare. Write a doc comment when it says something the code
  cannot; otherwise make the code clearer instead.
- Keep a pull request to one change.
