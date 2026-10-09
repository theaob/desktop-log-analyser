import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  api,
  defaultSettings,
  errorMessage,
  type FolderPreview,
  type FolderSettings,
  type FormatInfo,
  type Progress,
  type WorkspaceInfo,
} from "../api";
import { formatBytes, formatCount, formatTs } from "../time";
import SettingsFields from "./SettingsFields";

export function formatName(f: FormatInfo | null): string {
  if (!f) return "unreadable";
  switch (f.kind) {
    case "log4j":
      return "log4j pattern";
    case "jsonLines":
      return "JSON lines";
    case "plain":
      return f.timestamped ? "plain text with timestamps" : "plain text";
  }
}

interface Props {
  initialPath?: string | null;
  onOpened: (ws: WorkspaceInfo) => void;
  onCancel?: () => void;
}

export default function OpenFolder({ initialPath, onOpened, onCancel }: Props) {
  const [recent, setRecent] = useState<string[]>([]);
  const [path, setPath] = useState<string | null>(null);
  const [settings, setSettings] = useState<FolderSettings>(defaultSettings());
  const [preview, setPreview] = useState<FolderPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);

  useEffect(() => {
    api.recentFolders().then(setRecent).catch(() => undefined);
  }, []);

  useEffect(() => {
    if (initialPath) void choose(initialPath);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialPath]);

  const loadPreview = async (p: string, s: FolderSettings) => {
    setBusy(true);
    setError(null);
    try {
      setPreview(await api.previewFolder(p, s));
    } catch (e) {
      setError(errorMessage(e));
      setPreview(null);
    } finally {
      setBusy(false);
    }
  };

  const choose = async (p: string) => {
    setPreview(null);
    let s = defaultSettings();
    try {
      s = await api.folderSettings(p);
    } catch {
      /* first time: defaults */
    }
    setSettings(s);
    setPath(p);
    await loadPreview(p, s);
  };

  const browse = async () => {
    const picked = await open({ directory: true, multiple: false, title: "Choose a log folder" });
    if (typeof picked === "string") await choose(picked);
  };

  const index = async () => {
    if (!path) return;
    setError(null);
    setProgress({ filesTotal: 0, filesDone: 0, bytesTotal: 0, bytesDone: 0, events: 0 });
    const unlisten = await api.onProgress(setProgress);
    try {
      onOpened(await api.openFolder(path, settings, false));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      unlisten();
      setProgress(null);
    }
  };

  if (progress) {
    const pct = progress.bytesTotal ? Math.min(100, (progress.bytesDone / progress.bytesTotal) * 100) : 0;
    return (
      <div className="open-screen">
        <div className="open-card">
          <h2>Indexing {path}</h2>
          <div className="progress">
            <div className="progress-bar" style={{ width: `${pct}%` }} />
          </div>
          <p className="muted">
            {formatBytes(progress.bytesDone)} of {formatBytes(progress.bytesTotal)} read · {progress.filesDone} of{" "}
            {progress.filesTotal} files · {formatCount(progress.events)} events
          </p>
          <button onClick={() => void api.cancelIndexing()}>Cancel</button>
        </div>
      </div>
    );
  }

  return (
    <div className="open-screen">
      <div className="open-card wide">
        <div className="row spread">
          <h1>Open a log folder</h1>
          {onCancel && (
            <button className="ghost" onClick={onCancel}>
              Back
            </button>
          )}
        </div>
        <div className="row">
          <button className="primary" onClick={() => void browse()}>
            Choose folder…
          </button>
          {path && <code className="path">{path}</code>}
        </div>
        {!path && recent.length > 0 && (
          <div className="recent">
            <h3>Recent folders</h3>
            {recent.map((r) => (
              <button key={r} className="link" onClick={() => void choose(r)}>
                {r}
              </button>
            ))}
          </div>
        )}
        {error && <div className="error-box">{error}</div>}
        {path && (
          <>
            <SettingsFields key={path} settings={settings} onChange={setSettings} />
            <div className="row">
              <button onClick={() => void loadPreview(path, settings)} disabled={busy}>
                {busy ? "Detecting…" : "Re-detect formats"}
              </button>
              <button className="primary" onClick={() => void index()} disabled={busy || !preview || preview.files === 0}>
                Index folder
              </button>
              {preview && (
                <span className="muted">
                  {preview.files} files · {formatBytes(preview.bytes)} on disk
                </span>
              )}
            </div>
            {preview?.files === 0 && <div className="error-box">No log files matched the include and exclude globs.</div>}
            {preview?.sources.map((s) => (
              <div key={s.source} className="source-preview">
                <div className="row spread">
                  <strong>{s.source}</strong>
                  <span className="muted">
                    {s.files} file{s.files === 1 ? "" : "s"} · {formatBytes(s.bytes)} · {formatName(s.format)}
                    {s.format.kind !== "jsonLines" && ` · ${Math.round(s.score * 100)}% of sample lines start an event`}
                  </span>
                </div>
                {s.format.kind === "log4j" && <code className="pattern">{s.format.pattern}</code>}
                {s.format.kind === "plain" && (
                  <p className="hint">
                    No known log4j pattern matched {s.sampleFile}. Paste the PatternLayout from your log4j2.xml or
                    logback.xml above to get logger, thread and MDC fields.
                  </p>
                )}
                <table className="sample">
                  <tbody>
                    {s.events.slice(0, 8).map((e, i) => (
                      <tr key={i}>
                        <td className="nowrap">{e.tsInferred ? <span className="muted">no time</span> : formatTs(e.ts)}</td>
                        <td>
                          <span className={`lvl lvl-${e.level}`}>{e.level}</span>
                        </td>
                        <td className="nowrap muted">{e.logger}</td>
                        <td className="nowrap muted">{e.thread}</td>
                        <td className="msg">
                          {e.message.split("\n")[0]}
                          {e.message.includes("\n") && <span className="muted"> (+{e.message.split("\n").length - 1} lines)</span>}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ))}
          </>
        )}
      </div>
    </div>
  );
}
