import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { api, errorMessage } from "../api";
import { useStore } from "../store";
import { formatCount, formatTs, parseTs, PRESETS, rangeLabel, resolveRange } from "../time";

function basename(p: string) {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

export default function TopBar({ onChangeFolder, onSettings }: { onChangeFolder: () => void; onSettings: () => void }) {
  const workspace = useStore((s) => s.workspace)!;
  const range = useStore((s) => s.range);
  const setRange = useStore((s) => s.setRange);
  const direction = useStore((s) => s.direction);
  const setDirection = useStore((s) => s.setDirection);
  const run = useStore((s) => s.run);
  const loading = useStore((s) => s.loading);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [fromText, setFromText] = useState("");
  const [toText, setToText] = useState("");
  const [exportMsg, setExportMsg] = useState<string | null>(null);

  const openPicker = () => {
    const { from, to } = resolveRange(range, workspace.maxTs);
    setFromText(formatTs(from ?? workspace.minTs ?? Date.now(), false));
    setToText(formatTs(to ?? (workspace.maxTs ?? Date.now()) + 1000, false));
    setPickerOpen(!pickerOpen);
  };

  const applyAbsolute = () => {
    const from = parseTs(fromText);
    const to = parseTs(toText);
    if (from === null || to === null || to <= from) return;
    setRange({ kind: "absolute", from, to });
    setPickerOpen(false);
  };

  const zoomOut = () => {
    const { from, to } = resolveRange(range, workspace.maxTs);
    if (from === null || to === null) return;
    const span = to - from;
    setRange({ kind: "absolute", from: from - span / 2, to: to + span / 2 });
  };

  const doExport = async () => {
    const path = await save({
      title: "Export matching lines",
      defaultPath: "logs-export.csv",
      filters: [
        { name: "CSV", extensions: ["csv"] },
        { name: "JSON lines", extensions: ["json", "jsonl", "ndjson"] },
      ],
    });
    if (!path) return;
    const format = /\.(json|jsonl|ndjson)$/i.test(path) ? "json" : "csv";
    setExportMsg("Exporting…");
    try {
      const n = await api.exportRows(useStore.getState().searchRequest(), path, format);
      setExportMsg(`Exported ${formatCount(n)} lines to ${basename(path)}`);
    } catch (e) {
      setExportMsg(`Export failed: ${errorMessage(e)}`);
    }
    setTimeout(() => setExportMsg(null), 6000);
  };

  return (
    <header className="topbar">
      <div className="row">
        <button className="folder" title={workspace.root} onClick={onChangeFolder}>
          📁 {basename(workspace.root)}
        </button>
        <button className="ghost" onClick={onSettings} title="Folder settings">
          Settings
        </button>
      </div>
      <div className="row">
        {exportMsg && <span className="muted">{exportMsg}</span>}
        <select value={direction} onChange={(e) => setDirection(e.target.value as "backward" | "forward")} title="Sort order">
          <option value="backward">Newest first</option>
          <option value="forward">Oldest first</option>
        </select>
        <div className="picker">
          <button onClick={openPicker}>🕒 {rangeLabel(range)}</button>
          {pickerOpen && (
            <div className="picker-pop">
              <div className="picker-col">
                <strong>Absolute range</strong>
                <label>
                  From
                  <input value={fromText} onChange={(e) => setFromText(e.target.value)} placeholder="yyyy-MM-dd HH:mm:ss" />
                </label>
                <label>
                  To
                  <input value={toText} onChange={(e) => setToText(e.target.value)} placeholder="yyyy-MM-dd HH:mm:ss" />
                </label>
                <button className="primary" onClick={applyAbsolute}>
                  Apply
                </button>
              </div>
              <div className="picker-col">
                <button
                  className={range.kind === "all" ? "link active" : "link"}
                  onClick={() => {
                    setRange({ kind: "all" });
                    setPickerOpen(false);
                  }}
                >
                  Whole folder
                </button>
                {PRESETS.map((p) => (
                  <button
                    key={p.ms}
                    className={range.kind === "relative" && range.ms === p.ms ? "link active" : "link"}
                    onClick={() => {
                      setRange({ kind: "relative", ms: p.ms, label: p.label });
                      setPickerOpen(false);
                    }}
                  >
                    {p.label} of logs
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
        <button onClick={zoomOut} disabled={range.kind === "all"} title="Zoom out">
          −
        </button>
        <button className="primary" onClick={() => void run()} disabled={loading}>
          {loading ? "Running…" : "Run query"}
        </button>
        <button onClick={() => void doExport()}>Export</button>
      </div>
    </header>
  );
}
