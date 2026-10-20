import { useCallback, useMemo, useReducer, useRef } from "react";
import type { Change } from "../rgit/types";

export interface Selection {
  has(path: string): boolean;
  count: number;
  toggle(path: string): void;
  selectAll(): void;
  clear(): void;
  /** the live set, for planning a commit; do not mutate */
  set: ReadonlySet<string>;
}

/**
 * Per-file checkbox state for a (possibly huge) change list.
 * Toggling is O(1) (a mutable set plus a render tick), the count is kept incrementally, and
 * a refreshed change list is reconciled in one O(n) pass: paths the user unselected stay
 * unselected, new paths start selected, vanished paths are forgotten.
 */
export function useSelection(changes: readonly Change[]): Selection {
  const [, tick] = useReducer((n: number) => n + 1, 0);
  const sel = useRef(new Set<string>());
  const known = useRef<Set<string> | null>(null);
  const forChanges = useRef<readonly Change[] | null>(null);

  // Reconcile synchronously during render so the first paint after a refresh is already right.
  if (forChanges.current !== changes) {
    const prevKnown = known.current;
    const next = new Set<string>();
    const nowKnown = new Set<string>();
    for (const c of changes) {
      nowKnown.add(c.path);
      if (!prevKnown || !prevKnown.has(c.path) || sel.current.has(c.path)) next.add(c.path);
    }
    sel.current = next;
    known.current = nowKnown;
    forChanges.current = changes;
  }

  const toggle = useCallback((path: string) => {
    if (sel.current.has(path)) sel.current.delete(path);
    else sel.current.add(path);
    tick();
  }, []);
  const selectAll = useCallback(() => {
    const all = new Set<string>();
    for (const c of forChanges.current ?? []) all.add(c.path);
    sel.current = all;
    tick();
  }, []);
  const clear = useCallback(() => {
    sel.current = new Set();
    tick();
  }, []);

  const current = sel.current;
  // `current` changes identity on reconcile/selectAll/clear; its size is read live each render.
  return useMemo<Selection>(
    () => ({ has: (p) => sel.current.has(p), count: current.size, toggle, selectAll, clear, set: current }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [current, current.size, toggle, selectAll, clear, sel.current.size],
  );
}
