//! Label and field values for the sidebar and editor autocomplete.

use crate::model::Level;
use crate::query::eval::STREAM_LABELS;
use crate::store::Workspace;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelInfo {
    pub name: String,
    /// Stream labels prune whole chunks; others are checked per event.
    pub stream: bool,
    pub values: Vec<(String, u64)>,
    pub truncated: bool,
}

/// Labels with value counts for the whole folder, stream labels first.
pub fn labels(ws: &Workspace) -> Vec<LabelInfo> {
    let mut source: HashMap<&str, u64> = HashMap::new();
    let mut file: HashMap<&str, u64> = HashMap::new();
    let mut level: HashMap<&str, u64> = HashMap::new();
    for c in &ws.meta.chunks {
        let f = ws.file(c.file);
        *source.entry(&f.file.source).or_default() += c.count as u64;
        *file.entry(&f.file.rel).or_default() += c.count as u64;
        *level.entry(Level::from_u8(c.level).as_str()).or_default() += c.count as u64;
    }
    let sorted = |m: HashMap<&str, u64>| {
        let mut v: Vec<(String, u64)> = m.into_iter().map(|(k, c)| (k.to_string(), c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    };
    let mut out: Vec<LabelInfo> = [("level", level), ("source", source), ("file", file)]
        .into_iter()
        .map(|(name, m)| LabelInfo {
            name: name.to_string(),
            stream: true,
            values: sorted(m),
            truncated: false,
        })
        .collect();
    let mut fields: Vec<&String> = ws
        .meta
        .stats
        .fields
        .keys()
        .filter(|k| !STREAM_LABELS.contains(&k.as_str()))
        .collect();
    // Well-known log4j fields first, then by number of events that carry the field.
    let rank = |k: &str| match k {
        "logger" => 0,
        "exception" => 1,
        "thread" => 2,
        _ => 3,
    };
    let total = |k: &str| ws.meta.stats.fields[k].values.iter().map(|v| v.1).sum::<u64>();
    fields.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| total(b).cmp(&total(a)))
            .then_with(|| a.cmp(b))
    });
    for k in fields {
        let vc = &ws.meta.stats.fields[k];
        out.push(LabelInfo {
            name: k.clone(),
            stream: false,
            values: vc.values.clone(),
            truncated: vc.truncated,
        });
    }
    out
}
