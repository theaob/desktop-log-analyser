//! The on-disk workspace for one log folder: settings, metadata and the segment file.
//! Workspaces live in the app data folder, keyed by the log folder's path, never inside the
//! user's log folder.

use crate::chunk::{self, ChunkMeta};
use crate::discover::SourceFile;
use crate::parse::FormatInfo;
use crate::time::TzMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// Bumped whenever the segment or metadata layout changes; older workspaces are rebuilt.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderSettings {
    /// Include globs; empty means the defaults.
    pub include: Vec<String>,
    /// Exclude globs; empty means the defaults.
    pub exclude: Vec<String>,
    /// log4j / Logback PatternLayout patterns pasted by the user, tried before the defaults.
    pub patterns: Vec<String>,
    pub time_zone: TzMode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    #[serde(flatten)]
    pub file: SourceFile,
    pub format: Option<FormatInfo>,
    pub events: u64,
    pub error: Option<String>,
}

/// Value counts for one label or field, capped to the most frequent values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ValueCounts {
    pub values: Vec<(String, u64)>,
    /// True when values were dropped because there were too many distinct ones.
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub events: u64,
    pub min_ts: Option<i64>,
    pub max_ts: Option<i64>,
    /// Counts for labels and fields: logger, thread, exception and MDC/structured keys.
    pub fields: HashMap<String, ValueCounts>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMeta {
    pub version: u32,
    pub root: String,
    pub settings_hash: String,
    pub files: Vec<FileEntry>,
    pub chunks: Vec<ChunkMeta>,
    pub stats: Stats,
    pub indexed_at_ms: i64,
    pub index_millis: u64,
}

pub struct Workspace {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub settings: FolderSettings,
    pub meta: WorkspaceMeta,
    segments: File,
}

pub fn hash_hex(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().take(10).map(|b| format!("{b:02x}")).collect()
}

/// Directory holding the workspace for `root` under `data_dir`.
pub fn workspace_dir(data_dir: &Path, root: &Path) -> PathBuf {
    let key = root.to_string_lossy().to_string();
    #[cfg(windows)]
    let key = key.to_lowercase();
    data_dir.join("workspaces").join(hash_hex(&key))
}

pub fn settings_hash(settings: &FolderSettings) -> String {
    hash_hex(&format!(
        "{FORMAT_VERSION}:{}",
        serde_json::to_string(settings).unwrap_or_default()
    ))
}

pub fn load_settings(dir: &Path) -> FolderSettings {
    std::fs::read(dir.join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_settings(dir: &Path, settings: &FolderSettings) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("settings.json"), serde_json::to_vec_pretty(settings)?)?;
    Ok(())
}

impl Workspace {
    /// Opens an existing workspace if its metadata is readable.
    pub fn open(root: &Path, dir: &Path) -> anyhow::Result<Workspace> {
        let meta: WorkspaceMeta = serde_json::from_slice(&std::fs::read(dir.join("meta.json"))?)?;
        if meta.version != FORMAT_VERSION {
            anyhow::bail!("workspace format {} is outdated", meta.version);
        }
        let segments = File::open(dir.join("segments.bin"))?;
        Ok(Workspace {
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
            settings: load_settings(dir),
            meta,
            segments,
        })
    }

    pub(crate) fn from_parts(
        root: &Path,
        dir: &Path,
        settings: FolderSettings,
        meta: WorkspaceMeta,
    ) -> anyhow::Result<Workspace> {
        let segments = File::open(dir.join("segments.bin"))?;
        Ok(Workspace {
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
            settings,
            meta,
            segments,
        })
    }

    /// True when the files on disk and the settings still match this workspace.
    pub fn is_fresh(&self, files: &[SourceFile], settings: &FolderSettings) -> bool {
        self.meta.settings_hash == settings_hash(settings)
            && self.meta.files.len() == files.len()
            && self
                .meta
                .files
                .iter()
                .zip(files)
                .all(|(a, b)| a.file.rel == b.rel && a.file.size == b.size && a.file.mtime_ms == b.mtime_ms)
    }

    /// Reads and decompresses a chunk's body.
    pub fn read_chunk(&self, meta: &ChunkMeta) -> anyhow::Result<Vec<u8>> {
        let mut buf = vec![0u8; meta.len as usize];
        read_exact_at(&self.segments, &mut buf, meta.offset)?;
        chunk::decompress(&buf)
    }

    pub fn file(&self, idx: u32) -> &FileEntry {
        &self.meta.files[idx as usize]
    }
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, offset)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, offset) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
