// Time range model and formatting. Relative ranges are anchored to the newest event in the
// folder rather than "now": log folders are usually analysed after the fact.

export type TimeRange =
  | { kind: "all" }
  | { kind: "relative"; ms: number; label: string }
  | { kind: "absolute"; from: number; to: number };

export const PRESETS: { ms: number; label: string }[] = [
  { ms: 5 * 60_000, label: "Last 5 minutes" },
  { ms: 15 * 60_000, label: "Last 15 minutes" },
  { ms: 60 * 60_000, label: "Last 1 hour" },
  { ms: 6 * 3_600_000, label: "Last 6 hours" },
  { ms: 24 * 3_600_000, label: "Last 24 hours" },
  { ms: 7 * 86_400_000, label: "Last 7 days" },
];

/** Resolves a range to [from, to) in epoch ms; nulls mean unbounded. */
export function resolveRange(r: TimeRange, maxTs: number | null): { from: number | null; to: number | null } {
  switch (r.kind) {
    case "all":
      return { from: null, to: null };
    case "relative": {
      const end = (maxTs ?? Date.now()) + 1;
      return { from: end - r.ms, to: end };
    }
    case "absolute":
      return { from: r.from, to: r.to };
  }
}

const pad = (n: number, w = 2) => String(n).padStart(w, "0");

export function formatTs(ms: number, withMs = true): string {
  const d = new Date(ms);
  const s = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return withMs ? `${s}.${pad(d.getMilliseconds(), 3)}` : s;
}

/** Parses "yyyy-MM-dd HH:mm[:ss[.SSS]]" in local time. */
export function parseTs(text: string): number | null {
  const m = text.trim().match(/^(\d{4})-(\d{2})-(\d{2})(?:[ T](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?)?$/);
  if (!m) return null;
  const [, y, mo, d, h = "0", mi = "0", s = "0", f = "0"] = m;
  const date = new Date(+y, +mo - 1, +d, +h, +mi, +s, +f.padEnd(3, "0"));
  return isNaN(date.getTime()) ? null : date.getTime();
}

export function rangeLabel(r: TimeRange): string {
  switch (r.kind) {
    case "all":
      return "Whole folder";
    case "relative":
      return `${r.label} of logs`;
    case "absolute":
      return `${formatTs(r.from, false)} to ${formatTs(r.to, false)}`;
  }
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  return `${Math.floor(ms / 60_000)} min ${Math.round((ms % 60_000) / 1000)} s`;
}

export function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}
