//! Turning lines into events: format detection, first-line parsing and multi-line grouping
//! (stack traces and other continuation lines are appended to the previous event).

use crate::formats::{finish_xml, is_xml_wrapper, parse_logfmt, parse_syslog, parse_xml_start};
use crate::log4j::{json_keys, Capture, CompiledPattern, COMMON_PATTERNS};
use crate::model::{Event, Level};
use crate::time::{assemble, DateParts, GenericTimestamp, TzMode};
use chrono::NaiveDate;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

/// Maximum stored size of a single event; longer events are truncated.
pub const MAX_EVENT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub enum Format {
    /// log4j / Logback PatternLayout.
    Log4j(Arc<CompiledPattern>),
    /// One JSON object per line (JsonTemplateLayout, compact JSONLayout, Logstash encoder).
    JsonLines,
    /// log4j 1.x XMLLayout or log4j 2 XmlLayout; one event spans several lines.
    Xml,
    /// RFC 5424 or RFC 3164 (BSD) syslog lines.
    Syslog,
    /// `key=value` pairs with a time or level key.
    Logfmt,
    /// Unknown text: generic timestamp and level detection. `timestamped` means events start
    /// at lines with a leading timestamp; otherwise every line is an event.
    Plain { timestamped: bool },
}

/// Serializable description of a detected format, shown to the user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FormatInfo {
    Log4j { pattern: String },
    JsonLines,
    Xml,
    Syslog,
    Logfmt,
    Plain { timestamped: bool },
}

impl Format {
    pub fn info(&self) -> FormatInfo {
        match self {
            Format::Log4j(p) => FormatInfo::Log4j {
                pattern: p.pattern.clone(),
            },
            Format::JsonLines => FormatInfo::JsonLines,
            Format::Xml => FormatInfo::Xml,
            Format::Syslog => FormatInfo::Syslog,
            Format::Logfmt => FormatInfo::Logfmt,
            Format::Plain { timestamped } => FormatInfo::Plain {
                timestamped: *timestamped,
            },
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Format::Log4j(_) => "log4j",
            Format::JsonLines => "json",
            Format::Xml => "xml",
            Format::Syslog => "syslog",
            Format::Logfmt => "logfmt",
            Format::Plain { .. } => "plain",
        }
    }
}

pub struct ParseCtx {
    pub tz: TzMode,
    pub default_date: NaiveDate,
    /// Timestamp used for events before the first timestamped line (the file's mtime).
    pub fallback_ts: i64,
}

fn generic_ts() -> &'static GenericTimestamp {
    static G: OnceLock<GenericTimestamp> = OnceLock::new();
    G.get_or_init(GenericTimestamp::new)
}

fn level_word() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\b(TRACE|DEBUG|INFO|NOTICE|WARN|WARNING|ERROR|SEVERE|FATAL|CRITICAL|Trace|Debug|Info|Warn|Warning|Error|Fatal)\b").unwrap()
    })
}

fn exception_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^\s*(?:Caused by: |Exception in thread .*? )?((?:[a-zA-Z_$][\w$]*\.)+[A-Z][\w$]*(?:Exception|Error|Throwable|Fault)(?:\$[\w$]+)?)(?::|\s*$)").unwrap()
    })
}

/// Extracts an exception class name from a stack-trace line, e.g.
/// `java.net.SocketTimeoutException: Read timed out` -> `java.net.SocketTimeoutException`.
pub fn exception_class(line: &str) -> Option<&str> {
    exception_re().captures(line).and_then(|c| c.get(1)).map(|m| m.as_str())
}

impl Format {
    /// Parses a line that starts an event, or returns None for a continuation line.
    pub fn parse_start(&self, line: &str, ctx: &ParseCtx) -> Option<Event> {
        match self {
            Format::Log4j(p) => parse_log4j(p, line, ctx),
            Format::JsonLines => parse_json(line, ctx),
            Format::Xml => parse_xml_start(line),
            Format::Syslog => parse_syslog(line, ctx, generic_ts(), level_word()),
            Format::Logfmt => parse_logfmt(line, ctx, generic_ts()),
            Format::Plain { timestamped } => {
                let found = generic_ts().find(line, ctx.tz, ctx.default_date);
                if *timestamped && found.is_none() {
                    return None;
                }
                let mut ev = Event {
                    message: line.to_string(),
                    ..Default::default()
                };
                match found {
                    Some((ts, _)) => ev.ts = ts,
                    None => ev.ts_inferred = true,
                }
                let head = &line[..crate::time::floor_char_boundary(line, 160)];
                if let Some(m) = level_word().find(head) {
                    ev.level = Some(Level::parse(m.as_str()));
                }
                Some(ev)
            }
        }
    }
}

fn parse_log4j(p: &CompiledPattern, line: &str, ctx: &ParseCtx) -> Option<Event> {
    let caps = p.regex.captures(line)?;
    let mut ev = Event {
        message: line.to_string(),
        ..Default::default()
    };
    let mut parts = DateParts::default();
    for (idx, cap) in &p.captures {
        let Some(m) = caps.get(*idx) else { continue };
        let v = m.as_str();
        match cap {
            Capture::Date(part) => parts.values.push((*part, v)),
            Capture::Level => ev.level = Some(Level::parse(v)),
            Capture::Logger => ev.logger = v.to_string(),
            Capture::Thread => ev.thread = v.trim().to_string(),
            Capture::Message => {}
            Capture::Field(name) => {
                let v = v.trim();
                if !v.is_empty() {
                    ev.fields.push((name.clone(), v.to_string()));
                }
            }
            Capture::MdcMap => parse_mdc_map(v, &mut ev.fields),
        }
    }
    if p.has_date {
        match assemble(&parts, p.tz_override.unwrap_or(ctx.tz), ctx.default_date) {
            Some(ts) => ev.ts = ts,
            None => ev.ts_inferred = true,
        }
    } else {
        ev.ts_inferred = true;
    }
    Some(ev)
}

/// Parses `{k=v, k2=v2}` as printed by `%X`.
fn parse_mdc_map(v: &str, out: &mut Vec<(String, String)>) {
    let inner = v.trim().trim_start_matches('{').trim_end_matches('}');
    for pair in inner.split(", ") {
        if let Some((k, val)) = pair.split_once('=') {
            if !k.is_empty() {
                out.push((k.trim().to_string(), val.to_string()));
            }
        }
    }
}

fn parse_json(line: &str, ctx: &ParseCtx) -> Option<Event> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    let mut ev = Event {
        message: line.to_string(),
        ..Default::default()
    };
    let mut flat: Vec<(String, &serde_json::Value)> = Vec::new();
    flatten("", &value, &mut flat);

    let get = |keys: &[&str]| -> Option<&serde_json::Value> {
        keys.iter()
            .find_map(|k| flat.iter().find(|(fk, _)| fk == k).map(|(_, v)| *v))
    };
    let as_text = |v: &serde_json::Value| -> String {
        match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    };

    // Timestamp: string, epoch number, or JSONLayout's {"instant":{"epochSecond":..,"nanoOfSecond":..}}.
    let mut ts = None;
    if let Some(sec) = obj
        .get("instant")
        .and_then(|i| i.get("epochSecond"))
        .and_then(|s| s.as_i64())
    {
        let nanos = obj["instant"].get("nanoOfSecond").and_then(|n| n.as_i64()).unwrap_or(0);
        ts = Some(sec * 1000 + nanos / 1_000_000);
    }
    if ts.is_none() {
        ts = match get(json_keys::TIMESTAMP) {
            Some(serde_json::Value::Number(n)) => n
                .as_f64()
                .map(|f| if f > 1e11 { f as i64 } else { (f * 1000.0) as i64 }),
            Some(serde_json::Value::String(s)) => generic_ts().find(s, ctx.tz, ctx.default_date).map(|(t, _)| t),
            _ => None,
        };
    }
    match ts {
        Some(t) => ev.ts = t,
        None => ev.ts_inferred = true,
    }
    if let Some(l) = get(json_keys::LEVEL) {
        ev.level = Some(Level::parse(&as_text(l)));
    }
    if let Some(l) = get(json_keys::LOGGER) {
        ev.logger = as_text(l);
    }
    if let Some(t) = get(json_keys::THREAD) {
        ev.thread = as_text(t);
    }
    // Exception class.
    for key in [
        "thrown.name",
        "error.type",
        "exception.exception_class",
        "exception.class",
        "exception_class",
    ] {
        if let Some(v) = flat.iter().find(|(k, _)| k == key) {
            ev.exception = as_text(v.1);
            break;
        }
    }
    if ev.exception.is_empty() {
        for key in [
            "stack_trace",
            "error.stack_trace",
            "exception",
            "thrown.extendedStackTrace",
            "stacktrace",
        ] {
            if let Some((_, serde_json::Value::String(s))) = flat.iter().find(|(k, _)| k == key) {
                if let Some(c) = s.lines().find_map(exception_class) {
                    ev.exception = c.to_string();
                    break;
                }
            }
        }
    }
    let skip: Vec<&str> = json_keys::TIMESTAMP
        .iter()
        .chain(json_keys::LEVEL)
        .chain(json_keys::LOGGER)
        .chain(json_keys::THREAD)
        .chain(json_keys::MESSAGE)
        .copied()
        .collect();
    for (k, v) in &flat {
        if skip.contains(&k.as_str()) || k.starts_with("instant.") || k.starts_with("thrown.") {
            continue;
        }
        let key = json_keys::MDC
            .iter()
            .find_map(|p| k.strip_prefix(p).and_then(|r| r.strip_prefix('.')))
            .unwrap_or(k);
        let text = as_text(v);
        if text.len() <= 1024 {
            ev.fields.push((key.to_string(), text));
        }
    }
    Some(ev)
}

fn flatten<'a>(prefix: &str, v: &'a serde_json::Value, out: &mut Vec<(String, &'a serde_json::Value)>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, child, out);
            }
        }
        _ if !prefix.is_empty() => out.push((prefix.to_string(), v)),
        _ => {}
    }
}

/// Groups lines into events.
pub struct Assembler<'a> {
    format: &'a Format,
    ctx: ParseCtx,
    current: Option<Event>,
    last_ts: i64,
}

impl<'a> Assembler<'a> {
    pub fn new(format: &'a Format, ctx: ParseCtx) -> Self {
        let last_ts = ctx.fallback_ts;
        Assembler {
            format,
            ctx,
            current: None,
            last_ts,
        }
    }

    /// Feeds one line (without its line terminator). Returns an event when `line` started a
    /// new one and the previous event is complete.
    pub fn push(&mut self, line: &str, line_no: u32) -> Option<Event> {
        if matches!(self.format, Format::Xml) && is_xml_wrapper(line) {
            return None;
        }
        if let Some(mut ev) = self.format.parse_start(line, &self.ctx) {
            ev.line_no = line_no;
            if ev.ts_inferred {
                ev.ts = self.last_ts;
            } else {
                self.last_ts = ev.ts;
            }
            let done = self.current.replace(ev);
            return done.map(|e| self.complete(e));
        }
        match &mut self.current {
            Some(ev) => {
                append_line(ev, line);
                None
            }
            None => {
                // Continuation lines before any event start: keep them as their own event.
                let mut ev = Event {
                    ts: self.last_ts,
                    ts_inferred: true,
                    line_no,
                    ..Default::default()
                };
                append_line(&mut ev, line);
                ev.message.remove(0);
                self.current = Some(ev);
                None
            }
        }
    }

    pub fn finish(&mut self) -> Option<Event> {
        let done = self.current.take();
        done.map(|e| self.complete(e))
    }

    fn complete(&self, mut ev: Event) -> Event {
        // Lines before the first event are kept as they are.
        if matches!(self.format, Format::Xml) && parse_xml_start(ev.message.lines().next().unwrap_or("")).is_some() {
            finish_xml(&mut ev);
        }
        ev
    }
}

fn append_line(ev: &mut Event, line: &str) {
    if ev.exception.is_empty() {
        if let Some(c) = exception_class(line) {
            ev.exception = c.to_string();
        }
    }
    if ev.message.len() >= MAX_EVENT_BYTES {
        return;
    }
    ev.message.push('\n');
    let room = MAX_EVENT_BYTES - ev.message.len();
    if line.len() <= room {
        ev.message.push_str(line);
    } else {
        ev.message
            .push_str(&line[..crate::time::floor_char_boundary(line, room)]);
        ev.message.push_str(" …[truncated]");
    }
}

/// Result of format detection on a sample of lines.
#[derive(Clone, Debug)]
pub struct Detection {
    pub format: Format,
    /// Fraction of non-empty sample lines recognised as event starts.
    pub score: f32,
}

/// Picks the best format for a sample of lines. `user_patterns` (from the folder settings)
/// are tried first; a user pattern wins whenever it recognises any event starts.
pub fn detect(sample: &[String], user_patterns: &[Arc<CompiledPattern>], ctx: &ParseCtx) -> Detection {
    let lines: Vec<&str> = sample
        .iter()
        .map(|s| s.as_str())
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        let format = match user_patterns.first() {
            Some(p) => Format::Log4j(p.clone()),
            None => Format::Plain { timestamped: false },
        };
        return Detection { format, score: 0.0 };
    }
    let score = |f: &Format| -> f32 {
        let hits = lines.iter().filter(|l| f.parse_start(l, ctx).is_some()).count();
        hits as f32 / lines.len() as f32
    };
    let mut best: Option<Detection> = None;
    for p in user_patterns {
        let f = Format::Log4j(p.clone());
        let s = score(&f);
        if s > 0.0 && best.as_ref().is_none_or(|b| s > b.score) {
            best = Some(Detection { format: f, score: s });
        }
    }
    if let Some(b) = best {
        return b;
    }
    let json = Format::JsonLines;
    let js = score(&json);
    if js >= 0.5 {
        return Detection {
            format: json,
            score: js,
        };
    }
    // XML events span many lines, so look at the first line that isn't document wrapper.
    if let Some(first) = lines.iter().find(|l| !is_xml_wrapper(l)) {
        if parse_xml_start(first).is_some() {
            return Detection {
                format: Format::Xml,
                score: score(&Format::Xml),
            };
        }
    }
    for p in COMMON_PATTERNS {
        let Ok(c) = CompiledPattern::compile(p) else {
            continue;
        };
        let f = Format::Log4j(Arc::new(c));
        let s = score(&f);
        // Stack traces lower the share of matching lines, so a modest score is still a hit.
        if s >= 0.2 && best.as_ref().is_none_or(|b| s > b.score + 0.001) {
            best = Some(Detection { format: f, score: s });
        }
    }
    if let Some(b) = best {
        return b;
    }
    for f in [Format::Syslog, Format::Logfmt] {
        let s = score(&f);
        if s >= 0.5 {
            return Detection { format: f, score: s };
        }
    }
    let plain_ts = Format::Plain { timestamped: true };
    let s = score(&plain_ts);
    if s >= 0.2 {
        return Detection {
            format: plain_ts,
            score: s,
        };
    }
    Detection {
        format: Format::Plain { timestamped: false },
        score: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ParseCtx {
        ParseCtx {
            tz: TzMode::Utc,
            default_date: NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(),
            fallback_ts: 0,
        }
    }

    const SAMPLE: &str = "2026-10-08 18:30:45.123 [main] INFO  com.acme.App - starting
2026-10-08 18:30:46.000 [http-1] ERROR com.acme.payment.Client - call failed
java.net.SocketTimeoutException: Read timed out
\tat java.base/java.net.SocketInputStream.read(SocketInputStream.java:183)
Caused by: java.io.IOException: boom
\t... 12 more
2026-10-08 18:30:47.500 [http-2] WARN  com.acme.App - slow";

    #[test]
    fn detects_log4j_and_groups_stack_traces() {
        let lines: Vec<String> = SAMPLE.lines().map(String::from).collect();
        let det = detect(&lines, &[], &ctx());
        assert!(matches!(det.format, Format::Log4j(_)), "{:?}", det.format.info());
        let mut asm = Assembler::new(&det.format, ctx());
        let mut events = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            events.extend(asm.push(l, i as u32 + 1));
        }
        events.extend(asm.finish());
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].level, Some(Level::Error));
        assert_eq!(events[1].logger, "com.acme.payment.Client");
        assert_eq!(events[1].thread, "http-1");
        assert_eq!(events[1].exception, "java.net.SocketTimeoutException");
        assert_eq!(events[1].message.lines().count(), 5);
        assert_eq!(events[2].line_no, 7);
        assert_eq!(events[0].ts, 1_791_484_245_123);
    }

    #[test]
    fn json_layouts() {
        let line = r#"{"instant":{"epochSecond":1791484245,"nanoOfSecond":123000000},"thread":"main","level":"WARN","loggerName":"com.acme.App","message":"hi","contextMap":{"requestId":"r1"},"thrown":{"name":"java.lang.IllegalStateException"}}"#;
        let ev = Format::JsonLines.parse_start(line, &ctx()).unwrap();
        assert_eq!(ev.ts, 1_791_484_245_123);
        assert_eq!(ev.level, Some(Level::Warn));
        assert_eq!(ev.logger, "com.acme.App");
        assert_eq!(ev.exception, "java.lang.IllegalStateException");
        assert!(ev.fields.contains(&("requestId".into(), "r1".into())));

        let ecs = r#"{"@timestamp":"2026-10-08T18:30:45.123Z","log.level":"ERROR","log.logger":"x.Y","message":"m","error.type":"java.lang.NullPointerException"}"#;
        let ev = Format::JsonLines.parse_start(ecs, &ctx()).unwrap();
        assert_eq!(ev.ts, 1_791_484_245_123);
        assert_eq!(ev.exception, "java.lang.NullPointerException");
    }

    #[test]
    fn plain_fallback() {
        let lines: Vec<String> = ["some text", "more text"].iter().map(|s| s.to_string()).collect();
        let det = detect(&lines, &[], &ctx());
        assert!(matches!(det.format, Format::Plain { timestamped: false }));
    }

    #[test]
    fn exception_names() {
        assert_eq!(
            exception_class("java.lang.NullPointerException"),
            Some("java.lang.NullPointerException")
        );
        assert_eq!(
            exception_class("Caused by: org.x.FooError: msg"),
            Some("org.x.FooError")
        );
        assert_eq!(exception_class("\tat com.acme.Foo.bar(Foo.java:10)"), None);
        assert_eq!(exception_class("just an Error message"), None);
    }
}
