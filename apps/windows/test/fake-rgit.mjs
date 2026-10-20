#!/usr/bin/env node
// A stand-in for `rgit` that replays recorded `rgit --json` output.
// Usage mirrors the real binary: fake-rgit.mjs --json [-C dir] <command> [args...]
// The recording to play is chosen by test/fixtures/manifest.json (first entry whose `argv`
// matches; "*" matches any one argument). Fixture files hold the exact stdout of the real
// process; `exit` is its exit code. Entries may set `hang: true` to keep the process alive
// after printing (a transfer or `serve` that is still running) until it is terminated.
import { appendFileSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dir = process.env.RGIT_FIXTURES ?? join(here, "fixtures");

const raw = process.argv.slice(2);
const argv = [];
let project = null;
for (let i = 0; i < raw.length; i++) {
  if (raw[i] === "--json") continue;
  if (raw[i] === "-C" || raw[i] === "--dir") {
    project = raw[++i] ?? null;
    continue;
  }
  argv.push(raw[i]);
}
// `--paths-from -` / `--except-from -`: the list arrives on stdin (the real tool reads it to EOF).
let stdinLines;
if (argv.some((a, i) => a === "-" && /^--(paths|except)-from$/.test(argv[i - 1] ?? ""))) {
  stdinLines = readFileSync(0, "utf8").split("\n").filter(Boolean);
}
if (process.env.RGIT_FAKE_LOG) {
  appendFileSync(
    process.env.RGIT_FAKE_LOG,
    JSON.stringify({ project, argv, ...(stdinLines ? { stdinLines: stdinLines.length, stdinHead: stdinLines.slice(0, 3) } : {}) }) + "\n",
  );
}

const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"));
// "*" matches exactly one argument; a trailing "**" matches any number of remaining ones.
const matches = (pattern) => {
  const rest = pattern[pattern.length - 1] === "**";
  const fixed = rest ? pattern.slice(0, -1) : pattern;
  if (rest ? argv.length < fixed.length : argv.length !== fixed.length) return false;
  return fixed.every((a, i) => a === "*" || a === argv[i]);
};
const entry = manifest.recordings.find((r) => matches(r.argv));
if (!entry) {
  process.stderr.write(`fake-rgit: no recording for: ${argv.join(" ")}\n`);
  process.exit(64);
}
const out = readFileSync(resolve(dir, entry.file), "utf8");
const lines = out.split("\n").filter((l) => l.length > 0);
const delay = entry.delayMs ?? 0;
let i = 0;
const next = () => {
  if (i < lines.length) {
    process.stdout.write(lines[i++] + "\n");
    if (delay) setTimeout(next, delay);
    else next();
    return;
  }
  if (entry.stderr) process.stderr.write(entry.stderr);
  if (entry.hang) {
    // Stay alive until killed; exit when stdin closes like the real tool.
    process.stdin.resume();
    process.stdin.on("end", () => process.exit(130));
    setInterval(() => {}, 1 << 30);
    return;
  }
  process.exitCode = entry.exit ?? 0;
};
next();
