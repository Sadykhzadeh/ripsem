# Changelog

## Unreleased

- The model reads each chunk under its path and the declaration it belongs
  to. An index built before this embeds every chunk again on its next run.
- `ripsem model set potion-code-16M-v2` picks a static code model that
  indexes in seconds: 19 repositories of the semble benchmark indexed in 13 s
  instead of 1 108 s with bge-small-en-v1.5 on an M1, at NDCG@10 0.778
  against 0.769. It downloads 32 MB and runs without the ONNX runtime.
- `ripsem search` updates the index before searching, so an edit made since
  the last `ripsem index` is found where it is now. When nothing changed this
  costs a walk of the tree, about 15 ms on a repository of 700 files.
- The first `ripsem search` in a git repository builds the index instead of
  asking for `ripsem index` first.
- On Windows, indexing 256 chunks or more runs on the GPU through DirectML:
  a first index of 5 000 chunks took 45 s instead of 173 s, and used 29 s of
  CPU instead of 1 060 s. Queries and small updates stay on the CPU, where the
  model starts faster. `RIPSEM_NO_GPU=1` keeps everything on the CPU.
- `ripsem search` no longer crashes on its way out after an error, such as a
  scope with nothing indexed under it, while the model was still loading.
- `install.sh` and `install.ps1` install the latest release with one command,
  after checking it against the release's checksums.
- A search for a name lands on its declaration: typed as code or spelled out in
  words, the name puts the chunk that declares it above the chunks that only
  use it, and the hit reports that declaration with its own lines, label and
  snippet, even when it shares a chunk with its neighbours.
- Tests rank below the code they test, unless the query asks for tests.
- A score is measured from the index's noise floor, how closely text about
  anything but code matches it, so text unrelated to the repository scores
  near 0. Scores fall with rank, and `--min-score` and `min_score` keep the
  top of the ranking.
- The first three hits carry their code, up to 40 lines, and `search` prints
  whole snippets when its output goes to another program, so an agent reads
  the answer without opening the file.
- An edit embeds only the chunks whose text changed, not its whole file, in
  `ripsem index`, `ripsem watch` and the MCP server.
- `ripsem index` shows the chunks embedded so far and the time left. Output
  that is not a terminal gets a progress line every ten seconds.
- The MCP server sends instructions that tell the agent when to search, which
  reach the model in clients that load tool descriptions on demand, such as
  Claude Code.
- A server started outside a project, as a shared backend or a desktop client
  starts it, follows the client's workspace roots to the project.
- The MCP `search` tool answers arguments outside its schema with a tool error
  that names what is expected: `limit` runs from 1 to 50 and `min_score` from
  0 to 1.
- Releases list the server in the MCP Registry as
  `io.github.Sadykhzadeh/ripsem`.
- The index format changed. The first `ripsem index` after upgrading, or the
  MCP server on its first start, rebuilds it. A later change to how files are
  cut keeps the vector of every chunk whose text stays the same.

## 0.1.2 (2026-10-17)

- ripsem is on npm: `npm install -g ripsem` or `npx ripsem` runs the prebuilt
  binary for Linux x64, macOS on Apple silicon and Windows x64.

## 0.1.1 (2026-10-17)

- `cargo install ripsem` builds with the embedding runtime and tree-sitter,
  which are default features now. `--no-default-features` gives the
  lightweight build on the hashing backend.

## 0.1.0 (2026-10-17)

First public release.

- Hybrid search: BM25 over code-aware tokens merged with vector similarity
  from a local embedding model, so an identifier and a plain description of
  behaviour both find the right place. A chunk holding every term of the
  query ranks first even in a file with no declarations, such as a CI
  workflow or a config file.
- With the `treesitter` feature, chunks follow function and type boundaries in
  17 languages and carry the declaration's name, including JavaScript and
  TypeScript functions bound to a variable or class field. Doc comments become
  records of their own.
- `ripsem mcp` serves a single `search` tool over stdio and keeps the index
  current while it runs, also across a rebuild for another model. `ripsem install
  claude-code` and `ripsem install cursor` register it and add a note that tells
  the agent when to use it.
- `ripsem status` reports index size, model, backend and drift from the tree,
  as text or JSON (`ripsem/status/v1`).
- `ripsem watch` keeps the index in sync as files change, under the config as
  it is at each change.
- `ripsem completions <shell>` prints completion scripts for bash, zsh, fish,
  PowerShell and elvish.
- The index lives in `.ripsem/index.bin`, versioned and stored with bitcode. An
  index from another version, built by another backend or chunked with other
  settings is rebuilt by `ripsem index` instead of being misread or reused.
  `.ripsem/.gitignore` keeps it out of git.
- Release binaries for Linux x86_64 (glibc 2.35 or newer), macOS arm64 and
  Windows x86_64.
