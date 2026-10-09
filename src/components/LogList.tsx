import { useEffect, useMemo, useRef, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { Row } from "../api";
import { highlightTerms } from "../queryEdit";
import { rowId, useStore } from "../store";
import { formatCount, formatTs } from "../time";

function highlight(text: string, re: RegExp | null): ReactNode {
  if (!re || !text) return text;
  const out: ReactNode[] = [];
  let last = 0;
  re.lastIndex = 0;
  let m: RegExpExecArray | null;
  let guard = 0;
  while ((m = re.exec(text)) && guard++ < 500) {
    if (m[0].length === 0) {
      re.lastIndex++;
      continue;
    }
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(<mark key={m.index}>{m[0]}</mark>);
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function FieldRow({ name, value, kind }: { name: string; value: string; kind: "label" | "parsed" }) {
  const addFilter = useStore((s) => s.addFilter);
  if (!value) return null;
  return (
    <tr>
      <td className="field-actions">
        <button className="icon" title={`Filter for ${name}="${value}"`} onClick={() => void addFilter(name, value, false, kind)}>
          ⊕
        </button>
        <button className="icon" title={`Filter out ${name}="${value}"`} onClick={() => void addFilter(name, value, true, kind)}>
          ⊖
        </button>
      </td>
      <td className="field-name">{name}</td>
      <td className="field-value">{value}</td>
    </tr>
  );
}

function Details({ row, re }: { row: Row; re: RegExp | null }) {
  return (
    <div className="row-details" onClick={(e) => e.stopPropagation()}>
      <pre className="full-message">{highlight(row.message, re)}</pre>
      <table className="fields">
        <tbody>
          <FieldRow name="level" value={row.level} kind="label" />
          <FieldRow name="logger" value={row.logger} kind="label" />
          <FieldRow name="thread" value={row.thread} kind="label" />
          <FieldRow name="exception" value={row.exception} kind="label" />
          {row.fields.map(([k, v]) => (
            <FieldRow key={`f-${k}`} name={k} value={v} kind="label" />
          ))}
          {row.parsed.map(([k, v]) => (
            <FieldRow key={`p-${k}`} name={k} value={v} kind="parsed" />
          ))}
          <FieldRow name="source" value={row.source} kind="label" />
          <FieldRow name="file" value={row.file} kind="label" />
        </tbody>
      </table>
      <div className="row muted small">
        <span>
          {row.file}:{row.lineNo}
        </span>
        {row.tsInferred && <span>· no timestamp on this line; time taken from the previous line or the file</span>}
        <button className="link" onClick={() => void navigator.clipboard.writeText(row.message)}>
          Copy line
        </button>
      </div>
    </div>
  );
}

/** Virtualized log lines; only rendered rows exist in the DOM. Scrolling near the end loads the next page. */
export default function LogList() {
  const rows = useStore((s) => s.rows);
  const expanded = useStore((s) => s.expanded);
  const toggle = useStore((s) => s.toggleExpanded);
  const wrap = useStore((s) => s.wrap);
  const setWrap = useStore((s) => s.setWrap);
  const next = useStore((s) => s.next);
  const loadMore = useStore((s) => s.loadMore);
  const loadingMore = useStore((s) => s.loadingMore);
  const loading = useStore((s) => s.loading);
  const total = useStore((s) => s.total);
  const counting = useStore((s) => s.counting);
  const parsedQuery = useStore((s) => s.parsedQuery);
  const re = useMemo(() => highlightTerms(parsedQuery), [parsedQuery]);
  const scroller = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: rows.length + (next ? 1 : 0),
    getScrollElement: () => scroller.current,
    estimateSize: () => 22,
    overscan: 20,
    getItemKey: (i) => (i < rows.length ? rowId(rows[i].key) : "more"),
  });
  const items = virtualizer.getVirtualItems();
  const lastIndex = items.length ? items[items.length - 1].index : 0;

  useEffect(() => {
    if (next && lastIndex >= rows.length - 30) void loadMore();
  }, [lastIndex, rows.length, next, loadMore]);

  // New results start at the top.
  useEffect(() => {
    if (!loading) scroller.current?.scrollTo({ top: 0 });
  }, [loading]);

  return (
    <section className="loglist">
      <div className="loglist-toolbar">
        <span className="muted">
          {counting || total === null
            ? `Showing ${formatCount(rows.length)}${next ? "+" : ""}`
            : `Showing ${formatCount(rows.length)} of ${formatCount(total)}`}
          {loading && " · running…"}
        </span>
        <label className="toggle">
          <input type="checkbox" checked={wrap} onChange={(e) => setWrap(e.target.checked)} /> Wrap lines
        </label>
      </div>
      <div ref={scroller} className={`loglist-scroll${wrap ? " wrap" : ""}`}>
        {rows.length === 0 && !loading && <div className="empty-state">No lines match this query in the selected time range.</div>}
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {items.map((item) => {
            if (item.index >= rows.length) {
              return (
                <div key={item.key} className="logrow more" style={{ position: "absolute", top: item.start, left: 0, right: 0 }}>
                  {loadingMore ? "Loading more…" : ""}
                </div>
              );
            }
            const row = rows[item.index];
            const id = rowId(row.key);
            const open = !!expanded[id];
            const nl = row.message.indexOf("\n");
            const first = nl >= 0 ? row.message.slice(0, nl) : row.message;
            const extra = nl >= 0 ? row.message.split("\n").length - 1 : 0;
            return (
              <div
                key={item.key}
                data-index={item.index}
                ref={virtualizer.measureElement}
                className={`logrow lvl-border-${row.level}${open ? " open" : ""}`}
                style={{ position: "absolute", top: item.start, left: 0, right: 0 }}
                onClick={() => toggle(id)}
              >
                <div className="logrow-line">
                  <span className="ts">{formatTs(row.ts)}</span>
                  <span className={`lvl lvl-${row.level}`}>{row.level}</span>
                  <span className="msg">
                    {highlight(first, re)}
                    {extra > 0 && <span className="more-lines"> +{extra} lines</span>}
                  </span>
                </div>
                {open && <Details row={row} re={re} />}
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}
