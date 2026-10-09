//! Compiles a log4j 1.x/2.x or Logback `PatternLayout` conversion pattern into a regex that
//! recognises the first line of an event and captures its fields.
//!
//! Example: `%d{ISO8601} [%t] %-5level %logger{36} - %msg%n`

use crate::time::{compile_date_format, DatePart, TzMode};
use regex::Regex;

/// Field captured from a line by a compiled pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    Date(DatePart),
    Level,
    Logger,
    Thread,
    Message,
    /// A named extra field (MDC key, line number, method, ...).
    Field(String),
    /// `%X` / `%mdc` without a key: a `{k=v, k2=v2}` map.
    MdcMap,
}

#[derive(Clone, Debug)]
pub struct CompiledPattern {
    /// The conversion pattern as given.
    pub pattern: String,
    pub regex: Regex,
    /// Regex group index for each capture.
    pub captures: Vec<(usize, Capture)>,
    pub has_date: bool,
    /// Time zone given as the second option of `%d{...}{tz}`.
    pub tz_override: Option<TzMode>,
}

#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    #[error("pattern is empty")]
    Empty,
    #[error("pattern has no conversion specifiers")]
    NoConverters,
    #[error("unbalanced '{0}' in pattern")]
    Unbalanced(char),
    #[error("could not build regex: {0}")]
    Regex(#[from] regex::Error),
}

struct Builder {
    regex: String,
    group_names: Vec<(String, Capture)>,
    converters: usize,
    has_date: bool,
    tz_override: Option<TzMode>,
    ended: bool,
}

const WILDCARD: char = '\u{E000}';

impl CompiledPattern {
    pub fn compile(pattern: &str) -> Result<CompiledPattern, PatternError> {
        let trimmed = pattern.trim_matches(|c| c == '"' || c == '\'' || c == '\n' || c == '\r');
        if trimmed.trim().is_empty() {
            return Err(PatternError::Empty);
        }
        let substituted = substitute_properties(trimmed);
        let mut b = Builder {
            regex: String::from("^"),
            group_names: Vec::new(),
            converters: 0,
            has_date: false,
            tz_override: None,
            ended: false,
        };
        let chars: Vec<char> = substituted.chars().collect();
        b.compile_seq(&chars)?;
        if b.converters == 0 {
            return Err(PatternError::NoConverters);
        }
        b.regex.push_str(r"\s*$");
        let regex = Regex::new(&b.regex)?;
        let mut captures = Vec::new();
        for (name, cap) in b.group_names {
            if let Some(idx) = regex.capture_names().position(|n| n == Some(name.as_str())) {
                captures.push((idx, cap));
            }
        }
        Ok(CompiledPattern {
            pattern: pattern.to_string(),
            regex,
            captures,
            has_date: b.has_date,
            tz_override: b.tz_override,
        })
    }
}

/// Replaces `${NAME:-default}` with `default` and other `${...}` lookups with a wildcard.
fn substitute_properties(p: &str) -> String {
    let chars: Vec<char> = p.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            // Find matching close brace.
            let mut depth = 0;
            let mut j = i + 1;
            while j < chars.len() {
                match chars[j] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            let inner: String = chars[i + 2..j.min(chars.len())].iter().collect();
            // A default that is itself a pattern or date format is what the app most likely
            // used; a plain-value default (like Spring's `${PID:- }`) is usually overridden at
            // runtime, so it matches anything.
            match inner.find(":-") {
                Some(pos)
                    if inner[pos + 2..].contains('%')
                        || inner[pos + 2..].contains("yy")
                        || inner[pos + 2..].contains("HH") =>
                {
                    out.push_str(&substitute_properties(&inner[pos + 2..]))
                }
                _ => out.push(WILDCARD),
            }
            i = j + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Reads a balanced `open ... close` group starting at `chars[i] == open`; returns the inner
/// text and the index after the closing char.
fn read_group(chars: &[char], i: usize, open: char, close: char) -> Result<(Vec<char>, usize), PatternError> {
    let mut depth = 0;
    let mut j = i;
    while j < chars.len() {
        let c = chars[j];
        if c == '\\' {
            j += 2;
            continue;
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Ok((chars[i + 1..j].to_vec(), j + 1));
            }
        }
        j += 1;
    }
    Err(PatternError::Unbalanced(open))
}

impl Builder {
    fn compile_seq(&mut self, chars: &[char]) -> Result<(), PatternError> {
        let mut i = 0;
        let mut literal = String::new();
        while i < chars.len() && !self.ended {
            let c = chars[i];
            if c == WILDCARD {
                self.flush_literal(&mut literal);
                self.regex.push_str(".*?");
                i += 1;
                continue;
            }
            if c == '\\' && i + 1 < chars.len() {
                match chars[i + 1] {
                    't' => literal.push('\t'),
                    'n' | 'r' => {
                        self.flush_literal(&mut literal);
                        self.ended = true;
                        return Ok(());
                    }
                    other => literal.push(other),
                }
                i += 2;
                continue;
            }
            if c != '%' {
                literal.push(c);
                i += 1;
                continue;
            }
            // '%' conversion
            if chars.get(i + 1) == Some(&'%') {
                literal.push('%');
                i += 2;
                continue;
            }
            self.flush_literal(&mut literal);
            i += 1;
            // Format modifier: [-][min][.[-]max]
            let mut left_align = false;
            let mut min_width = 0usize;
            if chars.get(i) == Some(&'-') {
                left_align = true;
                i += 1;
            }
            while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
                min_width = min_width * 10 + d as usize;
                i += 1;
            }
            if chars.get(i) == Some(&'.') {
                i += 1;
                if chars.get(i) == Some(&'-') {
                    i += 1;
                }
                while chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
            }
            // Name
            let start = i;
            while chars.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            // Logback composite: %name(sub-pattern)
            let mut composite: Option<Vec<char>> = None;
            if chars.get(i) == Some(&'(') {
                let (inner, next) = read_group(chars, i, '(', ')')?;
                composite = Some(inner);
                i = next;
            }
            // Options: {..}{..}
            let mut options: Vec<Vec<char>> = Vec::new();
            while chars.get(i) == Some(&'{') {
                let (inner, next) = read_group(chars, i, '{', '}')?;
                options.push(inner);
                i = next;
            }
            if name.is_empty() && composite.is_some() {
                // Logback grouping: %(...) with padding.
                let inner = composite.take().unwrap();
                self.pad_before(left_align, min_width);
                self.compile_seq(&inner)?;
                self.pad_after(left_align, min_width);
                continue;
            }
            self.converter(&name, composite, options, left_align, min_width)?;
        }
        self.flush_literal(&mut literal);
        Ok(())
    }

    fn flush_literal(&mut self, literal: &mut String) {
        if literal.is_empty() {
            return;
        }
        let mut prev_space = false;
        for ch in literal.chars() {
            if ch == ' ' {
                if !prev_space {
                    self.regex.push_str(" +");
                }
                prev_space = true;
            } else {
                prev_space = false;
                self.regex.push_str(&regex::escape(&ch.to_string()));
            }
        }
        literal.clear();
    }

    fn pad_before(&mut self, left_align: bool, min_width: usize) {
        if min_width > 0 && !left_align {
            self.regex.push_str(" *");
        }
    }

    fn pad_after(&mut self, left_align: bool, min_width: usize) {
        if min_width > 0 && left_align {
            self.regex.push_str(" *");
        }
    }

    fn group(&mut self, cap: Capture, re: &str) {
        let name = match &cap {
            Capture::Level => "level".to_string(),
            Capture::Logger => "logger".to_string(),
            Capture::Thread => "thread".to_string(),
            Capture::Message => "msg".to_string(),
            Capture::MdcMap => "mdcmap".to_string(),
            Capture::Field(f) => format!("f_{}", sanitize(f)),
            Capture::Date(_) => unreachable!(),
        };
        if self.group_names.iter().any(|(n, _)| *n == name) {
            self.regex.push_str(&format!("(?:{re})"));
        } else {
            self.regex.push_str(&format!("(?P<{name}>{re})"));
            self.group_names.push((name, cap));
        }
    }

    fn converter(
        &mut self,
        name: &str,
        composite: Option<Vec<char>>,
        options: Vec<Vec<char>>,
        left_align: bool,
        min_width: usize,
    ) -> Result<(), PatternError> {
        self.converters += 1;
        let opt = |n: usize| options.get(n).map(|o| o.iter().collect::<String>()).unwrap_or_default();
        // Wrappers whose content is a sub-pattern.
        let wrapper = matches!(
            name,
            "highlight"
                | "style"
                | "clr"
                | "notEmpty"
                | "varsNotEmpty"
                | "variablesNotEmpty"
                | "maxLen"
                | "maxLength"
                | "enc"
                | "encode"
                | "black"
                | "red"
                | "green"
                | "yellow"
                | "blue"
                | "magenta"
                | "cyan"
                | "white"
                | "gray"
                | "boldRed"
                | "boldGreen"
                | "boldYellow"
                | "boldBlue"
                | "boldMagenta"
                | "boldCyan"
                | "boldWhite"
                | "equals"
                | "equalsIgnoreCase"
                | "replace"
                | "truncate"
        );
        if wrapper {
            let inner = composite.or_else(|| options.first().cloned()).unwrap_or_default();
            self.pad_before(left_align, min_width);
            match name {
                "notEmpty" | "varsNotEmpty" | "variablesNotEmpty" | "equals" | "equalsIgnoreCase" => {
                    // Content may be omitted entirely.
                    self.regex.push_str("(?:");
                    self.compile_seq(&inner)?;
                    self.regex.push_str(")?");
                }
                "replace" => self.regex.push_str(".*?"),
                _ => self.compile_seq(&inner)?,
            }
            self.pad_after(left_align, min_width);
            return Ok(());
        }
        self.pad_before(left_align, min_width);
        match name {
            "d" | "date" => {
                let fmt = opt(0);
                let mut compiled = compile_date_format(&fmt, "ts");
                if fmt.starts_with("ISO8601") {
                    compiled.regex = compiled.regex.replacen('T', "[T ]", 1);
                }
                if !self.has_date {
                    self.has_date = true;
                    self.tz_override = parse_tz(&opt(1));
                    self.regex.push_str(&compiled.regex);
                    for (n, part) in compiled.groups {
                        self.group_names.push((n, Capture::Date(part)));
                    }
                } else {
                    // Only the first date is captured; strip names from later ones.
                    let re = Regex::new(r"\(\?P<[^>]+>").unwrap();
                    self.regex.push_str(&re.replace_all(&compiled.regex, "(?:"));
                }
            }
            "p" | "le" | "level" => self.group(Capture::Level, "[A-Za-z]+"),
            "c" | "lo" | "logger" => self.group(Capture::Logger, r"\S+?"),
            "t" | "tn" | "thread" | "threadName" => self.group(Capture::Thread, ".*?"),
            "m" | "msg" | "message" => self.group(Capture::Message, ".*"),
            "n" => {
                self.ended = true;
                self.converters -= 1;
            }
            "X" | "mdc" | "MDC" => {
                let key = opt(0);
                if key.is_empty() {
                    self.group(Capture::MdcMap, r"\{.*?\}|");
                } else if key.contains(',') {
                    self.regex.push_str(".*?");
                } else {
                    self.group(Capture::Field(key.trim().to_string()), ".*?");
                }
            }
            "C" | "class" => self.group(Capture::Field("class".into()), r"\S+?"),
            "M" | "method" => self.group(Capture::Field("method".into()), r"\S+?"),
            "L" | "line" => self.group(Capture::Field("line".into()), r"\S+?"),
            "F" | "file" => self.group(Capture::Field("source_file".into()), r"\S+?"),
            "l" | "location" => self.group(Capture::Field("location".into()), r".*?"),
            "x" | "NDC" => self.group(Capture::Field("ndc".into()), r".*?"),
            "r" | "relative" => self.group(Capture::Field("relative".into()), r"\d+"),
            "sn" | "sequenceNumber" => self.group(Capture::Field("seq".into()), r"\d+"),
            "pid" | "processId" => self.group(Capture::Field("pid".into()), r"\d+"),
            "T" | "tid" | "threadId" => self.group(Capture::Field("thread_id".into()), r"\d+"),
            "tp" | "threadPriority" => self.regex.push_str(r"\d+"),
            "marker" | "markerSimpleName" => self.group(Capture::Field("marker".into()), r".*?"),
            "u" | "uuid" => self.group(Capture::Field("uuid".into()), r"\S*?"),
            "hostName" => self.group(Capture::Field("host".into()), r"\S+?"),
            "contextName" | "cn" => self.group(Capture::Field("context".into()), r"\S+?"),
            "N" | "nano" => self.regex.push_str(r"\d+"),
            // Exceptions are printed on the following lines; on the first line they are empty.
            "ex" | "exception" | "throwable" | "xEx" | "xException" | "xThrowable" | "rEx" | "rException"
            | "rThrowable" | "wEx" | "wex" | "nopex" | "nopexception" => {
                self.converters -= 1;
            }
            _ => self.regex.push_str(".*?"),
        }
        self.pad_after(left_align, min_width);
        Ok(())
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn parse_tz(s: &str) -> Option<TzMode> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("UTC") || s.eq_ignore_ascii_case("GMT") || s == "Z" {
        return Some(TzMode::Utc);
    }
    let rest = s.strip_prefix("GMT").or_else(|| s.strip_prefix("UTC")).unwrap_or(s);
    let sign = match rest.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let body = &rest[1..];
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None if body.len() == 4 => (body[..2].parse().ok()?, body[2..].parse().ok()?),
        None => (body.parse().ok()?, 0),
    };
    Some(TzMode::Fixed(sign * (h * 3600 + m * 60)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(p: &CompiledPattern, line: &'a str, cap: Capture) -> Option<&'a str> {
        let c = p.regex.captures(line)?;
        let idx = p.captures.iter().find(|(_, c)| *c == cap)?.0;
        c.get(idx).map(|m| m.as_str())
    }

    #[test]
    fn log4j2_typical() {
        let p = CompiledPattern::compile("%d{ISO8601} [%t] %-5level %logger{36} - %msg%n").unwrap();
        let line = "2026-10-08T18:30:45,123 [http-nio-8080-exec-1] INFO  com.acme.payment.Service - charged 42";
        assert!(p.regex.is_match(line));
        assert_eq!(field(&p, line, Capture::Level), Some("INFO"));
        assert_eq!(field(&p, line, Capture::Thread), Some("http-nio-8080-exec-1"));
        assert_eq!(field(&p, line, Capture::Logger), Some("com.acme.payment.Service"));
        assert_eq!(field(&p, line, Capture::Message), Some("charged 42"));
        assert!(!p.regex.is_match("\tat com.acme.Foo.bar(Foo.java:10)"));
        // Logback-style ISO8601 has a space instead of T.
        assert!(p.regex.is_match("2026-10-08 18:30:45,123 [main] WARN x.Y - z"));
    }

    #[test]
    fn mdc_and_properties_file_syntax() {
        let p = CompiledPattern::compile("%d{yyyy-MM-dd HH:mm:ss.SSS} %5p [%X{requestId}] %c{1}:%L - %m%n").unwrap();
        let line = "2026-10-08 18:30:45.123  WARN [req-77] Service:42 - slow";
        assert_eq!(field(&p, line, Capture::Level), Some("WARN"));
        assert_eq!(field(&p, line, Capture::Field("requestId".into())), Some("req-77"));
        assert_eq!(field(&p, line, Capture::Field("line".into())), Some("42"));
        assert_eq!(field(&p, line, Capture::Logger), Some("Service"));
    }

    #[test]
    fn spring_boot_default_with_clr_and_properties() {
        let p = CompiledPattern::compile(
            "%clr(%d{${LOG_DATEFORMAT_PATTERN:-yyyy-MM-dd'T'HH:mm:ss.SSSXXX}}){faint} %clr(${LOG_LEVEL_PATTERN:-%5p}) %clr(${PID:- }){magenta} %clr(---){faint} %clr([%15.15t]){faint} %clr(%-40.40logger{39}){cyan} %clr(:){faint} %m%n${LOG_EXCEPTION_CONVERSION_WORD:-%wEx}",
        )
        .unwrap();
        let line = "2026-10-08T18:30:45.123+02:00  INFO 12345 --- [           main] com.acme.Application                     : Started";
        assert_eq!(field(&p, line, Capture::Level), Some("INFO"));
        assert_eq!(field(&p, line, Capture::Message), Some("Started"));
    }

    #[test]
    fn highlight_wrapper_and_tz() {
        let p = CompiledPattern::compile("%d{HH:mm:ss.SSS}{UTC} %highlight{%-5level}{FATAL=red} %c - %m%n").unwrap();
        assert_eq!(p.tz_override, Some(TzMode::Utc));
        let line = "18:30:45.123 ERROR a.b.C - boom";
        assert_eq!(field(&p, line, Capture::Level), Some("ERROR"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(CompiledPattern::compile("").is_err());
        assert!(CompiledPattern::compile("just text").is_err());
    }
}
