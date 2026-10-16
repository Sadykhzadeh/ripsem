# Security

Report a vulnerability privately through
[GitHub's private reporting](https://github.com/Sadykhzadeh/ripsem/security/advisories/new),
not in a public issue. Include the version (`ripsem --version`), the platform
and the steps that reproduce it.

ripsem runs offline once its model is downloaded. It reads the files in the
repository it indexes, writes only inside `.ripsem/`, the model cache in the
user's cache directory, and `.mcp.json` or `.cursor/mcp.json` for `ripsem
install`. It makes network requests only to download the embedding model. Anything that breaks one of those is in scope.

Fixes go into the latest release.
