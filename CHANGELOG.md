# Changelog

## 0.1.0 (unreleased)

First public release.

- Hybrid search: BM25 over code-aware tokens merged with vector similarity
  from a local embedding model, so an identifier and a plain description of
  behaviour both find the right place.
- With the `treesitter` feature, chunks follow function and type boundaries in
  17 languages and carry the declaration's name, including JavaScript and
  TypeScript functions bound to a variable or class field. Doc comments become
  records of their own.
- `ripsem mcp` serves a single `search` tool over stdio; `ripsem install
  claude-code` and `ripsem install cursor` register it.
- `ripsem status` reports index size, model, backend and drift from the tree,
  also as JSON (`ripsem/status/v1`).
- `ripsem watch` keeps the index in sync as files change.
- The index lives in `.ripsem/index.bin`, versioned and stored with bitcode. An
  index from another version or built by another backend is rebuilt by
  `ripsem index` instead of being misread. `.ripsem/.gitignore` keeps it out of
  git.
- Release binaries for Linux x86_64 (glibc 2.35 or newer), macOS arm64 and
  Windows x86_64.
