import { useEffect, useMemo, useRef, useState } from "react";
import { api, type LineOp, type MatchOp } from "../api";
import {
  CMP_OPS,
  LINE_OPS,
  MATCH_OPS,
  exprText,
  fieldSuggestions,
  isNumericLiteral,
  isOrdering,
  move,
  newId,
  pickValue,
  regexpGroups,
  toModel,
  toQuery,
  type CmpOp,
  type LabelRow,
  type Model,
  type Op,
  type ParserKind,
  type Suggestion,
} from "../builderModel";
import { useStore } from "../store";
import Combobox from "./Combobox";

type AddKind = `line:${LineOp}` | `parser:${ParserKind}` | "field";

const PARSERS: { kind: ParserKind; label: string; hint: string }[] = [
  { kind: "json", label: "JSON", hint: "Extracts fields from JSON lines so later filters can use them." },
  { kind: "logfmt", label: "logfmt", hint: "Extracts key=value pairs as fields." },
  { kind: "regexp", label: "Regexp", hint: "Extracts named groups as fields, e.g. took (?P<ms>\\d+)ms." },
];

const LINE_HINTS: Record<LineOp, string> = {
  "|=": "Keeps lines that contain any of these texts.",
  "!=": "Drops lines that contain any of these texts.",
  "|~": "Keeps lines that match any of these regexes.",
  "!~": "Drops lines that match any of these regexes.",
};

const NUMERIC_HINT = "Use a number, a duration (250ms, 1m) or a size (10MB)";

/**
 * Visual query builder in the style of Grafana's Loki builder: label filters, then an
 * ordered pipeline of line filters, parsers and field filters. Every edit is formatted back
 * to text, so the builder and the code editor always show the same query.
 */
export default function QueryBuilder() {
  const queryText = useStore((s) => s.queryText);
  const setQueryText = useStore((s) => s.setQueryText);
  const labels = useStore((s) => s.labels);
  const rows = useStore((s) => s.rows);
  const [model, setModel] = useState<Model>({ labels: [], ops: [] });
  const [broken, setBroken] = useState(false);
  const [focusId, setFocusId] = useState<number | null>(null);
  const lastCommitted = useRef<string | null>(null);
  const commitSeq = useRef(0);

  // Text changed elsewhere (code editor, sidebar, a click in the log list): rebuild the model.
  useEffect(() => {
    if (queryText === lastCommitted.current) return;
    let live = true;
    api.parseQuery(queryText).then((p) => {
      if (!live || queryText !== useStore.getState().queryText) return;
      if (!p.query) {
        setBroken(true);
        return;
      }
      setBroken(false);
      lastCommitted.current = queryText;
      setModel(toModel(p.query));
    });
    return () => {
      live = false;
    };
  }, [queryText]);

  const commit = (next: Model) => {
    setModel(next);
    const my = ++commitSeq.current;
    void api.formatQuery(toQuery(next)).then((text) => {
      if (my !== commitSeq.current) return;
      lastCommitted.current = text;
      if (text !== useStore.getState().queryText) setQueryText(text);
    });
  };

  const labelNames = useMemo<Suggestion[]>(() => labels.map((l) => ({ value: l.name })), [labels]);
  const labelValues = useMemo(() => new Map(labels.map((l) => [l.name, l.values.map(([value, count]) => ({ value, count }))])), [labels]);

  // Field names seen so far stay suggested even when the current filters hide every row.
  const seenFields = useRef(new Map<string, Suggestion[]>());
  const fieldValues = useMemo(() => {
    for (const [k, v] of fieldSuggestions(rows)) seenFields.current.set(k, v);
    return new Map(seenFields.current);
  }, [rows]);
  const fieldNames = useMemo<Suggestion[]>(() => {
    const names = new Set<string>();
    for (const op of model.ops) if (op.kind === "parser" && op.parser === "regexp") regexpGroups(op.pattern).forEach((g) => names.add(g));
    for (const k of fieldValues.keys()) names.add(k);
    for (const l of labels) names.add(l.name);
    return [...names].map((value) => ({ value }));
  }, [model.ops, fieldValues, labels]);

  if (broken) {
    return (
      <div className="builder">
        <p className="hint">The query has a syntax error, so the builder can't show it. Fix it in Code mode, or start over.</p>
        <div className="row">
          <button onClick={() => setQueryText("{}", true)}>Clear query</button>
        </div>
      </div>
    );
  }

  const setLabel = (i: number, patch: Partial<LabelRow>) =>
    commit({ ...model, labels: model.labels.map((r, j) => (j === i ? { ...r, ...patch } : r)) });
  const setOp = (i: number, op: Op) => commit({ ...model, ops: model.ops.map((o, j) => (j === i ? op : o)) });

  const addLabel = () => {
    const used = new Set(model.labels.map((r) => r.name));
    const name = labels.find((l) => !used.has(l.name))?.name ?? "";
    const id = newId();
    setFocusId(id);
    // Not committed yet: an empty row doesn't change the query.
    setModel({ ...model, labels: [...model.labels, { id, name, op: "=", value: "" }] });
  };

  const addOp = (kind: AddKind) => {
    const id = newId();
    let op: Op;
    let at = model.ops.length;
    if (kind.startsWith("line:")) {
      op = { id, kind: "line", op: kind.slice(5) as LineOp, values: [""] };
      // Line filters are cheapest before any parser runs, so they go after the leading ones.
      const firstOther = model.ops.findIndex((o) => o.kind !== "line");
      if (firstOther >= 0) at = firstOther;
    } else if (kind.startsWith("parser:")) {
      op = { id, kind: "parser", parser: kind.slice(7) as ParserKind, pattern: "" };
    } else {
      op = { id, kind: "field", name: "", op: "==", value: "", numeric: false };
    }
    setFocusId(id);
    const ops = [...model.ops];
    ops.splice(at, 0, op);
    commit({ ...model, ops });
  };

  return (
    <div className="builder">
      <div className="builder-section">
        <div className="builder-section-title">Label filters</div>
        <div className="builder-labels">
          {model.labels.map((r, i) => (
            <div className="builder-chip" key={r.id}>
              <Combobox
                className="name"
                value={r.name}
                placeholder="label"
                suggestions={labelNames}
                autoFocus={r.id === focusId && !r.name}
                onChange={(name) => setLabel(i, { name })}
                onPick={(name) => setLabel(i, { name, value: name === r.name ? r.value : "" })}
              />
              <select value={r.op} title={MATCH_OPS.find((o) => o.op === r.op)?.hint} onChange={(e) => setLabel(i, { op: e.target.value as MatchOp })}>
                {MATCH_OPS.map((o) => (
                  <option key={o.op} value={o.op} title={o.hint}>
                    {o.label}
                  </option>
                ))}
              </select>
              <Combobox
                className="value"
                value={r.value}
                placeholder={r.op === "=~" || r.op === "!~" ? "regex (pick several)" : "value"}
                suggestions={labelValues.get(r.name) ?? []}
                autoFocus={r.id === focusId && !!r.name}
                onChange={(value) => setLabel(i, { value })}
                onPick={(v) => setLabel(i, { value: pickValue(r.op, r.value, v) })}
              />
              <button className="icon" title="Remove filter" onClick={() => commit({ ...model, labels: model.labels.filter((_, j) => j !== i) })}>
                ×
              </button>
            </div>
          ))}
          <button onClick={addLabel}>+ Label filter</button>
        </div>
      </div>

      <div className="builder-section">
        <div className="builder-section-title">Operations</div>
        <div className="builder-ops">
          {model.ops.map((op, i) => (
            <OpCard
              key={op.id}
              op={op}
              index={i}
              count={model.ops.length}
              autoFocus={op.id === focusId}
              fieldNames={fieldNames}
              fieldValues={fieldValues}
              onChange={(o) => setOp(i, o)}
              onMove={(to) => commit({ ...model, ops: move(model.ops, i, to) })}
              onRemove={() => commit({ ...model, ops: model.ops.filter((_, j) => j !== i) })}
            />
          ))}
          <select
            className="add-op"
            value=""
            onChange={(e) => {
              if (e.target.value) addOp(e.target.value as AddKind);
            }}
          >
            <option value="">+ Operation</option>
            <optgroup label="Line filters">
              {LINE_OPS.map((o) => (
                <option key={o.op} value={`line:${o.op}`}>
                  {o.label}
                </option>
              ))}
            </optgroup>
            <optgroup label="Parsers">
              {PARSERS.map((p) => (
                <option key={p.kind} value={`parser:${p.kind}`}>
                  {p.label}
                </option>
              ))}
            </optgroup>
            <optgroup label="Filters">
              <option value="field">Field filter</option>
            </optgroup>
          </select>
        </div>
      </div>

      <div className="builder-footer">
        <code className="query-preview">{queryText}</code>
        <button className="ghost" title="Remove all filters and operations" onClick={() => commit({ labels: [], ops: [] })}>
          Clear
        </button>
      </div>
    </div>
  );
}

interface OpCardProps {
  op: Op;
  index: number;
  count: number;
  autoFocus: boolean;
  fieldNames: Suggestion[];
  fieldValues: Map<string, Suggestion[]>;
  onChange: (op: Op) => void;
  onMove: (to: number) => void;
  onRemove: () => void;
}

function OpCard({ op, index, count, autoFocus, fieldNames, fieldValues, onChange, onMove, onRemove }: OpCardProps) {
  let title: string;
  let hint: string;
  let body: React.ReactNode;

  switch (op.kind) {
    case "line":
      title = "Line filter";
      hint = LINE_HINTS[op.op];
      body = (
        <>
          <select value={op.op} onChange={(e) => onChange({ ...op, op: e.target.value as LineOp })}>
            {LINE_OPS.map((o) => (
              <option key={o.op} value={o.op}>
                {o.label}
              </option>
            ))}
          </select>
          {op.values.map((v, j) => (
            <span className="line-value" key={j}>
              {j > 0 && <span className="muted">or</span>}
              <input
                value={v}
                spellCheck={false}
                autoFocus={autoFocus && j === op.values.length - 1}
                placeholder={op.op === "|~" || op.op === "!~" ? "regex" : "text"}
                onChange={(e) => onChange({ ...op, values: op.values.map((x, k) => (k === j ? e.target.value : x)) })}
              />
              {op.values.length > 1 && (
                <button className="icon" title="Remove alternative" onClick={() => onChange({ ...op, values: op.values.filter((_, k) => k !== j) })}>
                  ×
                </button>
              )}
            </span>
          ))}
          <button className="link" title="Also match another text" onClick={() => onChange({ ...op, values: [...op.values, ""] })}>
            + or
          </button>
        </>
      );
      break;
    case "parser": {
      const p = PARSERS.find((x) => x.kind === op.parser)!;
      title = "Parser";
      hint = p.hint;
      body = (
        <>
          <select value={op.parser} onChange={(e) => onChange({ ...op, parser: e.target.value as ParserKind })}>
            {PARSERS.map((x) => (
              <option key={x.kind} value={x.kind}>
                {x.label}
              </option>
            ))}
          </select>
          {op.parser === "regexp" && (
            <input
              className="grow"
              value={op.pattern}
              spellCheck={false}
              autoFocus={autoFocus}
              placeholder="pattern with named groups, e.g. user=(?P<user>\w+)"
              onChange={(e) => onChange({ ...op, pattern: e.target.value })}
            />
          )}
        </>
      );
      break;
    }
    case "field": {
      const numeric = isOrdering(op.op) || (op.numeric && op.op !== "=~" && op.op !== "!~");
      const invalid = numeric && op.value !== "" && !isNumericLiteral(op.value);
      title = "Field filter";
      hint = "Keeps lines whose field matches. Fields come from labels and from parsers placed earlier.";
      body = (
        <>
          <Combobox
            className="name"
            value={op.name}
            placeholder="field"
            suggestions={fieldNames}
            autoFocus={autoFocus}
            onChange={(name) => onChange({ ...op, name })}
          />
          <select value={op.op} onChange={(e) => onChange({ ...op, op: e.target.value as CmpOp })}>
            {CMP_OPS.map((o) => (
              <option key={o.op} value={o.op}>
                {o.label}
              </option>
            ))}
          </select>
          <Combobox
            className="value"
            value={op.value}
            placeholder={numeric ? "number, 250ms, 10MB" : "value"}
            invalid={invalid}
            title={invalid ? NUMERIC_HINT : undefined}
            suggestions={fieldValues.get(op.name) ?? []}
            onChange={(value) => onChange({ ...op, value })}
            onPick={(v) => onChange({ ...op, value: pickValue(op.op, op.value, v) })}
          />
          {invalid && <span className="field-error">{NUMERIC_HINT}</span>}
        </>
      );
      break;
    }
    case "expr":
      title = "Field filter";
      hint = "Combined conditions are edited in Code mode.";
      body = <code>{exprText(op.expr)}</code>;
      break;
  }

  return (
    <div className="op-card">
      <div className="op-head">
        <span className="op-title">{title}</span>
        <span className="op-hint muted">{hint}</span>
        <span className="op-actions">
          <button className="icon" title="Move up" disabled={index === 0} onClick={() => onMove(index - 1)}>
            ↑
          </button>
          <button className="icon" title="Move down" disabled={index === count - 1} onClick={() => onMove(index + 1)}>
            ↓
          </button>
          <button className="icon" title="Remove" onClick={onRemove}>
            ×
          </button>
        </span>
      </div>
      <div className="op-body">{body}</div>
    </div>
  );
}
