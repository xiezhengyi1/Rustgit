import { describe, expect, it } from "vitest";
import { MAX_ARGV_CHARS, commitArgs, planSelection } from "../src/rgit/selection";
import type { Change } from "../src/rgit/types";

const many = (n: number): Change[] => Array.from({ length: n }, (_, i) => ({ path: `src/module${i % 300}/file_${i}.rs`, kind: "modified" as const }));

describe("planSelection", () => {
  const changes: Change[] = [
    { path: "a", kind: "modified" },
    { path: "b2", kind: "renamed", from: "b" },
    { path: "c", kind: "added" },
    { path: "d", kind: "deleted" },
  ];
  it("everything selected means no list at all", () => {
    expect(planSelection(changes, new Set(["a", "b2", "c", "d"]))).toEqual({ kind: "all" });
  });
  it("a minority is listed as 'only', a rename names both paths", () => {
    expect(planSelection(changes, new Set(["b2"]))).toEqual({ kind: "only", paths: ["b2", "b"] });
  });
  it("a majority is described by what is left out", () => {
    expect(planSelection(changes, new Set(["a", "c", "d"]))).toEqual({ kind: "except", paths: ["b2", "b"] });
  });
  it("30,000 changes: plans in linear time and picks the shorter list", () => {
    const ch = many(30_000);
    const all = new Set(ch.map((c) => c.path));
    all.delete(ch[7]!.path);
    const t0 = performance.now();
    const plan = planSelection(ch, all);
    const ms = performance.now() - t0;
    expect(plan).toEqual({ kind: "except", paths: [ch[7]!.path] });
    const few = new Set([ch[1]!.path, ch[29_999]!.path]);
    expect(planSelection(ch, few)).toEqual({ kind: "only", paths: [ch[1]!.path, ch[29_999]!.path] });
    console.log(`planSelection(30k): ${ms.toFixed(1)} ms`);
    expect(ms).toBeLessThan(250);
  });
});

describe("commitArgs", () => {
  it("keeps a few short paths on the command line", () => {
    expect(commitArgs({ kind: "only", paths: ["a", "b"] })).toEqual({ args: ["a", "b"] });
    expect(commitArgs({ kind: "all" })).toEqual({ args: [] });
  });
  it("never puts a long list on the command line (Windows limit is ~32k characters)", () => {
    const paths = many(30_000).map((c) => c.path);
    const only = commitArgs({ kind: "only", paths });
    expect(only.args).toEqual(["--paths-from", "-"]);
    expect(only.stdin!.split("\n").filter(Boolean)).toHaveLength(30_000);
    const except = commitArgs({ kind: "except", paths: paths.slice(0, 29_990) });
    expect(except.args).toEqual(["--except-from", "-"]);
    expect(except.args.join(" ").length).toBeLessThan(MAX_ARGV_CHARS);
  });
  it("a few very long paths also go through stdin", () => {
    const long = "x".repeat(MAX_ARGV_CHARS + 1);
    expect(commitArgs({ kind: "only", paths: [long] }).args).toEqual(["--paths-from", "-"]);
  });
  it("'except' always uses stdin so a short exclusion list cannot be misread as an inclusion list", () => {
    expect(commitArgs({ kind: "except", paths: ["a"] })).toEqual({ args: ["--except-from", "-"], stdin: "a\n" });
  });
});
