// Typed wrappers around the Tauri commands in src-tauri/src/lib.rs. Shapes mirror the
// serde types in crates/logcore.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Level = "trace" | "debug" | "info" | "warn" | "error" | "fatal" | "unknown";
export const LEVELS: Level[] = ["trace", "debug", "info", "warn", "error", "fatal", "unknown"];

export type TzMode = { kind: "local" } | { kind: "utc" } | { kind: "fixed"; offsetSeconds: number };

export interface FolderSettings {
  include: string[];
  exclude: string[];
  patterns: string[];
  timeZone: TzMode;
}

export const defaultSettings = (): FolderSettings => ({ include: [], exclude: [], patterns: [], timeZone: { kind: "local" } });

export type FormatInfo = { kind: "log4j"; pattern: string } | { kind: "jsonLines" } | { kind: "plain"; timestamped: boolean };

export interface FileEntry {
  rel: string;
  size: number;
  mtimeMs: number;
  gz: boolean;
  source: string;
  format: FormatInfo | null;
  events: number;
  error: string | null;
}

export interface WorkspaceInfo {
  root: string;
  files: FileEntry[];
  events: number;
  chunks: number;
  minTs: number | null;
  maxTs: number | null;
  indexMillis: number;
  indexedAtMs: number;
  settings: FolderSettings;
}

export interface PreviewEvent {
  ts: number;
  tsInferred: boolean;
  level: Level;
  logger: string;
  thread: string;
  exception: string;
  fields: [string, string][];
  message: string;
}

export interface SourcePreview {
  source: string;
  files: number;
  bytes: number;
  sampleFile: string;
  format: FormatInfo;
  score: number;
  events: PreviewEvent[];
}

export interface FolderPreview {
  root: string;
  files: number;
  bytes: number;
  sources: SourcePreview[];
}

export interface Progress {
  filesTotal: number;
  filesDone: number;
  bytesTotal: number;
  bytesDone: number;
  events: number;
}

export interface RowKey {
  ts: number;
  chunk: number;
  idx: number;
}

export interface Row {
  key: RowKey;
  ts: number;
  tsInferred: boolean;
  level: Level;
  source: string;
  file: string;
  lineNo: number;
  logger: string;
  thread: string;
  exception: string;
  fields: [string, string][];
  parsed: [string, string][];
  message: string;
}

export interface Histogram {
  start: number;
  step: number;
  buckets: number[][];
}

export interface SearchRequest {
  query: string;
  from?: number | null;
  to?: number | null;
  limit?: number;
  direction?: "backward" | "forward";
  after?: RowKey | null;
  rowsOnly?: boolean;
}

export interface SearchResult {
  rows: Row[];
  total: number | null;
  histogram: Histogram | null;
  tookMs: number;
  chunksScanned: number;
  chunksTotal: number;
  eventsScanned: number;
  next: RowKey | null;
  cancelled: boolean;
}

export interface LabelInfo {
  name: string;
  stream: boolean;
  values: [string, number][];
  truncated: boolean;
}

// Query syntax tree (crates/logcore/src/query/ast.rs).
export type MatchOp = "=" | "!=" | "=~" | "!~";
export type LineOp = "|=" | "!=" | "|~" | "!~";
export interface Matcher {
  name: string;
  op: MatchOp;
  value: string;
}
export type Stage =
  | { type: "line"; op: LineOp; values: string[] }
  | { type: "json" }
  | { type: "logfmt" }
  | { type: "regexp"; pattern: string }
  | { type: "filter"; expr: FieldExpr };
export type FieldExpr =
  | { type: "cmp"; name: string; op: string; value: { type: string; value: string | number }; text?: string }
  | { type: "and"; left: FieldExpr; right: FieldExpr }
  | { type: "or"; left: FieldExpr; right: FieldExpr };
export interface Query {
  selector: Matcher[];
  stages: Stage[];
}

export interface CmdError {
  kind: "error" | "query";
  message: string;
  start?: number | null;
  end?: number | null;
}

export const errorMessage = (e: unknown): string =>
  typeof e === "object" && e !== null && "message" in e ? String((e as CmdError).message) : String(e);

export const api = {
  folderSettings: (path: string) => invoke<FolderSettings>("folder_settings", { path }),
  previewFolder: (path: string, settings: FolderSettings) => invoke<FolderPreview>("preview_folder", { path, settings }),
  openFolder: (path: string, settings: FolderSettings | null, force: boolean) =>
    invoke<WorkspaceInfo>("open_folder", { path, settings, force }),
  cancelIndexing: () => invoke<void>("cancel_indexing"),
  workspaceInfo: () => invoke<WorkspaceInfo | null>("workspace_info"),
  search: (request: SearchRequest) => invoke<SearchResult>("search", { request }),
  labels: () => invoke<LabelInfo[]>("labels"),
  parseQuery: (text: string) => invoke<{ query: Query | null; error: CmdError | null }>("parse_query", { text }),
  formatQuery: (query: Query) => invoke<string>("format_query", { query }),
  exportRows: (request: SearchRequest, path: string, format: "csv" | "json") =>
    invoke<number>("export", { request, path, format }),
  recentFolders: () => invoke<string[]>("recent_folders"),
  initialFolder: () => invoke<string | null>("initial_folder"),
  onProgress: (cb: (p: Progress) => void): Promise<UnlistenFn> => listen<Progress>("index-progress", (e) => cb(e.payload)),
};
