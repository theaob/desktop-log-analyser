//! Syntax tree of the LogQL subset. The visual builder edits this tree and the text editor
//! edits its text; `Display` turns a tree back into canonical query text.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchOp {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = "=~")]
    Re,
    #[serde(rename = "!~")]
    Nre,
}

impl MatchOp {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchOp::Eq => "=",
            MatchOp::Ne => "!=",
            MatchOp::Re => "=~",
            MatchOp::Nre => "!~",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Matcher {
    pub name: String,
    pub op: MatchOp,
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineOp {
    #[serde(rename = "|=")]
    Contains,
    #[serde(rename = "!=")]
    NotContains,
    #[serde(rename = "|~")]
    Re,
    #[serde(rename = "!~")]
    NotRe,
}

impl LineOp {
    pub fn as_str(self) -> &'static str {
        match self {
            LineOp::Contains => "|=",
            LineOp::NotContains => "!=",
            LineOp::Re => "|~",
            LineOp::NotRe => "!~",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = "=~")]
    Re,
    #[serde(rename = "!~")]
    Nre,
}

impl CmpOp {
    pub fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Re => "=~",
            CmpOp::Nre => "!~",
        }
    }
}

/// A literal on the right side of a field comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum Value {
    String(String),
    Number(f64),
    /// Milliseconds (fractional allowed).
    Duration(f64),
    Bytes(f64),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FieldExpr {
    Cmp {
        name: String,
        op: CmpOp,
        value: Value,
        #[serde(default)]
        text: String,
    },
    And {
        left: Box<FieldExpr>,
        right: Box<FieldExpr>,
    },
    Or {
        left: Box<FieldExpr>,
        right: Box<FieldExpr>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Stage {
    /// `|= "a"`, optionally with `or "b"` alternatives.
    Line {
        op: LineOp,
        values: Vec<String>,
    },
    Json,
    Logfmt,
    Regexp {
        pattern: String,
    },
    Filter {
        expr: FieldExpr,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub selector: Vec<Matcher>,
    pub stages: Vec<Stage>,
}

pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Quotes a regex with backticks when it contains backslashes, so users don't have to
/// double-escape.
fn quote_regex(s: &str) -> String {
    if s.contains('\\') && !s.contains('`') {
        format!("`{s}`")
    } else {
        quote(s)
    }
}

impl fmt::Display for FieldExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldExpr::Cmp { name, op, value, text } => {
                let v = match value {
                    Value::String(s) if matches!(op, CmpOp::Re | CmpOp::Nre) => quote_regex(s),
                    Value::String(s) => quote(s),
                    _ if !text.is_empty() => text.clone(),
                    Value::Number(n) => format!("{n}"),
                    Value::Duration(ms) => format!("{ms}ms"),
                    Value::Bytes(b) => format!("{b}B"),
                };
                write!(f, "{name} {} {v}", op.as_str())
            }
            FieldExpr::And { left, right } => write!(f, "{left} and {right}"),
            FieldExpr::Or { left, right } => {
                // Parenthesise so `a or b` inside `and` keeps its meaning.
                write!(f, "({left} or {right})")
            }
        }
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        for (i, m) in self.selector.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            let v = if matches!(m.op, MatchOp::Re | MatchOp::Nre) {
                quote_regex(&m.value)
            } else {
                quote(&m.value)
            };
            write!(f, "{}{}{}", m.name, m.op.as_str(), v)?;
        }
        write!(f, "}}")?;
        for s in &self.stages {
            match s {
                Stage::Line { op, values } => {
                    let regex = matches!(op, LineOp::Re | LineOp::NotRe);
                    let vals: Vec<String> = values
                        .iter()
                        .map(|v| if regex { quote_regex(v) } else { quote(v) })
                        .collect();
                    write!(f, " {} {}", op.as_str(), vals.join(" or "))?;
                }
                Stage::Json => write!(f, " | json")?,
                Stage::Logfmt => write!(f, " | logfmt")?,
                Stage::Regexp { pattern } => write!(f, " | regexp {}", quote_regex(pattern))?,
                Stage::Filter { expr } => {
                    let s = expr.to_string();
                    // A top-level `or` doesn't need the parentheses Display adds.
                    let s = if matches!(expr, FieldExpr::Or { .. }) {
                        s[1..s.len() - 1].to_string()
                    } else {
                        s
                    };
                    write!(f, " | {s}")?
                }
            }
        }
        Ok(())
    }
}
