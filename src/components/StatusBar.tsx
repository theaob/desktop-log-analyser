import { useStore } from "../store";
import { formatBytes, formatCount, formatDuration, formatTs } from "../time";

export default function StatusBar() {
  const ws = useStore((s) => s.workspace)!;
  const total = useStore((s) => s.total);
  const tookMs = useStore((s) => s.tookMs);
  const bytes = ws.files.reduce((a, f) => a + f.size, 0);
  const failed = ws.files.filter((f) => f.error);
  return (
    <footer className="statusbar">
      <span>{total !== null ? `${formatCount(total)} matches in ${tookMs} ms` : ""}</span>
      <span>
        {ws.files.length} files · {formatBytes(bytes)} · {formatCount(ws.events)} events
        {ws.minTs !== null && ws.maxTs !== null && ` · ${formatTs(ws.minTs, false)} to ${formatTs(ws.maxTs, false)}`}
      </span>
      <span title={new Date(ws.indexedAtMs).toLocaleString()}>Indexed in {formatDuration(ws.indexMillis)}</span>
      {failed.length > 0 && (
        <span className="error-text" title={failed.map((f) => `${f.rel}: ${f.error}`).join("\n")}>
          {failed.length} file{failed.length === 1 ? "" : "s"} could not be read
        </span>
      )}
    </footer>
  );
}
