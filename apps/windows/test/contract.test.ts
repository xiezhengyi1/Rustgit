// Replays recordings of the REAL rgit (test/fixtures-real, captured by
// `npm run capture-fixtures`) through the client and checks that every response still has the
// shape src/rgit/types.ts promises. If the engine's JSON drifts, this fails first.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { RgitClient } from "../src/rgit/client";
import { RgitError } from "../src/rgit/errors";
import { FIXTURES, nodeRunner, singleRecording } from "./node-runner";
import { check, shapes } from "./shapes";

const REAL = join(FIXTURES, "..", "fixtures-real");
const P = "/work/demo";
const have = existsSync(join(REAL, "manifest.json"));
/** A client that answers every call with the one named recording. */
const use = (file: string) => new RgitClient(nodeRunner({ fixtures: singleRecording(REAL, file) }));

const cases: [string, keyof typeof shapes, (c: RgitClient) => Promise<unknown>][] = [
  ["version.json", "version", (c) => c.version()],
  ["init.json", "init", (c) => c.init(P)],
  ["status.empty.json", "status", (c) => c.status(P)],
  ["status.fresh.json", "status", (c) => c.status(P)],
  ["status.merge.json", "status", (c) => c.status(P)],
  ["status.dirty.json", "status", (c) => c.status(P)],
  ["status.synced.json", "status", (c) => c.status(P)],
  ["commit.json", "commit", (c) => c.commit(P, "first")],
  ["log.json", "log", (c) => c.log(P, 50)],
  ["show.json", "show", (c) => c.show(P, "a".repeat(64))],
  ["diff.json", "diff", (c) => c.diff(P)],
  ["branch.json", "branches", (c) => c.branches(P)],
  ["branch.create.json", "branchRef", (c) => c.createBranch(P, "feature")],
  ["switch.json", "switch", (c) => c.switchBranch(P, "feature")],
  ["merge.uptodate.json", "mergeLike", (c) => c.merge(P, "feature")],
  ["merge.conflicts.json", "mergeLike", (c) => c.merge(P, "feature")],
  ["resolve.ours.json", "resolve", (c) => c.resolve(P, "docs/readme.md", "ours")],
  ["shelve.json", "shelve", (c) => c.shelve(P, "wip")],
  ["shelves.json", "shelves", (c) => c.shelves(P)],
  ["unshelve.json", "mergeLike", (c) => c.unshelve(P, "1")],
  ["revert.json", "mergeLike", (c) => c.revert(P, "a".repeat(64))],
  ["restore.force.json", "mergeLike", (c) => c.restore(P, ["a.txt"], "HEAD", true)],
  ["stats.json", "stats", (c) => c.stats(P)],
  ["verify.json", "verify", (c) => c.verify(P)],
  ["gc.json", "gc", (c) => c.gc(P)],
  ["gc.aggressive.json", "gc", (c) => c.gc(P, true)],
  ["reindex.json", "reindex", (c) => c.reindex(P)],
  ["remote.json", "remotes", (c) => c.remotes(P)],
  ["remote.empty.json", "remotes", (c) => c.remotes(P)],
  ["push.first.ndjson", "push", (c) => c.push(P)],
  ["push.second.ndjson", "push", (c) => c.push(P)],
  ["fetch.ndjson", "fetch", (c) => c.fetch(P)],
  ["pull.ndjson", "pull", (c) => c.pull(P)],
  ["clone.json", "clone", (c) => c.clone("tcp://127.0.0.1:1", "/work/clone1")],
];

describe.skipIf(!have)("contract with recordings of the real rgit", () => {
  for (const [file, shape, call] of cases) {
    it(`${file} matches the ${shape} shape`, async () => {
      const data = await call(use(file));
      expect(check(data, shapes[shape]!), JSON.stringify(data).slice(0, 300)).toEqual([]);
    });
  }

  it("merge state is reported with the label and per-file resolution", async () => {
    const merge = JSON.parse(readFileSync(join(REAL, "status.merge.json"), "utf8")).data;
    expect(merge.merge).toEqual({ kind: "merge", label: "feature" });
    expect(merge.conflicts[0]).toMatchObject({ path: "docs/readme.md", resolved: false });
  });

  it("merge with conflicts (exit 1, ok:true) lists the paths", async () => {
    const r = await use("merge.conflicts.json").merge(P, "feature");
    expect(r.conflicts).toEqual(["docs/readme.md"]);
  });

  it("push reports progress phases", async () => {
    const phases: string[] = [];
    await use("push.first.ndjson").push(P, { onProgress: (p) => phases.push(`${p.phase}:${p.done}/${p.total}`) });
    expect(phases).toHaveLength(2);
    expect(phases[0]).toMatch(/^negotiating:0\//);
    expect(phases[1]).toMatch(/^uploading:(\d+)\/\1$/);
  });

  it("fetch reports the relation event", async () => {
    const events: string[] = [];
    await use("fetch.ndjson").fetch(P, { onEvent: (e) => events.push(e.event) });
    expect(events).toContain("relation");
  });

  it("a rejected push is never reported as success (engine answers ok:true, exit 3)", async () => {
    const err = await use("push.rejected.json").push(P).catch((e) => e);
    expect(err).toBeInstanceOf(RgitError);
    expect(err.code).toBe("ref_conflict");
  });

  it.each([
    ["not_a_repository", "status.notrepo.json", (c: RgitClient) => c.status("/nope")],
    ["nothing_to_commit", "commit.nothing.json", (c: RgitClient) => c.commit(P, "x", ["nope.txt"])],
    ["branch_exists", "branch.exists.json", (c: RgitClient) => c.createBranch(P, "feature")],
    ["invalid_name", "branch.invalid.json", (c: RgitClient) => c.createBranch(P, "bad name")],
    ["branch_not_found", "switch.missing.json", (c: RgitClient) => c.switchBranch(P, "nope")],
    ["dirty_working_tree", "restore.dirty.json", (c: RgitClient) => c.restore(P, ["a.txt"], "HEAD")],
    ["host_unreachable", "push.unreachable.json", (c: RgitClient) => c.push("/work/lonely")],
  ])("error %s has a code, a message and (where relevant) paths", async (code, file, call) => {
    const err = (await call(use(file)).catch((e: unknown) => e)) as RgitError;
    expect(err).toBeInstanceOf(RgitError);
    expect(err.code).toBe(code);
    expect(err.message.length).toBeGreaterThan(0);
    if (code === "dirty_working_tree") expect(err.paths).toEqual(["a.txt"]);
  });
});
