//! Timestamp handling: compiling Java `SimpleDateFormat` / log4j named date formats into
//! regex fragments, and assembling epoch milliseconds from the captured parts.

use chrono::{FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// How timestamps without an explicit offset are interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "offsetSeconds", rename_all = "lowercase")]
pub enum TzMode {
    /// The machine's local time zone (default: logs are usually read where they were written).
    #[default]
    Local,
    Utc,
    /// A fixed offset east of UTC, in seconds.
    Fixed(i32),
}

impl TzMode {
    pub fn to_epoch_ms(self, dt: NaiveDateTime) -> i64 {
        match self {
            TzMode::Utc => dt.and_utc().timestamp_millis(),
            TzMode::Fixed(secs) => match FixedOffset::east_opt(secs) {
                Some(off) => (dt - chrono::Duration::seconds(off.local_minus_utc() as i64))
                    .and_utc()
                    .timestamp_millis(),
                None => dt.and_utc().timestamp_millis(),
            },
            TzMode::Local => match Local.from_local_datetime(&dt) {
                chrono::LocalResult::Single(t) => t.timestamp_millis(),
                chrono::LocalResult::Ambiguous(t, _) => t.timestamp_millis(),
                // Inside a DST gap: fall back to the current offset.
                chrono::LocalResult::None => {
                    let off = Local::now().offset().fix().local_minus_utc() as i64;
                    (dt - chrono::Duration::seconds(off)).and_utc().timestamp_millis()
                }
            },
        }
    }
}

/// A component of a date captured by a regex group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePart {
    Year4,
    Year2,
    Month,
    MonthName,
    Day,
    Hour24,
    Hour12,
    Minute,
    Second,
    Fraction,
    AmPm,
    Zone,
    UnixSeconds,
    UnixMillis,
}

/// A date format compiled to a regex fragment with named groups.
#[derive(Clone, Debug)]
pub struct CompiledDate {
    pub regex: String,
    /// (group name, part) pairs, in the order they appear.
    pub groups: Vec<(String, DatePart)>,
}

/// Expands log4j2 / Logback named date formats to a `SimpleDateFormat` string.
pub fn expand_named_format(name: &str) -> &str {
    match name {
        "" | "DEFAULT" => "yyyy-MM-dd HH:mm:ss,SSS",
        "ABSOLUTE" => "HH:mm:ss,SSS",
        "ABSOLUTE_MICROS" => "HH:mm:ss,nnnnnn",
        "ABSOLUTE_NANOS" => "HH:mm:ss,nnnnnnnnn",
        "COMPACT" => "yyyyMMddHHmmssSSS",
        "DATE" => "dd MMM yyyy HH:mm:ss,SSS",
        "ISO8601_BASIC" => "yyyyMMdd'T'HHmmss,SSS",
        "ISO8601_BASIC_PERIOD" => "yyyyMMdd'T'HHmmss.SSS",
        "ISO8601" => "yyyy-MM-dd'T'HH:mm:ss,SSS",
        "ISO8601_PERIOD" => "yyyy-MM-dd'T'HH:mm:ss.SSS",
        "ISO8601_OFFSET_DATE_TIME_HH" => "yyyy-MM-dd'T'HH:mm:ss,SSSX",
        "ISO8601_OFFSET_DATE_TIME_HHMM" => "yyyy-MM-dd'T'HH:mm:ss,SSSXX",
        "ISO8601_OFFSET_DATE_TIME_HHCMM" => "yyyy-MM-dd'T'HH:mm:ss,SSSXXX",
        "DEFAULT_MICROS" => "yyyy-MM-dd HH:mm:ss,nnnnnn",
        "DEFAULT_NANOS" => "yyyy-MM-dd HH:mm:ss,nnnnnnnnn",
        "DEFAULT_PERIOD" => "yyyy-MM-dd HH:mm:ss.SSS",
        "US_MONTH_DAY_YEAR2_TIME" => "dd/MM/yy HH:mm:ss.SSS",
        "US_MONTH_DAY_YEAR4_TIME" => "dd/MM/yyyy HH:mm:ss.SSS",
        other => other,
    }
}

/// Compiles a `SimpleDateFormat` (or `DateTimeFormatter`-style) pattern. Group names are
/// prefixed with `prefix` so the fragment can be embedded in a larger regex.
pub fn compile_date_format(format: &str, prefix: &str) -> CompiledDate {
    let format = expand_named_format(format.trim());
    let mut out = CompiledDate {
        regex: String::new(),
        groups: Vec::new(),
    };
    if format == "UNIX" {
        push_group(&mut out, prefix, DatePart::UnixSeconds, r"\d{9,11}");
        return out;
    }
    if format == "UNIX_MILLIS" {
        push_group(&mut out, prefix, DatePart::UnixMillis, r"\d{12,14}");
        return out;
    }
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // Quoted literal; '' is an escaped quote.
            i += 1;
            if i < chars.len() && chars[i] == '\'' {
                out.regex.push('\'');
                i += 1;
                continue;
            }
            while i < chars.len() && chars[i] != '\'' {
                out.regex.push_str(&regex::escape(&chars[i].to_string()));
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() {
            let mut n = 1;
            while i + n < chars.len() && chars[i + n] == c {
                n += 1;
            }
            i += n;
            match (c, n) {
                ('y' | 'u', 2) => push_group(&mut out, prefix, DatePart::Year2, r"\d{2}"),
                ('y' | 'u', _) => push_group(&mut out, prefix, DatePart::Year4, r"\d{4}"),
                ('M' | 'L', 1) => push_group(&mut out, prefix, DatePart::Month, r"\d{1,2}"),
                ('M' | 'L', 2) => push_group(&mut out, prefix, DatePart::Month, r"\d{2}"),
                ('M' | 'L', _) => push_group(&mut out, prefix, DatePart::MonthName, r"[A-Za-z]{3,9}\.?"),
                ('d', 1) => push_group(&mut out, prefix, DatePart::Day, r"\d{1,2}"),
                ('d', _) => push_group(&mut out, prefix, DatePart::Day, r"\d{2}"),
                ('H' | 'k', 1) => push_group(&mut out, prefix, DatePart::Hour24, r"\d{1,2}"),
                ('H' | 'k', _) => push_group(&mut out, prefix, DatePart::Hour24, r"\d{2}"),
                ('h' | 'K', 1) => push_group(&mut out, prefix, DatePart::Hour12, r"\d{1,2}"),
                ('h' | 'K', _) => push_group(&mut out, prefix, DatePart::Hour12, r"\d{2}"),
                ('m', 1) => push_group(&mut out, prefix, DatePart::Minute, r"\d{1,2}"),
                ('m', _) => push_group(&mut out, prefix, DatePart::Minute, r"\d{2}"),
                ('s', 1) => push_group(&mut out, prefix, DatePart::Second, r"\d{1,2}"),
                ('s', _) => push_group(&mut out, prefix, DatePart::Second, r"\d{2}"),
                ('S' | 'n', n) => {
                    // Accept either separator before the fraction: patterns say ',' but many
                    // configs are copied between log4j and Logback with '.'.
                    if out.regex.ends_with(',') || out.regex.ends_with(r"\.") {
                        let cut = if out.regex.ends_with(',') { 1 } else { 2 };
                        out.regex.truncate(out.regex.len() - cut);
                        out.regex.push_str("[,.]");
                    }
                    push_group(&mut out, prefix, DatePart::Fraction, &format!(r"\d{{{n}}}"));
                }
                ('a', _) => push_group(&mut out, prefix, DatePart::AmPm, r"(?i:AM|PM)"),
                ('E', _) => out.regex.push_str(r"[A-Za-z]{2,9}\.?"),
                ('X' | 'x' | 'Z', _) => push_group(&mut out, prefix, DatePart::Zone, r"(?:Z|[+-]\d{2}(?::?\d{2})?)"),
                ('z' | 'V' | 'O', _) => out.regex.push_str(r"[A-Za-z0-9_/+:-]+"),
                ('G', _) => out.regex.push_str("[A-Za-z]{2}"),
                ('T', _) => out.regex.push('T'),
                (_, _) => out.regex.push_str(r"\S+"),
            }
            continue;
        }
        out.regex.push_str(&regex::escape(&c.to_string()));
        i += 1;
    }
    out
}

fn push_group(out: &mut CompiledDate, prefix: &str, part: DatePart, re: &str) {
    if out.groups.iter().any(|(_, p)| *p == part) {
        out.regex.push_str(&format!("(?:{re})"));
        return;
    }
    let name = format!("{prefix}{}", out.groups.len());
    out.regex.push_str(&format!("(?P<{name}>{re})"));
    out.groups.push((name, part));
}

/// Values captured for a date, before assembly.
#[derive(Default, Debug)]
pub struct DateParts<'a> {
    pub values: Vec<(DatePart, &'a str)>,
}

fn month_from_name(s: &str) -> Option<u32> {
    let s = s.trim_end_matches('.');
    let lower = s.to_ascii_lowercase();
    let p = lower.get(..3)?;
    Some(match p {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

fn parse_zone(s: &str) -> Option<i32> {
    if s == "Z" {
        return Some(0);
    }
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let digits: String = s[1..].chars().filter(|c| c.is_ascii_digit()).collect();
    let h: i32 = digits.get(0..2)?.parse().ok()?;
    let m: i32 = digits.get(2..4).and_then(|m| m.parse().ok()).unwrap_or(0);
    Some(sign * (h * 3600 + m * 60))
}

/// Assembles epoch millis from captured parts. `default_date` fills in a missing date
/// (formats like `HH:mm:ss,SSS`); usually the file's modification date.
pub fn assemble(parts: &DateParts, tz: TzMode, default_date: NaiveDate) -> Option<i64> {
    let mut year = None;
    let mut month = None;
    let mut day = None;
    let (mut hour, mut minute, mut second, mut nanos) = (0u32, 0u32, 0u32, 0u32);
    let mut pm: Option<bool> = None;
    let mut h12 = false;
    let mut zone: Option<i32> = None;
    for &(part, v) in &parts.values {
        match part {
            DatePart::Year4 => year = v.parse::<i32>().ok(),
            DatePart::Year2 => year = v.parse::<i32>().ok().map(|y| 2000 + y),
            DatePart::Month => month = v.parse::<u32>().ok(),
            DatePart::MonthName => month = month_from_name(v),
            DatePart::Day => day = v.parse::<u32>().ok(),
            DatePart::Hour24 => hour = v.parse().ok()?,
            DatePart::Hour12 => {
                hour = v.parse().ok()?;
                h12 = true;
            }
            DatePart::Minute => minute = v.parse().ok()?,
            DatePart::Second => second = v.parse().ok()?,
            DatePart::Fraction => {
                let digits = &v[..v.len().min(9)];
                let n: u32 = digits.parse().ok()?;
                nanos = n * 10u32.pow(9 - digits.len() as u32);
            }
            DatePart::AmPm => pm = Some(v.eq_ignore_ascii_case("pm")),
            DatePart::Zone => zone = parse_zone(v),
            DatePart::UnixSeconds => return v.parse::<i64>().ok().map(|s| s * 1000),
            DatePart::UnixMillis => return v.parse::<i64>().ok(),
        }
    }
    if h12 {
        hour %= 12;
        if pm == Some(true) {
            hour += 12;
        }
    }
    if hour == 24 {
        hour = 0;
    }
    let date = NaiveDate::from_ymd_opt(
        year.unwrap_or(default_date.year_ce_compat()),
        month.unwrap_or(default_date.month_compat()),
        day.unwrap_or(default_date.day_compat()),
    )?;
    let time = NaiveTime::from_hms_nano_opt(hour, minute, second.min(59), nanos)?;
    let dt = NaiveDateTime::new(date, time);
    Some(match zone {
        Some(off) => TzMode::Fixed(off).to_epoch_ms(dt),
        None => tz.to_epoch_ms(dt),
    })
}

// Small helpers so callers don't need chrono's Datelike trait in scope.
trait DateCompat {
    fn year_ce_compat(&self) -> i32;
    fn month_compat(&self) -> u32;
    fn day_compat(&self) -> u32;
}
impl DateCompat for NaiveDate {
    fn year_ce_compat(&self) -> i32 {
        chrono::Datelike::year(self)
    }
    fn month_compat(&self) -> u32 {
        chrono::Datelike::month(self)
    }
    fn day_compat(&self) -> u32 {
        chrono::Datelike::day(self)
    }
}

/// Generic timestamp detector used for formats without a declared layout: finds the first
/// ISO-8601-like or common date/time stamp near the start of a line.
pub struct GenericTimestamp {
    re: regex::Regex,
}

impl Default for GenericTimestamp {
    fn default() -> Self {
        Self::new()
    }
}

impl GenericTimestamp {
    pub fn new() -> Self {
        // yyyy-MM-dd[T ]HH:mm:ss[.,fff][zone]  |  dd MMM yyyy HH:mm:ss  |  MMM dd HH:mm:ss (syslog)
        let re = regex::Regex::new(
            r"(?x)
            (?P<y>\d{4})[-/](?P<mo>\d{2})[-/](?P<d>\d{2})[T\s](?P<h>\d{2}):(?P<mi>\d{2}):(?P<s>\d{2})(?:[.,](?P<f>\d{1,9}))?(?P<z>Z|[+-]\d{2}:?\d{2})?
            |
            (?P<d2>\d{1,2})\s(?P<mon2>[A-Z][a-z]{2})\s(?P<y2>\d{4})\s(?P<h2>\d{2}):(?P<mi2>\d{2}):(?P<s2>\d{2})(?:[.,](?P<f2>\d{1,9}))?
            |
            (?P<mon3>[A-Z][a-z]{2})\s+(?P<d3>\d{1,2})\s(?P<h3>\d{2}):(?P<mi3>\d{2}):(?P<s3>\d{2})
            ",
        )
        .expect("generic timestamp regex");
        GenericTimestamp { re }
    }

    /// Returns (epoch ms, byte range of the match) when a timestamp starts within the first
    /// 40 bytes of the line.
    pub fn find(&self, line: &str, tz: TzMode, default_date: NaiveDate) -> Option<(i64, usize)> {
        let head = &line[..floor_char_boundary(line, 120)];
        let c = self.re.captures(head)?;
        let m = c.get(0)?;
        if m.start() > 40 {
            return None;
        }
        let mut parts = DateParts::default();
        let mut add = |name: &str, part: DatePart| {
            if let Some(v) = c.name(name) {
                parts.values.push((part, v.as_str()));
            }
        };
        add("y", DatePart::Year4);
        add("mo", DatePart::Month);
        add("d", DatePart::Day);
        add("h", DatePart::Hour24);
        add("mi", DatePart::Minute);
        add("s", DatePart::Second);
        add("f", DatePart::Fraction);
        add("z", DatePart::Zone);
        add("d2", DatePart::Day);
        add("mon2", DatePart::MonthName);
        add("y2", DatePart::Year4);
        add("h2", DatePart::Hour24);
        add("mi2", DatePart::Minute);
        add("s2", DatePart::Second);
        add("f2", DatePart::Fraction);
        add("mon3", DatePart::MonthName);
        add("d3", DatePart::Day);
        add("h3", DatePart::Hour24);
        add("mi3", DatePart::Minute);
        add("s3", DatePart::Second);
        assemble(&parts, tz, default_date).map(|ts| (ts, m.end()))
    }
}

pub fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Date of a file modification time, in the given zone, used as the default date.
pub fn date_of_ms(ms: i64, tz: TzMode) -> NaiveDate {
    let utc = Utc.timestamp_millis_opt(ms).single().unwrap_or_else(Utc::now);
    match tz {
        TzMode::Utc => utc.date_naive(),
        TzMode::Fixed(off) => (utc + chrono::Duration::seconds(off as i64)).date_naive(),
        TzMode::Local => utc.with_timezone(&Local).date_naive(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(fmt: &str, text: &str) -> Option<i64> {
        let c = compile_date_format(fmt, "d");
        let re = regex::Regex::new(&format!("^{}$", c.regex)).unwrap();
        let caps = re.captures(text)?;
        let mut parts = DateParts::default();
        for (name, part) in &c.groups {
            if let Some(m) = caps.name(name) {
                parts.values.push((*part, m.as_str()));
            }
        }
        assemble(&parts, TzMode::Utc, NaiveDate::from_ymd_opt(2026, 1, 2).unwrap())
    }

    #[test]
    fn named_and_custom_formats() {
        let iso = 1_791_484_245_123; // 2026-10-08T18:30:45.123Z
        assert_eq!(parse("ISO8601", "2026-10-08T18:30:45,123"), Some(iso));
        assert_eq!(parse("ISO8601", "2026-10-08T18:30:45.123"), Some(iso));
        assert_eq!(parse("", "2026-10-08 18:30:45,123"), Some(iso));
        assert_eq!(parse("yyyy-MM-dd HH:mm:ss.SSS", "2026-10-08 18:30:45.123"), Some(iso));
        assert_eq!(parse("dd MMM yyyy HH:mm:ss,SSS", "08 Oct 2026 18:30:45,123"), Some(iso));
        assert_eq!(parse("yyyyMMddHHmmssSSS", "20261008183045123"), Some(iso));
        assert_eq!(
            parse("yyyy-MM-dd'T'HH:mm:ss.SSSXXX", "2026-10-08T20:30:45.123+02:00"),
            Some(iso)
        );
        assert_eq!(parse("UNIX_MILLIS", "1791484245123"), Some(iso));
        assert_eq!(
            parse("dd/MM/yyyy hh:mm:ss a", "08/10/2026 06:30:45 PM"),
            Some(iso - 123)
        );
        // Time only: date comes from the default date.
        assert_eq!(
            parse("HH:mm:ss.SSS", "18:30:45.123"),
            Some(
                NaiveDate::from_ymd_opt(2026, 1, 2)
                    .unwrap()
                    .and_hms_milli_opt(18, 30, 45, 123)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis()
            )
        );
    }

    #[test]
    fn generic_detector() {
        let g = GenericTimestamp::new();
        let d = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let (ts, _) = g.find("2026-10-08T18:30:45.123Z INFO hello", TzMode::Local, d).unwrap();
        assert_eq!(ts, 1_791_484_245_123);
        assert!(g.find("no timestamp here", TzMode::Utc, d).is_none());
        assert!(g.find("[main] 08 Oct 2026 18:30:45,123 x", TzMode::Utc, d).is_some());
    }
}
