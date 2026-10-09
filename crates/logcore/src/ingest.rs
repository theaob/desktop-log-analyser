//! Building a workspace: discover files, detect each file's format, parse events in
//! parallel and write compressed chunks to the segment file.

use crate::chunk::{ChunkBuilder, ChunkMeta};
use crate::discover::{discover, SourceFile};
use crate::log4j::CompiledPattern;
use crate::model::{Event, Level};
use crate::parse::{detect, Assembler, Format, FormatInfo, ParseCtx};
use crate::reader::{for_each_line, sample_lines};
use crate::store::{self, FileEntry, FolderSettings, Stats, ValueCounts, Workspace, WorkspaceMeta, FORMAT_VERSION};
use crate::time::date_of_ms;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const SAMPLE_LINES: usize = 200;
const MAX_TRACKED_VALUES: usize = 5000;
const KEPT_VALUES: usize = 300;

/// Live progress, shared with the UI thread.
#[derive(Default)]
pub struct Progress {
    pub files_total: AtomicU64,
    pub files_done: AtomicU64,
    pub bytes_total: AtomicU64,
    pub bytes_done: AtomicU64,
    pub events: AtomicU64,
    pub cancel: AtomicBool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressSnapshot {
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub events: u64,
}

impl Progress {
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            files_total: self.files_total.load(Ordering::Relaxed),
            files_done: self.files_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            events: self.events.load(Ordering::Relaxed),
        }
    }
}

pub fn compile_patterns(settings: &FolderSettings) -> anyhow::Result<Vec<Arc<CompiledPattern>>> {
    settings
        .patterns
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            CompiledPattern::compile(p)
                .map(Arc::new)
                .map_err(|e| anyhow::anyhow!("pattern `{p}`: {e}"))
        })
        .collect()
}

fn ctx_for(file: &SourceFile, settings: &FolderSettings) -> ParseCtx {
    ParseCtx {
        tz: settings.time_zone,
        default_date: date_of_ms(file.mtime_ms, settings.time_zone),
        fallback_ts: file.mtime_ms,
    }
}

/// Detects the format of one file from its first lines.
pub fn detect_file(
    file: &SourceFile,
    patterns: &[Arc<CompiledPattern>],
    settings: &FolderSettings,
) -> std::io::Result<(Format, f32, Vec<String>)> {
    let sample = sample_lines(&file.path, file.gz, SAMPLE_LINES)?;
    let det = detect(&sample, patterns, &ctx_for(file, settings));
    Ok((det.format, det.score, sample))
}

#[derive(Default)]
struct Counter {
    map: HashMap<String, u64>,
    truncated: bool,
}

impl Counter {
    fn add(&mut self, v: &str) {
        // Long or multi-line values (stack traces, payloads) are not useful as filter values.
        if v.is_empty() || v.len() > 200 || v.contains('\n') {
            return;
        }
        if let Some(c) = self.map.get_mut(v) {
            *c += 1;
        } else if self.map.len() < MAX_TRACKED_VALUES {
            self.map.insert(v.to_string(), 1);
        } else {
            self.truncated = true;
        }
    }

    fn merge(&mut self, other: Counter) {
        self.truncated |= other.truncated;
        for (k, v) in other.map {
            if let Some(c) = self.map.get_mut(&k) {
                *c += v;
            } else if self.map.len() < MAX_TRACKED_VALUES {
                self.map.insert(k, v);
            } else {
                self.truncated = true;
            }
        }
    }

    fn finish(self) -> ValueCounts {
        let mut values: Vec<(String, u64)> = self.map.into_iter().collect();
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let truncated = self.truncated || values.len() > KEPT_VALUES;
        values.truncate(KEPT_VALUES);
        ValueCounts { values, truncated }
    }
}

#[derive(Default)]
struct FileStats {
    fields: HashMap<String, Counter>,
}

impl FileStats {
    fn add(&mut self, ev: &Event) {
        self.fields.entry("logger".into()).or_default().add(&ev.logger);
        self.fields.entry("thread".into()).or_default().add(&ev.thread);
        self.fields.entry("exception".into()).or_default().add(&ev.exception);
        for (k, v) in &ev.fields {
            if self.fields.len() < 200 || self.fields.contains_key(k) {
                self.fields.entry(k.clone()).or_default().add(v);
            }
        }
    }
}

struct Sealed {
    file: u32,
    level: u8,
    bytes: Vec<u8>,
    count: u32,
    min_ts: i64,
    max_ts: i64,
}

/// Opens the workspace for `root`, rebuilding it when files or settings changed.
pub fn open_or_build(
    root: &Path,
    data_dir: &Path,
    settings: Option<FolderSettings>,
    progress: &Progress,
    force: bool,
) -> anyhow::Result<Workspace> {
    let root = dunce::canonicalize(root)?;
    let dir = store::workspace_dir(data_dir, &root);
    let settings = match settings {
        Some(s) => {
            store::save_settings(&dir, &s)?;
            s
        }
        None => store::load_settings(&dir),
    };
    let files = discover(&root, &settings.include, &settings.exclude)?;
    if !force {
        if let Ok(ws) = Workspace::open(&root, &dir) {
            if ws.is_fresh(&files, &settings) {
                return Ok(ws);
            }
        }
    }
    build(&root, &dir, settings, files, progress)
}

fn build(
    root: &Path,
    dir: &Path,
    settings: FolderSettings,
    files: Vec<SourceFile>,
    progress: &Progress,
) -> anyhow::Result<Workspace> {
    let started = Instant::now();
    std::fs::create_dir_all(dir)?;
    let patterns = compile_patterns(&settings)?;
    progress.files_total.store(files.len() as u64, Ordering::Relaxed);
    progress
        .bytes_total
        .store(files.iter().map(|f| f.size).sum(), Ordering::Relaxed);

    let tmp_path = dir.join("segments.tmp");
    let (tx, rx) = crossbeam_channel::bounded::<Sealed>(64);
    let writer = {
        let tmp_path = tmp_path.clone();
        std::thread::spawn(move || -> anyhow::Result<Vec<ChunkMeta>> {
            let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp_path)?);
            let mut offset = 0u64;
            let mut metas = Vec::new();
            for s in rx {
                out.write_all(&s.bytes)?;
                metas.push(ChunkMeta {
                    file: s.file,
                    level: s.level,
                    min_ts: s.min_ts,
                    max_ts: s.max_ts,
                    count: s.count,
                    offset,
                    len: s.bytes.len() as u32,
                });
                offset += s.bytes.len() as u64;
            }
            out.flush()?;
            Ok(metas)
        })
    };

    // Largest files first so one huge file doesn't finish last on a single core.
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(files[i].size));

    let results: Vec<(usize, FileEntry, FileStats)> = order
        .par_iter()
        .map(|&idx| {
            let file = &files[idx];
            let mut stats = FileStats::default();
            let mut entry = FileEntry {
                file: file.clone(),
                format: None,
                events: 0,
                error: None,
            };
            if progress.cancel.load(Ordering::Relaxed) {
                return (idx, entry, stats);
            }
            let res = (|| -> anyhow::Result<FormatInfo> {
                let (format, _, _) = detect_file(file, &patterns, &settings)?;
                let mut asm = Assembler::new(&format, ctx_for(file, &settings));
                let mut builders: [ChunkBuilder; 7] = Default::default();
                let mut events = 0u64;
                let mut emit = |ev: Event, builders: &mut [ChunkBuilder; 7]| {
                    let level = Level::or_unknown(ev.level) as usize;
                    stats.add(&ev);
                    builders[level].push(&ev);
                    events += 1;
                    if builders[level].is_full() {
                        let (bytes, count, min_ts, max_ts) = builders[level].finish();
                        let _ = tx.send(Sealed {
                            file: idx as u32,
                            level: level as u8,
                            bytes,
                            count,
                            min_ts,
                            max_ts,
                        });
                    }
                };
                for_each_line(&file.path, file.gz, Some(&progress.bytes_done), |line, no| {
                    if let Some(ev) = asm.push(line, no) {
                        emit(ev, &mut builders);
                    }
                    !progress.cancel.load(Ordering::Relaxed)
                })?;
                if let Some(ev) = asm.finish() {
                    emit(ev, &mut builders);
                }
                for (level, b) in builders.iter_mut().enumerate() {
                    if !b.is_empty() {
                        let (bytes, count, min_ts, max_ts) = b.finish();
                        let _ = tx.send(Sealed {
                            file: idx as u32,
                            level: level as u8,
                            bytes,
                            count,
                            min_ts,
                            max_ts,
                        });
                    }
                }
                entry.events = events;
                progress.events.fetch_add(events, Ordering::Relaxed);
                Ok(format.info())
            })();
            match res {
                Ok(info) => entry.format = Some(info),
                Err(e) => entry.error = Some(e.to_string()),
            }
            progress.files_done.fetch_add(1, Ordering::Relaxed);
            (idx, entry, stats)
        })
        .collect();
    drop(tx);
    let chunks = writer
        .join()
        .map_err(|_| anyhow::anyhow!("segment writer panicked"))??;
    if progress.cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!("indexing cancelled");
    }

    let mut entries: Vec<Option<FileEntry>> = vec![None; files.len()];
    let mut merged: HashMap<String, Counter> = HashMap::new();
    for (idx, entry, stats) in results {
        entries[idx] = Some(entry);
        for (k, c) in stats.fields {
            merged.entry(k).or_default().merge(c);
        }
    }
    let mut stats = Stats::default();
    for c in &chunks {
        stats.events += c.count as u64;
        stats.min_ts = Some(stats.min_ts.map_or(c.min_ts, |m| m.min(c.min_ts)));
        stats.max_ts = Some(stats.max_ts.map_or(c.max_ts, |m| m.max(c.max_ts)));
    }
    stats.fields = merged
        .into_iter()
        .map(|(k, c)| (k, c.finish()))
        .filter(|(_, v)| !v.values.is_empty())
        .collect();

    let meta = WorkspaceMeta {
        version: FORMAT_VERSION,
        root: root.to_string_lossy().to_string(),
        settings_hash: store::settings_hash(&settings),
        files: entries
            .into_iter()
            .map(|e| e.expect("every file has an entry"))
            .collect(),
        chunks,
        stats,
        indexed_at_ms: chrono::Utc::now().timestamp_millis(),
        index_millis: started.elapsed().as_millis() as u64,
    };
    // Replace the old workspace atomically enough: segments first, then metadata.
    let seg_path = dir.join("segments.bin");
    let _ = std::fs::remove_file(dir.join("meta.json"));
    let _ = std::fs::remove_file(&seg_path);
    std::fs::rename(&tmp_path, &seg_path)?;
    std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta)?)?;
    Workspace::from_parts(root, dir, settings, meta)
}

/// What the "open folder" screen shows before indexing: detected format and a few parsed
/// events per rolled-file group.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePreview {
    pub source: String,
    pub files: usize,
    pub bytes: u64,
    pub sample_file: String,
    pub format: FormatInfo,
    pub score: f32,
    pub events: Vec<PreviewEvent>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewEvent {
    pub ts: i64,
    pub ts_inferred: bool,
    pub level: String,
    pub logger: String,
    pub thread: String,
    pub exception: String,
    pub fields: Vec<(String, String)>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPreview {
    pub root: String,
    pub files: usize,
    pub bytes: u64,
    pub sources: Vec<SourcePreview>,
}

pub fn preview(root: &Path, settings: &FolderSettings, max_events: usize) -> anyhow::Result<FolderPreview> {
    let root = dunce::canonicalize(root)?;
    let files = discover(&root, &settings.include, &settings.exclude)?;
    let patterns = compile_patterns(settings)?;
    let mut groups: Vec<(String, Vec<&SourceFile>)> = Vec::new();
    for f in &files {
        match groups.iter_mut().find(|(s, _)| *s == f.source) {
            Some((_, v)) => v.push(f),
            None => groups.push((f.source.clone(), vec![f])),
        }
    }
    let sources = groups
        .par_iter()
        .map(|(source, group)| {
            // Sample the most recently written file of the group.
            let sample = group.iter().max_by_key(|f| f.mtime_ms).unwrap();
            let (format, score, lines) =
                detect_file(sample, &patterns, settings).unwrap_or((Format::Plain { timestamped: false }, 0.0, vec![]));
            let mut asm = Assembler::new(&format, ctx_for(sample, settings));
            let mut events = Vec::new();
            for (i, l) in lines.iter().enumerate() {
                if let Some(ev) = asm.push(l, i as u32 + 1) {
                    events.push(ev);
                }
            }
            events.extend(asm.finish());
            events.truncate(max_events);
            SourcePreview {
                source: source.clone(),
                files: group.len(),
                bytes: group.iter().map(|f| f.size).sum(),
                sample_file: sample.rel.clone(),
                format: format.info(),
                score,
                events: events
                    .into_iter()
                    .map(|e| PreviewEvent {
                        ts: e.ts,
                        ts_inferred: e.ts_inferred,
                        level: Level::or_unknown(e.level).as_str().to_string(),
                        logger: e.logger,
                        thread: e.thread,
                        exception: e.exception,
                        fields: e.fields,
                        message: e.message,
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(FolderPreview {
        root: root.to_string_lossy().to_string(),
        files: files.len(),
        bytes: files.iter().map(|f| f.size).sum(),
        sources,
    })
}
