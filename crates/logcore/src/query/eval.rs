//! Compiled queries: per-chunk pruning on stream labels and per-event evaluation of the
//! remaining matchers, line filters, parsers and field filters.

use super::ast::*;
use super::parser::{parse_bytes, parse_duration_ms};
use crate::chunk::EventRef;
use crate::model::Level;
use memchr::memmem;
use regex::Regex;
use std::borrow::Cow;

/// Labels known per chunk (and so usable to skip whole chunks).
pub const STREAM_LABELS: &[&str] = &["source", "file", "level"];

#[derive(Debug)]
pub enum StrMatch {
    Eq(String),
    Ne(String),
    Re(Regex),
    Nre(Regex),
}

impl StrMatch {
    fn new(op: MatchOp, value: &str, case_insensitive: bool) -> anyhow::Result<StrMatch> {
        let ci = if case_insensitive { "(?i)" } else { "" };
        Ok(match op {
            MatchOp::Eq => StrMatch::Eq(if case_insensitive {
                value.to_lowercase()
            } else {
                value.to_string()
            }),
            MatchOp::Ne => StrMatch::Ne(if case_insensitive {
                value.to_lowercase()
            } else {
                value.to_string()
            }),
            // Label regexes are fully anchored, as in LogQL.
            MatchOp::Re => StrMatch::Re(Regex::new(&format!("{ci}^(?:{value})$"))?),
            MatchOp::Nre => StrMatch::Nre(Regex::new(&format!("{ci}^(?:{value})$"))?),
        })
    }

    pub fn matches(&self, v: &str) -> bool {
        match self {
            StrMatch::Eq(s) => v == s,
            StrMatch::Ne(s) => v != s,
            StrMatch::Re(r) => r.is_match(v),
            StrMatch::Nre(r) => !r.is_match(v),
        }
    }
}

#[derive(Debug)]
pub struct LabelMatcher {
    pub name: String,
    pub m: StrMatch,
}

#[derive(Debug)]
enum LineFilter {
    Contains(Vec<memmem::Finder<'static>>),
    NotContains(Vec<memmem::Finder<'static>>),
    Re(Regex),
    NotRe(Regex),
}

#[derive(Debug)]
enum CompiledStage {
    Line(LineFilter),
    Json,
    Logfmt,
    Regexp(Regex),
    Filter(CExpr),
}

#[derive(Debug)]
enum CValue {
    Str(StrMatch),
    Num(CmpOp, f64),
    Duration(CmpOp, f64),
    Bytes(CmpOp, f64),
}

#[derive(Debug)]
enum CExpr {
    Cmp(String, CValue),
    And(Box<CExpr>, Box<CExpr>),
    Or(Box<CExpr>, Box<CExpr>),
}

#[derive(Debug)]
pub struct CompiledQuery {
    /// Matchers on stream labels (`source`, `file`, `level`), checked per chunk.
    pub stream: Vec<LabelMatcher>,
    /// Matchers on per-event labels (`logger`, `thread`, MDC keys, ...).
    pub event: Vec<LabelMatcher>,
    stages: Vec<CompiledStage>,
    /// Literal substrings every matching event must contain (from `|=` with one value), used
    /// to skip chunks without decoding them.
    pub required_literals: Vec<memmem::Finder<'static>>,
    pub has_parsers: bool,
}

fn compile_expr(e: &FieldExpr) -> anyhow::Result<CExpr> {
    Ok(match e {
        FieldExpr::And { left, right } => CExpr::And(Box::new(compile_expr(left)?), Box::new(compile_expr(right)?)),
        FieldExpr::Or { left, right } => CExpr::Or(Box::new(compile_expr(left)?), Box::new(compile_expr(right)?)),
        FieldExpr::Cmp { name, op, value, .. } => {
            let level = name == "level";
            let v = match value {
                Value::String(s) => {
                    let mop = match op {
                        CmpOp::Eq => MatchOp::Eq,
                        CmpOp::Ne => MatchOp::Ne,
                        CmpOp::Re => MatchOp::Re,
                        CmpOp::Nre => MatchOp::Nre,
                        _ => anyhow::bail!("ordering comparisons need a number"),
                    };
                    CValue::Str(StrMatch::new(mop, s, level)?)
                }
                Value::Number(n) => CValue::Num(*op, *n),
                Value::Duration(d) => CValue::Duration(*op, *d),
                Value::Bytes(b) => CValue::Bytes(*op, *b),
            };
            CExpr::Cmp(name.clone(), v)
        }
    })
}

impl CompiledQuery {
    pub fn compile(q: &Query) -> anyhow::Result<CompiledQuery> {
        let mut stream = Vec::new();
        let mut event = Vec::new();
        for m in &q.selector {
            let ci = m.name == "level";
            let lm = LabelMatcher {
                name: m.name.clone(),
                m: StrMatch::new(m.op, &m.value, ci)?,
            };
            if STREAM_LABELS.contains(&m.name.as_str()) {
                stream.push(lm);
            } else {
                event.push(lm);
            }
        }
        let mut stages = Vec::new();
        let mut required_literals = Vec::new();
        let mut has_parsers = false;
        for s in &q.stages {
            stages.push(match s {
                Stage::Line { op, values } => {
                    let finders = || {
                        values
                            .iter()
                            .map(|v| memmem::Finder::new(v.as_bytes()).into_owned())
                            .collect::<Vec<_>>()
                    };
                    match op {
                        LineOp::Contains => {
                            if values.len() == 1 && !values[0].is_empty() {
                                required_literals.push(memmem::Finder::new(values[0].as_bytes()).into_owned());
                            }
                            CompiledStage::Line(LineFilter::Contains(finders()))
                        }
                        LineOp::NotContains => CompiledStage::Line(LineFilter::NotContains(finders())),
                        LineOp::Re => CompiledStage::Line(LineFilter::Re(Regex::new(&alternation(values))?)),
                        LineOp::NotRe => CompiledStage::Line(LineFilter::NotRe(Regex::new(&alternation(values))?)),
                    }
                }
                Stage::Json => {
                    has_parsers = true;
                    CompiledStage::Json
                }
                Stage::Logfmt => {
                    has_parsers = true;
                    CompiledStage::Logfmt
                }
                Stage::Regexp { pattern } => {
                    has_parsers = true;
                    CompiledStage::Regexp(Regex::new(pattern)?)
                }
                Stage::Filter { expr } => CompiledStage::Filter(compile_expr(expr)?),
            });
        }
        Ok(CompiledQuery {
            stream,
            event,
            stages,
            required_literals,
            has_parsers,
        })
    }

    /// Checks stream matchers against a chunk's labels.
    pub fn chunk_matches(&self, source: &str, file: &str, level: Level) -> bool {
        self.stream.iter().all(|m| {
            let v = match m.name.as_str() {
                "source" => source,
                "file" => file,
                _ => level.as_str(),
            };
            m.m.matches(v)
        })
    }

    /// Evaluates per-event matchers and the pipeline. Returns the fields extracted by
    /// parsers when the event matches.
    pub fn eval<'a>(&self, ev: &EventRef<'a>, level: Level) -> Option<Vec<(String, String)>> {
        for m in &self.event {
            let v = event_field(ev, &m.name, &[]);
            if !m.m.matches(v.as_deref().unwrap_or("")) {
                return None;
            }
        }
        let mut extracted: Vec<(String, String)> = Vec::new();
        for stage in &self.stages {
            match stage {
                CompiledStage::Line(f) => {
                    let line = ev.message.as_bytes();
                    let ok = match f {
                        LineFilter::Contains(fs) => fs.iter().any(|f| f.find(line).is_some()),
                        LineFilter::NotContains(fs) => fs.iter().all(|f| f.find(line).is_none()),
                        LineFilter::Re(r) => r.is_match(ev.message),
                        LineFilter::NotRe(r) => !r.is_match(ev.message),
                    };
                    if !ok {
                        return None;
                    }
                }
                CompiledStage::Json => extract_json(ev.message, &mut extracted),
                CompiledStage::Logfmt => extract_logfmt(first_line(ev.message), &mut extracted),
                CompiledStage::Regexp(re) => {
                    if let Some(c) = re.captures(ev.message) {
                        for name in re.capture_names().flatten() {
                            if let Some(m) = c.name(name) {
                                set_field(&mut extracted, name, m.as_str().to_string());
                            }
                        }
                    }
                }
                CompiledStage::Filter(expr) => {
                    if !eval_expr(expr, ev, level, &extracted) {
                        return None;
                    }
                }
            }
        }
        Some(extracted)
    }
}

fn alternation(values: &[String]) -> String {
    if values.len() == 1 {
        return values[0].clone();
    }
    values.iter().map(|v| format!("(?:{v})")).collect::<Vec<_>>().join("|")
}

fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or(s)
}

fn set_field(out: &mut Vec<(String, String)>, k: &str, v: String) {
    match out.iter_mut().find(|(ek, _)| ek == k) {
        Some(e) => e.1 = v,
        None => out.push((k.to_string(), v)),
    }
}

/// Looks up a label or field on an event: built-ins first, then structured fields captured
/// at ingest, then fields extracted by parsers in this query.
pub fn event_field<'a>(ev: &EventRef<'a>, name: &str, extracted: &'a [(String, String)]) -> Option<Cow<'a, str>> {
    if let Some((_, v)) = extracted.iter().rev().find(|(k, _)| k == name) {
        return Some(Cow::Borrowed(v.as_str()));
    }
    match name {
        "logger" => return Some(Cow::Borrowed(ev.logger)),
        "thread" => return Some(Cow::Borrowed(ev.thread)),
        "exception" => return Some(Cow::Borrowed(ev.exception)),
        _ => {}
    }
    ev.fields
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| Cow::Borrowed(*v))
}

fn cmp(op: CmpOp, a: f64, b: f64) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Re | CmpOp::Nre => false,
    }
}

fn eval_expr(e: &CExpr, ev: &EventRef, level: Level, extracted: &[(String, String)]) -> bool {
    match e {
        CExpr::And(a, b) => eval_expr(a, ev, level, extracted) && eval_expr(b, ev, level, extracted),
        CExpr::Or(a, b) => eval_expr(a, ev, level, extracted) || eval_expr(b, ev, level, extracted),
        CExpr::Cmp(name, v) => {
            if name == "level" {
                // Level is a stream label but may also be filtered in the pipeline.
                if let CValue::Str(m) = v {
                    let parsed = extracted
                        .iter()
                        .rev()
                        .find(|(k, _)| k == "level")
                        .map(|(_, v)| Level::parse(v));
                    return m.matches(parsed.unwrap_or(level).as_str());
                }
            }
            let field = event_field(ev, name, extracted);
            let text = field.as_deref().unwrap_or("");
            match v {
                CValue::Str(m) => m.matches(text),
                CValue::Num(op, n) => text.trim().parse::<f64>().is_ok_and(|x| cmp(*op, x, *n)),
                CValue::Duration(op, d) => {
                    let t = text.trim();
                    // A bare number is taken as seconds, like Go's duration of a float field.
                    let x = parse_duration_ms(t).or_else(|| t.parse::<f64>().ok().map(|s| s * 1000.0));
                    x.is_some_and(|x| cmp(*op, x, *d))
                }
                CValue::Bytes(op, b) => {
                    let t = text.trim();
                    let x = parse_bytes(t).or_else(|| t.parse::<f64>().ok());
                    x.is_some_and(|x| cmp(*op, x, *b))
                }
            }
        }
    }
}

/// `| json`: parses the whole event, or the `{...}` part of its first line (log4j messages
/// often carry JSON after the pattern prefix). Nested keys are joined with `_`, as in LogQL.
fn extract_json(message: &str, out: &mut Vec<(String, String)>) {
    let line = first_line(message);
    let candidate = if line.trim_start().starts_with('{') {
        line
    } else {
        match (line.find('{'), line.rfind('}')) {
            (Some(s), Some(e)) if e > s => &line[s..=e],
            _ => return,
        }
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) else {
        return;
    };
    fn walk(prefix: &str, v: &serde_json::Value, out: &mut Vec<(String, String)>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    let key: String = k
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                        .collect();
                    let key = if prefix.is_empty() {
                        key
                    } else {
                        format!("{prefix}_{key}")
                    };
                    walk(&key, child, out);
                }
            }
            serde_json::Value::String(s) => set_field(out, prefix, s.clone()),
            serde_json::Value::Null => {}
            other => set_field(out, prefix, other.to_string()),
        }
    }
    walk("", &v, out);
}

/// `| logfmt`: `key=value key2="quoted value"` pairs; tokens without `=` are skipped.
fn extract_logfmt(line: &str, out: &mut Vec<(String, String)>) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b' ' {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            continue;
        }
        let key = &line[ks..i];
        i += 1;
        let value = if i < b.len() && b[i] == b'"' {
            i += 1;
            let mut v = String::new();
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                let ch = line[i..].chars().next().unwrap();
                v.push(ch);
                i += ch.len_utf8();
            }
            i += 1;
            v
        } else {
            let vs = i;
            while i < b.len() && b[i] != b' ' {
                i += 1;
            }
            line[vs..i].to_string()
        };
        if !key.is_empty()
            && key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-')
        {
            set_field(out, key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    fn ev(message: &str) -> EventRef<'_> {
        EventRef {
            ts: 0,
            line_no: 1,
            ts_inferred: false,
            logger: "com.acme.payment.Client",
            thread: "main",
            exception: "",
            fields: vec![("requestId", "r-1")],
            message,
        }
    }

    fn run(q: &str, message: &str) -> bool {
        let c = CompiledQuery::compile(&parse(q).unwrap()).unwrap();
        c.eval(&ev(message), Level::Info).is_some()
    }

    #[test]
    fn matchers_and_filters() {
        assert!(run(r#"{logger=~"com\\.acme\\.payment.*"}"#, "x"));
        assert!(!run(r#"{logger=~"com\\.acme\\.order.*"}"#, "x"));
        assert!(run(r#"{requestId="r-1"} |= "time""#, "a timeout"));
        assert!(!run(r#"{} |= "time" != "out""#, "a timeout"));
        assert!(run(r#"{} |~ "t[a-z]+out""#, "a timeout"));
        assert!(run(r#"{} |= "nope" or "time""#, "a timeout"));
        assert!(run(r#"{missing!="x"}"#, "x"));
    }

    #[test]
    fn parsers_and_field_filters() {
        let line = r#"2026-10-08 INFO c.Svc - {"status":503,"duration":"2.5s","req":{"path":"/pay"}}"#;
        assert!(run(r#"{} | json | status >= 500 and duration > 2s"#, line));
        assert!(run(r#"{} | json | req_path = "/pay""#, line));
        assert!(!run(r#"{} | json | status < 500"#, line));
        assert!(run(
            r#"{} | logfmt | took > 100ms"#,
            r#"msg="done x" took=250ms user=bob"#
        ));
        assert!(run(r#"{} | logfmt | user = "bob" or user = "alice""#, "user=bob"));
        assert!(run(r#"{} | regexp "took (?P<ms>\\d+)ms" | ms > 100"#, "took 250ms"));
        assert!(!run(r#"{} | regexp "took (?P<ms>\\d+)ms" | ms > 300"#, "took 250ms"));
    }
}
