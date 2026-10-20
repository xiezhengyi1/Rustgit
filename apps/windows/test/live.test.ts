// Drives the REAL rgit binary through the client (no fakes). Skipped when the binary is not
// built or predates --json. Build it with: cargo build --release   (from the repository root)
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { RgitClient } from "../src/rgit/client";
import { RgitCancelled, RgitError } from "../src/rgit/errors";
import { nodeRunner } from "./node-runner";

const here = dirname(fileURLToPath(import.meta.url));
const BIN = resolve(process.env.RGIT_BIN ?? join(here, "..", "..", "..", "target", "release", process.platform === "win32" ? "rgit.exe" : "rgit"));

function supportsJson(): boolean {
  if (!existsSync(BIN)) return false;
  const r = spawnSync(BIN, ["--json", "version"], { encoding: "utf8" });
  return r.status === 0 && r.stdout.includes('"json_interface"');
}
const live = supportsJson();

let root = "";
let client: RgitClient;
let host: ReturnType<typeof spawn> | null = null;
let hostAddr = "";

const plain = (cwd: string, ...args: string[]) => {
  const r = spawnSync(BIN, ["-C", cwd, ...args], { encoding: "utf8" });
  if (r.status !== 0) throw new Error(`rgit ${args.join(" ")} failed: ${r.stderr}`);
};
const put = (p: string, content: string) => {
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, content);
};

describe.skipIf(!live)("real rgit through the client", () => {
  beforeAll(() => {
    root = realpathSync(mkdtempSync(join(tmpdir(), "rgit-live-")));
    client = new RgitClient(nodeRunner({ realBinary: BIN }));
  });
  afterAll(() => {
    host?.kill();
    rmSync(root, { recursive: true, force: true });
  });

  const demo = () => join(root, "demo");

  it("speaks the JSON interface this app understands", async () => {
    expect((await client.ensureCompatible()).json_interface).toBe(1);
  });

  it("runs a local workflow: init, commit selected files, branch, merge conflict, resolve", async () => {
    mkdirSync(demo());
    await expect(client.status(demo())).rejects.toMatchObject({ code: "not_a_repository" });
    await client.init(demo(), "main");
    plain(demo(), "config", "user.name", "Ann");
    plain(demo(), "config", "user.email", "ann@example.com");
    put(join(demo(), "docs/readme.md"), "l1\nl2\nl3\n");
    put(join(demo(), "a.txt"), "a\n");
    put(join(demo(), "b.txt"), "b\n");
    const st = await client.status(demo());
    expect(st.changes.map((c) => c.path).sort()).toEqual(["a.txt", "b.txt", "docs/readme.md"]);

    // per-file selection: commit only a.txt
    const c1 = await client.commit(demo(), "only a", ["a.txt"]);
    expect(c1.branch).toBe("main");
    const after = await client.status(demo());
    expect(after.changes.map((c) => c.path).sort()).toEqual(["b.txt", "docs/readme.md"]);
    await client.commit(demo(), "rest");

    await client.createBranch(demo(), "feature");
    await client.switchBranch(demo(), "feature");
    put(join(demo(), "docs/readme.md"), "l1\nFEATURE\nl3\n");
    await client.commit(demo(), "feature edit");
    await client.switchBranch(demo(), "main");
    put(join(demo(), "docs/readme.md"), "l1\nMAIN\nl3\n");
    await client.commit(demo(), "main edit");

    const m = await client.merge(demo(), "feature");
    expect(m.conflicts).toEqual(["docs/readme.md"]);
    const merging = await client.status(demo());
    expect(merging.merge?.label).toBe("feature");
    expect(merging.conflicts).toEqual([{ kind: "content", path: "docs/readme.md", resolved: false }]);

    await client.resolve(demo(), "docs/readme.md", "theirs");
    const fin = await client.commit(demo(), "merge feature");
    expect(fin.short).toHaveLength(10);

    const log = await client.log(demo(), 20);
    expect(log.commits[0]!.parents).toHaveLength(2);
    expect(log.commits[0]!.branches).toContain("main");
    const shown = await client.show(demo(), log.commits[0]!.id);
    expect(shown.commit.id).toBe(log.commits[0]!.id);
  });

  it("refuses to overwrite uncommitted work and explains which files", async () => {
    put(join(demo(), "a.txt"), "edited\n");
    const err = (await client.restore(demo(), ["a.txt"], "HEAD").catch((e) => e)) as RgitError;
    expect(err).toMatchObject({ code: "dirty_working_tree", paths: ["a.txt"] });
    await client.restore(demo(), ["a.txt"], "HEAD", true); // forced: recovery snapshot kept
    expect((await client.status(demo())).changes).toEqual([]);
  });

  it("reports storage statistics and verifies, collects, reindexes", async () => {
    const s = await client.stats(demo());
    expect(s.commits).toBeGreaterThanOrEqual(5);
    const v = await client.verify(demo());
    expect(v.problems).toEqual([]);
    expect((await client.gc(demo())).repo_bytes_after).toBeGreaterThan(0);
    expect((await client.reindex(demo())).problems).toEqual([]);
  });

  it("hosts, pushes, clones, fetches, and reports a rejected push as ref_conflict", async () => {
    const hostDir = join(root, "host.rgit");
    host = spawn(BIN, ["--json", "serve", hostDir, "--init", "--insecure-open-access", "--listen", "127.0.0.1:0"], { stdio: ["pipe", "pipe", "pipe"] });
    hostAddr = await new Promise<string>((res, rej) => {
      const t = setTimeout(() => rej(new Error("host did not report listening")), 10000);
      let buf = "";
      host!.stdout!.on("data", (d: Buffer) => {
        buf += d.toString();
        const line = buf.split("\n").find((l) => l.includes('"listening"'));
        if (line) {
          clearTimeout(t);
          res((JSON.parse(line) as { addr: string }).addr);
        }
      });
    });
    const url = `tcp://${hostAddr}`;
    await client.addRemote(demo(), "origin", url);
    expect((await client.remotes(demo())).remotes).toMatchObject([{ name: "origin", url, secure: false }]);

    // before the first push the server has no copy of the branch
    const before = await client.syncPosition(demo(), "main", "origin");
    expect(before.neverPushed).toBe(true);
    expect(before.ahead).toBeGreaterThan(0);

    const phases: string[] = [];
    const pushed = await client.push(demo(), { onProgress: (p) => phases.push(p.phase) });
    expect(pushed.updated.length).toBeGreaterThan(0);
    expect(phases.length).toBeGreaterThan(0);
    expect(await client.syncPosition(demo(), "main", "origin")).toEqual({ ahead: 0, behind: 0, neverPushed: false });

    const other = join(root, "other");
    const cloned = await client.clone(url, other);
    expect(cloned.dest).toBe(other);
    plain(other, "config", "user.name", "Bo");
    plain(other, "config", "user.email", "bo@example.com");
    put(join(other, "bo.txt"), "bo\n");
    await client.commit(other, "bo work");
    await client.push(other);

    put(join(demo(), "ann.txt"), "ann\n");
    await client.commit(demo(), "ann work");
    const rejected = (await client.push(demo()).catch((e) => e)) as RgitError;
    expect(rejected).toBeInstanceOf(RgitError);
    expect(rejected.code).toBe("ref_conflict");

    const fetched = await client.fetch(demo());
    expect(fetched.state).toBe("diverged");
    expect(await client.syncPosition(demo(), "main", "origin")).toMatchObject({ ahead: 1, behind: 1 });
    const pulled = await client.pull(demo());
    expect(pulled.merge?.conflicts ?? []).toEqual([]);
    await client.push(demo());
    expect(await client.syncPosition(demo(), "main", "origin")).toMatchObject({ ahead: 0, behind: 0 });
  });

  it("commits a 30,000-file project: bulk selections go through stdin and the engine honours them", async () => {
    const big = join(root, "big");
    mkdirSync(big);
    await client.init(big, "main");
    plain(big, "config", "user.name", "Ann");
    plain(big, "config", "user.email", "ann@example.com");
    const total = 30_000;
    for (let i = 0; i < total; i++) put(join(big, `pkg${i % 200}`, `file_${i}.txt`), `content ${i}\n`);
    const t0 = performance.now();
    const st = await client.status(big);
    const statusMs = performance.now() - t0;
    expect(st.changes).toHaveLength(total);

    // minority: 1,000 paths via --paths-from
    const some = st.changes.slice(0, 1000).map((c) => c.path);
    await client.commitSelection(big, "first thousand", { kind: "only", paths: some });
    expect((await client.status(big)).changes).toHaveLength(total - 1000);

    // majority: everything except three files via --except-from
    const left = (await client.status(big)).changes.slice(0, 3).map((c) => c.path);
    const t1 = performance.now();
    await client.commitSelection(big, "all but three", { kind: "except", paths: left });
    const commitMs = performance.now() - t1;
    const after = await client.status(big);
    expect(after.changes.map((c) => c.path).sort()).toEqual([...left].sort());
    // the same selection as a command line would have exceeded Windows' ~32k limit
    expect(st.changes.map((c) => c.path).join(" ").length).toBeGreaterThan(32_000);
    console.log(`30k project (real rgit): status ${statusMs.toFixed(0)} ms, commit of ~29k files via --except-from ${commitMs.toFixed(0)} ms`);
  }, 180000);

  it("an unreachable host is host_unreachable", async () => {
    const lone = join(root, "lonely");
    mkdirSync(lone);
    await client.init(lone);
    plain(lone, "config", "user.name", "Cy");
    plain(lone, "config", "user.email", "cy@example.com");
    put(join(lone, "x"), "x\n");
    await client.commit(lone, "x");
    await client.addRemote(lone, "origin", "tcp://127.0.0.1:1");
    await expect(client.push(lone)).rejects.toMatchObject({ code: "host_unreachable" });
  });

  it("cancels a long-running command (fetch --watch) and the process ends", async () => {
    const ac = new AbortController();
    const p = client.call(["fetch", "--watch"], { project: join(root, "other"), signal: ac.signal });
    setTimeout(() => ac.abort(), 700);
    await expect(p).rejects.toBeInstanceOf(RgitCancelled);
  });
});
