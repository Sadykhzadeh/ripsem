// Builds the npm packages for one release from its unpacked archives:
//
//   node npm/stage.mjs <version> <unpacked> <out>
//
// <unpacked> holds the ripsem-v<version>-<target>/ directories of the release
// archives. <out> receives one package per platform and the ripsem package
// that depends on them, all at <version>.

import { chmodSync, cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const [version, unpacked, out] = process.argv.slice(2);
if (!version || !unpacked || !out) {
  console.error("usage: node npm/stage.mjs <version> <unpacked> <out>");
  process.exit(2);
}

const here = path.dirname(fileURLToPath(import.meta.url));
const main = JSON.parse(readFileSync(path.join(here, "ripsem", "package.json"), "utf8"));

const platforms = [
  { name: "ripsem-darwin-arm64", target: "aarch64-apple-darwin", os: "darwin", cpu: "arm64", files: ["ripsem"] },
  { name: "ripsem-linux-x64", target: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", libc: "glibc", files: ["ripsem"] },
  { name: "ripsem-win32-x64", target: "x86_64-pc-windows-msvc", os: "win32", cpu: "x64", files: ["ripsem.exe", "DirectML.dll"] },
];

for (const p of platforms) {
  const from = path.join(unpacked, `ripsem-v${version}-${p.target}`);
  const dir = path.join(out, p.name);
  mkdirSync(dir, { recursive: true });
  for (const file of [...p.files, "LICENSE", "third-party"]) {
    cpSync(path.join(from, file), path.join(dir, file), { recursive: true });
  }
  if (p.os !== "win32") {
    chmodSync(path.join(dir, "ripsem"), 0o755);
  }
  const manifest = {
    name: p.name,
    version,
    description: `The ripsem binary for ${p.os} ${p.cpu}, installed by the ripsem package`,
    homepage: main.homepage,
    repository: main.repository,
    license: main.license,
    os: [p.os],
    cpu: [p.cpu],
    ...(p.libc && { libc: [p.libc] }),
  };
  writeFileSync(path.join(dir, "package.json"), JSON.stringify(manifest, null, 2) + "\n");
}

const dir = path.join(out, "ripsem");
cpSync(path.join(here, "ripsem"), dir, { recursive: true });
cpSync(path.join(here, "..", "LICENSE"), path.join(dir, "LICENSE"));
main.version = version;
for (const p of platforms) {
  main.optionalDependencies[p.name] = version;
}
writeFileSync(path.join(dir, "package.json"), JSON.stringify(main, null, 2) + "\n");
