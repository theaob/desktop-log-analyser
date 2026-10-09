import { useState } from "react";
import type { FolderSettings, TzMode } from "../api";

const lines = (s: string) =>
  s
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);

const TZ_OPTIONS: { label: string; tz: TzMode }[] = [
  { label: "This computer's time zone", tz: { kind: "local" } },
  { label: "UTC", tz: { kind: "utc" } },
  ...[-8, -5, -3, 1, 2, 3, 5.5, 8, 9].map((h) => ({
    label: `UTC${h >= 0 ? "+" : "-"}${Math.floor(Math.abs(h))}${Math.abs(h) % 1 ? ":30" : ""}`,
    tz: { kind: "fixed", offsetSeconds: h * 3600 } as TzMode,
  })),
];

const tzKey = (tz: TzMode) => (tz.kind === "fixed" ? `fixed:${tz.offsetSeconds}` : tz.kind);

/** Folder settings: log4j patterns, include/exclude globs and the time zone of timestamps. */
export default function SettingsFields({ settings, onChange }: { settings: FolderSettings; onChange: (s: FolderSettings) => void }) {
  const [advanced, setAdvanced] = useState(settings.include.length > 0 || settings.exclude.length > 0);
  // Raw text per field, so typing blank lines doesn't fight the parsed value.
  const [raw, setRaw] = useState({
    patterns: settings.patterns.join("\n"),
    include: settings.include.join("\n"),
    exclude: settings.exclude.join("\n"),
  });
  const edit = (key: "patterns" | "include" | "exclude", value: string) => {
    setRaw({ ...raw, [key]: value });
    onChange({ ...settings, [key]: lines(value) });
  };
  return (
    <div className="settings-fields">
      <label>
        log4j / Logback PatternLayout <span className="muted">(optional, one per line; tried before the built-in patterns)</span>
        <textarea
          rows={2}
          spellCheck={false}
          placeholder="%d{ISO8601} [%t] %-5level %logger{36} [%X{requestId}] - %msg%n"
          value={raw.patterns}
          onChange={(e) => edit("patterns", e.target.value)}
        />
      </label>
      <label>
        Timestamps without a zone are in
        <select
          value={tzKey(settings.timeZone)}
          onChange={(e) => onChange({ ...settings, timeZone: TZ_OPTIONS.find((o) => tzKey(o.tz) === e.target.value)!.tz })}
        >
          {TZ_OPTIONS.map((o) => (
            <option key={tzKey(o.tz)} value={tzKey(o.tz)}>
              {o.label}
            </option>
          ))}
        </select>
      </label>
      <button className="link" onClick={() => setAdvanced(!advanced)}>
        {advanced ? "Hide" : "Show"} file filters
      </button>
      {advanced && (
        <div className="row top">
          <label className="grow">
            Include globs <span className="muted">(empty: *.log, *.log.*, *.txt, *.out, *.json, *.jsonl, *.gz)</span>
            <textarea
              rows={3}
              spellCheck={false}
              value={raw.include}
              onChange={(e) => edit("include", e.target.value)}
            />
          </label>
          <label className="grow">
            Exclude globs <span className="muted">(empty: .git, node_modules, archives)</span>
            <textarea
              rows={3}
              spellCheck={false}
              value={raw.exclude}
              onChange={(e) => edit("exclude", e.target.value)}
            />
          </label>
        </div>
      )}
    </div>
  );
}
