//! Running a query over a workspace: chunk pruning, a parallel scan, top-N rows for the
//! requested page, a total count and a level-stacked histogram, all in one pass.

use crate::chunk::{decode, ChunkMeta, EventRef};
use crate::model::Level;
use crate::query::{parse, CompiledQuery, ParseError};
use crate::store::Workspace;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub const DEFAULT_LIMIT: usize = 500;
const TARGET_BUCKETS: i64 = 120;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Newest first.
    #[default]
    Backward,
    /// Oldest first.
    Forward,
}

/// Position of a row in the global order: (timestamp, chunk, index in chunk).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RowKey {
    pub ts: i64,
    pub chunk: u32,
    pub idx: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchRequest {
    pub query: String,
    /// Inclusive start, epoch ms.
    pub from: Option<i64>,
    /// Exclusive end, epoch ms.
    pub to: Option<i64>,
    pub limit: Option<usize>,
    pub direction: Direction,
    /// Return rows after this key (in `direction` order), for "load more".
    pub after: Option<RowKey>,
    /// Skip the count and histogram (used when only paging).
    pub rows_only: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub key: RowKey,
    pub ts: i64,
    pub ts_inferred: bool,
    pub level: Level,
    pub source: String,
    pub file: String,
    pub line_no: u32,
    pub logger: String,
    pub thread: String,
    pub exception: String,
    /// Structured fields from ingest (MDC, JSON keys, ...).
    pub fields: Vec<(String, String)>,
    /// Fields extracted by parsers in the query.
    pub parsed: Vec<(String, String)>,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Histogram {
    pub start: i64,
    pub step: i64,
    /// Counts per bucket, indexed by `Level as usize`.
    pub buckets: Vec<[u64; 7]>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub rows: Vec<Row>,
    pub total: Option<u64>,
    pub histogram: Option<Histogram>,
    pub took_ms: u64,
    pub chunks_scanned: u64,
    pub chunks_total: u64,
    pub events_scanned: u64,
    /// Present when more rows exist after the last returned one.
    pub next: Option<RowKey>,
    pub cancelled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

const STEPS: &[i64] = &[
    1,
    2,
    5,
    10,
    20,
    50,
    100,
    200,
    500,
    1_000,
    2_000,
    5_000,
    10_000,
    15_000,
    30_000,
    60_000,
    120_000,
    300_000,
    600_000,
    900_000,
    1_800_000,
    3_600_000,
    7_200_000,
    10_800_000,
    21_600_000,
    43_200_000,
    86_400_000,
    172_800_000,
    604_800_000,
    1_209_600_000,
    2_592_000_000,
];

/// Picks a "nice" bucket size giving at most ~120 buckets over the range.
pub fn bucket_step(from: i64, to: i64) -> i64 {
    let span = (to - from).max(1);
    for &s in STEPS {
        if span / s <= TARGET_BUCKETS {
            return s;
        }
    }
    let last = *STEPS.last().unwrap();
    (span / TARGET_BUCKETS / last + 1) * last
}

/// Bigger is better in the requested direction.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Goodness(i64, i64, i64);

fn goodness(k: RowKey, dir: Direction) -> Goodness {
    match dir {
        Direction::Backward => Goodness(k.ts, k.chunk as i64, k.idx as i64),
        Direction::Forward => Goodness(-k.ts, -(k.chunk as i64), -(k.idx as i64)),
    }
}

struct Cand {
    g: Goodness,
    row: Row,
}
impl PartialEq for Cand {
    fn eq(&self, o: &Self) -> bool {
        self.g == o.g
    }
}
impl Eq for Cand {}
impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Cand {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.g.cmp(&o.g)
    }
}

struct Acc {
    /// Min-heap on goodness: the top is the worst kept row.
    heap: BinaryHeap<Reverse<Cand>>,
    total: u64,
    buckets: Vec<[u64; 7]>,
}

/// Visits every event in the time range matching the query, in no particular order.
/// Used by search and export.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_chunks<F>(
    ws: &Workspace,
    q: &CompiledQuery,
    from: i64,
    to: i64,
    extra_prune: impl Fn(&ChunkMeta) -> bool + Sync,
    cancel: &AtomicBool,
    init: impl Fn() -> F + Sync + Send,
    visit: impl Fn(&mut F, u32, &ChunkMeta, u32, &EventRef, Vec<(String, String)>) + Sync + Send,
) -> anyhow::Result<Vec<(F, u64, u64)>>
where
    F: Send,
{
    let candidates: Vec<u32> = ws
        .meta
        .chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.max_ts >= from && c.min_ts < to && extra_prune(c))
        .filter(|(_, c)| {
            let f = ws.file(c.file);
            q.chunk_matches(&f.file.source, &f.file.rel, Level::from_u8(c.level))
        })
        .map(|(i, _)| i as u32)
        .collect();
    let error: parking_lot::Mutex<Option<anyhow::Error>> = parking_lot::Mutex::new(None);
    let out = candidates
        .par_iter()
        .fold(
            || (init(), 0u64, 0u64),
            |(mut state, mut scanned, mut events), &ci| {
                if cancel.load(Ordering::Relaxed) {
                    return (state, scanned, events);
                }
                let meta = &ws.meta.chunks[ci as usize];
                let body = match ws.read_chunk(meta) {
                    Ok(b) => b,
                    Err(e) => {
                        *error.lock() = Some(e);
                        return (state, scanned, events);
                    }
                };
                scanned += 1;
                if !q.required_literals.iter().all(|f| f.find(&body).is_some()) {
                    return (state, scanned, events);
                }
                let decoded = match decode(&body) {
                    Ok(d) => d,
                    Err(e) => {
                        *error.lock() = Some(e);
                        return (state, scanned, events);
                    }
                };
                let level = Level::from_u8(meta.level);
                events += decoded.len() as u64;
                for (i, ev) in decoded.iter().enumerate() {
                    if ev.ts < from || ev.ts >= to {
                        continue;
                    }
                    if let Some(parsed) = q.eval(ev, level) {
                        visit(&mut state, ci, meta, i as u32, ev, parsed);
                    }
                }
                (state, scanned, events)
            },
        )
        .collect();
    if let Some(e) = error.into_inner() {
        return Err(e);
    }
    Ok(out)
}

pub fn make_row(ws: &Workspace, key: RowKey, meta: &ChunkMeta, ev: &EventRef, parsed: Vec<(String, String)>) -> Row {
    let f = ws.file(meta.file);
    Row {
        key,
        ts: ev.ts,
        ts_inferred: ev.ts_inferred,
        level: Level::from_u8(meta.level),
        source: f.file.source.clone(),
        file: f.file.rel.clone(),
        line_no: ev.line_no,
        logger: ev.logger.to_string(),
        thread: ev.thread.to_string(),
        exception: ev.exception.to_string(),
        fields: ev.fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        parsed,
        message: ev.message.to_string(),
    }
}

pub fn search(ws: &Workspace, req: &SearchRequest, cancel: &AtomicBool) -> Result<SearchResult, SearchError> {
    let started = Instant::now();
    let query = parse(&req.query)?;
    let q = CompiledQuery::compile(&query)?;
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 10_000);
    let from = req.from.unwrap_or(i64::MIN);
    let to = req.to.unwrap_or(i64::MAX);
    let dir = req.direction;
    let after = req.after;

    // Histogram over the requested range, or the whole folder when unbounded.
    let h_from = req.from.or(ws.meta.stats.min_ts).unwrap_or(0);
    let h_to = req.to.or(ws.meta.stats.max_ts.map(|t| t + 1)).unwrap_or(h_from + 1);
    let step = bucket_step(h_from, h_to);
    let h_start = h_from.div_euclid(step) * step;
    let n_buckets = ((h_to - h_start + step - 1) / step).max(1) as usize;
    let want_aggs = !req.rows_only;

    // When only paging, chunks entirely on the wrong side of the cursor can be skipped.
    let prune = |c: &ChunkMeta| match (want_aggs, after) {
        (false, Some(a)) => match dir {
            Direction::Backward => c.min_ts <= a.ts,
            Direction::Forward => c.max_ts >= a.ts,
        },
        _ => true,
    };
    let after_g = after.map(|a| goodness(a, dir));

    let parts = scan_chunks(
        ws,
        &q,
        from,
        to,
        prune,
        cancel,
        || Acc {
            heap: BinaryHeap::new(),
            total: 0,
            buckets: if want_aggs { vec![[0; 7]; n_buckets] } else { vec![] },
        },
        |acc, ci, meta, idx, ev, parsed| {
            acc.total += 1;
            if want_aggs {
                let b = ((ev.ts - h_start) / step).clamp(0, n_buckets as i64 - 1) as usize;
                acc.buckets[b][meta.level as usize] += 1;
            }
            let key = RowKey {
                ts: ev.ts,
                chunk: ci,
                idx,
            };
            let g = goodness(key, dir);
            // Rows at or before the cursor were already returned.
            if after_g.is_some_and(|a| g >= a) {
                return;
            }
            if acc.heap.len() > limit {
                if acc.heap.peek().is_some_and(|w| g <= w.0.g) {
                    return;
                }
                acc.heap.pop();
            }
            acc.heap.push(Reverse(Cand {
                g,
                row: make_row(ws, key, meta, ev, parsed),
            }));
        },
    )?;

    let mut total = 0u64;
    let mut chunks_scanned = 0u64;
    let mut events_scanned = 0u64;
    let mut buckets = if want_aggs { vec![[0u64; 7]; n_buckets] } else { vec![] };
    let mut all: Vec<Cand> = Vec::new();
    for (acc, scanned, events) in parts {
        total += acc.total;
        chunks_scanned += scanned;
        events_scanned += events;
        for (b, src) in buckets.iter_mut().zip(&acc.buckets) {
            for l in 0..7 {
                b[l] += src[l];
            }
        }
        all.extend(acc.heap.into_iter().map(|r| r.0));
    }
    all.sort_by_key(|c| Reverse(c.g));
    let has_more = all.len() > limit;
    all.truncate(limit);
    let rows: Vec<Row> = all.into_iter().map(|c| c.row).collect();
    let next = if has_more { rows.last().map(|r| r.key) } else { None };
    Ok(SearchResult {
        rows,
        total: want_aggs.then_some(total),
        histogram: want_aggs.then_some(Histogram {
            start: h_start,
            step,
            buckets,
        }),
        took_ms: started.elapsed().as_millis() as u64,
        chunks_scanned,
        chunks_total: ws.meta.chunks.len() as u64,
        events_scanned,
        next,
        cancelled: cancel.load(Ordering::Relaxed),
    })
}
