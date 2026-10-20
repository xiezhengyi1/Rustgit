import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";

/**
 * A windowed list for very long lists (tens of thousands of rows): only the rows in view plus a
 * small overscan exist in the DOM, rows have a fixed height, and the keyboard moves a roving
 * "active" row (Up/Down/Home/End/PageUp/PageDown), scrolling it into view.
 */
export function VirtualList<T>({
  items,
  rowHeight,
  activeIndex,
  onActiveIndexChange,
  renderRow,
  label,
  maxHeight = 420,
  overscan = 8,
  fallbackViewport = 420,
  keyOf,
}: {
  items: readonly T[];
  rowHeight: number;
  activeIndex: number;
  onActiveIndexChange: (i: number) => void;
  /** `tabIndex` is 0 for the active row only (roving tabindex); put it on the row's main control. */
  renderRow: (item: T, index: number, ctx: { active: boolean; tabIndex: 0 | -1 }) => ReactNode;
  label: string;
  maxHeight?: number;
  overscan?: number;
  /** viewport height assumed when the element has no layout yet (first paint, jsdom) */
  fallbackViewport?: number;
  keyOf: (item: T, index: number) => string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(Math.min(maxHeight, fallbackViewport));
  const wantFocus = useRef(false);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setViewport(el.clientHeight || Math.min(maxHeight, fallbackViewport));
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [maxHeight, fallbackViewport]);

  const total = items.length * rowHeight;
  const first = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan);
  const last = Math.min(items.length, Math.ceil((scrollTop + viewport) / rowHeight) + overscan);

  const reveal = useCallback(
    (index: number) => {
      const el = ref.current;
      if (!el) return;
      const top = index * rowHeight;
      if (top < el.scrollTop) el.scrollTop = top;
      else if (top + rowHeight > el.scrollTop + viewport) el.scrollTop = top + rowHeight - viewport;
      setScrollTop(el.scrollTop);
    },
    [rowHeight, viewport],
  );

  // After a keyboard move the new row may not exist yet; focus it once it has been rendered.
  useEffect(() => {
    if (!wantFocus.current) return;
    const el = ref.current?.querySelector<HTMLElement>(`[data-index="${activeIndex}"] [data-row]`);
    if (el) {
      wantFocus.current = false;
      el.focus({ preventScroll: true });
    }
  });

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const page = Math.max(1, Math.floor(viewport / rowHeight) - 1);
    let next = activeIndex;
    switch (e.key) {
      case "ArrowDown": next = activeIndex + 1; break;
      case "ArrowUp": next = activeIndex - 1; break;
      case "PageDown": next = activeIndex + page; break;
      case "PageUp": next = activeIndex - page; break;
      case "Home": next = 0; break;
      case "End": next = items.length - 1; break;
      default: return;
    }
    // Only intercept when the focus is on a row control, not on a checkbox's own keys.
    if (!(e.target as HTMLElement).hasAttribute("data-row")) return;
    e.preventDefault();
    next = Math.max(0, Math.min(items.length - 1, next));
    if (next === activeIndex) return;
    wantFocus.current = true;
    onActiveIndexChange(next);
    reveal(next);
  };

  const rows: ReactNode[] = [];
  for (let i = first; i < last; i++) {
    const item = items[i]!;
    rows.push(
      <div
        key={keyOf(item, i)}
        role="listitem"
        aria-posinset={i + 1}
        aria-setsize={items.length}
        data-index={i}
        className={`vrow ${i === activeIndex ? "row-active" : ""}`}
        style={{ top: i * rowHeight, height: rowHeight }}
      >
        {renderRow(item, i, { active: i === activeIndex, tabIndex: i === activeIndex ? 0 : -1 })}
      </div>,
    );
  }

  return (
    <div
      ref={ref}
      role="list"
      aria-label={label}
      className="vlist"
      style={{ maxHeight, height: Math.min(total, maxHeight) }}
      onScroll={(e) => setScrollTop((e.target as HTMLDivElement).scrollTop)}
      onKeyDown={onKeyDown}
    >
      <div style={{ height: total, position: "relative" }}>{rows}</div>
    </div>
  );
}
