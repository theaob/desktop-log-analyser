import { useState } from "react";
import { useStore } from "../store";
import TopBar from "./TopBar";
import LabelSidebar from "./LabelSidebar";
import QueryPanel from "./QueryPanel";
import Histogram from "./Histogram";
import LogList from "./LogList";
import StatusBar from "./StatusBar";
import SettingsDialog from "./SettingsDialog";

/** Grafana Explore-style main window: query, volume and lines on one screen. */
export default function Explore({ onChangeFolder }: { onChangeFolder: () => void }) {
  const error = useStore((s) => s.error);
  const [settingsOpen, setSettingsOpen] = useState(false);
  return (
    <div className="explore">
      <TopBar onChangeFolder={onChangeFolder} onSettings={() => setSettingsOpen(true)} />
      <div className="explore-body">
        <LabelSidebar />
        <main className="explore-main">
          <QueryPanel />
          {error && <div className="error-box">{error}</div>}
          <Histogram />
          <LogList />
        </main>
      </div>
      <StatusBar />
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
    </div>
  );
}
