#!/usr/bin/env node
// Copy the built rgit executable to src-tauri/binaries/rgit-<target-triple>[.exe], the name the
// Tauri bundler expects for `externalBin`.
// Usage: node scripts/stage-sidecar.mjs [--target <triple>] [--profile release] [--from <path>]
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const appDir = resolve(here, "..");
const repoRoot = resolve(appDir, "..", "..");

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i === -1 ? fallback : args[i + 1];
};

const hostTriple = execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)?.[1];
const triple = opt("target", hostTriple);
if (!triple) throw new Error("cannot determine the target triple");
const exe = triple.includes("windows") ? ".exe" : "";
const profile = opt("profile", "release");

const explicit = opt("from");
const candidates = explicit
  ? [resolve(explicit)]
  : [
      join(repoRoot, "target", triple, profile, `rgit${exe}`),
      // a host build lands in target/<profile> when --target was not passed to cargo
      ...(triple === hostTriple ? [join(repoRoot, "target", profile, `rgit${exe}`)] : []),
    ];
const src = candidates.find((p) => existsSync(p));
if (!src) {
  console.error(`rgit${exe} not found. Looked in:\n  ${candidates.join("\n  ")}\nBuild it first: cargo build --release -p rustgit-cli --features iroh${triple !== hostTriple ? ` --target ${triple}` : ""}`);
  process.exit(1);
}
const dest = join(appDir, "src-tauri", "binaries", `rgit-${triple}${exe}`);
mkdirSync(dirname(dest), { recursive: true });
copyFileSync(src, dest);
console.log(`staged ${src} -> ${dest}`);
