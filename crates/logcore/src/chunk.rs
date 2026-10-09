//! Chunk encoding. A chunk holds up to [`CHUNK_EVENTS`] events of one file and one level,
//! LZ4-compressed, stored back to back in the workspace's segment file.

use crate::bloom;
use crate::model::Event;
use serde::{Deserialize, Serialize};

pub const CHUNK_EVENTS: usize = 2048;
pub const CHUNK_RAW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub file: u32,
    pub level: u8,
    pub min_ts: i64,
    pub max_ts: i64,
    pub count: u32,
    pub offset: u64,
    pub len: u32,
    /// Location of the chunk's field Bloom filter in `blooms.bin`.
    pub bloom_offset: u64,
    pub bloom_len: u32,
}

/// An encoded chunk ready to be written.
pub struct FinishedChunk {
    pub bytes: Vec<u8>,
    pub bloom: Vec<u8>,
    pub count: u32,
    pub min_ts: i64,
    pub max_ts: i64,
}

/// A decoded event borrowing from a decompressed chunk buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRef<'a> {
    pub ts: i64,
    pub line_no: u32,
    pub ts_inferred: bool,
    pub logger: &'a str,
    pub thread: &'a str,
    pub exception: &'a str,
    pub fields: Vec<(&'a str, &'a str)>,
    pub message: &'a str,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Accumulates events and produces encoded (uncompressed) chunk bodies.
#[derive(Default)]
pub struct ChunkBuilder {
    buf: Vec<u8>,
    count: u32,
    prev_ts: i64,
    pub min_ts: i64,
    pub max_ts: i64,
    keys: std::collections::HashSet<u64>,
}

impl ChunkBuilder {
    pub fn push(&mut self, ev: &Event) {
        if self.count == 0 {
            self.min_ts = ev.ts;
            self.max_ts = ev.ts;
        } else {
            self.min_ts = self.min_ts.min(ev.ts);
            self.max_ts = self.max_ts.max(ev.ts);
        }
        put_varint(&mut self.buf, zigzag(ev.ts - self.prev_ts));
        self.prev_ts = ev.ts;
        put_varint(&mut self.buf, ev.line_no as u64);
        self.buf.push(ev.ts_inferred as u8);
        put_str(&mut self.buf, &ev.logger);
        put_str(&mut self.buf, &ev.thread);
        put_str(&mut self.buf, &ev.exception);
        for (name, value) in [
            ("logger", &ev.logger),
            ("thread", &ev.thread),
            ("exception", &ev.exception),
        ] {
            if !value.is_empty() {
                self.keys.insert(bloom::key_hash(name, value));
            }
        }
        put_varint(&mut self.buf, ev.fields.len() as u64);
        for (k, v) in &ev.fields {
            put_str(&mut self.buf, k);
            put_str(&mut self.buf, v);
            self.keys.insert(bloom::key_hash(k, v));
        }
        put_str(&mut self.buf, &ev.message);
        self.count += 1;
    }

    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn is_full(&self) -> bool {
        self.count as usize >= CHUNK_EVENTS || self.buf.len() >= CHUNK_RAW_BYTES
    }

    /// Compresses the chunk, builds its Bloom filter and resets the builder.
    pub fn finish(&mut self) -> FinishedChunk {
        let mut body = Vec::with_capacity(self.buf.len() + 8);
        put_varint(&mut body, self.count as u64);
        body.extend_from_slice(&self.buf);
        let keys: Vec<u64> = self.keys.iter().copied().collect();
        let out = FinishedChunk {
            bytes: lz4_flex::compress_prepend_size(&body),
            bloom: bloom::build(&keys),
            count: self.count,
            min_ts: self.min_ts,
            max_ts: self.max_ts,
        };
        *self = ChunkBuilder::default();
        out
    }
}

pub fn decompress(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    Ok(lz4_flex::decompress_size_prepended(bytes)?)
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn varint(&mut self) -> anyhow::Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = *self
                .buf
                .get(self.pos)
                .ok_or_else(|| anyhow::anyhow!("truncated chunk"))?;
            self.pos += 1;
            v |= ((b & 0x7F) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                anyhow::bail!("bad varint");
            }
        }
    }

    fn str(&mut self) -> anyhow::Result<&'a str> {
        let len = self.varint()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| anyhow::anyhow!("truncated chunk"))?;
        let s = std::str::from_utf8(&self.buf[self.pos..end])?;
        self.pos = end;
        Ok(s)
    }

    fn byte(&mut self) -> anyhow::Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| anyhow::anyhow!("truncated chunk"))?;
        self.pos += 1;
        Ok(b)
    }
}

/// Decodes all events in a decompressed chunk body.
pub fn decode(body: &[u8]) -> anyhow::Result<Vec<EventRef<'_>>> {
    let mut c = Cursor { buf: body, pos: 0 };
    let count = c.varint()? as usize;
    let mut out = Vec::with_capacity(count);
    let mut ts = 0i64;
    for _ in 0..count {
        ts += unzigzag(c.varint()?);
        let line_no = c.varint()? as u32;
        let ts_inferred = c.byte()? != 0;
        let logger = c.str()?;
        let thread = c.str()?;
        let exception = c.str()?;
        let nfields = c.varint()? as usize;
        let mut fields = Vec::with_capacity(nfields);
        for _ in 0..nfields {
            let k = c.str()?;
            let v = c.str()?;
            fields.push((k, v));
        }
        let message = c.str()?;
        out.push(EventRef {
            ts,
            line_no,
            ts_inferred,
            logger,
            thread,
            exception,
            fields,
            message,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut b = ChunkBuilder::default();
        let e1 = Event {
            ts: 1000,
            line_no: 1,
            logger: "a.B".into(),
            message: "hello\nworld".into(),
            fields: vec![("k".into(), "v".into())],
            ..Default::default()
        };
        let e2 = Event {
            ts: 900,
            line_no: 3,
            thread: "main".into(),
            exception: "x.YException".into(),
            message: "ü".into(),
            ts_inferred: true,
            ..Default::default()
        };
        b.push(&e1);
        b.push(&e2);
        let done = b.finish();
        assert_eq!((done.count, done.min_ts, done.max_ts), (2, 900, 1000));
        assert!(bloom::may_contain(&done.bloom, bloom::key_hash("k", "v")));
        assert!(bloom::may_contain(&done.bloom, bloom::key_hash("thread", "main")));
        let body = decompress(&done.bytes).unwrap();
        let evs = decode(&body).unwrap();
        assert_eq!(evs[0].ts, 1000);
        assert_eq!(evs[0].fields, vec![("k", "v")]);
        assert_eq!(evs[1].ts, 900);
        assert_eq!(evs[1].exception, "x.YException");
        assert!(evs[1].ts_inferred);
        assert_eq!(evs[1].message, "ü");
    }
}
