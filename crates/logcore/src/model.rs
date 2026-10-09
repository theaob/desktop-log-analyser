//! Core data types shared by ingest, storage and search.

use serde::{Deserialize, Serialize};

/// Log level, normalised across log4j 1.x/2.x, Logback, JUL-style names and syslog-ish names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
    Unknown = 6,
}

impl Level {
    pub const ALL: [Level; 7] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
        Level::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
            Level::Unknown => "unknown",
        }
    }

    pub fn from_u8(v: u8) -> Level {
        Level::ALL.get(v as usize).copied().unwrap_or(Level::Unknown)
    }

    /// Parses a level token as it appears in a log line (case-insensitive).
    pub fn parse(s: &str) -> Level {
        let s = s.trim();
        if s.len() > 9 {
            return Level::Unknown;
        }
        let mut buf = [0u8; 9];
        for (i, b) in s.bytes().enumerate() {
            buf[i] = b.to_ascii_uppercase();
        }
        match &buf[..s.len()] {
            b"TRACE" | b"TRC" | b"FINEST" | b"FINER" | b"VERBOSE" => Level::Trace,
            b"DEBUG" | b"DBG" | b"FINE" | b"CONFIG" => Level::Debug,
            b"INFO" | b"INF" | b"NOTICE" | b"INFORMATION" => Level::Info,
            b"WARN" | b"WARNING" | b"WRN" => Level::Warn,
            b"ERROR" | b"ERR" | b"SEVERE" => Level::Error,
            b"FATAL" | b"CRITICAL" | b"CRIT" | b"ALERT" | b"EMERG" | b"PANIC" => Level::Fatal,
            _ => Level::Unknown,
        }
    }
}

/// One log event as produced by ingest: a line plus any continuation lines (stack traces).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    /// Unix epoch milliseconds.
    pub ts: i64,
    pub level: Option<Level>,
    /// 1-based line number of the event's first line in its source file.
    pub line_no: u32,
    pub logger: String,
    pub thread: String,
    /// Exception class found in the event's stack trace, if any.
    pub exception: String,
    /// MDC / ThreadContext and other structured fields captured by the format.
    pub fields: Vec<(String, String)>,
    /// The full event text: the first line followed by continuation lines.
    pub message: String,
    /// True when no timestamp was found and `ts` was inherited.
    pub ts_inferred: bool,
}

impl Level {
    pub fn or_unknown(l: Option<Level>) -> Level {
        l.unwrap_or(Level::Unknown)
    }
}
