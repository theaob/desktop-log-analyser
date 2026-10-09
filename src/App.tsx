import { useEffect, useState } from "react";
import { api } from "./api";
import { useStore } from "./store";
import OpenFolder from "./components/OpenFolder";
import Explore from "./components/Explore";

export default function App() {
  const workspace = useStore((s) => s.workspace);
  const setWorkspace = useStore((s) => s.setWorkspace);
  const [choosing, setChoosing] = useState(false);
  const [ready, setReady] = useState(false);
  const [initial, setInitial] = useState<string | null>(null);

  // The Rust side keeps the open workspace across webview reloads.
  useEffect(() => {
    (async () => {
      const ws = await api.workspaceInfo().catch(() => null);
      if (ws) await setWorkspace(ws);
      else setInitial(await api.initialFolder().catch(() => null));
      setReady(true);
    })();
  }, [setWorkspace]);

  if (!ready) return null;
  if (!workspace || choosing) {
    return (
      <OpenFolder
        initialPath={initial}
        onOpened={(ws) => {
          setChoosing(false);
          void setWorkspace(ws);
        }}
        onCancel={workspace ? () => setChoosing(false) : undefined}
      />
    );
  }
  return <Explore onChangeFolder={() => setChoosing(true)} />;
}
