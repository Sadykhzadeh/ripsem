#!/usr/bin/env node
"use strict";

// Runs the ripsem binary from the platform package npm installed beside this
// one, passing arguments, stdio and the exit code straight through, so
// `npx ripsem mcp` serves MCP exactly as the binary does.

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const packages = {
  "darwin arm64": "ripsem-darwin-arm64",
  "linux x64": "ripsem-linux-x64",
  "win32 x64": "ripsem-win32-x64",
};

const name = packages[`${process.platform} ${process.arch}`];
if (!name) {
  console.error(`ripsem has no build for ${process.platform} ${process.arch}`);
  process.exit(1);
}

let dir;
try {
  dir = path.dirname(require.resolve(`${name}/package.json`));
} catch {
  console.error(`ripsem: ${name} is not installed; install ripsem again with optional dependencies`);
  process.exit(1);
}

const binary = path.join(dir, process.platform === "win32" ? "ripsem.exe" : "ripsem");
const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`ripsem: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
