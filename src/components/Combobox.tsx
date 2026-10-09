import { useEffect, useMemo, useRef, useState } from "react";
import type { Suggestion } from "../builderModel";
import { formatCount } from "../time";

const MAX_SHOWN = 200;

interface Props {
  value: string;
  onChange: (v: string) => void;
  /** Called instead of onChange when a suggestion is picked; defaults to onChange. */
  onPick?: (v: string) => void;
  suggestions: Suggestion[];
  placeholder?: string;
  className?: string;
  invalid?: boolean;
  title?: string;
  autoFocus?: boolean;
}

/**
 * A text input with a filterable suggestion list (values with counts). Free text is always
 * allowed. Arrow keys move, Enter or Tab picks, Escape closes.
 */
export default function Combobox({ value, onChange, onPick, suggestions, placeholder, className, invalid, title, autoFocus }: Props) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  // What the user typed since opening; picking a regex alternative shouldn't filter by the whole value.
  const [typed, setTyped] = useState<string | null>(null);
  // Enter only picks after the user typed or moved through the list.
  const [moved, setMoved] = useState(false);
  const listRef = useRef<HTMLUListElement>(null);

  const shown = useMemo(() => {
    const needle = (typed ?? "").toLowerCase();
    const list = needle ? suggestions.filter((s) => s.value.toLowerCase().includes(needle)) : suggestions;
    return list.slice(0, MAX_SHOWN);
  }, [suggestions, typed]);

  useEffect(() => {
    setActive(0);
    setMoved(false);
  }, [typed, open]);
  useEffect(() => {
    listRef.current?.children[active]?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const pick = (v: string) => {
    (onPick ?? onChange)(v);
    setOpen(false);
    setTyped(null);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) setOpen(true);
      else {
        // The first press selects the highlighted top item rather than skipping it.
        if (moved) setActive((a) => Math.min(a + 1, shown.length - 1));
        setMoved(true);
      }
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
      setMoved(true);
    } else if ((e.key === "Enter" || e.key === "Tab") && open && shown[active] && (typed || moved)) {
      e.preventDefault();
      pick(shown[active].value);
    } else if (e.key === "Escape" && open) {
      e.stopPropagation();
      setOpen(false);
    }
  };

  return (
    <div className={`combobox ${className ?? ""}`}>
      <input
        value={value}
        placeholder={placeholder}
        title={title}
        autoFocus={autoFocus}
        className={invalid ? "invalid" : undefined}
        spellCheck={false}
        role="combobox"
        aria-expanded={open}
        aria-autocomplete="list"
        onChange={(e) => {
          onChange(e.target.value);
          setTyped(e.target.value);
          setOpen(true);
        }}
        onFocus={() => {
          setTyped("");
          setOpen(true);
        }}
        onClick={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onKeyDown={onKeyDown}
      />
      {open && shown.length > 0 && (
        <ul className="combobox-list" role="listbox" ref={listRef}>
          {shown.map((s, i) => (
            <li
              key={s.value}
              role="option"
              aria-selected={i === active}
              className={i === active ? "active" : undefined}
              // mousedown, not click: it fires before the input's blur closes the list.
              onMouseDown={(e) => {
                e.preventDefault();
                pick(s.value);
              }}
              onMouseEnter={() => setActive(i)}
            >
              <span className="value-text">{s.value === "" ? <em className="muted">(empty)</em> : s.value}</span>
              {s.count !== undefined && <span className="muted">{formatCount(s.count)}</span>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
