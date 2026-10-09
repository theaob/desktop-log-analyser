import { create } from "zustand";
import {
  api,
  errorMessage,
  type CmdError,
  type Histogram,
  type LabelInfo,
  type Query,
  type Row,
  type RowKey,
  type WorkspaceInfo,
} from "./api";
import { editQuery, withFieldFilter, withLabel, withLineFilter } from "./queryEdit";
import { resolveRange, type TimeRange } from "./time";

const PAGE = 500;

export const rowId = (k: RowKey) => `${k.chunk}:${k.idx}`;

interface State {
  workspace: WorkspaceInfo | null;
  labels: LabelInfo[];
  queryText: string;
  parsedQuery: Query | null;
  queryError: CmdError | null;
  range: TimeRange;
  direction: "backward" | "forward";
  rows: Row[];
  total: number | null;
  histogram: Histogram | null;
  tookMs: number;
  next: RowKey | null;
  loading: boolean;
  loadingMore: boolean;
  error: string | null;
  expanded: Record<string, boolean>;
  wrap: boolean;

  setWorkspace: (ws: WorkspaceInfo | null) => Promise<void>;
  setQueryText: (text: string, runNow?: boolean) => void;
  run: () => Promise<void>;
  loadMore: () => Promise<void>;
  setRange: (r: TimeRange) => void;
  setDirection: (d: "backward" | "forward") => void;
  addFilter: (name: string, value: string, negate: boolean, kind: "label" | "parsed" | "line") => Promise<void>;
  toggleExpanded: (id: string) => void;
  setWrap: (w: boolean) => void;
  searchRequest: () => { query: string; from: number | null; to: number | null; direction: "backward" | "forward" };
}

let seq = 0;
let debounce: ReturnType<typeof setTimeout> | undefined;

export const useStore = create<State>((set, get) => ({
  workspace: null,
  labels: [],
  queryText: "{}",
  parsedQuery: null,
  queryError: null,
  range: { kind: "all" },
  direction: "backward",
  rows: [],
  total: null,
  histogram: null,
  tookMs: 0,
  next: null,
  loading: false,
  loadingMore: false,
  error: null,
  expanded: {},
  wrap: false,

  setWorkspace: async (ws) => {
    set({ workspace: ws, rows: [], total: null, histogram: null, next: null, expanded: {}, error: null, range: { kind: "all" } });
    if (!ws) return;
    try {
      set({ labels: await api.labels() });
    } catch (e) {
      set({ error: errorMessage(e) });
    }
    await get().run();
  },

  setQueryText: (text, runNow = false) => {
    set({ queryText: text });
    clearTimeout(debounce);
    debounce = setTimeout(() => void get().run(), runNow ? 0 : 400);
  },

  searchRequest: () => {
    const { queryText, range, workspace, direction } = get();
    const { from, to } = resolveRange(range, workspace?.maxTs ?? null);
    return { query: queryText, from, to, direction };
  },

  run: async () => {
    if (!get().workspace) return;
    const my = ++seq;
    const req = get().searchRequest();
    set({ loading: true });
    const parsed = await api.parseQuery(req.query);
    if (my !== seq) return;
    if (parsed.error) {
      set({ queryError: parsed.error, loading: false });
      return;
    }
    set({ parsedQuery: parsed.query, queryError: null });
    try {
      const res = await api.search({ ...req, limit: PAGE });
      if (my !== seq || res.cancelled) return;
      set({
        rows: res.rows,
        total: res.total,
        histogram: res.histogram,
        tookMs: res.tookMs,
        next: res.next,
        loading: false,
        error: null,
        expanded: {},
      });
    } catch (e) {
      if (my !== seq) return;
      const err = e as CmdError;
      if (err?.kind === "query") set({ queryError: err, loading: false });
      else set({ error: errorMessage(e), loading: false });
    }
  },

  loadMore: async () => {
    const { next, loadingMore, loading } = get();
    if (!next || loadingMore || loading) return;
    const my = seq;
    set({ loadingMore: true });
    try {
      const res = await api.search({ ...get().searchRequest(), limit: PAGE, after: next, rowsOnly: true });
      if (my !== seq) return;
      set((s) => ({ rows: [...s.rows, ...res.rows], next: res.next, loadingMore: false }));
    } catch (e) {
      set({ error: errorMessage(e), loadingMore: false });
    }
  },

  setRange: (range) => {
    set({ range });
    void get().run();
  },

  setDirection: (direction) => {
    set({ direction });
    void get().run();
  },

  addFilter: async (name, value, negate, kind) => {
    const text = await editQuery(get().queryText, (q) =>
      kind === "label" ? withLabel(q, name, value, negate) : kind === "parsed" ? withFieldFilter(q, name, value, negate) : withLineFilter(q, value, negate),
    );
    if (text !== null) get().setQueryText(text, true);
  },

  toggleExpanded: (id) => set((s) => ({ expanded: { ...s.expanded, [id]: !s.expanded[id] } })),
  setWrap: (wrap) => set({ wrap }),
}));
