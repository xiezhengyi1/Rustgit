// Lane assignment for drawing a commit graph from `log` output (newest first).
import type { Commit } from "./types";

export interface GraphRow {
  commit: Commit;
  /** lane of the commit's node */
  lane: number;
  /** lanes carrying a line from the previous row, indexed by lane: the commit id expected there */
  before: (string | null)[];
  /** lanes carrying a line to the next row */
  after: (string | null)[];
  /** target lane for each parent (same order as commit.parents) */
  parentLanes: number[];
  /** total number of lanes used by this row, for sizing */
  width: number;
}

export function layoutGraph(commits: Commit[]): GraphRow[] {
  const rows: GraphRow[] = [];
  let active: (string | null)[] = [];
  for (const commit of commits) {
    const before = active.slice();
    let lane = active.indexOf(commit.id);
    if (lane === -1) {
      lane = active.indexOf(null);
      if (lane === -1) lane = active.length;
    }
    const next = active.slice();
    while (next.length <= lane) next.push(null);
    // Other lanes waiting for this commit converge into it.
    for (let i = 0; i < next.length; i++) if (next[i] === commit.id) next[i] = null;
    const parentLanes: number[] = [];
    commit.parents.forEach((p, idx) => {
      const existing = next.indexOf(p);
      if (existing !== -1) {
        parentLanes.push(existing);
        return;
      }
      if (idx === 0) {
        next[lane] = p;
        parentLanes.push(lane);
      } else {
        let free = next.indexOf(null);
        if (free === -1) free = next.length;
        while (next.length <= free) next.push(null);
        next[free] = p;
        parentLanes.push(free);
      }
    });
    while (next.length && next[next.length - 1] === null) next.pop();
    const width = Math.max(before.length, next.length, lane + 1);
    rows.push({ commit, lane, before, after: next.slice(), parentLanes, width });
    active = next;
  }
  return rows;
}
