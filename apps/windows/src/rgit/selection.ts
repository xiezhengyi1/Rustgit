import type { Change } from "./types";

/** How to tell `rgit commit` which files go into the commit. */
export type CommitSelection =
  | { kind: "all" }
  | { kind: "only"; paths: string[] }
  | { kind: "except"; paths: string[] };

/**
 * Turn the user's checkbox state into the smallest description of it.
 * Everything selected -> "all" (no list at all); a majority -> list the few that are left out;
 * a minority -> list the few that are in. A rename names both its new and its old path.
 * O(n) in the number of changes, whatever the selection.
 */
export function planSelection(changes: readonly Change[], selected: ReadonlySet<string>): CommitSelection {
  let chosen = 0;
  for (const c of changes) if (selected.has(c.path)) chosen++;
  if (chosen === changes.length) return { kind: "all" };
  const wantSelected = chosen * 2 <= changes.length;
  const out: string[] = [];
  for (const c of changes) {
    if (selected.has(c.path) !== wantSelected) continue;
    out.push(c.path);
    if (c.kind === "renamed" && c.from) out.push(c.from);
  }
  return wantSelected ? { kind: "only", paths: out } : { kind: "except", paths: out };
}

/** Lists up to this many short paths still go on the command line; anything bigger goes through stdin. */
export const MAX_ARGV_PATHS = 40;
export const MAX_ARGV_CHARS = 2000;

/** Command-line arguments and stdin text for `rgit commit` (after `-m <message>`). */
export function commitArgs(sel: CommitSelection): { args: string[]; stdin?: string } {
  if (sel.kind === "all") return { args: [] };
  const joined = sel.paths.join("\n");
  const small = sel.paths.length <= MAX_ARGV_PATHS && joined.length <= MAX_ARGV_CHARS;
  if (sel.kind === "only" && small && sel.paths.every((p) => !p.includes("\n"))) return { args: sel.paths };
  // Windows limits a command line to about 32k characters, so big lists never go through argv.
  return { args: [sel.kind === "only" ? "--paths-from" : "--except-from", "-"], stdin: joined + "\n" };
}
