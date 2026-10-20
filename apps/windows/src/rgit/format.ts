export function formatBytes(n: number, locale?: string): string {
  if (!Number.isFinite(n)) return "-";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let u = 0;
  while (Math.abs(v) >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  const digits = u === 0 || v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return `${new Intl.NumberFormat(locale, { maximumFractionDigits: digits }).format(v)} ${units[u]}`;
}

export function formatCommitTime(timestamp: number, tzOffsetMin: number, locale?: string): string {
  // Show the author's local time: shift by their offset and print as UTC.
  const d = new Date((timestamp + tzOffsetMin * 60) * 1000);
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "short", timeZone: "UTC" }).format(d);
}

export function firstLine(message: string): string {
  const i = message.indexOf("\n");
  return (i === -1 ? message : message.slice(0, i)).trim();
}

export function dedupRatio(logical: number, stored: number): number | null {
  if (!stored) return null;
  return logical / stored;
}

/** Last path segment, for display of project folders on any OS separator. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}
