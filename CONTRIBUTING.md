# Contributing

Bug reports, missed searches and pull requests are all welcome.

## Building and testing

```
cargo test --no-default-features --features treesitter,hnsw --all-targets
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all
```

Without default features ripsem builds on the hashing backend, so the tests
above run without the embedding runtime or a model. Anything that touches the
runtime also needs:

```
cargo test --all-targets
cargo test --test semantic_search -- --ignored
```

CI runs all of these on Linux, the pure-Rust ones on Windows, and a check on
the minimum Rust version in `Cargo.toml`.

## Changes to ranking

A change to chunking, tokenising or scoring has to show its effect on the
retrieval benchmark (see Quality in the README):

```
cargo test --release \
  --test retrieval_quality -- --ignored --nocapture fusion_beats_either_retriever_alone
```

Put the numbers before and after in the pull request, per corpus and per query
shape, and say plainly which shapes it lifts and which it costs. Name one
benchmark per run as above: running them all takes a long time and downloads
every model in the catalogue. The corpora are pinned, so two runs differ only
by what changed in the code.

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

## Releasing

Give the version's heading in `CHANGELOG.md` its date, make the version in
`Cargo.toml` match, and push a `vX.Y.Z` tag. The release workflow refuses a tag
that disagrees with either, builds and tests every archive, and publishes them
with their checksums, build provenance and that changelog section as the notes.
It then publishes the crate to crates.io and the packages under `npm/` to npm,
both through trusted publishing, so neither needs a token.
