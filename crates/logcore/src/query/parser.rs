//! Hand-written lexer and recursive-descent parser for the LogQL subset, with byte spans
//! on errors so the editor can underline the problem.

use super::ast::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct ParseError {
    pub message: String,
    /// Byte offsets into the query text.
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LBrace,
    RBrace,
    LParen,
    RParen,
    Comma,
    Pipe,
    PipeEq,
    PipeTilde,
    Eq,
    EqEq,
    Ne,
    Re,
    Nre,
    Gt,
    Ge,
    Lt,
    Le,
    Str(String),
    Ident(String),
    /// Numeric literal, possibly with a unit; keeps its source text.
    Num(String),
    Eof,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Str(s) => format!("string \"{s}\""),
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Num(s) => format!("number {s}"),
            Tok::Eof => "end of query".into(),
            other => format!("`{}`", tok_text(other)),
        }
    }
}

fn tok_text(t: &Tok) -> &'static str {
    match t {
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::Comma => ",",
        Tok::Pipe => "|",
        Tok::PipeEq => "|=",
        Tok::PipeTilde => "|~",
        Tok::Eq => "=",
        Tok::EqEq => "==",
        Tok::Ne => "!=",
        Tok::Re => "=~",
        Tok::Nre => "!~",
        Tok::Gt => ">",
        Tok::Ge => ">=",
        Tok::Lt => "<",
        Tok::Le => "<=",
        _ => "?",
    }
}

struct Spanned {
    tok: Tok,
    start: usize,
    end: usize,
}

fn err(message: impl Into<String>, start: usize, end: usize) -> ParseError {
    ParseError {
        message: message.into(),
        start,
        end,
    }
}

fn lex(src: &str) -> Result<Vec<Spanned>, ParseError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'#' {
            // Comment to end of line.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        let two = if i + 1 < bytes.len() {
            &bytes[i..i + 2]
        } else {
            &bytes[i..i + 1]
        };
        let (tok, len) = match two {
            b"|=" => (Tok::PipeEq, 2),
            b"|~" => (Tok::PipeTilde, 2),
            b"==" => (Tok::EqEq, 2),
            b"!=" => (Tok::Ne, 2),
            b"=~" => (Tok::Re, 2),
            b"!~" => (Tok::Nre, 2),
            b">=" => (Tok::Ge, 2),
            b"<=" => (Tok::Le, 2),
            _ => match c {
                b'{' => (Tok::LBrace, 1),
                b'}' => (Tok::RBrace, 1),
                b'(' => (Tok::LParen, 1),
                b')' => (Tok::RParen, 1),
                b',' => (Tok::Comma, 1),
                b'|' => (Tok::Pipe, 1),
                b'=' => (Tok::Eq, 1),
                b'>' => (Tok::Gt, 1),
                b'<' => (Tok::Lt, 1),
                b'"' => {
                    let mut s = String::new();
                    let mut j = i + 1;
                    let mut closed = false;
                    while j < bytes.len() {
                        match bytes[j] {
                            b'"' => {
                                closed = true;
                                j += 1;
                                break;
                            }
                            b'\\' if j + 1 < bytes.len() => {
                                match bytes[j + 1] {
                                    b'n' => s.push('\n'),
                                    b't' => s.push('\t'),
                                    b'r' => s.push('\r'),
                                    b'"' => s.push('"'),
                                    b'\\' => s.push('\\'),
                                    other => {
                                        // Keep unknown escapes (common in regexes: "\d").
                                        s.push('\\');
                                        s.push(other as char);
                                    }
                                }
                                j += 2;
                            }
                            _ => {
                                let ch = src[j..].chars().next().unwrap();
                                s.push(ch);
                                j += ch.len_utf8();
                            }
                        }
                    }
                    if !closed {
                        return Err(err("unterminated string", i, bytes.len()));
                    }
                    out.push(Spanned {
                        tok: Tok::Str(s),
                        start,
                        end: j,
                    });
                    i = j;
                    continue;
                }
                b'`' => {
                    let Some(rel) = src[i + 1..].find('`') else {
                        return Err(err("unterminated raw string", i, bytes.len()));
                    };
                    let s = src[i + 1..i + 1 + rel].to_string();
                    let end = i + rel + 2;
                    out.push(Spanned {
                        tok: Tok::Str(s),
                        start,
                        end,
                    });
                    i = end;
                    continue;
                }
                c if c.is_ascii_digit() || (c == b'-' && bytes.get(i + 1).is_some_and(|d| d.is_ascii_digit())) => {
                    let mut j = i + 1;
                    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.') {
                        j += 1;
                    }
                    // µs
                    if src[j..].starts_with('µ') {
                        j += 'µ'.len_utf8();
                        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                            j += 1;
                        }
                    }
                    out.push(Spanned {
                        tok: Tok::Num(src[i..j].to_string()),
                        start,
                        end: j,
                    });
                    i = j;
                    continue;
                }
                c if c.is_ascii_alphabetic() || c == b'_' || c == b'@' => {
                    let mut j = i + 1;
                    while j < bytes.len()
                        && (bytes[j].is_ascii_alphanumeric()
                            || bytes[j] == b'_'
                            || bytes[j] == b'.'
                            || bytes[j] == b'@')
                    {
                        j += 1;
                    }
                    out.push(Spanned {
                        tok: Tok::Ident(src[i..j].to_string()),
                        start,
                        end: j,
                    });
                    i = j;
                    continue;
                }
                _ => {
                    let ch = src[i..].chars().next().unwrap();
                    return Err(err(format!("unexpected character `{ch}`"), i, i + ch.len_utf8()));
                }
            },
        };
        out.push(Spanned {
            tok,
            start,
            end: start + len,
        });
        i += len;
    }
    out.push(Spanned {
        tok: Tok::Eof,
        start: src.len(),
        end: src.len(),
    });
    Ok(out)
}

/// Parses a duration like `2s`, `1.5m`, `150ms`, `1h30m` into milliseconds.
pub fn parse_duration_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    let mut total = 0.0;
    let mut rest = s;
    let mut any = false;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let n: f64 = rest[..num_end].parse().ok()?;
        rest = &rest[num_end..];
        let unit_end = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = &rest[unit_end..];
        let mult = match unit {
            "ns" => 1e-6,
            "us" | "µs" => 1e-3,
            "ms" => 1.0,
            "s" => 1000.0,
            "m" => 60_000.0,
            "h" => 3_600_000.0,
            "d" => 86_400_000.0,
            _ => return None,
        };
        total += n * mult;
        any = true;
    }
    any.then_some(total)
}

/// Parses a byte size like `10MB`, `512KiB`, `1.5GB` into bytes.
pub fn parse_bytes(s: &str) -> Option<f64> {
    let s = s.trim();
    let num_end = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))?;
    let n: f64 = s[..num_end].parse().ok()?;
    let mult = match s[num_end..].to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" | "k" => 1024.0,
        "mib" | "m" => 1024.0 * 1024.0,
        "gib" | "g" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some(n * mult)
}

fn parse_num_literal(text: &str) -> Option<Value> {
    if let Ok(n) = text.parse::<f64>() {
        return Some(Value::Number(n));
    }
    if let Some(d) = parse_duration_ms(text) {
        return Some(Value::Duration(d));
    }
    parse_bytes(text).map(Value::Bytes)
}

struct Parser {
    toks: Vec<Spanned>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn span(&self) -> (usize, usize) {
        let t = &self.toks[self.pos];
        (t.start, t.end)
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn unexpected(&self, expected: &str) -> ParseError {
        let (s, e) = self.span();
        err(format!("expected {expected}, found {}", self.peek().describe()), s, e)
    }

    fn expect(&mut self, tok: Tok, expected: &str) -> Result<(), ParseError> {
        if *self.peek() == tok {
            self.next();
            Ok(())
        } else {
            Err(self.unexpected(expected))
        }
    }

    fn string(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek().clone() {
            Tok::Str(s) => {
                self.next();
                Ok(s)
            }
            _ => Err(self.unexpected(what)),
        }
    }

    fn query(&mut self) -> Result<Query, ParseError> {
        let mut q = Query::default();
        if *self.peek() == Tok::LBrace {
            self.next();
            while *self.peek() != Tok::RBrace {
                let name = match self.next() {
                    Tok::Ident(n) => n,
                    _ => {
                        self.pos -= 1;
                        return Err(self.unexpected("a label name or `}`"));
                    }
                };
                let op = match self.peek() {
                    Tok::Eq => MatchOp::Eq,
                    Tok::Ne => MatchOp::Ne,
                    Tok::Re => MatchOp::Re,
                    Tok::Nre => MatchOp::Nre,
                    _ => return Err(self.unexpected("`=`, `!=`, `=~` or `!~`")),
                };
                self.next();
                let (vs, ve) = self.span();
                let value = self.string("a quoted label value")?;
                if matches!(op, MatchOp::Re | MatchOp::Nre) {
                    if let Err(e) = regex::Regex::new(&value) {
                        return Err(err(format!("invalid regex: {e}"), vs, ve));
                    }
                }
                q.selector.push(Matcher { name, op, value });
                match self.peek() {
                    Tok::Comma => {
                        self.next();
                    }
                    Tok::RBrace => {}
                    _ => return Err(self.unexpected("`,` or `}`")),
                }
            }
            self.next();
        }
        loop {
            match self.peek().clone() {
                Tok::Eof => break,
                Tok::PipeEq | Tok::Ne | Tok::PipeTilde | Tok::Nre => {
                    let op = match self.next() {
                        Tok::PipeEq => LineOp::Contains,
                        Tok::Ne => LineOp::NotContains,
                        Tok::PipeTilde => LineOp::Re,
                        _ => LineOp::NotRe,
                    };
                    let mut values = Vec::new();
                    loop {
                        let (vs, ve) = self.span();
                        let v = self.string("a quoted string after the line filter")?;
                        if matches!(op, LineOp::Re | LineOp::NotRe) {
                            if let Err(e) = regex::Regex::new(&v) {
                                return Err(err(format!("invalid regex: {e}"), vs, ve));
                            }
                        }
                        values.push(v);
                        if matches!(self.peek(), Tok::Ident(w) if w == "or") {
                            self.next();
                            continue;
                        }
                        break;
                    }
                    q.stages.push(Stage::Line { op, values });
                }
                Tok::Pipe => {
                    self.next();
                    let stage = match self.peek().clone() {
                        Tok::Ident(w) if w == "json" => {
                            self.next();
                            Stage::Json
                        }
                        Tok::Ident(w) if w == "logfmt" => {
                            self.next();
                            Stage::Logfmt
                        }
                        Tok::Ident(w) if w == "regexp" => {
                            self.next();
                            let (vs, ve) = self.span();
                            let pattern = self.string("a quoted regex after `regexp`")?;
                            let re =
                                regex::Regex::new(&pattern).map_err(|e| err(format!("invalid regex: {e}"), vs, ve))?;
                            if re.capture_names().flatten().next().is_none() {
                                return Err(err("regexp needs at least one named group, like (?P<name>...)", vs, ve));
                            }
                            Stage::Regexp { pattern }
                        }
                        Tok::Ident(w)
                            if matches!(
                                w.as_str(),
                                "line_format" | "label_format" | "pattern" | "unpack" | "drop" | "keep" | "decolorize"
                            ) =>
                        {
                            let (s, e) = self.span();
                            return Err(err(format!("`{w}` is not supported yet"), s, e));
                        }
                        Tok::Ident(_) | Tok::LParen => Stage::Filter { expr: self.or_expr()? },
                        _ => return Err(self.unexpected("`json`, `logfmt`, `regexp` or a field filter")),
                    };
                    q.stages.push(stage);
                }
                _ => return Err(self.unexpected("`|=`, `!=`, `|~`, `!~` or `|`")),
            }
        }
        Ok(q)
    }

    fn or_expr(&mut self) -> Result<FieldExpr, ParseError> {
        let mut left = self.and_expr()?;
        while matches!(self.peek(), Tok::Ident(w) if w == "or") {
            self.next();
            let right = self.and_expr()?;
            left = FieldExpr::Or {
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<FieldExpr, ParseError> {
        let mut left = self.unary()?;
        loop {
            match self.peek() {
                Tok::Ident(w) if w == "and" => {
                    self.next();
                }
                Tok::Comma => {
                    self.next();
                }
                _ => break,
            }
            let right = self.unary()?;
            left = FieldExpr::And {
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<FieldExpr, ParseError> {
        if *self.peek() == Tok::LParen {
            self.next();
            let e = self.or_expr()?;
            self.expect(Tok::RParen, "`)`")?;
            return Ok(e);
        }
        let name = match self.peek().clone() {
            Tok::Ident(n) => {
                self.next();
                n
            }
            _ => return Err(self.unexpected("a field name")),
        };
        let op = match self.peek() {
            Tok::Eq | Tok::EqEq => CmpOp::Eq,
            Tok::Ne => CmpOp::Ne,
            Tok::Gt => CmpOp::Gt,
            Tok::Ge => CmpOp::Ge,
            Tok::Lt => CmpOp::Lt,
            Tok::Le => CmpOp::Le,
            Tok::Re => CmpOp::Re,
            Tok::Nre => CmpOp::Nre,
            _ => return Err(self.unexpected("a comparison operator")),
        };
        self.next();
        let (vs, ve) = self.span();
        let (value, text) = match self.next() {
            Tok::Str(s) => {
                if matches!(op, CmpOp::Re | CmpOp::Nre) {
                    if let Err(e) = regex::Regex::new(&s) {
                        return Err(err(format!("invalid regex: {e}"), vs, ve));
                    }
                }
                (Value::String(s), String::new())
            }
            Tok::Num(t) => match parse_num_literal(&t) {
                Some(v) => (v, t),
                None => return Err(err(format!("`{t}` is not a number, duration or byte size"), vs, ve)),
            },
            _ => {
                self.pos -= 1;
                return Err(self.unexpected("a value"));
            }
        };
        if matches!(op, CmpOp::Re | CmpOp::Nre) && !matches!(value, Value::String(_)) {
            return Err(err("regex comparisons need a quoted string", vs, ve));
        }
        if matches!(op, CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le) && matches!(value, Value::String(_)) {
            return Err(err("ordering comparisons need a number, duration or byte size", vs, ve));
        }
        Ok(FieldExpr::Cmp { name, op, value, text })
    }
}

/// Parses query text. An empty query matches everything.
pub fn parse(src: &str) -> Result<Query, ParseError> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    p.query()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_query() {
        let q = parse(r#"{source="app.log", level=~"error|warn", file!~".*debug.*"} |= "timeout" or "refused" !~ "healthcheck" | json | status >= 500 and duration > 2s"#).unwrap();
        assert_eq!(q.selector.len(), 3);
        assert_eq!(q.stages.len(), 4);
        assert_eq!(
            q.stages[0],
            Stage::Line {
                op: LineOp::Contains,
                values: vec!["timeout".into(), "refused".into()]
            }
        );
        match &q.stages[3] {
            Stage::Filter {
                expr: FieldExpr::And { right, .. },
            } => match right.as_ref() {
                FieldExpr::Cmp {
                    value: Value::Duration(ms),
                    ..
                } => assert_eq!(*ms, 2000.0),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
        // Round trip through Display.
        let text = q.to_string();
        assert_eq!(parse(&text).unwrap(), q);
    }

    #[test]
    fn empty_and_selector_only() {
        assert_eq!(parse("").unwrap(), Query::default());
        assert_eq!(parse("{}").unwrap(), Query::default());
        assert_eq!(parse(r#"|= "x""#).unwrap().stages.len(), 1);
    }

    #[test]
    fn errors_have_spans() {
        let e = parse(r#"{level="error" |= "x""#).unwrap_err();
        assert_eq!(e.start, 15);
        let e = parse(r#"{level=~"("}"#).unwrap_err();
        assert!(e.message.contains("invalid regex"));
        let e = parse(r#"{level="x"} | line_format "{{.a}}""#).unwrap_err();
        assert!(e.message.contains("not supported"));
        let e = parse(r#"{level="x"} |= "unterminated"#).unwrap_err();
        assert!(e.message.contains("unterminated"));
    }

    #[test]
    fn regex_backticks_and_logger_prefix() {
        let q = parse(r#"{logger=~"com\.acme\.payment.*"} |~ `\d{3}ms`"#).unwrap();
        assert_eq!(q.selector[0].value, r"com\.acme\.payment.*");
        assert_eq!(
            q.stages[0],
            Stage::Line {
                op: LineOp::Re,
                values: vec![r"\d{3}ms".into()]
            }
        );
    }

    #[test]
    fn units() {
        assert_eq!(parse_duration_ms("1h30m"), Some(5_400_000.0));
        assert_eq!(parse_duration_ms("150ms"), Some(150.0));
        assert_eq!(parse_bytes("10MB"), Some(1e7));
        assert_eq!(parse_bytes("1KiB"), Some(1024.0));
    }
}
