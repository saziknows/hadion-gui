//! Small helpers over `serde_yaml::Value` for workflow files, and a line
//! finder (serde_yaml values carry no positions, so findings locate their
//! key in the text instead).

use serde_yaml::{Mapping, Value};

/// Strip YAML tags (`!!str x`).
pub fn untag(v: &Value) -> &Value {
    match v {
        Value::Tagged(t) => untag(&t.value),
        other => other,
    }
}

/// `m[key]`, untagged.
pub fn get<'a>(m: &'a Mapping, key: &str) -> Option<&'a Value> {
    m.get(key).map(untag)
}

/// The `on` key. YAML 1.1 reads a bare `on` as the boolean `true`, so a
/// parser following that schema stores the triggers under `true`; accept
/// both spellings.
pub fn get_on(m: &Mapping) -> Option<&Value> {
    get(m, "on").or_else(|| m.get(Value::Bool(true)).map(untag))
}

/// Whether the mapping has an `on` key under either spelling.
pub fn has_on(m: &Mapping) -> bool {
    m.contains_key("on") || m.contains_key(Value::Bool(true))
}

/// A string, number or boolean as text.
pub fn scalar(v: &Value) -> Option<String> {
    match untag(v) {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn mapping(v: &Value) -> Option<&Mapping> {
    untag(v).as_mapping()
}

pub fn sequence(v: &Value) -> Option<&Vec<Value>> {
    untag(v).as_sequence()
}

/// A string, a sequence of strings, or the keys of a mapping.
pub fn str_list(v: &Value) -> Vec<String> {
    match untag(v) {
        Value::Sequence(seq) => seq.iter().filter_map(scalar).collect(),
        Value::Mapping(m) => m.keys().filter_map(key_name).collect(),
        Value::Null => Vec::new(),
        other => scalar(other).into_iter().collect(),
    }
}

/// A mapping key as text (`true` for the YAML 1.1 `on`).
pub fn key_name(k: &Value) -> Option<String> {
    match untag(k) {
        Value::Bool(true) => Some("on".to_owned()),
        other => scalar(other),
    }
}

pub fn is_true(v: &Value) -> bool {
    match untag(v) {
        Value::Bool(b) => *b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Where keys and values sit in the text, for findings.
pub struct Lines<'a> {
    lines: Vec<&'a str>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines().collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 1-based text of a line.
    pub fn text(&self, line: u32) -> &str {
        self.lines
            .get((line as usize).saturating_sub(1))
            .copied()
            .unwrap_or("")
    }

    /// Indentation of a line in spaces.
    pub fn indent(&self, line: u32) -> usize {
        let t = self.text(line);
        t.len() - t.trim_start().len()
    }

    /// The first line at or after `from` (1-based) declaring `key:`
    /// (optionally as a list item `- key:`), or `None`.
    pub fn key(&self, key: &str, from: u32) -> Option<u32> {
        let start = (from.max(1) - 1) as usize;
        for (i, l) in self.lines.iter().enumerate().skip(start) {
            let t = l.trim_start();
            let t = t.strip_prefix("- ").map(str::trim_start).unwrap_or(t);
            let t = t
                .strip_prefix('"')
                .and_then(|r| r.strip_prefix(key).and_then(|r| r.strip_prefix('"')))
                .or_else(|| {
                    t.strip_prefix('\'')
                        .and_then(|r| r.strip_prefix(key).and_then(|r| r.strip_prefix('\'')))
                })
                .or_else(|| t.strip_prefix(key));
            if let Some(rest) = t
                && rest.trim_start().starts_with(':')
            {
                return Some(i as u32 + 1);
            }
        }
        None
    }

    /// A key that belongs to the block opened at `parent` (indented
    /// deeper than it, before the block ends).
    pub fn key_in(&self, key: &str, parent: u32) -> Option<u32> {
        let base = self.indent(parent);
        let mut line = parent + 1;
        while (line as usize) <= self.lines.len() {
            let t = self.text(line);
            if !t.trim().is_empty() && !t.trim_start().starts_with('#') && self.indent(line) <= base
            {
                return None;
            }
            if let Some(found) = self.key(key, line)
                && found == line
            {
                return Some(line);
            }
            line += 1;
        }
        None
    }

    /// The first line at or after `from` containing `needle`.
    pub fn find(&self, needle: &str, from: u32) -> Option<u32> {
        if needle.is_empty() {
            return None;
        }
        let start = (from.max(1) - 1) as usize;
        self.lines
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, l)| l.contains(needle))
            .map(|(i, _)| i as u32 + 1)
    }

    /// The end (last line) of the block opened at `start`.
    pub fn block_end(&self, start: u32) -> u32 {
        let base = self.indent(start);
        let mut last = start;
        let mut line = start + 1;
        while (line as usize) <= self.lines.len() {
            let t = self.text(line);
            if !t.trim().is_empty() && !t.trim_start().starts_with('#') {
                if self.indent(line) <= base && !t.trim_start().starts_with("- ") {
                    break;
                }
                if self.indent(line) < base {
                    break;
                }
                last = line;
            }
            line += 1;
        }
        last
    }

    /// Line of job `id` under the top-level `jobs:`.
    pub fn job(&self, id: &str) -> Option<u32> {
        let jobs = self.top("jobs")?;
        self.key_in(id, jobs)
    }

    /// A top-level key (no indentation).
    pub fn top(&self, key: &str) -> Option<u32> {
        let mut from = 1;
        while let Some(l) = self.key(key, from) {
            if self.indent(l) == 0 {
                return Some(l);
            }
            from = l + 1;
        }
        None
    }

    /// The `n`-th step (0-based) of the job at `job_line`: the n-th list item
    /// under its `steps:` key.
    pub fn step(&self, job_line: u32, n: usize) -> Option<u32> {
        let steps = self.key_in("steps", job_line)?;
        let end = self.block_end(steps);
        let mut item_indent: Option<usize> = None;
        let mut seen = 0usize;
        for line in steps + 1..=end {
            let t = self.text(line);
            let trimmed = t.trim_start();
            if !trimmed.starts_with("- ") && trimmed != "-" {
                continue;
            }
            let ind = self.indent(line);
            match item_indent {
                None => item_indent = Some(ind),
                Some(i) if i != ind => continue,
                _ => {}
            }
            if seen == n {
                return Some(line);
            }
            seen += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WF: &str = "name: CI\non:\n  push:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - name: Test\n        run: cargo test\n  lint:\n    runs-on: ubuntu-latest\n";

    #[test]
    fn finds_keys_jobs_and_steps() {
        let l = Lines::new(WF);
        assert_eq!(l.top("jobs"), Some(4));
        assert_eq!(l.job("build"), Some(5));
        assert_eq!(l.job("lint"), Some(11));
        assert_eq!(l.key_in("runs-on", 5), Some(6));
        assert_eq!(l.step(5, 0), Some(8));
        assert_eq!(l.step(5, 1), Some(9));
        assert_eq!(l.step(5, 2), None);
        assert_eq!(l.key_in("steps", 11), None);
        assert_eq!(l.find("cargo", 1), Some(10));
    }

    #[test]
    fn on_spellings() {
        let v: Value = serde_yaml::from_str("on: push\n").unwrap();
        assert!(get_on(v.as_mapping().unwrap()).is_some());
        let mut m = Mapping::new();
        m.insert(Value::Bool(true), Value::String("push".into()));
        assert_eq!(scalar(get_on(&m).unwrap()).as_deref(), Some("push"));
        assert!(has_on(&m));
        assert_eq!(key_name(&Value::Bool(true)).as_deref(), Some("on"));
    }
}
