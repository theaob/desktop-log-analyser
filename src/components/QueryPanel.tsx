import { useEffect, useRef, useState } from "react";
import { api, type LineOp, type MatchOp, type Query, type Stage } from "../api";
import { useStore } from "../store";
import QueryEditor from "./QueryEditor";

type Mode = "builder" | "code";

interface LabelRow {
  name: string;
  op: MatchOp;
  value: string;
}
interface LineRow {
  op: LineOp;
  value: string;
}

const MATCH_OPS: { op: MatchOp; label: string }[] = [
  { op: "=", label: "=" },
  { op: "!=", label: "!=" },
  { op: "=~", label: "=~ regex" },
  { op: "!~", label: "!~ regex" },
];
const LINE_OPS: { op: LineOp; label: string }[] = [
  { op: "|=", label: "Line contains" },
  { op: "!=", label: "Line does not contain" },
  { op: "|~", label: "Line matches regex" },
  { op: "!~", label: "Line does not match regex" },
];

/** Basic visual builder: label matchers and line filters, kept in sync with the text query. */
function Builder() {
  const queryText = useStore((s) => s.queryText);
  const setQueryText = useStore((s) => s.setQueryText);
  const labels = useStore((s) => s.labels);
  const [labelRows, setLabelRows] = useState<LabelRow[]>([]);
  const [lineRows, setLineRows] = useState<LineRow[]>([]);
  const [rest, setRest] = useState<Stage[]>([]);
  const [broken, setBroken] = useState(false);
  const lastCommitted = useRef<string | null>(null);

  useEffect(() => {
    if (queryText === lastCommitted.current) return;
    let live = true;
    api.parseQuery(queryText).then((p) => {
      if (!live) return;
      if (!p.query) {
        setBroken(true);
        return;
      }
      setBroken(false);
      setLabelRows(p.query.selector.map((m) => ({ ...m })));
      const lines: LineRow[] = [];
      const others: Stage[] = [];
      for (const s of p.query.stages) {
        if (s.type === "line" && s.values.length === 1) lines.push({ op: s.op, value: s.values[0] });
        else others.push(s);
      }
      setLineRows(lines);
      setRest(others);
    });
    return () => {
      live = false;
    };
  }, [queryText]);

  const commit = async (lr: LabelRow[], li: LineRow[]) => {
    setLabelRows(lr);
    setLineRows(li);
    const q: Query = {
      selector: lr.filter((r) => r.name && r.value !== ""),
      stages: [...li.filter((r) => r.value !== "").map((r) => ({ type: "line", op: r.op, values: [r.value] }) as Stage), ...rest],
    };
    const text = await api.formatQuery(q);
    lastCommitted.current = text;
    setQueryText(text);
  };

  if (broken) return <p className="hint">The query has a syntax error. Fix it in Code mode to use the builder.</p>;

  return (
    <div className="builder">
      {labelRows.map((r, i) => {
        const info = labels.find((l) => l.name === r.name);
        const update = (patch: Partial<LabelRow>) => void commit(labelRows.map((x, j) => (j === i ? { ...x, ...patch } : x)), lineRows);
        return (
          <div className="builder-row" key={`l${i}`}>
            <select value={r.name} onChange={(e) => update({ name: e.target.value, value: "" })}>
              {!info && <option value={r.name}>{r.name}</option>}
              {labels.map((l) => (
                <option key={l.name} value={l.name}>
                  {l.name}
                </option>
              ))}
            </select>
            <select value={r.op} onChange={(e) => update({ op: e.target.value as MatchOp })}>
              {MATCH_OPS.map((o) => (
                <option key={o.op} value={o.op}>
                  {o.label}
                </option>
              ))}
            </select>
            <input list={`values-${i}`} value={r.value} placeholder="value" onChange={(e) => update({ value: e.target.value })} />
            <datalist id={`values-${i}`}>
              {info?.values.slice(0, 300).map(([v]) => (
                <option key={v} value={v} />
              ))}
            </datalist>
            <button className="ghost" title="Remove" onClick={() => void commit(labelRows.filter((_, j) => j !== i), lineRows)}>
              ×
            </button>
          </div>
        );
      })}
      {lineRows.map((r, i) => {
        const update = (patch: Partial<LineRow>) => void commit(labelRows, lineRows.map((x, j) => (j === i ? { ...x, ...patch } : x)));
        return (
          <div className="builder-row" key={`f${i}`}>
            <select value={r.op} onChange={(e) => update({ op: e.target.value as LineOp })}>
              {LINE_OPS.map((o) => (
                <option key={o.op} value={o.op}>
                  {o.label}
                </option>
              ))}
            </select>
            <input className="grow" value={r.value} placeholder="text" onChange={(e) => update({ value: e.target.value })} />
            <button className="ghost" title="Remove" onClick={() => void commit(labelRows, lineRows.filter((_, j) => j !== i))}>
              ×
            </button>
          </div>
        );
      })}
      <div className="row">
        <button onClick={() => setLabelRows([...labelRows, { name: "level", op: "=", value: "" }])}>+ Label filter</button>
        <button onClick={() => setLineRows([...lineRows, { op: "|=", value: "" }])}>+ Line filter</button>
        {rest.length > 0 && <span className="muted">Parsers and field filters are edited in Code mode.</span>}
      </div>
      <code className="query-preview">{queryText}</code>
    </div>
  );
}

export default function QueryPanel() {
  const [mode, setMode] = useState<Mode>("code");
  const queryError = useStore((s) => s.queryError);
  return (
    <section className="query-panel">
      <div className="tabs">
        <button className={mode === "builder" ? "tab active" : "tab"} onClick={() => setMode("builder")}>
          Builder
        </button>
        <button className={mode === "code" ? "tab active" : "tab"} onClick={() => setMode("code")}>
          Code
        </button>
        <span className="muted hint-inline">Runs as you type · Shift+Enter to run now</span>
      </div>
      {mode === "code" ? <QueryEditor /> : <Builder />}
      {queryError && <div className="query-error">{queryError.message}</div>}
    </section>
  );
}
