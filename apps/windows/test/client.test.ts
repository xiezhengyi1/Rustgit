import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { RgitClient, parseLine } from "../src/rgit/client";
import { RgitCancelled, RgitError } from "../src/rgit/errors";
import type { EngineEvent, ProgressEvent } from "../src/rgit/types";
import { fixturesWith, nodeRunner, readCalls } from "./node-runner";

const P = "/work/demo";

function setup(overrides: Parameters<typeof fixturesWith>[0] = []) {
  const logFile = join(mkdtempSync(join(tmpdir(), "rgit-log-")), "calls.ndjson");
  const client = new RgitClient(nodeRunner({ fixtures: fixturesWith(overrides), logFile }));
  return { client, calls: () => readCalls(logFile) };
}

describe("parseLine", () => {
  it("tells events, results and noise apart", () => {
    expect(parseLine('{"event":"progress","phase":"x","done":1,"total":2}').kind).toBe("event");
    expect(parseLine('{"ok":true,"data":{}}').kind).toBe("result");
    expect(parseLine("warning: something").kind).toBe("garbage");
    expect(parseLine("{broken").kind).toBe("garbage");
    expect(parseLine("[1,2]").kind).toBe("garbage");
  });
});

describe("RgitClient against the fake rgit", () => {
  it("adds --json and -C and parses version", async () => {
    const { client, calls } = setup();
    expect(await client.ensureCompatible()).toMatchObject({ json_interface: 1 });
    await client.status(P);
    expect(calls()).toEqual([
      { project: null, argv: ["version"] },
      { project: P, argv: ["status"] },
    ]);
  });

  it("refuses a different json_interface", async () => {
    const { client } = setup([{ argv: ["version"], file: "version.mismatch.json" }]);
    await expect(client.ensureCompatible()).rejects.toMatchObject({ code: "interface_mismatch" });
  });

  it("parses status with renames, skipped files and ahead/behind", async () => {
    const { client } = setup();
    const s = await client.status(P);
    expect(s.branch).toBe("main");
    expect(s.ahead).toBe(2);
    expect(s.behind).toBe(1);
    expect(s.changes.find((c) => c.kind === "renamed")?.from).toBe("src/lib.rs");
    expect(s.skipped[0]?.path).toBe("assets/CON.png");
  });

  it("passes selected paths to commit", async () => {
    const { client, calls } = setup();
    const r = await client.commit(P, "msg", ["a.txt", "b/c.txt"]);
    expect(r.branch).toBe("main");
    expect(calls().at(-1)?.argv).toEqual(["commit", "-m", "msg", "a.txt", "b/c.txt"]);
  });

  it("commits 30,000 selected paths through stdin, never argv", async () => {
    const { client, calls } = setup();
    const paths = Array.from({ length: 30_000 }, (_, i) => `dir${i % 50}/some_longer_file_name_${i}.txt`);
    await client.commitSelection(P, "bulk", { kind: "only", paths });
    await client.commitSelection(P, "bulk", { kind: "except", paths: paths.slice(0, 25_000) });
    const [only, except] = calls().slice(-2);
    expect(only!.argv).toEqual(["commit", "-m", "bulk", "--paths-from", "-"]);
    expect(only!.stdinLines).toBe(30_000);
    expect(only!.stdinHead).toEqual(paths.slice(0, 3));
    expect(except!.argv).toEqual(["commit", "-m", "bulk", "--except-from", "-"]);
    expect(except!.stdinLines).toBe(25_000);
  });

  it("turns {ok:false} into RgitError with code and paths, whatever the exit code", async () => {
    const { client } = setup([{ argv: ["switch", "dirty"], file: "switch.dirty.json", exit: 1 }]);
    const e = await client.switchBranch(P, "dirty").catch((x) => x);
    expect(e).toBeInstanceOf(RgitError);
    expect(e).toMatchObject({ code: "dirty_working_tree", paths: ["docs/readme.md"] });
  });

  it("accepts ok:true with exit code 1 (merge conflicts) and 2 (verify problems)", async () => {
    const { client } = setup([
      { argv: ["merge", "conflicting"], file: "merge.conflicts.json", exit: 1 },
      { argv: ["verify"], file: "verify.problems.ndjson", exit: 2 },
    ]);
    const m = await client.merge(P, "conflicting");
    expect(m.conflicts).toEqual(["docs/readme.md"]);
    const v = await client.verify(P);
    expect(v.problems).toHaveLength(1);
  });

  it("streams progress events before the result and reads the last line as the result", async () => {
    const { client } = setup();
    const seen: ProgressEvent[] = [];
    const all: EngineEvent[] = [];
    const r = await client.fetch(P, { onProgress: (p) => seen.push(p), onEvent: (e) => all.push(e) });
    expect(seen.map((p) => [p.phase, p.done])).toEqual([["downloading", 3]]);
    expect(all.map((e) => e.event)).toEqual(["progress", "relation"]);
    expect(r.state).toBe("diverged");
    expect(r.stats.objects_transferred).toBe(3);
    expect(r.updated[0]?.ref).toBe("origin/feature");
  });

  it("surfaces a rejected push as ref_conflict after progress", async () => {
    const { client } = setup([{ argv: ["push"], file: "push.rejected.ndjson", exit: 1 }]);
    const e = await client.push(P).catch((x) => x);
    expect(e).toMatchObject({ code: "ref_conflict" });
  });

  it("clone needs no project and reports progress", async () => {
    const { client, calls } = setup();
    const done: number[] = [];
    const r = await client.clone("tcp://127.0.0.1:7878", "/work/clone", { onProgress: (p) => done.push(p.done) });
    expect(done).toEqual([]); // the recorded clone of a tiny project reports no progress
    expect(r.dest).toBe("/work/clone1");
    expect(r.branch).toBe("feature");
    expect(calls().at(-1)).toEqual({ project: null, argv: ["clone", "tcp://127.0.0.1:7878", "/work/clone"] });
  });

  it("builds argv for branch, restore, resolve and gc", async () => {
    const { client, calls } = setup();
    await client.createBranch(P, "x", "abc");
    await client.deleteBranch(P, "x");
    await client.switchBranch(P, "n", true);
    await client.restore(P, ["a.txt"], "abc", true);
    await client.resolve(P, "docs/readme.md", "theirs");
    await client.resolve(P, "docs/readme.md", "edit");
    await client.gc(P, true);
    await client.addRemote(P, "origin", "tcp://127.0.0.1:7878");
    expect(calls().map((c) => c.argv)).toEqual([
      ["branch", "x", "--from", "abc"],
      ["branch", "-d", "x"],
      ["switch", "-c", "n"],
      ["restore", "--from", "abc", "--force", "a.txt"],
      ["resolve", "docs/readme.md", "--theirs"],
      ["resolve", "docs/readme.md"],
      ["gc", "--aggressive"],
      ["remote", "add", "origin", "tcp://127.0.0.1:7878"],
    ]);
  });

  it("reports a protocol error when the process prints no result", async () => {
    const { client } = setup([{ argv: ["status"], file: "push.hang.ndjson", exit: 3 }]);
    const e = await client.status(P).catch((x) => x);
    expect(e).toMatchObject({ code: "protocol_error" });
  });

  it("reports unknown commands (fake exits 64 with stderr) as protocol_error with stderr", async () => {
    const { client } = setup();
    const e = await client.call(["no-such-command"]).catch((x) => x);
    expect(e).toMatchObject({ code: "protocol_error" });
    expect((e as RgitError).message).toContain("no recording");
  });

  it("cancels a running transfer via AbortSignal", async () => {
    const { client } = setup([{ argv: ["push"], file: "push.hang.ndjson", hang: true }]);
    const ac = new AbortController();
    let progress = 0;
    const p = client.push(P, {
      signal: ac.signal,
      onProgress: () => {
        progress++;
        ac.abort();
      },
    });
    await expect(p).rejects.toBeInstanceOf(RgitCancelled);
    expect(progress).toBe(1);
  });

  it("an already-aborted signal never starts the process", async () => {
    const { client, calls } = setup();
    const ac = new AbortController();
    ac.abort();
    await expect(client.fetch(P, { signal: ac.signal })).rejects.toBeInstanceOf(RgitCancelled);
    expect(calls()).toHaveLength(0);
  });

  it("start handle allows direct cancel", async () => {
    const { client } = setup([{ argv: ["fetch"], file: "push.hang.ndjson", hang: true }]);
    const p = client.fetch(P, {
      onStart: (h) => setTimeout(() => h.cancel(), 150),
    });
    await expect(p).rejects.toBeInstanceOf(RgitCancelled);
  });
});
