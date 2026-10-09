//! Command-line front end to the core engine, for benchmarking and debugging without the UI.
//!
//! ```text
//! logq <folder> [query] [--from <ms>] [--to <ms>] [--limit N] [--pattern <PatternLayout>]
//!                       [--data <dir>] [--force] [--preview] [--labels] [--forward]
//! ```

use logcore::search::Direction;
use logcore::{FolderSettings, Progress, SearchRequest};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut folder: Option<PathBuf> = None;
    let mut query = String::new();
    let mut req = SearchRequest {
        limit: Some(20),
        ..Default::default()
    };
    let mut patterns = Vec::new();
    let mut data_dir = std::env::temp_dir().join("logq-data");
    let (mut force, mut preview, mut labels) = (false, false, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--from" => req.from = args.next().and_then(|v| v.parse().ok()),
            "--to" => req.to = args.next().and_then(|v| v.parse().ok()),
            "--limit" => req.limit = args.next().and_then(|v| v.parse().ok()),
            "--pattern" => patterns.extend(args.next()),
            "--data" => data_dir = args.next().map(PathBuf::from).unwrap_or(data_dir),
            "--force" => force = true,
            "--preview" => preview = true,
            "--labels" => labels = true,
            "--forward" => req.direction = Direction::Forward,
            "-h" | "--help" => {
                println!("usage: logq <folder> [query] [--from ms] [--to ms] [--limit N] [--pattern P] [--data dir] [--force] [--preview] [--labels] [--forward]");
                return Ok(());
            }
            _ if folder.is_none() => folder = Some(PathBuf::from(a)),
            _ => query = a,
        }
    }
    let folder = folder.ok_or_else(|| anyhow::anyhow!("missing <folder>; see --help"))?;
    let settings = (!patterns.is_empty()).then(|| FolderSettings {
        patterns,
        ..Default::default()
    });

    if preview {
        let p = logcore::preview(&folder, &settings.unwrap_or_default(), 5)?;
        println!("{}", serde_json::to_string_pretty(&p)?);
        return Ok(());
    }

    let started = Instant::now();
    let progress = Progress::default();
    let ws = logcore::open_or_build(&folder, &data_dir, settings, &progress, force)?;
    let raw: u64 = ws.meta.files.iter().map(|f| f.file.size).sum();
    eprintln!(
        "workspace: {} files, {:.1} MB, {} events, {} chunks, opened in {:.2}s (last index took {:.2}s)",
        ws.meta.files.len(),
        raw as f64 / 1e6,
        ws.meta.stats.events,
        ws.meta.chunks.len(),
        started.elapsed().as_secs_f64(),
        ws.meta.index_millis as f64 / 1000.0
    );
    for f in ws.meta.files.iter().filter(|f| f.error.is_some()) {
        eprintln!("  error in {}: {}", f.file.rel, f.error.as_deref().unwrap_or(""));
    }
    if labels {
        for l in logcore::labels::labels(&ws) {
            let vals: Vec<String> = l.values.iter().take(8).map(|(v, c)| format!("{v}({c})")).collect();
            println!(
                "{}{}: {}",
                l.name,
                if l.stream { " [stream]" } else { "" },
                vals.join(", ")
            );
        }
        return Ok(());
    }
    req.query = query;
    let res = logcore::search(&ws, &req, &AtomicBool::new(false))?;
    for r in &res.rows {
        let first = r.message.lines().next().unwrap_or("");
        let extra = r.message.lines().count().saturating_sub(1);
        let more = if extra > 0 {
            format!("  (+{extra} lines)")
        } else {
            String::new()
        };
        println!("{:>5} {}{more}", r.level.as_str(), first);
    }
    eprintln!(
        "{} matches, {} rows shown, {}/{} chunks scanned, {} events scanned, {} ms",
        res.total.unwrap_or(0),
        res.rows.len(),
        res.chunks_scanned,
        res.chunks_total,
        res.events_scanned,
        res.took_ms
    );
    Ok(())
}
