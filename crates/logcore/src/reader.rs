//! Reading log files line by line: gzip, UTF-8/UTF-16 with BOM detection, CRLF, and
//! lossy decoding of invalid UTF-8.

use flate2::read::MultiGzDecoder;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts raw bytes read from disk, for progress reporting.
struct Counting<'a, R> {
    inner: R,
    counter: Option<&'a AtomicU64>,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(c) = self.counter {
            c.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(n)
    }
}

fn open<'a>(path: &Path, gz: bool, counter: Option<&'a AtomicU64>) -> io::Result<Box<dyn BufRead + 'a>> {
    let file = Counting {
        inner: File::open(path)?,
        counter,
    };
    let raw: Box<dyn Read + 'a> = if gz {
        Box::new(MultiGzDecoder::new(BufReader::with_capacity(256 * 1024, file)))
    } else {
        Box::new(file)
    };
    let mut buffered = BufReader::with_capacity(256 * 1024, raw);
    // Sniff a UTF-16 byte order mark; such files are transcoded to UTF-8.
    let head = buffered.fill_buf()?;
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        let decoder = encoding_rs_io::DecodeReaderBytesBuilder::new()
            .bom_sniffing(true)
            .build(buffered);
        return Ok(Box::new(BufReader::with_capacity(256 * 1024, decoder)));
    }
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        buffered.consume(3);
    }
    Ok(Box::new(buffered))
}

/// Calls `f(line, line_no)` for every line. Line terminators (`\n`, `\r\n`) are stripped.
/// Returning `false` from `f` stops reading.
pub fn for_each_line(
    path: &Path,
    gz: bool,
    counter: Option<&AtomicU64>,
    mut f: impl FnMut(&str, u32) -> bool,
) -> io::Result<()> {
    let mut reader = open(path, gz, counter)?;
    let mut buf = Vec::with_capacity(4096);
    let mut line_no: u32 = 0;
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        line_no = line_no.saturating_add(1);
        let mut end = buf.len();
        if end > 0 && buf[end - 1] == b'\n' {
            end -= 1;
        }
        if end > 0 && buf[end - 1] == b'\r' {
            end -= 1;
        }
        let line = String::from_utf8_lossy(&buf[..end]);
        if !f(&line, line_no) {
            break;
        }
    }
    Ok(())
}

/// Reads up to `max_lines` lines from the start of a file.
pub fn sample_lines(path: &Path, gz: bool, max_lines: usize) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for_each_line(path, gz, None, |l, _| {
        out.push(l.to_string());
        out.len() < max_lines
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn crlf_gzip_and_utf16() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.log");
        std::fs::write(&p, b"\xEF\xBB\xBFone\r\ntwo\nthree").unwrap();
        assert_eq!(sample_lines(&p, false, 10).unwrap(), vec!["one", "two", "three"]);

        let gz = dir.path().join("a.log.gz");
        let mut enc = flate2::write::GzEncoder::new(File::create(&gz).unwrap(), flate2::Compression::fast());
        enc.write_all(b"x\ny\n").unwrap();
        enc.finish().unwrap();
        assert_eq!(sample_lines(&gz, true, 10).unwrap(), vec!["x", "y"]);

        let u16p = dir.path().join("w.log");
        let mut bytes = vec![0xFF, 0xFE];
        for c in "hi\r\nyo".encode_utf16() {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        std::fs::write(&u16p, bytes).unwrap();
        assert_eq!(sample_lines(&u16p, false, 10).unwrap(), vec!["hi", "yo"]);
    }
}
