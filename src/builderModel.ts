// The visual builder's editable model of a query, and the conversions to and from the
// syntax tree. Kept free of React and Tauri so it can be unit tested.

import type { FieldExpr, LineOp, MatchOp, Query, Row, Stage } from "./api";

export type CmpOp = "==" | "!=" | "=~" | "!~" | ">" | ">=" | "<" | "<=";
export type ParserKind = "json" | "logfmt" | "regexp";

export interface LabelRow {
  id: number;
  name: string;
  op: MatchOp;
  value: string;
}

export type Op =
  | { id: number; kind: "line"; op: LineOp; values: string[] }
  | { id: number; kind: "parser"; parser: ParserKind; pattern: string }
  | { id: number; kind: "field"; name: string; op: CmpOp; value: string; numeric: boolean }
  /** A filter the builder can't edit field by field (`and`/`or`); kept as is. */
  | { id: number; kind: "expr"; expr: FieldExpr };

export interface Model {
  labels: LabelRow[];
  ops: Op[];
}

let nextId = 1;
export const newId = () => nextId++;

export const MATCH_OPS: { op: MatchOp; label: string; hint: string }[] = [
  { op: "=", label: "=", hint: "equals" },
  { op: "!=", label: "!=", hint: "does not equal" },
  { op: "=~", label: "=~", hint: "matches regex" },
  { op: "!~", label: "!~", hint: "does not match regex" },
];

export const LINE_OPS: { op: LineOp; label: string }[] = [
  { op: "|=", label: "Line contains" },
  { op: "!=", label: "Line does not contain" },
  { op: "|~", label: "Line matches regex" },
  { op: "!~", label: "Line does not match regex" },
];

export const CMP_OPS: { op: CmpOp; label: string }[] = [
  { op: "==", label: "==" },
  { op: "!=", label: "!=" },
  { op: "=~", label: "=~ regex" },
  { op: "!~", label: "!~ regex" },
  { op: ">", label: ">" },
  { op: ">=", label: ">=" },
  { op: "<", label: "<" },
  { op: "<=", label: "<=" },
];

export const isOrdering = (op: CmpOp) => op === ">" || op === ">=" || op === "<" || op === "<=";
export const isRegexOp = (op: string) => op === "=~" || op === "!~" || op === "|~";

const NUMBER = /^-?\d+(\.\d+)?$/;
// Durations (`250ms`, `1h30m`) and byte sizes (`10MB`, `512KiB`), as the Rust parser takes them.
const DURATION = /^-?(\d+(\.\d+)?(ns|us|µs|ms|s|m|h|d))+$/;
const BYTES = /^-?\d+(\.\d+)?(b|kb|mb|gb|tb|kib|k|mib|m|gib|g|tib)$/i;

/** Whether `value` is a literal the query language accepts after `>`, `<` and friends. */
export function isNumericLiteral(value: string): boolean {
  const v = value.trim();
  return NUMBER.test(v) || DURATION.test(v) || BYTES.test(v);
}

function valueText(expr: Extract<FieldExpr, { type: "cmp" }>): string {
  const { value, text } = expr;
  if (value.type === "string") return String(value.value);
  if (text) return text;
  if (value.type === "duration") return `${value.value}ms`;
  if (value.type === "bytes") return `${value.value}B`;
  return String(value.value);
}

export function toModel(q: Query): Model {
  const labels = q.selector.map((m) => ({ id: newId(), ...m }));
  const ops = q.stages.map((s): Op => {
    const id = newId();
    switch (s.type) {
      case "line":
        return { id, kind: "line", op: s.op, values: [...s.values] };
      case "json":
      case "logfmt":
        return { id, kind: "parser", parser: s.type, pattern: "" };
      case "regexp":
        return { id, kind: "parser", parser: "regexp", pattern: s.pattern };
      case "filter":
        if (s.expr.type === "cmp") {
          const op = s.expr.op as CmpOp;
          return { id, kind: "field", name: s.expr.name, op, value: valueText(s.expr), numeric: s.expr.value.type !== "string" };
        }
        return { id, kind: "expr", expr: s.expr };
    }
  });
  return { labels, ops };
}

/** Whether an operation is complete enough to go into the query. */
export function isComplete(op: Op): boolean {
  switch (op.kind) {
    case "line":
      return op.values.some((v) => v !== "");
    case "parser":
      return op.parser !== "regexp" || op.pattern !== "";
    case "field":
      if (!op.name || op.value === "") return false;
      return !(isOrdering(op.op) || op.numeric) || isNumericLiteral(op.value);
    case "expr":
      return true;
  }
}

function toStage(op: Op): Stage {
  switch (op.kind) {
    case "line":
      return { type: "line", op: op.op, values: op.values.filter((v) => v !== "") };
    case "parser":
      return op.parser === "regexp" ? { type: "regexp", pattern: op.pattern } : { type: op.parser };
    case "field": {
      const numeric = !isRegexOp(op.op) && (isOrdering(op.op) || op.numeric);
      const raw = op.value.trim();
      // Durations and byte sizes go through `text`, which the formatter writes verbatim.
      const value = numeric
        ? NUMBER.test(raw)
          ? { expr: { type: "number", value: Number(raw) }, text: "" }
          : { expr: { type: "number", value: 0 }, text: raw }
        : { expr: { type: "string", value: op.value }, text: "" };
      return { type: "filter", expr: { type: "cmp", name: op.name, op: op.op, value: value.expr, text: value.text } };
    }
    case "expr":
      return { type: "filter", expr: op.expr };
  }
}

/** The query the model stands for; rows that are still being filled in are left out. */
export function toQuery(m: Model): Query {
  return {
    selector: m.labels.filter((r) => r.name && r.value !== "").map(({ name, op, value }) => ({ name, op, value })),
    stages: m.ops.filter(isComplete).map(toStage),
  };
}

export function move<T>(list: T[], from: number, to: number): T[] {
  if (to < 0 || to >= list.length || from === to) return list;
  const out = [...list];
  const [item] = out.splice(from, 1);
  out.splice(to, 0, item);
  return out;
}

/** Display text for a filter the builder shows read-only. */
export function exprText(e: FieldExpr): string {
  if (e.type === "cmp") {
    const v = valueText(e);
    return `${e.name} ${e.op} ${e.value.type === "string" ? JSON.stringify(v) : v}`;
  }
  return e.type === "and" ? `${exprText(e.left)} and ${exprText(e.right)}` : `(${exprText(e.left)} or ${exprText(e.right)})`;
}

/** Named capture groups in a `regexp` stage pattern; these become fields. */
export function regexpGroups(pattern: string): string[] {
  return [...pattern.matchAll(/\(\?P?<([A-Za-z_][A-Za-z0-9_]*)>/g)].map((m) => m[1]);
}

export function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Picking a suggested value: plain operators take it as is; regex operators add it as
 * another alternative, so several values can be picked like Grafana's multi-select.
 */
export function pickValue(op: string, current: string, picked: string): string {
  if (!isRegexOp(op)) return picked;
  const alt = escapeRegex(picked);
  if (!current) return alt;
  if (current.split("|").includes(alt)) return current;
  return `${current}|${alt}`;
}

export interface Suggestion {
  value: string;
  count?: number;
}

/** Field names and their most common values in the rows on screen, for field filters. */
export function fieldSuggestions(rows: Row[]): Map<string, Suggestion[]> {
  const counts = new Map<string, Map<string, number>>();
  for (const row of rows) {
    for (const [k, v] of [...row.parsed, ...row.fields]) {
      let m = counts.get(k);
      if (!m) counts.set(k, (m = new Map()));
      if (m.size < 1000 || m.has(v)) m.set(v, (m.get(v) ?? 0) + 1);
    }
  }
  const out = new Map<string, Suggestion[]>();
  for (const [k, m] of counts) {
    const values = [...m].sort((a, b) => b[1] - a[1]).map(([value, count]) => ({ value, count }));
    out.set(k, values);
  }
  return out;
}
