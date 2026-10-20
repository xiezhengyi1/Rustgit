// Parser for the unified diff text carried in `diff` / `show` results.
// The engine emits, per file: `--- a/<path>` (or /dev/null), `+++ b/<path>` (or /dev/null),
// then hunks; binary files are a single `Binary files a/<p> and b/<p> differ` line.

export type DiffLineKind = "context" | "add" | "del" | "meta";

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
  oldNo?: number;
  newNo?: number;
}

export interface FileDiff {
  path: string;
  binary: boolean;
  added: number;
  removed: number;
  lines: DiffLine[];
}

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

function stripPrefix(p: string): string {
  if (p === "/dev/null") return p;
  return p.replace(/^[ab]\//, "");
}

export function parseUnifiedDiff(text: string): FileDiff[] {
  const files: FileDiff[] = [];
  if (!text) return files;
  const rows = text.split("\n");
  if (rows.length && rows[rows.length - 1] === "") rows.pop();
  let cur: FileDiff | null = null;
  let oldNo = 0;
  let newNo = 0;
  for (let i = 0; i < rows.length; i++) {
    const row = (rows[i] ?? "").replace(/\r$/, "");
    const bin = /^Binary files (?:a\/)?(.*?) and (?:b\/)?.* differ$/.exec(row);
    if (bin) {
      files.push({ path: bin[1] ?? "", binary: true, added: 0, removed: 0, lines: [{ kind: "meta", text: row }] });
      cur = null;
      continue;
    }
    const nextRow = (rows[i + 1] ?? "").replace(/\r$/, "");
    if (row.startsWith("--- ") && nextRow.startsWith("+++ ")) {
      const from = stripPrefix(row.slice(4).split("\t")[0] ?? "");
      const to = stripPrefix(nextRow.slice(4).split("\t")[0] ?? "");
      cur = { path: to === "/dev/null" ? from : to, binary: false, added: 0, removed: 0, lines: [] };
      files.push(cur);
      i++;
      continue;
    }
    if (!cur) continue;
    const h = HUNK.exec(row);
    if (h) {
      oldNo = Number(h[1]);
      newNo = Number(h[2]);
      cur.lines.push({ kind: "meta", text: row });
    } else if (row.startsWith("+")) {
      cur.lines.push({ kind: "add", text: row.slice(1), newNo: newNo++ });
      cur.added++;
    } else if (row.startsWith("-")) {
      cur.lines.push({ kind: "del", text: row.slice(1), oldNo: oldNo++ });
      cur.removed++;
    } else if (row.startsWith("\\")) {
      cur.lines.push({ kind: "meta", text: row });
    } else {
      cur.lines.push({ kind: "context", text: row.startsWith(" ") ? row.slice(1) : row, oldNo: oldNo++, newNo: newNo++ });
    }
  }
  return files;
}

/** Diff of a single path out of a combined diff (renames: look the new path up). */
export function diffForPath(files: FileDiff[], path: string): FileDiff | undefined {
  return files.find((f) => f.path === path);
}
