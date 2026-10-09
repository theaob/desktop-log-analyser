//! Exporting all rows matching a query to CSV or JSON lines, in time order.

use crate::chunk::{decode, EventRef};
use crate::query::{parse, CompiledQuery};
use crate::search::{make_row, scan_chunks, Direction, Row, RowKey, SearchRequest};
use crate::store::Workspace;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Csv,
    Json,
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn write_row(out: &mut impl Write, row: &Row, format: ExportFormat) -> std::io::Result<()> {
    match format {
        ExportFormat::Csv => {
            let fields: Vec<String> = row
                .fields
                .iter()
                .chain(&row.parsed)
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            writeln!(
                out,
                "{},{},{},{},{},{},{},{},{}",
                iso(row.ts),
                row.level.as_str(),
                csv_cell(&row.logger),
                csv_cell(&row.thread),
                csv_cell(&row.exception),
                csv_cell(&row.file),
                row.line_no,
                csv_cell(&fields.join(" ")),
                csv_cell(&row.message)
            )
        }
        ExportFormat::Json => {
            let mut obj = serde_json::Map::new();
            obj.insert("time".into(), iso(row.ts).into());
            obj.insert("level".into(), row.level.as_str().into());
            obj.insert("logger".into(), row.logger.clone().into());
            obj.insert("thread".into(), row.thread.clone().into());
            if !row.exception.is_empty() {
                obj.insert("exception".into(), row.exception.clone().into());
            }
            obj.insert("file".into(), row.file.clone().into());
            obj.insert("line".into(), row.line_no.into());
            for (k, v) in row.fields.iter().chain(&row.parsed) {
                obj.entry(k.clone()).or_insert_with(|| v.clone().into());
            }
            obj.insert("message".into(), row.message.clone().into());
            writeln!(out, "{}", serde_json::Value::Object(obj))
        }
    }
}

/// Writes every matching row to `path`; returns the number of rows written.
pub fn export(
    ws: &Workspace,
    req: &SearchRequest,
    path: &Path,
    format: ExportFormat,
    cancel: &AtomicBool,
) -> anyhow::Result<u64> {
    let q = CompiledQuery::compile(&parse(&req.query)?)?;
    let from = req.from.unwrap_or(i64::MIN);
    let to = req.to.unwrap_or(i64::MAX);
    let parts = scan_chunks(
        ws,
        &q,
        from,
        to,
        |_| true,
        cancel,
        Vec::new,
        |keys: &mut Vec<RowKey>, ci, _, idx, ev, _| {
            keys.push(RowKey {
                ts: ev.ts,
                chunk: ci,
                idx,
            });
        },
    )?;
    let mut keys: Vec<RowKey> = parts.into_iter().flat_map(|(k, _, _)| k).collect();
    keys.sort_unstable();
    if req.direction == Direction::Backward {
        keys.reverse();
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    if format == ExportFormat::Csv {
        writeln!(out, "time,level,logger,thread,exception,file,line,fields,message")?;
    }
    // Rows interleave across chunks; keep recently used chunks decoded.
    let mut cache: HashMap<u32, Vec<Row>> = HashMap::new();
    let mut order: Vec<u32> = Vec::new();
    for key in &keys {
        let meta = &ws.meta.chunks[key.chunk as usize];
        if !cache.contains_key(&key.chunk) {
            if order.len() >= 64 {
                let old = order.remove(0);
                cache.remove(&old);
            }
            let body = ws.read_chunk(meta)?;
            let rows = decode(&body)?
                .iter()
                .enumerate()
                .map(|(i, ev)| {
                    make_row(
                        ws,
                        RowKey {
                            ts: ev.ts,
                            chunk: key.chunk,
                            idx: i as u32,
                        },
                        meta,
                        ev,
                        Vec::new(),
                    )
                })
                .collect();
            cache.insert(key.chunk, rows);
            order.push(key.chunk);
        }
        let mut row = cache[&key.chunk][key.idx as usize].clone();
        if q.has_parsers {
            let ev = EventRef {
                ts: row.ts,
                line_no: row.line_no,
                ts_inferred: row.ts_inferred,
                logger: &row.logger,
                thread: &row.thread,
                exception: &row.exception,
                fields: row.fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect(),
                message: &row.message,
            };
            row.parsed = q.eval(&ev, row.level).unwrap_or_default();
        }
        write_row(&mut out, &row, format)?;
    }
    out.flush()?;
    Ok(keys.len() as u64)
}
