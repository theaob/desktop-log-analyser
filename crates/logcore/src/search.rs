//! Running a query over a workspace. Two independent phases the UI runs concurrently:
//!
//! - [`rows`]: the requested page. Candidate chunks are visited in time order (newest first for
//!   backward) in parallel batches, and the scan stops as soon as no remaining chunk can hold a
//!   row better than the worst one kept. First results come back without reading the whole folder.
//! - [`aggregate`]: total count and level-stacked histogram, which need every matching event.
//!
//! Both prune chunks by time, stream labels (source, file, level) and per-chunk Bloom filters
//! for exact field matchers, then skip chunks missing a required `|=` literal before decoding.

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
    /// [`search`] only: skip the count and histogram (used when only paging).
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Aggregates {
    pub total: u64,
    pub histogram: Histogram,
    pub took_ms: u64,
    pub chunks_scanned: u64,
    pub chunks_total: u64,
    pub events_scanned: u64,
    pub cancelled: bool,
}

/// Chunks that can hold a match: time range, stream labels, Bloom filter and `extra`.
pub(crate) fn candidates(
    ws: &Workspace,
    q: &CompiledQuery,
    from: i64,
    to: i64,
    extra: impl Fn(&ChunkMeta) -> bool,
) -> Vec<u32> {
    ws.meta
        .chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.max_ts >= from && c.min_ts < to && extra(c))
        .filter(|(_, c)| {
            let f = ws.file(c.file);
            q.chunk_matches(&f.file.source, &f.file.rel, Level::from_u8(c.level))
        })
        .filter(|(_, c)| q.bloom_matches(ws.bloom(c)))
        .map(|(i, _)| i as u32)
        .collect()
}

/// Reads one chunk and calls `visit` for each matching event in the time range.
/// Returns the number of events decoded (0 when the literal prefilter skipped the chunk).
fn scan_chunk(
    ws: &Workspace,
    q: &CompiledQuery,
    ci: u32,
    from: i64,
    to: i64,
    mut visit: impl FnMut(&ChunkMeta, u32, &EventRef, Vec<(String, String)>),
) -> anyhow::Result<u64> {
    let meta = &ws.meta.chunks[ci as usize];
    let body = ws.read_chunk(meta)?;
    if !q.required_literals.iter().all(|f| f.find(&body).is_some()) {
        return Ok(0);
    }
    let decoded = decode(&body)?;
    let level = Level::from_u8(meta.level);
    for (i, ev) in decoded.iter().enumerate() {
        if ev.ts < from || ev.ts >= to {
            continue;
        }
        if let Some(parsed) = q.eval(ev, level) {
            visit(meta, i as u32, ev, parsed);
        }
    }
    Ok(decoded.len() as u64)
}

/// Visits every event in the time range matching the query, in no particular order.
/// Returns one `(state, chunks scanned, events decoded)` per rayon fold. Used by aggregates
/// and export.
pub(crate) fn scan_chunks<F>(
    ws: &Workspace,
    q: &CompiledQuery,
    from: i64,
    to: i64,
    cancel: &AtomicBool,
    init: impl Fn() -> F + Sync + Send,
    visit: impl Fn(&mut F, u32, &ChunkMeta, u32, &EventRef, Vec<(String, String)>) + Sync + Send,
) -> anyhow::Result<Vec<(F, u64, u64)>>
where
    F: Send,
{
    let cands = candidates(ws, q, from, to, |_| true);
    let error: parking_lot::Mutex<Option<anyhow::Error>> = parking_lot::Mutex::new(None);
    let out = cands
        .par_iter()
        .fold(
            || (init(), 0u64, 0u64),
            |(mut state, mut scanned, mut events), &ci| {
                if cancel.load(Ordering::Relaxed) {
                    return (state, scanned, events);
                }
                match scan_chunk(ws, q, ci, from, to, |meta, idx, ev, parsed| {
                    visit(&mut state, ci, meta, idx, ev, parsed)
                }) {
                    Ok(n) => {
                        scanned += 1;
                        events += n;
                    }
                    Err(e) => *error.lock() = Some(e),
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

/// Keeps the best `cap` rows; the heap top is the worst kept one.
struct TopK {
    heap: BinaryHeap<Reverse<Cand>>,
    cap: usize,
}

impl TopK {
    fn new(cap: usize) -> Self {
        TopK {
            heap: BinaryHeap::new(),
            cap,
        }
    }

    fn would_keep(&self, g: Goodness) -> bool {
        self.heap.len() < self.cap || self.heap.peek().is_some_and(|w| g > w.0.g)
    }

    fn push(&mut self, c: Cand) {
        if self.heap.len() >= self.cap {
            self.heap.pop();
        }
        self.heap.push(Reverse(c));
    }

    fn worst(&self) -> Option<Goodness> {
        (self.heap.len() >= self.cap)
            .then(|| self.heap.peek().map(|w| w.0.g))
            .flatten()
    }
}

/// Returns the requested page of rows, scanning only as many chunks as needed.
pub fn rows(ws: &Workspace, req: &SearchRequest, cancel: &AtomicBool) -> Result<SearchResult, SearchError> {
    let started = Instant::now();
    let q = CompiledQuery::compile(&parse(&req.query)?)?;
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 10_000);
    let from = req.from.unwrap_or(i64::MIN);
    let to = req.to.unwrap_or(i64::MAX);
    let dir = req.direction;
    let after = req.after;
    let after_g = after.map(|a| goodness(a, dir));

    // Chunks entirely on the wrong side of the cursor were already returned.
    let mut cands = candidates(ws, &q, from, to, |c| match (after, dir) {
        (None, _) => true,
        (Some(a), Direction::Backward) => c.min_ts <= a.ts,
        (Some(a), Direction::Forward) => c.max_ts >= a.ts,
    });
    // Best chunk first: the best row a chunk can hold has its max_ts (backward) or min_ts (forward).
    let best_ts = |ci: u32| {
        let c = &ws.meta.chunks[ci as usize];
        match dir {
            Direction::Backward => c.max_ts,
            Direction::Forward => -c.min_ts,
        }
    };
    cands.sort_by_key(|&ci| Reverse(best_ts(ci)));

    // One extra row tells whether there is a next page.
    let mut top = TopK::new(limit + 1);
    let mut chunks_scanned = 0u64;
    let mut events_scanned = 0u64;
    let mut pos = 0;
    let mut batch = rayon::current_num_threads().max(2);
    while pos < cands.len() && !cancel.load(Ordering::Relaxed) {
        // A chunk can't beat the worst kept row when its best timestamp is strictly worse
        // (equal timestamps may still win on the chunk/index tiebreak).
        if let Some(w) = top.worst() {
            if best_ts(cands[pos]) < w.0 {
                break;
            }
        }
        let end = (pos + batch).min(cands.len());
        let threshold = top.worst();
        let parts: Vec<anyhow::Result<(Vec<Cand>, u64)>> = cands[pos..end]
            .par_iter()
            .map(|&ci| {
                let mut local = TopK::new(limit + 1);
                let n = scan_chunk(ws, &q, ci, from, to, |meta, idx, ev, parsed| {
                    let key = RowKey {
                        ts: ev.ts,
                        chunk: ci,
                        idx,
                    };
                    let g = goodness(key, dir);
                    if after_g.is_some_and(|a| g >= a) || threshold.is_some_and(|t| g <= t) || !local.would_keep(g) {
                        return;
                    }
                    local.push(Cand {
                        g,
                        row: make_row(ws, key, meta, ev, parsed),
                    });
                })?;
                Ok((local.heap.into_iter().map(|r| r.0).collect(), n))
            })
            .collect();
        for part in parts {
            let (cands, n) = part?;
            chunks_scanned += 1;
            events_scanned += n;
            for c in cands {
                if top.would_keep(c.g) {
                    top.push(c);
                }
            }
        }
        pos = end;
        batch = (batch * 2).min(256);
    }

    let mut all: Vec<Cand> = top.heap.into_iter().map(|r| r.0).collect();
    all.sort_by_key(|c| Reverse(c.g));
    let has_more = all.len() > limit;
    all.truncate(limit);
    let rows: Vec<Row> = all.into_iter().map(|c| c.row).collect();
    let next = if has_more { rows.last().map(|r| r.key) } else { None };
    Ok(SearchResult {
        rows,
        total: None,
        histogram: None,
        took_ms: started.elapsed().as_millis() as u64,
        chunks_scanned,
        chunks_total: ws.meta.chunks.len() as u64,
        events_scanned,
        next,
        cancelled: cancel.load(Ordering::Relaxed),
    })
}

/// Total matches and the level-stacked histogram over the requested range (or the whole
/// folder when unbounded). Ignores `limit`, `direction` and `after`.
pub fn aggregate(ws: &Workspace, req: &SearchRequest, cancel: &AtomicBool) -> Result<Aggregates, SearchError> {
    let started = Instant::now();
    let q = CompiledQuery::compile(&parse(&req.query)?)?;
    let from = req.from.unwrap_or(i64::MIN);
    let to = req.to.unwrap_or(i64::MAX);
    let h_from = req.from.or(ws.meta.stats.min_ts).unwrap_or(0);
    let h_to = req.to.or(ws.meta.stats.max_ts.map(|t| t + 1)).unwrap_or(h_from + 1);
    let step = bucket_step(h_from, h_to);
    let h_start = h_from.div_euclid(step) * step;
    let n_buckets = ((h_to - h_start + step - 1) / step).max(1) as usize;

    let parts = scan_chunks(
        ws,
        &q,
        from,
        to,
        cancel,
        || (0u64, vec![[0u64; 7]; n_buckets]),
        |(total, buckets), _, meta, _, ev, _| {
            *total += 1;
            let b = ((ev.ts - h_start) / step).clamp(0, n_buckets as i64 - 1) as usize;
            buckets[b][meta.level as usize] += 1;
        },
    )?;
    let mut total = 0;
    let mut buckets = vec![[0u64; 7]; n_buckets];
    let mut chunks_scanned = 0;
    let mut events_scanned = 0;
    for ((t, b), scanned, events) in parts {
        total += t;
        chunks_scanned += scanned;
        events_scanned += events;
        for (dst, src) in buckets.iter_mut().zip(&b) {
            for l in 0..7 {
                dst[l] += src[l];
            }
        }
    }
    Ok(Aggregates {
        total,
        histogram: Histogram {
            start: h_start,
            step,
            buckets,
        },
        took_ms: started.elapsed().as_millis() as u64,
        chunks_scanned,
        chunks_total: ws.meta.chunks.len() as u64,
        events_scanned,
        cancelled: cancel.load(Ordering::Relaxed),
    })
}

/// Rows plus, unless `rows_only`, the aggregates, one after the other (CLI and tests).
pub fn search(ws: &Workspace, req: &SearchRequest, cancel: &AtomicBool) -> Result<SearchResult, SearchError> {
    let mut res = rows(ws, req, cancel)?;
    if !req.rows_only {
        let aggs = aggregate(ws, req, cancel)?;
        res.total = Some(aggs.total);
        res.histogram = Some(aggs.histogram);
        res.took_ms += aggs.took_ms;
        res.chunks_scanned += aggs.chunks_scanned;
        res.events_scanned += aggs.events_scanned;
        res.cancelled = aggs.cancelled;
    }
    Ok(res)
}
