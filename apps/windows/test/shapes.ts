// Tiny structural checker so contract tests fail when the engine's JSON drifts from
// src/rgit/types.ts (field names and primitive types; extra fields are allowed).
export type Shape = "string" | "number" | "boolean" | "any" | { optional: Shape } | { nullable: Shape } | { array: Shape } | { [k: string]: Shape };

export function check(value: unknown, shape: Shape, path = "$"): string[] {
  if (shape === "any") return [];
  if (typeof shape === "string") return typeof value === shape ? [] : [`${path}: expected ${shape}, got ${describe(value)}`];
  if ("optional" in shape && Object.keys(shape).length === 1) return value === undefined ? [] : check(value, shape.optional as Shape, path);
  if ("nullable" in shape && Object.keys(shape).length === 1) return value === null ? [] : check(value, shape.nullable as Shape, path);
  if ("array" in shape && Object.keys(shape).length === 1) {
    if (!Array.isArray(value)) return [`${path}: expected array, got ${describe(value)}`];
    return value.flatMap((v, i) => check(v, shape.array as Shape, `${path}[${i}]`));
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return [`${path}: expected object, got ${describe(value)}`];
  const o = value as Record<string, unknown>;
  const errs: string[] = [];
  for (const [k, s] of Object.entries(shape)) {
    const isOptional = typeof s === "object" && "optional" in s && Object.keys(s).length === 1;
    if (!(k in o)) {
      if (!isOptional) errs.push(`${path}.${k}: missing`);
      continue;
    }
    errs.push(...check(o[k], s as Shape, `${path}.${k}`));
  }
  return errs;
}

function describe(v: unknown): string {
  return v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
}

const id = "string";
export const commitShape: Shape = {
  id, short: "string", parents: { array: "string" }, author_name: "string", author_email: "string",
  timestamp: "number", tz_offset_min: "number", message: "string", branches: { array: "string" },
};
const change: Shape = { path: "string", kind: "string", from: { optional: "string" } };
const mergeLike: Shape = { commit: { nullable: "string" }, fast_forward: "boolean", already_up_to_date: "boolean", conflicts: { array: "string" }, restored: { optional: { array: "string" } } };
const stats: Shape = { logical_bytes: "number", objects_already_complete: "number", objects_resumed: "number", objects_transferred: "number", wire_bytes_received: "number", wire_bytes_sent: "number" };
const update: Shape = { ref: "string", new: { nullable: "string" }, old: { optional: { nullable: "string" } } };

export const shapes: Record<string, Shape> = {
  version: { cli: "string", json_interface: "number", format: "number" },
  init: { uuid: "string", root: "string" },
  status: {
    branch: "string", head: { nullable: id }, changes: { array: change },
    skipped: { array: { path: "string", reason: "string" } },
    merge: { nullable: { kind: "string", label: "string" } },
    conflicts: { array: { path: "string", kind: "string", resolved: "boolean" } },
    ahead: { optional: "number" }, behind: { optional: "number" },
  },
  commit: { id, short: "string", branch: "string" },
  log: { commits: { array: commitShape } },
  show: { commit: commitShape, changes: { array: change }, diff: "string" },
  diff: { changes: { array: change }, diff: "string" },
  branches: { current: "string", branches: { array: { name: "string", id, short: "string" } } },
  branchRef: { name: "string", id },
  switch: { branch: "string" },
  mergeLike,
  resolve: { path: "string" },
  shelve: { name: "string" },
  shelves: { shelves: { array: { name: "string", ref: "string", id, message: "string" } }, recovery: { array: "any" } },
  stats: {
    commits: "number", branches: "number", objects: "number", logical_bytes: "number", stored_bytes: "number",
    delta_objects: "number", pack_files: "number", pack_bytes: "number", index_bytes: "number", repo_bytes: "number",
  },
  verify: { objects_checked: "number", commits: "number", problems: { array: "string" } },
  gc: { moved: "number", dropped: "number", freed_bytes: "number", repo_bytes_before: "number", repo_bytes_after: "number" },
  reindex: { objects: "number", packs: "number", problems: { array: "any" } },
  remotes: { remotes: { array: { name: "string", url: "string" } } },
  push: { already_applied: "boolean", up_to_date: "boolean", stats, updated: { array: update } },
  fetch: { state: "string", up_to_date: "boolean", stats, updated: { array: update } },
  pull: { fetch: { state: "string", up_to_date: "boolean", stats, updated: { array: update } }, merge: mergeLike },
  clone: { branch: "string", dest: "string", head: { nullable: id } },
};
