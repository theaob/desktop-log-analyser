import { useState } from "react";
import { useStore } from "../store";
import { formatCount } from "../time";

const SHOWN = 12;

/** Every label and field with its values and counts. Click adds `=`, Alt-click adds `!=`. */
export default function LabelSidebar() {
  const labels = useStore((s) => s.labels);
  const addFilter = useStore((s) => s.addFilter);
  const [open, setOpen] = useState<Record<string, boolean>>({ level: true, source: true, logger: true, exception: true });
  const [showAll, setShowAll] = useState<Record<string, boolean>>({});
  const [filter, setFilter] = useState("");

  const visible = labels.filter((l) => !filter || l.name.toLowerCase().includes(filter.toLowerCase()));
  return (
    <aside className="sidebar">
      <input className="sidebar-search" placeholder="Filter labels" value={filter} onChange={(e) => setFilter(e.target.value)} />
      <p className="hint">Click a value to filter for it, Alt-click to filter it out.</p>
      {visible.map((l) => (
        <div key={l.name} className="label-group">
          <button className="label-name" onClick={() => setOpen({ ...open, [l.name]: !open[l.name] })}>
            <span>{open[l.name] ? "▾" : "▸"}</span> {l.name}
            <span className="muted">
              {" "}
              {l.values.length}
              {l.truncated ? "+" : ""}
            </span>
          </button>
          {open[l.name] && (
            <ul>
              {(showAll[l.name] ? l.values : l.values.slice(0, SHOWN)).map(([v, c]) => (
                <li key={v}>
                  <button
                    className={`label-value${l.name === "level" ? ` lvl-text-${v}` : ""}`}
                    title={`${l.name}="${v}"  (Alt-click to exclude)`}
                    onClick={(e) => void addFilter(l.name, v, e.altKey, "label")}
                  >
                    <span className="value-text">{v}</span>
                    <span className="muted">{formatCount(c)}</span>
                  </button>
                </li>
              ))}
              {l.values.length > SHOWN && (
                <li>
                  <button className="link" onClick={() => setShowAll({ ...showAll, [l.name]: !showAll[l.name] })}>
                    {showAll[l.name] ? "Show fewer" : `Show all ${l.values.length}`}
                  </button>
                </li>
              )}
            </ul>
          )}
        </div>
      ))}
    </aside>
  );
}
