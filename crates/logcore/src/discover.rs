//! Finding log files in a folder and grouping rolled files (`app.log`, `app.log.1`,
//! `app-2026-10-08-1.log.gz`) into one source per appender.

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

pub const DEFAULT_INCLUDE: &[&str] = &[
    "**/*.log",
    "**/*.log.*",
    "**/*.txt",
    "**/*.out",
    "**/*.json",
    "**/*.jsonl",
    "**/*.gz",
];
pub const DEFAULT_EXCLUDE: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/*.tar.gz",
    "**/*.tgz",
    "**/*.zip",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    #[serde(skip)]
    pub path: PathBuf,
    /// Path relative to the folder root, with `/` separators.
    pub rel: String,
    pub size: u64,
    pub mtime_ms: i64,
    pub gz: bool,
    /// Rolled-file group, e.g. `payments/app.log`.
    pub source: String,
}

fn build_set(globs: &[String]) -> anyhow::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        let g = g.trim();
        if g.is_empty() {
            continue;
        }
        // A bare pattern like `*.log` matches at any depth.
        let pattern = if g.contains('/') {
            g.to_string()
        } else {
            format!("**/{g}")
        };
        b.add(Glob::new(&pattern)?);
    }
    Ok(b.build()?)
}

/// Lists log files under `root`, sorted by relative path.
pub fn discover(root: &Path, include: &[String], exclude: &[String]) -> anyhow::Result<Vec<SourceFile>> {
    let include = if include.is_empty() {
        DEFAULT_INCLUDE.iter().map(|s| s.to_string()).collect()
    } else {
        include.to_vec()
    };
    let exclude = if exclude.is_empty() {
        DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect()
    } else {
        exclude.to_vec()
    };
    let inc = build_set(&include)?;
    let exc = build_set(&exclude)?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel_path) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel_path
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if !inc.is_match(&rel) || exc.is_match(&rel) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let gz = rel.to_ascii_lowercase().ends_with(".gz");
        files.push(SourceFile {
            path: entry.path().to_path_buf(),
            source: source_key(&rel),
            rel,
            size: meta.len(),
            mtime_ms,
            gz,
        });
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}

/// Maps a rolled file name to its appender's base name.
pub fn source_key(rel: &str) -> String {
    static DATE: OnceLock<Regex> = OnceLock::new();
    static INDEX_AFTER_EXT: OnceLock<Regex> = OnceLock::new();
    static INDEX_BEFORE_EXT: OnceLock<Regex> = OnceLock::new();
    let date = DATE.get_or_init(|| Regex::new(r"[-_.]?\d{4}-?\d{2}-?\d{2}(?:[-_.T]?\d{2}(?:-?\d{2}){0,2})?").unwrap());
    let index_after = INDEX_AFTER_EXT.get_or_init(|| Regex::new(r"\.(?:log|txt|out)(\.\d+)$").unwrap());
    let index_before = INDEX_BEFORE_EXT.get_or_init(|| Regex::new(r"[-_.]\d{1,5}(\.(?:log|txt|out|json))$").unwrap());

    let (dir, name) = match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    };
    let mut name = name.to_string();
    for ext in [".gz", ".GZ"] {
        if let Some(n) = name.strip_suffix(ext) {
            name = n.to_string();
        }
    }
    if let Some(c) = index_after.captures(&name) {
        let r = c.get(1).unwrap().range();
        name.replace_range(r, "");
    }
    let without_date = date.replace(&name, "").into_owned();
    if without_date != name && !without_date.starts_with('.') && !without_date.is_empty() {
        name = index_before.replace(&without_date, "$1").into_owned();
    }
    format!("{dir}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolled_names_group() {
        assert_eq!(source_key("app.log"), "app.log");
        assert_eq!(source_key("app.log.1"), "app.log");
        assert_eq!(source_key("app.log.12.gz"), "app.log");
        assert_eq!(source_key("app-2026-10-08-1.log.gz"), "app.log");
        assert_eq!(source_key("logs/app.2026-10-08.log"), "logs/app.log");
        assert_eq!(source_key("logs/app-20261008.log"), "logs/app.log");
        assert_eq!(source_key("node-1.log"), "node-1.log");
        assert_eq!(source_key("2026-10-08.log"), "2026-10-08.log");
    }

    #[test]
    fn discovers_with_globs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("svc/.git")).unwrap();
        std::fs::write(dir.path().join("svc/app.log"), "x").unwrap();
        std::fs::write(dir.path().join("svc/app.log.1"), "x").unwrap();
        std::fs::write(dir.path().join("svc/.git/HEAD.log"), "x").unwrap();
        std::fs::write(dir.path().join("svc/image.png"), "x").unwrap();
        let files = discover(dir.path(), &[], &[]).unwrap();
        let rels: Vec<_> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, vec!["svc/app.log", "svc/app.log.1"]);
        assert!(files.iter().all(|f| f.source == "svc/app.log"));
        let only = discover(dir.path(), &["app.log".into()], &[]).unwrap();
        assert_eq!(only.len(), 1);
    }
}
