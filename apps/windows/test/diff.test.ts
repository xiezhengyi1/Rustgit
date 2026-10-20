import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { diffForPath, parseUnifiedDiff } from "../src/rgit/diff";
import { layoutGraph } from "../src/rgit/graph";
import type { Commit } from "../src/rgit/types";
import { baseName, dedupRatio, firstLine, formatBytes } from "../src/rgit/format";
import { FIXTURES } from "./node-runner";

const fx = (n: string) => JSON.parse(readFileSync(join(FIXTURES, n), "utf8")).data;

describe("parseUnifiedDiff", () => {
  it("splits files, counts lines and numbers them", () => {
    const files = parseUnifiedDiff(fx("diff.json").diff);
    expect(files.map((f) => f.path)).toEqual(["docs/readme.md", "src/new_file.rs"]);
    const readme = files[0]!;
    expect([readme.added, readme.removed]).toEqual([1, 1]);
    const del = readme.lines.find((l) => l.kind === "del")!;
    expect(del.oldNo).toBe(2);
    const add = readme.lines.find((l) => l.kind === "add")!;
    expect(add.newNo).toBe(2);
    expect(files[1]!.added).toBe(2);
    expect(diffForPath(files, "src/new_file.rs")?.lines.at(-1)?.text).toBe("// new");
  });

  it("recognises binary files and handles deleted files", () => {
    const files = parseUnifiedDiff(fx("show.json").diff);
    expect(files.at(-1)).toMatchObject({ path: "assets/logo.png", binary: true });
    const del = parseUnifiedDiff("--- a/x.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n");
    expect(del[0]).toMatchObject({ path: "x.txt", removed: 2 });
  });

  it("copes with empty input and CRLF", () => {
    expect(parseUnifiedDiff("")).toEqual([]);
    const f = parseUnifiedDiff("--- a/a\r\n+++ b/a\r\n@@ -1 +1 @@\r\n-x\r\n+y\r\n");
    expect(f[0]).toMatchObject({ path: "a", added: 1, removed: 1 });
  });
});

describe("layoutGraph", () => {
  const mk = (id: string, parents: string[]): Commit => ({
    id, short: id, parents, author_name: "", author_email: "", timestamp: 0, tz_offset_min: 0, message: id, branches: [],
  });

  it("keeps a linear history in lane 0", () => {
    const rows = layoutGraph([mk("c", ["b"]), mk("b", ["a"]), mk("a", [])]);
    expect(rows.map((r) => r.lane)).toEqual([0, 0, 0]);
    expect(rows[2]!.after).toEqual([]);
  });

  it("opens a second lane for a merge and joins it at the fork point", () => {
    // m merges b (second parent) into c; both descend from a
    const rows = layoutGraph([mk("m", ["c", "b"]), mk("c", ["a"]), mk("b", ["a"]), mk("a", [])]);
    expect(rows[0]!.parentLanes).toEqual([0, 1]);
    expect(rows[1]!.lane).toBe(0);
    expect(rows[2]!.lane).toBe(1);
    // b frees lane 1 by drawing its edge into lane 0, where a is expected
    expect(rows[2]!.parentLanes).toEqual([0]);
    expect(rows[3]!.lane).toBe(0);
    expect(rows[3]!.after).toEqual([]);
  });

  it("lays out the fixture history without losing a parent", () => {
    const commits = fx("log.json").commits as Commit[];
    const rows = layoutGraph(commits);
    expect(rows).toHaveLength(commits.length);
    expect(rows[0]!.parentLanes).toHaveLength(2);
    expect(rows.at(-1)!.after).toEqual([]);
  });
});

describe("format helpers", () => {
  it("formats sizes, ratios and names", () => {
    expect(formatBytes(0, "en")).toBe("0 B");
    expect(formatBytes(1536, "en")).toBe("1.5 KiB");
    expect(formatBytes(52428800, "en")).toBe("50 MiB");
    expect(dedupRatio(100, 25)).toBe(4);
    expect(dedupRatio(100, 0)).toBeNull();
    expect(baseName("C:\\Users\\ann\\proj\\")).toBe("proj");
    expect(firstLine("a\n\nb")).toBe("a");
  });
});
