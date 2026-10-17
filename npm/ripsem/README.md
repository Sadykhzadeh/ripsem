# ripsem

Local, offline code search that answers one question: where is the code that
does this? Ask with an identifier, an error message or a plain description of
the behaviour, and get back the functions and types that answer it.

```
npx ripsem index
npx ripsem search "strip the auth header when redirecting to another host"
```

As an MCP server for a coding agent:

```json
{
  "mcpServers": {
    "ripsem": { "command": "npx", "args": ["-y", "ripsem", "mcp"] }
  }
}
```

This package runs the prebuilt binary for Linux x64 (glibc 2.35 or newer),
macOS on Apple silicon and Windows x64. Documentation lives at
https://github.com/Sadykhzadeh/ripsem.
