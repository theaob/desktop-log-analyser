//! Tauri shell: exposes the logcore engine to the React UI as typed commands. All parsing,
//! indexing and querying runs here in Rust; the webview only receives one page of rows or a
//! pre-bucketed histogram at a time.

use logcore::export::ExportFormat;
use logcore::ingest::{FolderPreview, Progress};
use logcore::labels::LabelInfo;
use logcore::query::{parse, Query};
use logcore::search::{SearchError, SearchRequest, SearchResult};
use logcore::store::{FileEntry, FolderSettings, Workspace};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
struct AppState {
    workspace: RwLock<Option<Arc<Workspace>>>,
    indexing: Mutex<Option<Arc<Progress>>>,
    /// Cancel flag of the running search; a new search cancels the previous one.
    search_cancel: Mutex<Arc<AtomicBool>>,
}

/// Error returned to the UI. Query errors carry the span to underline in the editor.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CmdError {
    kind: &'static str,
    message: String,
    start: Option<usize>,
    end: Option<usize>,
}

impl From<anyhow::Error> for CmdError {
    fn from(e: anyhow::Error) -> Self {
        CmdError {
            kind: "error",
            message: format!("{e:#}"),
            start: None,
            end: None,
        }
    }
}

impl From<SearchError> for CmdError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::Parse(p) => CmdError {
                kind: "query",
                message: p.message,
                start: Some(p.start),
                end: Some(p.end),
            },
            SearchError::Other(e) => e.into(),
        }
    }
}

fn msg(m: impl Into<String>) -> CmdError {
    CmdError {
        kind: "error",
        message: m.into(),
        start: None,
        end: None,
    }
}

type CmdResult<T> = Result<T, CmdError>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceInfo {
    root: String,
    files: Vec<FileEntry>,
    events: u64,
    chunks: usize,
    min_ts: Option<i64>,
    max_ts: Option<i64>,
    index_millis: u64,
    indexed_at_ms: i64,
    settings: FolderSettings,
}

fn info(ws: &Workspace) -> WorkspaceInfo {
    WorkspaceInfo {
        root: ws.root.to_string_lossy().to_string(),
        files: ws.meta.files.clone(),
        events: ws.meta.stats.events,
        chunks: ws.meta.chunks.len(),
        min_ts: ws.meta.stats.min_ts,
        max_ts: ws.meta.stats.max_ts,
        index_millis: ws.meta.index_millis,
        indexed_at_ms: ws.meta.indexed_at_ms,
        settings: ws.settings.clone(),
    }
}

fn data_dir(app: &AppHandle) -> CmdResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|e| msg(format!("no app data folder: {e}")))
}

fn current(state: &AppState) -> CmdResult<Arc<Workspace>> {
    state.workspace.read().clone().ok_or_else(|| msg("no folder is open"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| msg(format!("task failed: {e}")))?
}

#[tauri::command]
fn folder_settings(app: AppHandle, path: String) -> CmdResult<FolderSettings> {
    let root = logcore_canonical(&path)?;
    Ok(logcore::store::load_settings(&logcore::store::workspace_dir(
        &data_dir(&app)?,
        &root,
    )))
}

fn logcore_canonical(path: &str) -> CmdResult<PathBuf> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(msg(format!("{path} is not a folder")));
    }
    dunce::canonicalize(p).map_err(|e| msg(e.to_string()))
}

#[tauri::command]
async fn preview_folder(path: String, settings: FolderSettings) -> CmdResult<FolderPreview> {
    blocking(move || Ok(logcore::preview(Path::new(&path), &settings, 12)?)).await
}

#[tauri::command]
async fn open_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    settings: Option<FolderSettings>,
    force: bool,
) -> CmdResult<WorkspaceInfo> {
    let data = data_dir(&app)?;
    let progress = Arc::new(Progress::default());
    {
        let mut slot = state.indexing.lock();
        if let Some(p) = slot.as_ref() {
            p.cancel.store(true, Ordering::Relaxed);
        }
        *slot = Some(progress.clone());
    }
    // Release the open workspace first: Windows can't replace files that are still open.
    let previous = state.workspace.write().take();
    drop(previous);

    let done = Arc::new(AtomicBool::new(false));
    {
        let (app, progress, done) = (app.clone(), progress.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let _ = app.emit("index-progress", progress.snapshot());
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }
    let result = {
        let progress = progress.clone();
        blocking(move || {
            Ok(logcore::open_or_build(
                Path::new(&path),
                &data,
                settings,
                &progress,
                force,
            )?)
        })
        .await
    };
    done.store(true, Ordering::Relaxed);
    let _ = app.emit("index-progress", progress.snapshot());
    let ws = Arc::new(result?);
    remember_recent(&app, &ws.root);
    let out = info(&ws);
    *state.workspace.write() = Some(ws);
    Ok(out)
}

#[tauri::command]
fn cancel_indexing(state: State<'_, AppState>) {
    if let Some(p) = state.indexing.lock().as_ref() {
        p.cancel.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn workspace_info(state: State<'_, AppState>) -> Option<WorkspaceInfo> {
    state.workspace.read().as_ref().map(|ws| info(ws))
}

#[tauri::command]
async fn search(state: State<'_, AppState>, request: SearchRequest) -> CmdResult<SearchResult> {
    let ws = current(&state)?;
    let cancel = Arc::new(AtomicBool::new(false));
    if !request.rows_only {
        let mut slot = state.search_cancel.lock();
        slot.store(true, Ordering::Relaxed);
        *slot = cancel.clone();
    }
    blocking(move || Ok(logcore::search(&ws, &request, &cancel)?)).await
}

#[tauri::command]
fn labels(state: State<'_, AppState>) -> CmdResult<Vec<LabelInfo>> {
    let ws = current(&state)?;
    Ok(logcore::labels::labels(&ws))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ParsedQuery {
    query: Option<Query>,
    error: Option<logcore::query::ParseError>,
}

#[tauri::command]
fn parse_query(text: String) -> ParsedQuery {
    match parse(&text) {
        Ok(q) => ParsedQuery {
            query: Some(q),
            error: None,
        },
        Err(e) => ParsedQuery {
            query: None,
            error: Some(e),
        },
    }
}

#[tauri::command]
fn format_query(query: Query) -> String {
    query.to_string()
}

#[tauri::command]
async fn export(
    state: State<'_, AppState>,
    request: SearchRequest,
    path: String,
    format: ExportFormat,
) -> CmdResult<u64> {
    let ws = current(&state)?;
    blocking(move || {
        Ok(logcore::export::export(
            &ws,
            &request,
            Path::new(&path),
            format,
            &AtomicBool::new(false),
        )?)
    })
    .await
}

#[derive(Serialize, Deserialize, Default)]
struct Recent {
    folders: Vec<String>,
}

fn recent_path(app: &AppHandle) -> Option<PathBuf> {
    data_dir(app).ok().map(|d| d.join("recent.json"))
}

fn remember_recent(app: &AppHandle, root: &Path) {
    let Some(path) = recent_path(app) else { return };
    let mut recent: Recent = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let s = root.to_string_lossy().to_string();
    recent.folders.retain(|f| *f != s);
    recent.folders.insert(0, s);
    recent.folders.truncate(10);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, serde_json::to_vec(&recent).unwrap_or_default());
}

#[tauri::command]
fn recent_folders(app: AppHandle) -> Vec<String> {
    recent_path(&app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Recent>(&b).ok())
        .map(|r| r.folders.into_iter().filter(|f| Path::new(f).is_dir()).collect())
        .unwrap_or_default()
}

/// A folder passed on the command line (`log-analyser /var/log/myapp`) is offered on start.
#[tauri::command]
fn initial_folder() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && Path::new(a).is_dir())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            folder_settings,
            preview_folder,
            open_folder,
            cancel_indexing,
            workspace_info,
            search,
            labels,
            parse_query,
            format_query,
            export,
            recent_folders,
            initial_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
