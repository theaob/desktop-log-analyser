// Edits to a query's syntax tree. The visual builder, click-to-filter and the label sidebar
// all go through these, then format the tree back to text with the Rust formatter, so the
// text query stays the single source of truth.

import { api, type Matcher, type Query, type Stage } from "./api";

export async function editQuery(text: string, edit: (q: Query) => Query): Promise<string | null> {
  const parsed = await api.parseQuery(text);
  if (!parsed.query) return null;
  return api.formatQuery(edit(parsed.query));
}

/** Adds `name="value"` (or `!=`) to the selector, replacing an existing positive matcher. */
export function withLabel(q: Query, name: string, value: string, negate: boolean): Query {
  let selector = q.selector.filter((m) => !(m.name === name && (m.op === "=" || m.op === "!=") && m.value === value));
  if (!negate) selector = selector.filter((m) => !(m.name === name && m.op === "="));
  selector = [...selector, { name, op: negate ? "!=" : "=", value }];
  return { ...q, selector };
}

/** Adds `| name="value"` (or `!=`) after the parsers, for fields extracted at query time. */
export function withFieldFilter(q: Query, name: string, value: string, negate: boolean): Query {
  const stage: Stage = {
    type: "filter",
    expr: { type: "cmp", name, op: negate ? "!=" : "==", value: { type: "string", value }, text: "" },
  };
  return { ...q, stages: [...q.stages, stage] };
}

export function withLineFilter(q: Query, value: string, negate: boolean): Query {
  return { ...q, stages: [...q.stages, { type: "line", op: negate ? "!=" : "|=", values: [value] }] };
}

export function setSelector(q: Query, selector: Matcher[]): Query {
  return { ...q, selector };
}

/** Terms to highlight in result rows: `|=` strings and `|~` regexes. */
export function highlightTerms(q: Query | null): RegExp | null {
  if (!q) return null;
  const parts: string[] = [];
  for (const s of q.stages) {
    if (s.type !== "line") continue;
    if (s.op === "|=") parts.push(...s.values.filter(Boolean).map(escapeRegExp));
    if (s.op === "|~") {
      for (const v of s.values) {
        try {
          // Rust and JS regex syntax overlap for common cases; skip what JS rejects.
          new RegExp(v);
          parts.push(v.replace(/\(\?P</g, "(?<"));
        } catch {
          /* not highlightable */
        }
      }
    }
  }
  if (!parts.length) return null;
  try {
    return new RegExp(parts.map((p) => `(?:${p})`).join("|"), "g");
  } catch {
    return null;
  }
}

export function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
