//! Formats besides PatternLayout and JSON: log4j XML layouts, syslog and logfmt.

use crate::model::{Event, Level};
use crate::parse::{exception_class, ParseCtx};
use crate::time::GenericTimestamp;
use regex::Regex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------------------------
// XML layouts
//
// log4j 1.x XMLLayout:
//   <log4j:event logger="a.B" timestamp="1700000000123" level="INFO" thread="main">
//   <log4j:message><![CDATA[text]]></log4j:message>
//   <log4j:throwable><![CDATA[java.lang.X: boom
//   	at ...]]></log4j:throwable>
//   <log4j:properties><log4j:data name="requestId" value="r1"/></log4j:properties>
//   </log4j:event>
//
// log4j 2 XmlLayout (pretty or compact):
//   <Event xmlns="..." timeMillis="1700000000123" thread="main" level="INFO" loggerName="a.B">
//     <Instant epochSecond="1700000000" nanoOfSecond="123000000"/>
//     <Message>text</Message>
//     <ContextMap><item key="requestId" value="r1"/></ContextMap>
//     <Thrown name="java.lang.X" message="boom">...</Thrown>
//   </Event>
// ---------------------------------------------------------------------------------------------

fn attr_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"([\w:.-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap())
}

fn attrs(tag: &str) -> Vec<(&str, String)> {
    attr_re()
        .captures_iter(tag)
        .map(|c| {
            let v = c.get(2).or_else(|| c.get(3)).map_or("", |m| m.as_str());
            (c.get(1).unwrap().as_str(), unescape(v))
        })
        .collect()
}

fn attr<'a>(attrs: &'a [(&str, String)], names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|n| attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str()))
}

/// Decodes the five predefined entities and numeric character references.
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Lines around events that carry nothing: XML declaration and document wrappers.
pub fn is_xml_wrapper(line: &str) -> bool {
    let t = line.trim();
    t.is_empty()
        || t.starts_with("<?xml")
        || t.starts_with("<Events")
        || t.starts_with("</Events")
        || t.starts_with("<log4j:eventSet")
        || t.starts_with("</log4j:eventSet")
        || t.starts_with("<!DOCTYPE")
}

/// Parses the opening tag of an XML event. The rest of the event arrives as continuation
/// lines and is decoded by [`finish_xml`].
pub fn parse_xml_start(line: &str) -> Option<Event> {
    let t = line.trim_start();
    let v1 = t.starts_with("<log4j:event ");
    if !v1 && !(t.starts_with("<Event ") || t.starts_with("<Event>")) {
        return None;
    }
    let open_end = t.find('>').unwrap_or(t.len());
    let a = attrs(&t[..open_end]);
    let mut ev = Event {
        message: line.to_string(),
        ..Default::default()
    };
    match attr(&a, &["timestamp", "timeMillis"]).and_then(|s| s.parse::<i64>().ok()) {
        Some(ts) => ev.ts = ts,
        None => ev.ts_inferred = true,
    }
    ev.level = attr(&a, &["level"]).map(Level::parse);
    ev.logger = attr(&a, &["logger", "loggerName"]).unwrap_or_default().to_string();
    ev.thread = attr(&a, &["thread"]).unwrap_or_default().to_string();
    Some(ev)
}

fn element_text<'a>(xml: &'a str, names: &[&str]) -> Option<&'a str> {
    for name in names {
        let Some(start) = xml.find(&format!("<{name}")) else {
            continue;
        };
        let after = &xml[start + name.len() + 1..];
        // `<Message>` but not `<MessageX>`; self-closing elements have no text.
        if !after.starts_with('>') && !after.starts_with(' ') {
            continue;
        }
        let open_end = after.find('>')?;
        if after[..open_end].ends_with('/') {
            return Some("");
        }
        let body = &after[open_end + 1..];
        let close = body.find(&format!("</{name}>")).unwrap_or(body.len());
        return Some(&body[..close]);
    }
    None
}

fn text_content(raw: &str) -> String {
    let raw = raw.trim();
    match raw.strip_prefix("<![CDATA[") {
        Some(rest) => rest.strip_suffix("]]>").unwrap_or(rest).to_string(),
        None => unescape(raw),
    }
}

/// Turns the accumulated raw XML of an event into a readable message (text, then the stack
/// trace) and fills fields from MDC entries and the exception class.
pub fn finish_xml(ev: &mut Event) {
    let raw = std::mem::take(&mut ev.message);
    let mut message = element_text(&raw, &["log4j:message", "Message"])
        .map(text_content)
        .unwrap_or_default();

    let item_re = {
        static R: OnceLock<Regex> = OnceLock::new();
        R.get_or_init(|| Regex::new(r"<(?:log4j:data|item)\s[^>]*>").unwrap())
    };
    for m in item_re.find_iter(&raw) {
        let a = attrs(m.as_str());
        if let (Some(k), Some(v)) = (attr(&a, &["name", "key"]), attr(&a, &["value"])) {
            ev.fields.push((k.to_string(), v.to_string()));
        }
    }
    if let Some(t) = element_text(&raw, &["log4j:throwable"]) {
        let trace = text_content(t);
        ev.exception = trace.lines().find_map(exception_class).unwrap_or_default().to_string();
        message.push('\n');
        message.push_str(trace.trim_end());
    } else if let Some(start) = raw.find("<Thrown") {
        let tag_end = raw[start..].find('>').map_or(raw.len(), |e| start + e);
        let a = attrs(&raw[start..tag_end]);
        let name = attr(&a, &["name"]).unwrap_or_default();
        ev.exception = name.to_string();
        message.push('\n');
        message.push_str(name);
        if let Some(m) = attr(&a, &["message", "localizedMessage"]) {
            message.push_str(": ");
            message.push_str(m);
        }
        // ExtendedStackTrace items: <ExtendedStackTraceItem class=".." method=".." file=".." line=".."/>
        let frame_re = {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r"<(?:ExtendedStackTraceItem|StackTraceElement)\s[^>]*>").unwrap())
        };
        for f in frame_re.find_iter(&raw[start..]) {
            let a = attrs(f.as_str());
            let class = attr(&a, &["class", "declaringClass"]).unwrap_or_default();
            let method = attr(&a, &["method", "methodName"]).unwrap_or_default();
            let file = attr(&a, &["file", "fileName"]).unwrap_or_default();
            let line = attr(&a, &["line", "lineNumber"]).unwrap_or_default();
            message.push_str(&format!("\n\tat {class}.{method}({file}:{line})"));
        }
    }
    ev.message = if message.is_empty() { raw } else { message };
}

// ---------------------------------------------------------------------------------------------
// Syslog: RFC 5424 (`<PRI>1 TIMESTAMP HOST APP PROCID MSGID SD MSG`) and RFC 3164 / BSD files
// (`[<PRI>]Mmm dd HH:mm:ss HOST TAG[PID]: MSG`, as in /var/log/syslog).
// ---------------------------------------------------------------------------------------------

fn syslog5424() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^<(\d{1,3})>1 (\S+) (\S+) (\S+) (\S+) (\S+) (-|(?:\[(?:[^\]\\]|\\.)*\])+) ?(.*)$").unwrap()
    })
}

fn syslog3164() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"^(?:<(\d{1,3})>)?((?:[A-Z][a-z]{2} [ \d]\d \d{2}:\d{2}:\d{2})|(?:\d{4}-\d{2}-\d{2}T\S+)) (\S+) ([^\s:\[]+)(?:\[(\d+)\])?: ?(.*)$",
        )
        .unwrap()
    })
}

fn severity_level(pri: &str) -> Option<Level> {
    let sev = pri.parse::<u32>().ok()? % 8;
    Some(match sev {
        0..=2 => Level::Fatal,
        3 => Level::Error,
        4 => Level::Warn,
        5 | 6 => Level::Info,
        _ => Level::Debug,
    })
}

fn nil(s: &str) -> Option<&str> {
    (s != "-").then_some(s)
}

pub fn parse_syslog(line: &str, ctx: &ParseCtx, ts: &GenericTimestamp, level_word: &Regex) -> Option<Event> {
    let mut ev = Event {
        message: line.to_string(),
        ..Default::default()
    };
    let msg_start;
    if let Some(c) = syslog5424().captures(line) {
        ev.level = severity_level(&c[1]);
        match nil(&c[2]).and_then(|t| ts.find(t, ctx.tz, ctx.default_date)) {
            Some((t, _)) => ev.ts = t,
            None => ev.ts_inferred = true,
        }
        for (name, i) in [("host", 3), ("app", 4), ("pid", 5), ("msgid", 6)] {
            if let Some(v) = nil(&c[i]) {
                ev.fields.push((name.to_string(), v.to_string()));
            }
        }
        ev.logger = nil(&c[4]).unwrap_or_default().to_string();
        let sd = &c[7];
        let sd_re = {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r#"([^\s="\]]+)="((?:[^"\\]|\\.)*)""#).unwrap())
        };
        for p in sd_re.captures_iter(sd) {
            ev.fields.push((p[1].to_string(), p[2].replace("\\\"", "\"")));
        }
        msg_start = c.get(8).map_or(line.len(), |m| m.start());
    } else {
        let c = syslog3164().captures(line)?;
        ev.level = c.get(1).and_then(|p| severity_level(p.as_str()));
        match ts.find(&c[2], ctx.tz, ctx.default_date) {
            Some((t, _)) => ev.ts = t,
            None => ev.ts_inferred = true,
        }
        ev.fields.push(("host".into(), c[3].to_string()));
        ev.fields.push(("app".into(), c[4].to_string()));
        if let Some(pid) = c.get(5) {
            ev.fields.push(("pid".into(), pid.as_str().to_string()));
        }
        ev.logger = c[4].to_string();
        msg_start = c.get(6).map_or(line.len(), |m| m.start());
    }
    if ev.level.is_none() {
        let msg = &line[msg_start..];
        let head = &msg[..crate::time::floor_char_boundary(msg, 120)];
        ev.level = level_word.find(head).map(|m| Level::parse(m.as_str()));
    }
    Some(ev)
}

// ---------------------------------------------------------------------------------------------
// logfmt: `time=2026-10-09T08:00:00Z level=info msg="started" user=bob`
// ---------------------------------------------------------------------------------------------

const LOGFMT_TS: &[&str] = &["time", "ts", "timestamp", "t", "@timestamp", "date"];
const LOGFMT_LEVEL: &[&str] = &["level", "lvl", "severity", "loglevel"];
const LOGFMT_LOGGER: &[&str] = &["logger", "component", "module", "caller", "source"];
const LOGFMT_THREAD: &[&str] = &["thread", "goroutine"];

pub fn parse_logfmt(line: &str, ctx: &ParseCtx, ts: &GenericTimestamp) -> Option<Event> {
    let mut pairs = Vec::new();
    crate::query::eval::extract_logfmt(line, &mut pairs);
    let key = |names: &[&str]| pairs.iter().position(|(k, _)| names.contains(&k.as_str()));
    let ts_i = key(LOGFMT_TS);
    let level_i = key(LOGFMT_LEVEL);
    // A line is a logfmt event when it has at least two pairs, one of them a time or level.
    if pairs.len() < 2 || (ts_i.is_none() && level_i.is_none()) {
        return None;
    }
    let mut ev = Event {
        message: line.to_string(),
        ..Default::default()
    };
    match ts_i.and_then(|i| {
        let v = &pairs[i].1;
        v.parse::<f64>()
            .ok()
            .map(|f| if f > 1e11 { f as i64 } else { (f * 1000.0) as i64 })
            .or_else(|| ts.find(v, ctx.tz, ctx.default_date).map(|(t, _)| t))
    }) {
        Some(t) => ev.ts = t,
        None => ev.ts_inferred = true,
    }
    ev.level = level_i.map(|i| Level::parse(&pairs[i].1));
    let logger_i = key(LOGFMT_LOGGER);
    if let Some(i) = logger_i {
        ev.logger = pairs[i].1.clone();
    }
    let thread_i = key(LOGFMT_THREAD);
    if let Some(i) = thread_i {
        ev.thread = pairs[i].1.clone();
    }
    for (i, (k, v)) in pairs.into_iter().enumerate() {
        let used = [ts_i, level_i, logger_i, thread_i].contains(&Some(i));
        if !used && !matches!(k.as_str(), "msg" | "message") && v.len() <= 1024 {
            if k == "err" || k == "error" {
                if let Some(c) = exception_class(&v) {
                    ev.exception = c.to_string();
                }
            }
            ev.fields.push((k, v));
        }
    }
    Some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{detect, Assembler, Format};
    use crate::time::TzMode;
    use chrono::NaiveDate;

    fn ctx() -> ParseCtx {
        ParseCtx {
            tz: TzMode::Utc,
            default_date: NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
            fallback_ts: 0,
        }
    }

    fn assemble(format: &Format, text: &str) -> Vec<Event> {
        let mut asm = Assembler::new(format, ctx());
        let mut out: Vec<Event> = text
            .lines()
            .enumerate()
            .filter_map(|(i, l)| asm.push(l, i as u32 + 1))
            .collect();
        out.extend(asm.finish());
        out
    }

    fn sample(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn log4j1_xml() {
        let text = r#"<?xml version="1.0" encoding="UTF-8" ?>
<log4j:eventSet version="1.2" xmlns:log4j="http://jakarta.apache.org/log4j/">
<log4j:event logger="com.acme.Pay" timestamp="1791533580123" level="ERROR" thread="http-1">
<log4j:message><![CDATA[charge failed for <card>]]></log4j:message>
<log4j:throwable><![CDATA[java.net.SocketTimeoutException: Read timed out
	at com.acme.Pay.charge(Pay.java:42)
]]></log4j:throwable>
<log4j:properties>
<log4j:data name="requestId" value="req-7"/>
</log4j:properties>
</log4j:event>

<log4j:event logger="com.acme.App" timestamp="1791533581000" level="INFO" thread="main">
<log4j:message><![CDATA[ok &amp; done]]></log4j:message>
</log4j:event>
</log4j:eventSet>
"#;
        let det = detect(&sample(text), &[], &ctx());
        assert!(matches!(det.format, Format::Xml), "{:?}", det.format.info());
        let evs = assemble(&det.format, text);
        assert_eq!(evs.len(), 2, "{evs:#?}");
        let e = &evs[0];
        assert_eq!(e.ts, 1791533580123);
        assert_eq!(e.level, Some(Level::Error));
        assert_eq!(e.logger, "com.acme.Pay");
        assert_eq!(e.thread, "http-1");
        assert_eq!(e.exception, "java.net.SocketTimeoutException");
        assert_eq!(e.fields, vec![("requestId".to_string(), "req-7".to_string())]);
        assert!(e
            .message
            .starts_with("charge failed for <card>\njava.net.SocketTimeoutException"));
        assert_eq!(evs[1].message, "ok &amp; done");
    }

    #[test]
    fn log4j2_xml_pretty_and_compact() {
        let text = r#"<?xml version="1.0" encoding="UTF-8"?>
<Events xmlns="http://logging.apache.org/log4j/2.0/events">
<Event xmlns="http://logging.apache.org/log4j/2.0/events" timeMillis="1791533580123" thread="main" level="WARN" loggerName="com.acme.Orders" endOfBatch="false">
  <Instant epochSecond="1791533580" nanoOfSecond="123000000"/>
  <Message>slow &quot;query&quot;</Message>
  <ContextMap>
    <item key="requestId" value="req-9"/>
  </ContextMap>
  <Thrown commonElementCount="0" message="boom" name="java.lang.IllegalStateException">
    <ExtendedStackTrace>
      <ExtendedStackTraceItem class="com.acme.Orders" method="place" file="Orders.java" line="12" exact="true"/>
    </ExtendedStackTrace>
  </Thrown>
</Event>
<Event timeMillis="1791533581000" thread="main" level="INFO" loggerName="com.acme.App"><Message>hi</Message></Event>
</Events>
"#;
        let det = detect(&sample(text), &[], &ctx());
        assert!(matches!(det.format, Format::Xml));
        let evs = assemble(&det.format, text);
        assert_eq!(evs.len(), 2, "{evs:#?}");
        assert_eq!(evs[0].level, Some(Level::Warn));
        assert_eq!(evs[0].exception, "java.lang.IllegalStateException");
        assert_eq!(
            evs[0].message,
            "slow \"query\"\njava.lang.IllegalStateException: boom\n\tat com.acme.Orders.place(Orders.java:12)"
        );
        assert_eq!(evs[0].fields, vec![("requestId".to_string(), "req-9".to_string())]);
        assert_eq!(evs[1].message, "hi");
        assert_eq!(evs[1].ts, 1791533581000);
    }

    #[test]
    fn syslog_formats() {
        let text = "\
<165>1 2026-10-09T08:13:00.003Z web01 payments 812 ID47 [exampleSDID@32473 requestId=\"req-1\"] charge ok
<11>1 2026-10-09T08:13:01Z web01 payments - - - card declined
Oct  9 08:13:02 web01 sshd[1234]: Accepted publickey for onur
Oct  9 08:13:03 web01 kernel: ERROR: disk sda1 failing
";
        let det = detect(&sample(text), &[], &ctx());
        assert!(matches!(det.format, Format::Syslog), "{:?}", det.format.info());
        let evs = assemble(&det.format, text);
        assert_eq!(evs.len(), 4);
        assert_eq!(evs[0].level, Some(Level::Info));
        assert_eq!(evs[0].logger, "payments");
        assert!(evs[0].fields.contains(&("requestId".into(), "req-1".into())));
        assert!(evs[0].fields.contains(&("host".into(), "web01".into())));
        assert_eq!(evs[1].level, Some(Level::Error));
        assert_eq!(evs[2].logger, "sshd");
        assert!(evs[2].fields.contains(&("pid".into(), "1234".into())));
        assert_eq!(evs[2].ts, 1791533582000);
        assert_eq!(evs[3].level, Some(Level::Error));
    }

    #[test]
    fn logfmt_lines() {
        let text = "\
time=2026-10-09T08:13:00.120Z level=info msg=\"server started\" port=8080
time=2026-10-09T08:13:01Z level=error component=db msg=\"query failed\" err=\"java.sql.SQLException: timeout\"
ts=1791533582.5 lvl=warn msg=slow took=1.2s
";
        let det = detect(&sample(text), &[], &ctx());
        assert!(matches!(det.format, Format::Logfmt), "{:?}", det.format.info());
        let evs = assemble(&det.format, text);
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0].ts, 1791533580120);
        assert_eq!(evs[0].fields, vec![("port".to_string(), "8080".to_string())]);
        assert_eq!(evs[1].logger, "db");
        assert_eq!(evs[1].exception, "java.sql.SQLException");
        assert_eq!(evs[2].ts, 1791533582500);
        assert_eq!(evs[2].level, Some(Level::Warn));
    }

    #[test]
    fn unescapes() {
        assert_eq!(unescape("a &lt;b&gt; &amp;&#65;&#x42; & c"), "a <b> &AB & c");
    }
}
