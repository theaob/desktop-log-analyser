import { useEffect, useRef } from "react";
import { EditorState, StateEffect, StateField, type Extension } from "@codemirror/state";
import { Decoration, EditorView, keymap, placeholder, type DecorationSet } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from "@codemirror/language";
import { autocompletion, closeBrackets, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { tags } from "@lezer/highlight";
import { useStore } from "../store";
import type { LabelInfo } from "../api";

// Minimal LogQL tokenizer for highlighting.
const logql = StreamLanguage.define<{ inSelector: boolean }>({
  startState: () => ({ inSelector: false }),
  token(stream, state) {
    if (stream.eatSpace()) return null;
    if (stream.match("#")) {
      stream.skipToEnd();
      return "comment";
    }
    if (stream.match(/^"(?:[^"\\]|\\.)*"?/) || stream.match(/^`[^`]*`?/)) return "string";
    if (stream.match("{")) {
      state.inSelector = true;
      return "brace";
    }
    if (stream.match("}")) {
      state.inSelector = false;
      return "brace";
    }
    if (stream.match(/^(\|=|\|~|!=|!~|=~|==|>=|<=|[|=<>,()])/)) return "operator";
    if (stream.match(/^-?\d[\w.]*/)) return "number";
    if (stream.match(/^(json|logfmt|regexp|and|or)\b/)) return "keyword";
    if (stream.match(/^[A-Za-z_@][\w.@]*/)) return state.inSelector ? "labelName" : "propertyName";
    stream.next();
    return null;
  },
  tokenTable: { labelName: tags.labelName, brace: tags.brace },
});

const highlight = HighlightStyle.define([
  { tag: tags.string, color: "var(--q-string)" },
  { tag: tags.operator, color: "var(--q-op)" },
  { tag: tags.keyword, color: "var(--q-keyword)", fontWeight: "600" },
  { tag: tags.number, color: "var(--q-number)" },
  { tag: [tags.labelName, tags.propertyName], color: "var(--q-label)" },
  { tag: tags.comment, color: "var(--muted)", fontStyle: "italic" },
  { tag: tags.brace, color: "var(--fg)" },
]);

const setError = StateEffect.define<{ from: number; to: number } | null>();
const errorField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (e.is(setError)) {
        if (!e.value) deco = Decoration.none;
        else {
          const len = tr.state.doc.length;
          // An error at the very end (e.g. "found end of query") marks the last character.
          const from = Math.min(e.value.from, Math.max(len - 1, 0));
          const to = Math.min(Math.max(e.value.to, from + 1), len);
          deco = from < to ? Decoration.set([Decoration.mark({ class: "cm-query-error" }).range(from, to)]) : Decoration.none;
        }
      }
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const STAGES = [
  { label: "json", detail: "parse the line as JSON" },
  { label: "logfmt", detail: "parse key=value pairs" },
  { label: 'regexp "(?P<name>...)"', detail: "extract named groups" },
];
const LINE_OPS = [
  { label: '|= ""', detail: "line contains" },
  { label: '!= ""', detail: "line does not contain" },
  { label: '|~ ""', detail: "line matches regex" },
  { label: '!~ ""', detail: "line does not match regex" },
];

function completions(getLabels: () => LabelInfo[]) {
  return (ctx: CompletionContext): CompletionResult | null => {
    const before = ctx.state.sliceDoc(0, ctx.pos);
    const labels = getLabels();
    const open = before.lastIndexOf("{");
    const inSelector = open >= 0 && before.indexOf("}", open) < 0;

    // Value inside quotes: name op "partial
    const value = before.match(/([A-Za-z_@][\w.@]*)\s*(=~|!~|!=|==|=)\s*"([^"]*)$/);
    if (value) {
      const label = labels.find((l) => l.name === value[1]);
      if (!label) return null;
      return {
        from: ctx.pos - value[3].length,
        options: label.values.slice(0, 200).map(([v, c]) => ({
          label: v,
          detail: c.toLocaleString("en-US"),
          apply: (view: EditorView, _c: unknown, from: number, to: number) => {
            const closing = view.state.sliceDoc(to, to + 1) === '"' ? "" : '"';
            view.dispatch({ changes: { from, to, insert: v + closing }, selection: { anchor: from + v.length + 1 } });
          },
        })),
        validFor: /^[^"]*$/,
      };
    }
    // Label name inside the selector.
    if (inSelector) {
      const word = before.match(/(?:^|[{,\s])([\w.@]*)$/);
      if (word && !/(=~|!~|!=|=)\s*$/.test(before)) {
        return {
          from: ctx.pos - word[1].length,
          options: labels.map((l) => ({ label: l.name, type: l.stream ? "keyword" : "property", detail: l.stream ? "stream" : "field", apply: `${l.name}="` })),
          validFor: /^[\w.@]*$/,
        };
      }
      return null;
    }
    // After a pipe: parsers and field names.
    const pipe = before.match(/\|\s*([\w.@]*)$/);
    if (pipe) {
      return {
        from: ctx.pos - pipe[1].length,
        options: [
          ...STAGES.map((s) => ({ label: s.label, type: "keyword", detail: s.detail })),
          ...labels.filter((l) => !l.stream).map((l) => ({ label: l.name, type: "property", detail: "field filter" })),
        ],
        validFor: /^[\w.@]*$/,
      };
    }
    // After the selector: line filters.
    if (ctx.explicit || /}\s*$/.test(before)) {
      return { from: ctx.pos, options: LINE_OPS.map((o) => ({ label: o.label, detail: o.detail, apply: o.label.slice(0, 4) })) };
    }
    return null;
  };
}

/** Rust reports byte offsets; the editor counts UTF-16 units. */
function byteToIndex(text: string, byte: number): number {
  const bytes = new TextEncoder().encode(text);
  return new TextDecoder().decode(bytes.slice(0, byte)).length;
}

export default function QueryEditor() {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const queryText = useStore((s) => s.queryText);
  const queryError = useStore((s) => s.queryError);

  useEffect(() => {
    const store = useStore.getState;
    const runKeys = keymap.of([
      { key: "Shift-Enter", run: () => (store().setQueryText(view.current!.state.doc.toString(), true), true) },
      { key: "Mod-Enter", run: () => (store().setQueryText(view.current!.state.doc.toString(), true), true) },
    ]);
    const extensions: Extension[] = [
      runKeys,
      history(),
      keymap.of([...defaultKeymap, ...historyKeymap]),
      closeBrackets(),
      logql,
      syntaxHighlighting(highlight),
      autocompletion({ override: [completions(() => store().labels)], activateOnTyping: true }),
      errorField,
      placeholder('{level="error"} |= "timeout"'),
      EditorView.lineWrapping,
      EditorView.updateListener.of((u) => {
        if (u.docChanged) {
          const text = u.state.doc.toString();
          if (text !== store().queryText) store().setQueryText(text);
        }
      }),
    ];
    view.current = new EditorView({
      parent: host.current!,
      state: EditorState.create({ doc: store().queryText, extensions }),
    });
    return () => view.current?.destroy();
  }, []);

  // Text changed elsewhere (builder, click-to-filter): replace the document.
  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== queryText) {
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: queryText } });
    }
  }, [queryText]);

  useEffect(() => {
    const v = view.current;
    if (!v) return;
    const text = v.state.doc.toString();
    const e =
      queryError && queryError.start != null
        ? { from: byteToIndex(text, queryError.start), to: byteToIndex(text, queryError.end ?? queryError.start + 1) }
        : null;
    v.dispatch({ effects: setError.of(e) });
  }, [queryError]);

  return <div className="query-editor" ref={host} />;
}
