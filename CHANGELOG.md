# Changelog

## Unreleased

- The MCP `outline` and `symbol` tools answer from the index the session holds,
  as `search` does, instead of reading and decoding the index on every call. A
  file or tree that changed on disk is still read again before the answer.
  Before, an `outline` call took about 170 ms and a `symbol` call about 190 ms
  on an index of 96,000 chunks; now they take under 1 ms and about 30 ms.
- `ripsem symbol`, `ripsem outline` and the MCP `symbol` and `outline` tools
  free the index they read for each answer. Before, the index was left to the
  process exit, so a running MCP server grew by a whole index in memory with
  every name an agent looked up.
- `install.ps1` says so when the release's `SHA256SUMS` has no line for the
  archive, as `install.sh` does. Before, it reported that the checksum did not
  match.
- `ripsem install` adds its note to a CRLF file, such as `CLAUDE.md`, and its
  table to a Codex `config.toml` with CRLF line breaks, as the JSON files
  already get. Before, the added lines ended in LF, so the file had mixed line
  endings.
- `ripsem symbol` takes the filters of `search`: `--in` for a directory,
  `-t`/`--lang`, `-g`/`--glob` and `--since`. A name declared in many places
  can be narrowed to one of them. Before, every definition in the index was
  listed, and a filter had to be applied by reading the output.
- The MCP `symbol` tool takes the same `path`, `lang`, `glob` and `since`
  arguments as `search`, so a client can narrow a name to one directory or
  language. Before, an argument other than `name` was refused as unknown.
- The MCP tools write a control character in a file, its name or a declaration
  as an escape, as the command line does, so a repository holding terminal
  control sequences cannot reach an agent's terminal through a tool result.
  Before, the text was returned as the file held it.
- `ripsem outline`, `ripsem symbol`, `ripsem index --dry-run` and
  `ripsem status --verify` write a control character in a file name or a
  declaration as an escape, as `search` already does. Before, a file named
  with a control character was printed as the terminal would act on it.
- The MCP `outline` tool, given a `path:line` or `path:start-end` such as a
  search hit's header, says that it takes a file and names the file to send.
  Before, the same string was reported as a path that is not in the index.
- `ripsem install roo` sets up Roo Code: it adds the server to `.roo/mcp.json`
  and writes a note on when to search into `.roo/rules/ripsem.md`, as the other
  install targets get theirs. Before, Roo Code had to be configured by hand.
- `ripsem symbol <name>` lists where a declaration with that name is defined,
  in every file of the index, with its line range, and `--json` prints the same
  as an array. A bare name matches the last part of a qualified name, so
  `refresh` finds `Session::refresh`. Before, finding a definition meant a
  search, which ranks the places that use a name along with its declaration.
- The MCP server has a `symbol` tool, which answers `ripsem symbol` for MCP
  clients: the files and line ranges of the declarations with a name. Before, a
  client that knew a name had to search for it and read the hits.
- The MCP server has an `outline` tool, which lists the declarations of one
  file with their kinds, names and line ranges, as `ripsem outline` does. A
  client can see a file's layout before it reads any of its lines. Before, the
  outline was only on the command line.
- A file that ripsem replaces, such as `CLAUDE.md` or `.ripsem/config.toml`,
  takes the temporary files that a killed write left beside it with it on
  the next write, once they are an hour old. Only names of that form are
  removed. Before, each killed write left a copy of the file behind for good.
- `ripsem index --dry-run` counts an empty file among the blank files it
  leaves out, so the files seen add up to the files in the tree. Before,
  empty files were not counted anywhere.
- An index that cannot be read is built again from the files, and the run
  says why: `ripsem index`, `watch` and the refresh that a search makes
  print `rebuilt the index, which could not be read` with the reason. Before,
  a corrupt index was discarded without a word, and a refresh that rebuilt it
  said it had updated the index.
- An index build saves what it has embedded at most once a minute, and on a
  large index less often: each save is followed by a wait of thirty times its
  own length. An interrupt such as Ctrl-C keeps the files already done and the
  next run embeds only the rest. Before, the index was written only at the end,
  and an interrupted first build lost all of its embedding work.
- The next index save removes the temporary files that an interrupted save left
  in `.ripsem`, once they are an hour old. Before, each killed save left a copy
  of the index behind for good.
- `install.ps1` gives up on a download that stops answering, with the same
  limits as `install.sh`. Before, a stalled download hung the install with no
  output.
- `ripsem init` that cannot write its config no longer leaves an empty
  `.ripsem/.gitignore` behind. Before, the empty file stayed, so the index was
  not ignored by git from then on.
- A file that ripsem writes, such as `CLAUDE.md`, `AGENTS.md`, `.mcp.json` or
  `.ripsem/config.toml`, is replaced through a new temporary file beside it that
  is then renamed over it, so a write that fails part way, on a full disk or
  after an interrupt, leaves the file as it was. Before, the file was truncated
  first, and a full disk left a `CLAUDE.md` holding only the part that was
  written. A link is still written through, and a read-only file is still
  refused.
- A model cache directory that cannot be created is named in the error. Before,
  the failure was reported as a download that needs the network.
- `install.sh` gives up on a download that stops answering: a connection that
  does not open within 30 seconds, or a transfer that moves less than a byte a
  second for a minute, fails with curl's message. Before, a stalled download
  hung the install with no output.
- The npm `ripsem` command ends with the signal that ended the binary, so a
  search killed by a signal is not reported as status 1, which `search -q`
  means as no match.
- A `--glob` or MCP `glob` that does not parse is reported once, as
  `invalid glob 'src/[a': unclosed character class; missing ']'`. Before, the
  pattern was quoted twice in the message.
- `install.sh` says so when the release's `SHA256SUMS` has no line for the
  archive, and installs nothing. Before, it exited with status 1 and no message.
- A `--lang` value with an entry that names no language, such as `rs,,py`, is
  refused with that entry named. Before, the error quoted the whole value,
  which made `rs,,py` look like a language that does not exist.
- `search --offset` given a value that is not a whole number says that it must
  be a whole number, 0 or more, as `-k` does. Before, `--offset 1e3` was
  answered with the parser's own text, `invalid digit found in string`.
- `ripsem model list` lines up its columns under the longest model id. The
  `jina-embeddings-v2-base-code` id is four characters longer than the width
  the columns were set for, so its dimensions and size sat four columns out of
  line.
- `ripsem outline` given a line range, as `src/auth.rs:40-72` appears in
  search output, says that it takes a file and names the command to run.
  Before, it reported the whole argument as a path that was not in the index.
- A plural acronym such as `URLs`, `IDs` or `APIs` is one word to the lexical
  search. Before, `getURLs` was split into `get`, `UR` and `Ls`, so a query for
  `urls` did not find it. An index built before is cut again on the next
  `ripsem index`.
- A blank entry in `exclude`, or `./` alone, is an error that names it. Before,
  `exclude = [""]` and `exclude = ["./"]` matched every path, so the index came
  out empty with no message.
- `--lang md` and `--lang markdown` find `.md`, `.markdown` and `.mdx` files
  alike, and `--lang yml` finds `.yml` and `.yaml` files. Before, a name that
  was not a listed language matched only the one extension it spelled, so
  `--lang markdown` skipped `README.md`. A value with a leading dot, such as
  `--lang .md`, still matches only that extension.
- `--lang` takes a leading `!` to leave a language out, as `--glob` takes one
  for paths, so `--lang '!json'` skips JSON files. The MCP `lang` argument
  takes it too.
- Protocol buffer output (`*.pb.go`, `*.pb.gw.go`, `*_pb2.py`, `*_pb2_grpc.py`,
  `*.pb.h` and `*.pb.cc`) is left out with `skip_generated`, which is on by
  default, as the lock files and minified bundles already are. These files are
  long and machine-written, and their names filled results. An index built
  before drops them on the next `ripsem index`.
- `ripsem outline <path>` lists the declarations the index holds for a file,
  in the order they start, as `start-end  kind name` lines or as a JSON array
  with `--json`. It reads the index; a file changed since the index was built
  is read again first, and no model runs unless that file has to be embedded.
- A `.ripsem` that is a symbolic link, or anything other than a directory, is
  refused with an error. Before, a repository that shipped `.ripsem` as a link
  had its index and configuration written to the directory the link named.
- `ripsem model set` and `ripsem model backend` change one setting and leave
  the rest of `.ripsem/config.toml` as it was, comments included. Before, they
  rewrote the whole file from the settings they read, and dropped every comment
  a user had written in it.
- A glob that starts with `./`, in `exclude` or in `search --glob`, is taken
  from the project root, as a glob that starts with `/` is. Before, the `./`
  was dropped, so `./vendor` excluded every directory named `vendor`, nested
  ones included. A `vendor` excluded in the same way is unchanged.
- A query written in Japanese or Chinese finds the code and comments that use
  the same characters. Scripts written without spaces, such as Han, Hiragana
  and Katakana, were one word to the lexical side, so `日本語` matched only a
  run that was exactly that word. Such a run is now indexed as pairs of
  neighbouring characters, so a query of two or more characters finds it inside
  a longer run. A single character is found only where it stands alone. The chunking revision moves from 19 to 20, so the
  first `ripsem index` after upgrading reads every file once.
- `.ripsem/config.toml` that is a symbolic link, or anything other than a file,
  is refused with an error instead of being followed. Before, a link to a file
  outside the project was read, and a TOML error quoted one of its lines, and
  `ripsem model set` wrote the new settings into that file.
- Control characters in a file, or in a file name, are printed as escapes such
  as `\u{1b}` in the text output of `search`, `similar`, `index` and `watch`, so
  a file cannot clear the screen, set the title or write the clipboard of the
  terminal that shows a hit. A tab is printed as it is. `--json` and `-l -0`
  print the text as before.
- A file whose name is not UTF-8 is left out of the index and named with the
  unreadable files in the warning from `ripsem index`. Before, it was indexed
  under a name with replacement characters that no search could open, so its
  hits were dropped from every result.
- `ripsem watch` and `ripsem mcp` keep the index current when a directory in
  the tree cannot be read. The directories that can be read are still watched,
  and a line on stderr names the first one left out. Before, one unreadable
  directory ended the watch, and an MCP server went on answering from an index
  that no longer followed the files. The watcher no longer follows a symbolic
  link, so a tree a link points to outside the repository is not watched.
- `ripsem search --color <auto|always|never>` colours the path and the line
  numbers of each hit in a terminal, and the query's words in its snippet.
  `auto`, the default, leaves piped output plain, and a `NO_COLOR` set to
  anything but an empty string turns colour off.
- The MCP `search` and `find_similar` tools take a `since` git revision, as
  `--since` does, so an agent can ask for hits in the files a branch changed.
- `ripsem init` in a subdirectory of a project that already has an index warns
  on stderr that the new `.ripsem` hides the parent's index from commands run
  there. The index is still created.
- `ripsem search` names the unreadable files its index refresh skipped, the
  same warning `ripsem index` prints, once per run, also when the search reads
  a changed file again. `-q` keeps it quiet, and the MCP server does not print it.
- `ripsem callers <NAME> [PATH...]` lists the calls to a function or method in
  the indexed files, as `path:line:column` with the function that holds each
  call. The calls are read from the syntax tree, so a name in a string or a
  comment is not one. It takes the output options of `search`. Rust, Python,
  JavaScript, TypeScript, Go, Java, C#, C, C++ and Ruby are read. The MCP
  server offers the same search as the `find_callers` tool.
- `ripsem similar <path>[:LINE[-LINE]]` finds the code nearest to a file, or to
  a line range of it, from the vectors the index already stores, so no model
  runs. The chunks the location covers are left out of the answer. The MCP
  server offers the same search as the `find_similar` tool.
- `ripsem search` takes more options. `-q` prints nothing and sets the exit
  status to 0 when a hit is left and 1 when none is. `-l` lists each file that
  has a hit once. `--sort path` orders a page by file and then by line, and
  `--since <rev>` keeps only the hits in files that differ from a revision, as
  the working tree has them.
- `ripsem search ... -l -0` ends each listed path with a NUL byte instead of a
  line break, for `xargs -0` and paths with spaces or newlines.
- `ripsem search --vimgrep` and `ripsem similar --vimgrep` print one
  `path:line:column:text` line per hit, for Vim's quickfix list and grep tools.
- `ripsem search` takes ripgrep's short names for two options: `-c` for
  `--count-by-file` and `-t` for `--lang`, which also accepts `--type`.
- `ripsem index` names the files it could not read: a warning on stderr says
  `skipped 2 unreadable files: a.rs, b.rs, and 3 more` (the first three), and the
  exit status is unchanged. The files are still left out of the index.
- `ripsem similar src/a.rs:abc` says what a line range looks like, `PATH:LINE`
  or `PATH:LINE-LINE`, when the path is not in the index, since the colon was
  read as part of a path.
- `ripsem similar src` says that `src` is a directory and `similar` takes a
  file, instead of saying it is not in the index.
- `ripsem search -l --json` prints a JSON array of the file paths, as strings.
  `--help` now says so, and a test pins the output.
- The README has an environment variables table: `RIPSEM_HASH_ALL`,
  `RIPSEM_MODEL_DIR` and `RIPSEM_NO_GPU`, with defaults and effects. A test fails
  when the table and the variables the source reads differ.
- The README has a JSON output section with a table of every field that
  `search`, `search --count-by-file`, `similar` and `status` print with
  `--json`, and a test fails when a table and the output differ.
- `-h` prints the short summary of each command and `--help` adds an examples
  section, for every top-level command. The one-line summary of
  `ripsem similar` in the top-level help is short enough to read.
- The exit codes are written down in the README: 0 for a run, 1 for a failure
  while running (and for a `-q` search with no hit), 2 for a command line that
  is not accepted. The codes did not change.
- `ripsem search --since <rev>` outside a git repository says only that the
  directory is not in one. It used to repeat git's own `fatal:` line after it.
- `ripsem install opencode` is accepted as the README names it; `open-code`,
  the name the subcommand had, still works.
- The query is read from stdin with `ripsem search -`, or when `search` gets no
  words and stdin is not a terminal. One trailing line break is not part of it.
- A file that is not UTF-8 is read as Windows-1252, so `café` in a file saved in
  a Western code page is searchable by its word. A file that is mostly UTF-8
  with one stray byte keeps its characters, and only that byte becomes U+FFFD.
- UTF-16 without a byte order mark, as some Windows tools write `.ps1`, `.reg`
  and `.rc` files, is read as text instead of being skipped as binary.
- A C or C++ function is named by its declarator. A function that returns a
  named type was labelled after that type, as in `fn Point` for
  `Point make(int)`.
- `.rake`, `.gemspec`, `.pyw`, `.zsh`, `.ipp` and `.tpp` files are cut into
  declarations like the other languages they belong to, instead of into line
  windows.
- A minified or generated bundle, a file of at least 8 KiB whose lines average
  4 KiB or more, is left out of the index, as a binary file is.
- An `exclude` or `-g` glob that starts with `./` is anchored at the root, as
  a glob without it already was. Before this, `./vendor` matched nothing.
- A `model` in `config.toml` that the catalogue does not hold is refused when the
  config is read, and the message names the file and the key. Before, it was
  indexed under its own name at a width of 384.
- `ripsem status` says when the index was built, in a `built:` line.
- `ripsem index --dry-run` says why each file it reaches was left out. Under
  the list of files that would be indexed it prints a line such as `5 files
  seen, left out: 1 binary, 1 minified, 1 over max_file_kb, 1 excluded by
  pattern`, so a file missing from the list has a reason. A normal run and
  `status` are unchanged.
- In a build with the `hnsw` feature, an index of more than 500 000 chunks has
  its nearest-neighbour graph decoded by the first query that needs vectors,
  not by every search. On the hashing backend a query the words answer alone
  does not decode it. Measured on 766 767 chunks, a cold search went from a
  minimum of 2559 and 2332 ms to 1758 and 1775 ms.
- The chunking revision moves from 12 to 19. The first `ripsem index` after
  upgrading reads every file once, and reuses the stored vectors for chunks
  whose text did not change, so only the chunks that changed are embedded
  again.
- The MCP `search`, `find_similar` and `index_status` tools return their results
  as `structuredContent` and advertise an `outputSchema` to clients of MCP
  revision 2025-06-18 and later. Older revisions get the same text as before
  and no new fields.
- A piece of a line too long for one chunk is read only from a file inside the
  repository, and only when the file still matches its hash, as the other
  snippets are. An index that named a path outside the root could make a search
  show a stretch of that file.
- A `chunk_overlap` written in the config that is not below `chunk_lines` and
  `max_chunk_lines` is refused when the config is read. The walk stepped back
  one line at a time and joined a window for every line, so a config file in a
  repository could make indexing that file quadratic. An overlap left at its
  default is instead scaled down to half of a small window, so a window such as
  `max_chunk_lines = 5` still works.
- The MCP server refuses a message whose `jsonrpc` member is missing, not only
  one that names another version. JSON-RPC 2.0 requires it, and the server
  answered such a request as if it were valid.
- A `backend = "fastembed"` in `config.toml` in a build without the model
  runtime is reported with the config file's path, as the other config errors
  are, instead of the bare backend message.
- A nearest-neighbour graph in the index that a search cannot use is reported
  once, with the `ripsem index --force` fix, and no panic message is printed.
  Later queries in the same run go straight to the exact scan instead of
  trying the graph again.
- `ripsem search` no longer prints the old text of a file rewritten with the
  same size and modification time. Each hit's file is checked against the hash
  the index holds before the hit is shown. A file that differs is read again,
  and the search is answered once more from the index as it now stands. The MCP
  server leaves such hits out until a CLI search or `ripsem index --force` reads
  the file.
- `ripsem status --verify` reads every indexed file and lists the ones whose
  content no longer matches the index, so a rewrite that kept its size and
  modification time shows up. Plain `status` still judges the tree by stat, and
  the exit status is unchanged. With `--json` the list is `changed_on_disk` under
  `index`, and only when `--verify` is given.

## 0.3.0 (2026-11-15)

- A `ripsem index` that finds nothing to do, the update every `ripsem search`
  starts with, and `ripsem status` (which the MCP `index_status` tool shares)
  no longer read and hash every file. `index.bin` keeps each file's size and
  modification time, and a file that still has both is taken at the hash
  stored for it, the way git trusts its index. A binary or blank file is
  remembered too, until the chunk settings change. A no-op `index` takes
  about 359 ms instead of 465 ms, a `search` 368 ms instead of 483 ms and
  `status` 437 ms instead of 589 ms, on 19k files (86 MB, 141k chunks) with
  the hashing backend (medians of 12 interleaved runs), and the 86 MB the
  check read are no longer read. Results and scores are unchanged. The
  trade-off: a file rewritten with different contents, the same size and the
  same modification time is not seen as changed, by `search` or by `status`.
  Set `RIPSEM_HASH_ALL=1` for a run, or run `ripsem index --force`, to read
  every file. A file written in the last two seconds before a run, or dated in
  the future, is read again on the next one. An index without these stats
  reads every file once to store them.
- The hashing backend ranks by the words of the query alone. Its vectors only
  record which words occur, so they ranked almost at random and still got half
  the vote, which put the merged ranking below its own lexical arm. The
  lexical arm now decides, and the vectors only place records no word of the
  query matched, below all that one did. Scores keep their meaning, and
  nothing in an existing index changes. In the benchmark in `eval/` the region
  MRR@10 of `backend = "hash"`, averaged over the four projects, goes from
  0.152 to 0.322 on the hand-written queries (the lexical arm alone: 0.269)
  and from 0.271 to 0.369 on the automatic ones (lexical alone: 0.337); those
  numbers are for this change alone, on the chunking before the changes
  below, which bring the hashing backend to 0.326 and 0.349. The model
  backend is not affected.
- `jina-embeddings-v2-base-code` (Apache-2.0, 768 dimensions) is a model to
  choose with `ripsem model set`: the one model offered that was trained on
  code, 30 languages of it. It takes queries and code with no prefix. On the
  benchmark in `eval/` it beats the default on both query sets: region MRR@10
  of the merged ranking 0.464 against 0.425 on the 1,043 automatic queries and
  0.493 against 0.397 on the 100 hand-written ones (means over four projects,
  with the chunking and the embedded line of this release; pooled differences
  +0.043, 95% bootstrap interval +0.032 to +0.054, and +0.096, +0.040 to
  +0.158), and of the dense retriever alone 0.649 against 0.478 and 0.493
  against 0.297. It costs a 644 MB download against 128 MB, an index that takes
  about twice as long on a GPU (1.3 to 3 times over two runs on this machine,
  which was not equally quiet in both), 940 MB of memory while indexing on a
  GPU (default 460 MB) and 890 MB for a search (default 240 MB), which takes
  about 0.75 s at the median against 0.4 s. The default model does not change. Switching
  models embeds the whole index again, as for any model.
- A chunk is embedded with its path on a line above its code, so a question
  that names the module (`routing`, `session`) finds the vector of code that
  never says so itself. Only the embedding input changes: the snippet, the
  stored text and the keyword terms are what they were. On the benchmark in
  `eval/` (default model, four corpora) the dense retriever alone goes from
  0.261 to 0.305 region MRR@10 on the automatic queries and from 0.269 to
  0.300 on the hand-written ones, and the merged ranking from 0.369 to 0.391
  and from 0.383 to 0.414 (for this change alone, on the chunking before the
  changes below). Pooled over all queries the merged gain is +0.023
  (95% bootstrap interval +0.013 to +0.034) on the automatic set and +0.031
  (-0.004 to +0.073) on the hand-written one, whose 100 queries are too few to
  call it. Indexing takes a few percent longer (median of three cold builds per
  project on a GPU: 21.4 s against 20.7 s on axum, 101.7 s against 89.9 s on
  nest; 75 s against 69 s on a CPU, axum). After the upgrade `ripsem status` reports that the chunk settings
  differ from the index's, and the first search or `ripsem index` embeds every
  chunk again, as it does for a new chunking revision: about as long as a
  first index of that repository.
- Fewer records of one or two lines that are not answers. On the four projects
  of the benchmark in `eval/` the previous change left 912 of the 11,430
  top-ten places of the doc-comment and hand-written queries (8.0%, default
  model) to records of one or two lines, 29 of them the right place. By kind,
  of 10,231 such records: 5,234 were the doc-comment record of a declaration
  (an enum variant, a field, a constant), 2,551 were a single comment line,
  because a long comment above a declaration was cut line by line, 1,025 the
  few lines that open a block too long for one chunk (`describe('suite', () =>
  {`, a signature) or close one, 481 a constant, alias or type on one line, 359
  the tail of a split function, and 90 an import or `mod` line. A comment line
  is no longer taken for the lead of the comment below it, so a long comment is
  cut by size; the opening lines of a block too long for one chunk go into its
  first piece, and the closing lines into its last; a fragment of two lines or
  fewer between two declarations joins the one that follows it, or the one
  before it when only a comment cut off by a blank line is left; and a
  declaration of under three lines that is not a function no longer gets a
  record for its doc comment. A function of one or two lines keeps its record.
  Together with the previous change the projects hold 40,853 records instead of
  35,382 (+15%; the previous change alone gave 50,527), and their indexes are
  46.6 MB against 39.9 (51.7 without this change). Records of one or two lines
  fill 673 top-ten places (5.9%), 28 of them the right place. Region-level
  MRR@10 with the default model, with the path alone on the embedded line, is
  0.423 and 0.374 (0.423 and 0.388 before;
  the second difference is -0.015, 95% interval -0.040 to +0.007 over 100
  queries), and on the hashing backend 0.349 and 0.326 (0.352 and 0.320), so
  this is no gain in ranking, and the saving is the reason for it. Ranking
  the records that remain below all longer ones was tried and not kept: it
  changes the hand-written queries by +0.001 and costs the automatic ones
  0.008 (-0.013 to -0.003) with the default model. The chunk revision goes to
  11.
- The benchmark in `eval/` reports a tight level next to the region and file
  ones: a region hit that is at most three times as long as the answer, plus
  ten lines. Region level credits any chunk that holds the answer, which
  favours coarse cutting.
- The README states what was measured: a "How well it finds things" section
  with the benchmark's numbers for both models, the hashing backend and
  ripgrep, and what it does not show; the 86% of the retrieval-quality suite
  is said to be 73 file-level questions; the GPU is about three times faster
  than the CPU (axum: 17 s against 61 s, a mid-range GPU against a 6-core
  CPU), not four; and changing `chunk_lines`, `chunk_overlap` or
  `max_chunk_lines` cuts every file again but embeds only the chunks whose
  text changed (axum, `chunk_lines` 40 to 30: 50 of 4,213).
- A function or method is now a record of its own however small it is, where
  its neighbours used to share one chunk of up to 50 lines. A class, `impl`,
  trait or module is cut into its members: one record per member, and the lines
  from the container's first line to its first member, when there are more than
  two of them, as a record that opens it (two lines or fewer, such as
  `impl Road {`, open the first member's record instead). Declarations that are
  not functions (a constant, an alias, a signature without a body) stay grouped
  with their neighbours unless they take three lines or more, and a function
  inside a function stays inside it. A doc comment or decorator above a
  declaration is still the first lines of its record. The cause was dilution:
  in a sample of 40 misses, 25 on the doc-comment queries and 5 of 35 on the
  hand-written ones were a small function inside a much larger chunk. On the
  benchmark in `eval/` with the default model, region-level MRR@10 goes from
  0.391 to 0.423 on the doc-comment queries (recall@10 0.649 to 0.722; pooled
  difference +0.025, 95% interval +0.008 to +0.042) and from 0.414 to 0.388 on
  the hand-written ones (recall@10 0.690 to 0.650; -0.026, -0.076 to +0.025:
  100 queries cannot tell). File-level MRR@10 goes from 0.689 to 0.703 and from
  0.616 to 0.585. On the hashing backend region MRR@10 goes from 0.369 to 0.352
  (-0.024, -0.039 to -0.009) and from 0.322 to 0.320, and the file level stays
  at 0.654 on the first set. Region level counts any chunk that holds the
  answer, so it also credited the large chunk around a small answer; counting
  only hits at most three times as long as the answer plus ten lines (the
  tight level), the default model goes from 0.309 to 0.418 and from 0.396 to
  0.388, and the hashing backend from 0.282 to 0.344 and from 0.305 to 0.320.
  The `nest` project, whose answers are often accessors of two or three lines,
  loses at region level on the doc-comment queries (hashing backend 0.231 to
  0.168, default model 0.254 to 0.227), is level at file level (0.477 to 0.476,
  0.513 to 0.504) and gains at tight level (0.144 to 0.157 and 0.189 to
  0.227). The four projects hold 50,527 records instead of 35,382 (+43%), and
  their indexes are 30% larger. The chunk revision goes to 10. The numbers of this
  and of the next two changes are each measured with the path alone on the
  embedded line. All three together, against the cutting before them and with
  the default model: clearly better on the doc-comment queries (region MRR@10
  0.391 to 0.425, +0.025, 95% interval +0.008 to +0.042), level within noise on
  the hand-written ones (0.414 to 0.397, -0.018, -0.064 to +0.031), and hits
  much closer to the size of the answer (tight level 0.309 to 0.420 and 0.396
  to 0.395); the hashing backend is slightly lower at region level on the
  doc-comment queries (0.369 to 0.349, -0.027, -0.042 to -0.011) and the same at
  file level (0.654 to 0.652), because a large chunk counted as a hit for every
  function in it and the smaller records no longer do (tight level 0.282 to
  0.341).
- The line above a chunk that the model reads now also names the declaration
  the chunk is cut from, `src/io.ts: fn IoAdapter::mapPayload`, where it was
  the path alone (a chunk that declares nothing keeps the path alone). A method
  cut out of its class says little of the class, and a description of it names
  the class. Only the embedding input changes: the snippet, the stored text and
  the keyword terms are what they were, and the hashing backend, which ranks by
  words, does not change. On the benchmark in `eval/` with the default model
  and the chunking above, region-level MRR@10 goes from 0.374 to 0.397 on the
  hand-written queries (+0.023, 95% interval -0.018 to +0.062) and from 0.423 to
  0.425 on the doc-comment ones (+0.001, -0.006 to +0.007); file-level from
  0.567 to 0.587 and from 0.703 to 0.703. Neither interval excludes zero; the
  line costs a few tokens per chunk and it is kept for what it restores, a
  method's class, and not for what the numbers can tell. The chunk revision goes to 11, so
  existing indexes are embedded again.
- `ripsem search` takes `-e`/`--regex` and the MCP `search` tool a `regex`
  argument, to keep only the hits whose code matches a regular expression:
  the place that does one thing and also mentions a name. The pattern is
  matched against the whole chunk, `(?i)` ignores case, and it goes with the
  language, glob and path filters, `min_score` and paging. Chunk text is read
  from the files, not held in the index, so the pattern is tried on the 200
  best-ranked places, or eight times the page's end when that is more. A
  pattern that does not parse, or compiles to more than 1 MiB, is an error:
  an `isError` result over MCP, a one-line message and a non-zero exit on the
  command line. `regex` is now a direct dependency; it was already in the
  tree through tree-sitter and tokenizers.
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
