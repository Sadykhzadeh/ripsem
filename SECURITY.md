# Security

## Supported versions

Only the latest release gets security fixes. Older versions are not patched;
upgrade to the latest release from crates.io or npm.

## Reporting a vulnerability

Report it privately through
[GitHub's private vulnerability reporting](https://github.com/Sadykhzadeh/ripsem/security/advisories/new),
not in a public issue. Include the version (`ripsem --version`), the platform,
and the steps that reproduce it. A crafted repository or index file that
triggers the problem is the most useful report.

## In scope

- Anything in a repository that ripsem indexes: source files,
  `.ripsem/config.toml` and `.ripsem/index.bin`, since a repository can ship
  them. A crash, a hang, unbounded memory use, or reading a file outside the
  repository counts.
- Input to `ripsem mcp` over stdio: JSON-RPC messages and tool arguments.
- Writes outside `.ripsem/`, the model cache, and the MCP config files that
  `ripsem install` writes.
- Network requests other than downloading the embedding model.

## Out of scope

- Results that are wrong or missing. Those are ordinary bugs; open an issue.
- Vulnerabilities in a dependency that ripsem's code does not reach. Report
  those upstream.
- Someone who can already write to your user profile or your model cache.
- Resource use that a large repository causes in an ordinary indexing run.
