import { useState } from "react";
import { api, errorMessage, type FolderSettings } from "../api";
import { useStore } from "../store";
import { formatCount } from "../time";
import SettingsFields from "./SettingsFields";
import { formatName } from "./OpenFolder";

/** Folder settings and per-file detected formats, with re-indexing. */
export default function SettingsDialog({ onClose }: { onClose: () => void }) {
  const ws = useStore((s) => s.workspace)!;
  const setWorkspace = useStore((s) => s.setWorkspace);
  const [settings, setSettings] = useState<FolderSettings>(ws.settings);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reindex = async () => {
    setBusy(true);
    setError(null);
    try {
      const info = await api.openFolder(ws.root, settings, true);
      await setWorkspace(info);
      onClose();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="row spread">
          <h2>Folder settings</h2>
          <button className="ghost" onClick={onClose}>
            ×
          </button>
        </div>
        <code className="path">{ws.root}</code>
        <SettingsFields settings={settings} onChange={setSettings} />
        {error && <div className="error-box">{error}</div>}
        <div className="row">
          <button className="primary" onClick={() => void reindex()} disabled={busy}>
            {busy ? "Re-indexing…" : "Save and re-index"}
          </button>
        </div>
        <h3>Files</h3>
        <div className="file-table">
          <table>
            <thead>
              <tr>
                <th>File</th>
                <th>Format</th>
                <th>Events</th>
              </tr>
            </thead>
            <tbody>
              {ws.files.map((f) => (
                <tr key={f.rel} title={f.format?.kind === "log4j" ? f.format.pattern : undefined}>
                  <td>{f.rel}</td>
                  <td>{f.error ? <span className="error-text">{f.error}</span> : formatName(f.format)}</td>
                  <td className="num">{formatCount(f.events)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
