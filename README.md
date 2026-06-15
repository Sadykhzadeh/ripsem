# ripsem

Local, offline semantic code search. Ask your codebase questions in plain
language and get back the most relevant chunks — no account, no API key, no
network. Embeddings run on a model that lives on your disk, and the index is a
single file inside your repo.

Pairs with `grep`/`ripgrep`: use those for exact patterns, use `ripsem` when you
know *what* you want but not the exact words.

## Install

```
cargo install --path . --features fastembed
```

The `fastembed` feature pulls in the local embedding runtime. Without it the
binary still builds and runs on a lightweight lexical backend (useful for tests
and CI), but real semantic search needs the model.

## Usage

```
ripsem init                       # write .ripsem/config.toml
ripsem index                      # build the index (incremental on re-runs)
ripsem search "where is auth set up?"
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
max_file_kb = 1024
exclude = []
```

## License

Apache-2.0
