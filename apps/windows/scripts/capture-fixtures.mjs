#!/usr/bin/env node
// Record real `rgit --json` output into test/fixtures-real/ by running a scripted scenario
// (local history, branches, a merge conflict, maintenance, and push/fetch/clone against a real
// `rgit serve`). The recordings replace nothing automatically: test/fixtures holds the small,
// UI-oriented set; test/fixtures-real is the untouched output of the real binary, replayed by
// test/contract.test.ts through the same client.
//
// Usage: node scripts/capture-fixtures.mjs [--rgit /path/to/rgit] [--out test/fixtures-real]
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const appDir = resolve(here, "..");
const args = process.argv.slice(2);
const opt = (n, d) => (args.includes(`--${n}`) ? args[args.indexOf(`--${n}`) + 1] : d);
const RGIT = resolve(opt("rgit", process.env.RGIT_BIN ?? join(appDir, "..", "..", "target", "release", "rgit")));
const OUT = resolve(appDir, opt("out", "test/fixtures-real"));

if (!existsSync(RGIT)) {
  console.error(`rgit not found at ${RGIT}; build it with: cargo build --release -p rustgit-cli`);
  process.exit(1);
}
const probe = spawnSync(RGIT, ["--json", "version"], { encoding: "utf8" });
let version;
try {
  version = JSON.parse(probe.stdout.trim().split("\n").pop()).data;
} catch {
  console.error("this rgit does not support --json yet");
  process.exit(1);
}

const root = realpathSync(mkdtempSync(join(tmpdir(), "rgit-capture-")));
const norm = (s) => s.split(root).join("/work");
rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });
const recordings = [];
const files = new Map();

const wild = (a) => (a.includes(root) || /^[0-9a-f]{64}$/.test(a) || /^tcp:\/\//.test(a) ? "*" : a);

/** Run one command, record stdout verbatim (paths normalised) under `name`. */
function rec(name, project, argv, { pattern, ext } = {}) {
  const full = ["--json", ...(project ? ["-C", project] : []), ...argv];
  const r = spawnSync(RGIT, full, { encoding: "utf8" });
  const stdout = norm(r.stdout);
  const lines = stdout.split("\n").filter(Boolean);
  const progress = lines.some((l) => l.includes('"event"'));
  const file = `${name}.${ext ?? (progress ? "ndjson" : "json")}`;
  files.set(file, stdout);
  recordings.push({ argv: pattern ?? argv.map(wild), file, exit: r.status ?? 0 });
  const last = lines.at(-1) ?? "";
  console.log(`${String(r.status).padStart(2)}  ${name.padEnd(28)} ${last.slice(0, 90)}`);
  return { exit: r.status, lines, result: JSON.parse(last || "{}") };
}
const plain = (project, argv) => spawnSync(RGIT, ["-C", project, ...argv], { encoding: "utf8" });
const write = (p, c) => {
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, c);
};

const demo = join(root, "demo");
mkdirSync(demo);
rec("version", null, ["version"]);
rec("status.notrepo", demo, ["status"]);
rec("init", demo, ["init"]);
plain(demo, ["config", "user.name", "Ann"]);
plain(demo, ["config", "user.email", "ann@example.com"]);
rec("status.empty", demo, ["status"]);
write(join(demo, "a.txt"), "hello\n");
write(join(demo, "docs/readme.md"), "line1\nline2\nline3\n");
write(join(demo, "old.txt"), "to be removed\n");
rec("status.fresh", demo, ["status"]);
rec("commit.nothing", demo, ["commit", "-m", "x", "nope.txt"]);
const first = rec("commit", demo, ["commit", "-m", "first"], { pattern: ["commit", "-m", "*"] });
rec("commit.empty", demo, ["commit", "-m", "again"], { pattern: ["commit", "-m", "again"] });
rec("branch", demo, ["branch"]);
rec("branch.create", demo, ["branch", "feature"], { pattern: ["branch", "feature"] });
rec("branch.exists", demo, ["branch", "feature"], { pattern: ["branch", "feature"] });
rec("branch.invalid", demo, ["branch", "bad name"], { pattern: ["branch", "bad name"] });
rec("switch.missing", demo, ["switch", "nope"], { pattern: ["switch", "nope"] });
rec("switch", demo, ["switch", "feature"], { pattern: ["switch", "feature"] });
write(join(demo, "docs/readme.md"), "line1\nFEATURE\nline3\n");
write(join(demo, "feature.txt"), "feature work\n");
rec("commit.feature", demo, ["commit", "-m", "feature work\n\nsecond paragraph"], { pattern: ["commit", "-m", "feature work", "**"] });
rec("switch.main", demo, ["switch", "main"], { pattern: ["switch", "main"] });
write(join(demo, "docs/readme.md"), "line1\nMAIN\nline3\n");
rec("commit.main", demo, ["commit", "-m", "main edit"], { pattern: ["commit", "-m", "main edit"] });
const merge = rec("merge.conflicts", demo, ["merge", "feature"], { pattern: ["merge", "feature"] });
rec("status.merge", demo, ["status"], { pattern: ["status"] });
rec("resolve.ours", demo, ["resolve", "docs/readme.md", "--ours"], { pattern: ["resolve", "docs/readme.md", "--ours"] });
rec("commit.merge", demo, ["commit", "-m", "merge feature"], { pattern: ["commit", "-m", "merge feature"] });
const log = rec("log", demo, ["log", "-n", "50"], { pattern: ["log", "-n", "*"] });
const head = log.result.data.commits[0].id;
rec("show", demo, ["show", head], { pattern: ["show", "*"] });
rec("merge.uptodate", demo, ["merge", "feature"], { pattern: ["merge", "feature"] });

// working-tree changes: modified, added, deleted, renamed (same content, new name)
write(join(demo, "a.txt"), "hello\nmore\n");
write(join(demo, "b.txt"), "brand new\n");
rmSync(join(demo, "old.txt"));
rec("status.dirty", demo, ["status"], { pattern: ["status"] });
rec("diff", demo, ["diff"], { pattern: ["diff"] });
rec("restore.dirty", demo, ["restore", "--from", "HEAD", "a.txt"], { pattern: ["restore", "--from", "HEAD", "a.txt"] });
rec("switch.dirty", demo, ["switch", "feature"], { pattern: ["switch", "feature"] });
rec("shelve", demo, ["shelve", "-m", "wip"], { pattern: ["shelve", "-m", "wip"] });
const shelves = rec("shelves", demo, ["shelves"]);
const shelf = shelves.result.data.shelves?.[0];
if (shelf) rec("unshelve", demo, ["unshelve", shelf.name], { pattern: ["unshelve", "*"] });
rec("restore.force", demo, ["restore", "--from", "HEAD", "--force", "a.txt"], { pattern: ["restore", "--from", "HEAD", "--force", "a.txt"] });
rec("revert.dirty", demo, ["revert", head], { pattern: ["revert", "*"] });
rec("commit.cleanup", demo, ["commit", "-m", "cleanup"], { pattern: ["commit", "-m", "cleanup"] });
rec("revert", demo, ["revert", head], { pattern: ["revert", "*"] });
rec("branch.delete", demo, ["branch", "-d", "feature"], { pattern: ["branch", "-d", "feature"] });
rec("stats", demo, ["stats"]);
rec("verify", demo, ["verify"]);
rec("gc", demo, ["gc"]);
rec("gc.aggressive", demo, ["gc", "--aggressive"]);
rec("reindex", demo, ["reindex"]);
rec("remote.empty", demo, ["remote"]);

// ----- sync against a real host -----
const hostDir = join(root, "host.rgit");
const host = spawn(RGIT, ["--json", "serve", hostDir, "--init", "--listen", "127.0.0.1:0"], { stdio: ["pipe", "pipe", "pipe"] });
let hostOut = "";
host.stdout.on("data", (d) => (hostOut += d));
const listening = await new Promise((res, rej) => {
  const t = setTimeout(() => rej(new Error("host did not start: " + hostOut)), 10000);
  const iv = setInterval(() => {
    const l = hostOut.split("\n").find((x) => x.includes('"listening"'));
    if (l) {
      clearTimeout(t);
      clearInterval(iv);
      res(JSON.parse(l));
    }
  }, 50);
});
const url = `tcp://${listening.addr}`;
await new Promise((r) => setTimeout(r, 300));
files.set("serve.ndjson", norm(hostOut.split("\n").filter((l) => l.includes('"listening"')).join("\n") + "\n"));
recordings.push({ argv: ["serve", "*", "--listen", "*"], file: "serve.ndjson", exit: 0, hang: true });
console.log(`    host listening on ${listening.addr}`);

rec("remote.add", demo, ["remote", "add", "origin", url], { pattern: ["remote", "add", "origin", "*"] });
rec("remote", demo, ["remote"]);
rec("status.noupstream", demo, ["status"], { pattern: ["status"] });
rec("push.first", demo, ["push"]);
rec("status.synced", demo, ["status"], { pattern: ["status"] });
const clone1 = join(root, "clone1");
rec("clone", null, ["clone", url, clone1], { pattern: ["clone", "*", "*"] });
plain(clone1, ["config", "user.name", "Bo"]);
plain(clone1, ["config", "user.email", "bo@example.com"]);
write(join(clone1, "bo.txt"), "from bo\n");
rec("commit.clone", clone1, ["commit", "-m", "bo work"], { pattern: ["commit", "-m", "bo work"] });
rec("push.clone", clone1, ["push"], { pattern: ["push"] });
write(join(demo, "docs/readme.md"), "line1\nAnn again\nline3\n");
rec("commit.ann", demo, ["commit", "-m", "ann work"], { pattern: ["commit", "-m", "ann work"] });
rec("push.rejected", demo, ["push"], { pattern: ["push"] });
rec("fetch", demo, ["fetch"]);
rec("status.behind", demo, ["status"], { pattern: ["status"] });
rec("pull", demo, ["pull"]);
rec("push.second", demo, ["push"], { pattern: ["push"] });

const lone = join(root, "lonely");
mkdirSync(lone);
rec("init.lonely", lone, ["init"], { pattern: ["init"] });
plain(lone, ["config", "user.name", "Cy"]);
plain(lone, ["config", "user.email", "cy@example.com"]);
write(join(lone, "x.txt"), "x\n");
plain(lone, ["commit", "-m", "x"]);
rec("remote.add.bad", lone, ["remote", "add", "origin", "tcp://127.0.0.1:1"], { pattern: ["remote", "add", "origin", "tcp://127.0.0.1:1"] });
rec("push.unreachable", lone, ["push"], { pattern: ["push"] });

host.kill();
for (const [f, c] of files) writeFileSync(join(OUT, f), c);
writeFileSync(
  join(OUT, "manifest.json"),
  JSON.stringify({ source: `captured from rgit ${version.cli} (json_interface ${version.json_interface}) by scripts/capture-fixtures.mjs`, version, recordings }, null, 2) + "\n",
);
rmSync(root, { recursive: true, force: true });
console.log(`\nwrote ${files.size} recordings to ${OUT}`);
