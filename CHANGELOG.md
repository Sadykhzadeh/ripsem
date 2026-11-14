# Changelog

## Unreleased

- `ripsem search` takes `--offset` and the MCP `search` tool an `offset`, to
  read the next page of a ranking instead of raising the limit and reading
  everything again. The offset counts after the language, glob and path
  filters and `min_score`. When more results follow, a text result ends with
  `more results below this; pass --offset 5 to see them` (`offset=5` over
  MCP); `--json` prints the page as the same array of hits as before. An
  offset past the last result answers `no further matches` and is not an
  error. A search without an offset prints what it did before, plus that line.
- The MCP `search` and `index_status` tools carry a title and the standard
  annotations: read-only, not destructive, idempotent, not open-world. A
  client that asks the user before every call to a tool it knows nothing
  about can now run them without asking. The fields are hints that a client
  of any protocol revision may ignore.
- On Windows, a glob written with backslashes in `-g`/`--glob` or the MCP
  `glob` argument, such as `-g '!vendor\'`, matches as it does with slashes.
  The backslash was read as an escape, so `vendor\` was refused as an invalid
  glob and `src\*.rs` matched nothing.
- The MCP `search` tool states its default `limit` (5) in the input schema as
  `default`, where a client that fills in a form from the schema reads it,
  as well as in the description.
- An MCP `search` whose hits were all dropped by `min_score` now says so and
  how many there were, instead of answering "No matches.", which an agent
  reads as the code not existing.
- An MCP server started in a directory `ripsem init` has set up, with no index
  yet, answers a search that comes in before the first index is built with
  "the index is being built in the background" instead of telling the agent
  to run `ripsem index`, which the server is already doing.
- The MCP server has an `index_status` tool, which takes no arguments and
  returns what `ripsem status --json` prints, plus `updating` for whether the
  server is indexing right now. An agent that finds nothing it expects can see
  whether the index exists, how many files it holds and how many have changed
  since it was built.
- The index is read faster and takes less memory. A record no longer owns five
  allocations of its own: `index.bin` stores one column per field, and loading
  hands every record a range of a few shared buffers. Reading the index takes
  about 210 ms instead of 310 ms, a `ripsem index` that finds nothing to do
  about 620 ms instead of 770 ms and a `ripsem search` about 690 ms instead of
  790 ms, and a search peaks at 617 MB instead of 776 MB, on 17k files
  (169 MB, 260k chunks) with the hashing backend. The file is 3 % smaller.
  Results and scores are unchanged. An index from the previous version is
  read as it is and rewritten in the new layout by the next save, so nothing
  is embedded again.
- A `ripsem index` that finds nothing to do, and the update every `ripsem
  search` starts with, return the index as it was read instead of taking it
  apart by file and putting it back together: about 860 ms instead of
  1 020 ms for `index` and 935 ms instead of 1 060 ms for `search` on 17k
  files (169 MB, 250k chunks) with the hashing backend.
- `ripsem search` takes `--lang` to keep only hits in files of a language,
  by name or extension (`--lang rust`, `--lang toml`), and `-g`/`--glob` to
  keep only hits whose path matches a glob, where a leading `!` leaves paths
  out (`-g '!vendor/'`). The MCP `search` tool takes the same as `lang` and
  `glob`. Both are applied before the ranking is cut, so `-k 10 --lang rust`
  returns ten Rust hits when the index has them.
- Markdown is cut at its headings instead of in 40-line windows. A section runs
  from its heading to the next one, so a hit starts at the heading that says
  what the text is about, a fenced code block is not cut in half, and a section
  too long for one chunk is cut at blank lines. Each chunk is named after its
  heading, as `heading Install`. Files with no heading are cut in windows as
  before. An index built by an earlier version is cut again on the next
  `ripsem index`.
- A `ripsem index` that finds nothing to do, and the update every `ripsem
  search` starts with, take about 600 ms instead of 1 050 ms on 13.8k files
  (169 MB, 211k chunks) with the hashing backend. The size of a file comes
  from the directory listing, each file is read once to check that it is text
  and to hash it, the old index is decoded while the tree is walked, the
  index is left to the exit instead of being freed, and the terms of a chunk
  are lowered and split without allocating for plain ASCII. Indexes are
  unchanged.
- `ripsem mcp` keeps watching for changes when the first index it builds
  fails, such as when a virus scanner holds the index file open. It stopped
  for the rest of the session, which went on answering from an index that was
  missing or out of date. `ripsem watch` still exits on that error.
- A file saved as UTF-32 with a byte order mark is read as text. It was
  taken for UTF-16, so every character was followed by a NUL and nothing in
  it could be found. Run `ripsem index --force` to read such a file again.
- `ripsem install codex` adds the server to `.codex/config.toml` for Codex CLI,
  keeping the rest of that file, and the note on when to use it to
  `AGENTS.md`. Codex reads a project's config only when the project is
  trusted.
- `ripsem install opencode` adds the server to `opencode.json` for OpenCode,
  keeping the other settings there, and the note on when to use it to
  `AGENTS.md`.
- `ripsem install gemini` adds the server to `.gemini/settings.json` for
  Gemini CLI, keeping the other settings there, and the note on when to use
  it to `GEMINI.md`.
- `ripsem install vscode` adds the server to `.vscode/mcp.json` for GitHub
  Copilot in VS Code and the note on when to use it to
  `.github/copilot-instructions.md`.
- A hit printed in a terminal shows the lines that name the query's words
  when they come after its first three lines. It always showed the first
  three, often a signature or a doc comment above the code that matched.
- A hit below the first three, whose snippet is 400 bytes of its code, starts
  that snippet where the query's words are when they come later in the
  chunk. A long function matched near its end showed only its start.
- A hit in a piece of a line too long for one chunk, such as a minified
  bundle, shows about 400 bytes around the query's words instead of the first
  400 bytes of the piece.

## 0.2.0 (2026-11-01)

- A doc comment or attribute stays in the chunk of the declaration below it.
  Where a chunk filled up between the two, the comment went to the end of the
  chunk before, so a hit on `measure_noise_floor` ended with the doc comment of
  the next method and the method's own chunk started without it. An index
  built by an earlier version is cut again on the next `ripsem index`.
- The model runtime is fastembed 7.5 on ONNX Runtime 1.31 (ort 2.0.0-rc.15).
  Building from source needs Rust 1.92 or newer, up from 1.90.
- An `exclude` pattern written with backslashes on Windows, such as
  `vendor\third_party`, matches that directory. The backslash was read as an
  escape, so the pattern matched nothing.
- A hit inside a line longer than 2 KB shows the piece that matched. It showed
  the start of the line, which is not where the match is.
- A line longer than 2 KB, as in a minified bundle or a one-line JSON file, is
  cut into pieces that each get their own chunk and embedding. Only the
  first 2 KB of such a file could be matched by meaning, and a hit showed
  the start of the line rather than the match.
- A UTF-8 byte order mark at the start of a file, as Windows editors write it,
  is no longer part of the first line of the indexed text, and a file of
  nothing but the mark is skipped as blank.
- A stretch of a source file the parser could not make sense of is cut into
  chunks like any other text. Lines after an error were left out of the index
  and could not be found.
- A `chunk_lines`, `max_chunk_lines` or `max_file_kb` of 0 in `config.toml` is
  an error. `max_file_kb = 0` indexed nothing and said nothing.
- An `exclude` pattern that starts with `!` is an error that points to
  `.ripsemignore`, where a negation works. It used to have no effect.
- The MCP server answers a request whose `id` is not a string or an integer,
  or whose `jsonrpc` is not `"2.0"`, with an invalid-request error.
- A failed write of the index removes its temporary file.
- `install.sh` with a relative `RIPSEM_INSTALL_DIR`, such as `bin`, installs
  there instead of into its temporary directory, which it then deleted while
  reporting `installed ripsem v0.1.2 to bin/ripsem`.
- The first `ripsem search` after `ripsem init` builds the index, as it does
  in a git repository, instead of answering "no index yet".
- `ripsem watch` and the MCP server reindex when `.ripsem/config.toml`
  changes. A new `exclude`, `max_file_kb` or `skip_generated` took effect
  only once some other file changed.
- `ripsem search` takes a query of several words without quotes:
  `ripsem search where is auth set up` was refused with "unexpected
  argument 'is' found".
- `search --in` reads a relative path from the current directory, as the
  shell does. Run from `src`, `--in auth` was read from the repository root
  and answered "nothing is indexed under 'auth'", and `--in .` searched the
  whole repository; they now mean `src/auth` and `src`.
- `ripsem index --dry-run` leaves out files with nothing but whitespace in
  them, which the index never takes in, and the file counts of `ripsem index`
  and `ripsem watch` agree with `ripsem status`: a repository with one blank
  file read `26 files: 0 changed, 25 unchanged` while the index held 25.
- The model that answers a search skips packing its weights for repeated runs,
  which one short query never earns back: a search took about 37 ms less
  (586 ms to 549 ms median over 25 searches, with the machine busy). The
  ranking is the same and scores differ by less than 1e-6.
- `RIPSEM_MODEL_DIR` sets where the embedding model is kept, on every system.
  When the model cannot be loaded, the error names it, the directory it was
  looked for in and `ripsem model backend hash` as the way to search without it.
- On Windows, `ripsem watch` and the MCP server reindex once for a change instead
  of twice: the late notice that the parent directory was modified no longer
  counts as a change.
- `ripsem install` adds its entry to `.mcp.json` or `.cursor/mcp.json` as text,
  so the other servers keep their order, indentation and line endings instead
  of being rewritten with sorted keys.
- On Windows, `--in` and the MCP `path` argument ignore the case of ASCII letters,
  as the filesystem does: `--in SRC` finds what is under `src`.
- `--path` naming a file is an error instead of treating the file as the
  project directory.
- `ripsem search` refuses `-k 0` and a `--min-score` outside 0 to 1 with an error,
  as the MCP tool already does, instead of answering "no matches".
- Output piped into a program that stops reading, such as `ripsem search | head`,
  ends quietly instead of panicking with "failed printing to stdout".
- Releases include a Linux build for 64-bit Arm (`aarch64-unknown-linux-gnu`),
  and `install.sh` installs it.
- A search no longer reads a file outside the repository when the index on
  disk names one. A repository can ship its own `.ripsem` index, and a record
  pointing at `../` or an absolute path was re-read for its snippet.
- `ripsem index` cuts files into chunks on every core instead of one. A first
  index of 10 300 files (137 MB, 147 000 chunks) took 12.5 s instead of 52 s on
  the hashing backend, and the index file is byte for byte the same.
- The walk over the tree runs on several threads, and files are indexed in path
  order. On 10 300 files it took 0.4 s instead of 1.2 s, which every `ripsem
  search` and `ripsem index` pays before anything else.
- `ripsem search` searches the index its update ends with instead of reading
  and decoding the file once more, about 1 ms per MB of index saved.
- The build leaves out fastembed's image models, which ripsem never used:
  71 crates fewer to compile, among them the image, AVIF and EXR codecs.
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
