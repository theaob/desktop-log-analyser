import { useState } from "react";
import { useStore } from "../store";
import QueryBuilder from "./QueryBuilder";
import QueryEditor from "./QueryEditor";

type Mode = "builder" | "code";

const MODE_KEY = "queryPanelMode";

function savedMode(): Mode {
  try {
    return localStorage.getItem(MODE_KEY) === "builder" ? "builder" : "code";
  } catch {
    return "code";
  }
}

export default function QueryPanel() {
  const [mode, setModeState] = useState<Mode>(savedMode);
  const setMode = (m: Mode) => {
    setModeState(m);
    try {
      localStorage.setItem(MODE_KEY, m);
    } catch {
      /* the choice just isn't remembered */
    }
  };
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
        <span className="muted hint-inline">{mode === "code" ? "Runs as you type · Shift+Enter to run now" : "Runs as you edit"}</span>
      </div>
      {mode === "code" ? <QueryEditor /> : <QueryBuilder />}
      {queryError && <div className="query-error">{queryError.message}</div>}
    </section>
  );
}
