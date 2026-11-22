#!/usr/bin/env node
"use strict";

// Runs the ripsem binary from the platform package npm installed beside this
// one, passing arguments, stdio, the exit code and any signal straight through,
// so `npx ripsem mcp` serves MCP exactly as the binary does.

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

// npm installs the Linux package only where glibc is present, and the binary
// needs it, so a musl system such as Alpine has no build to install again.
function hasGlibc() {
  return Boolean(process.report?.getReport().header.glibcVersionRuntime);
}

let dir;
try {
  dir = path.dirname(require.resolve(`${name}/package.json`));
} catch {
  console.error(
    process.platform === "linux" && !hasGlibc()
      ? "ripsem: there is no build for Linux without glibc, such as Alpine; it needs glibc 2.35 or newer"
      : `ripsem: ${name} is not installed; install ripsem again with optional dependencies`
  );
  process.exit(1);
}

const binary = path.join(dir, process.platform === "win32" ? "ripsem.exe" : "ripsem");
const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`ripsem: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 1);
