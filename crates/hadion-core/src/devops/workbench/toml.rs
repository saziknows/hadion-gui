//! A small TOML reader for task discovery: `Cargo.toml`,
//! `.cargo/config.toml` and `pyproject.toml`. It keeps the order of keys
//! and the line each key was defined on, which a task list needs ("source
//! file and line"). Covers tables, arrays of tables, dotted keys, every
//! string form, arrays and inline tables; dates and other scalars are kept
//! as their raw text.

/// A parsed value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Dates, times and anything else, as written.
    Other(String),
    Array(Vec<Value>),
    Table(Table),
}

/// Key/value pairs in definition order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    /// 1-based line of the key (or of the `[table]` header).
    pub line: u32,
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Strings of an array (non-strings skipped).
    pub fn strings(&self) -> Vec<String> {
        self.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Table {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|e| e.key == key).map(|e| &e.value)
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// A nested value by path: `get_path(&["tool", "poe", "tasks"])`.
    pub fn get_path(&self, path: &[&str]) -> Option<&Value> {
        let (first, rest) = path.split_first()?;
        let v = self.get(first)?;
        if rest.is_empty() {
            return Some(v);
        }
        v.as_table()?.get_path(rest)
    }

    pub fn table(&self, path: &[&str]) -> Option<&Table> {
        self.get_path(path)?.as_table()
    }

    pub fn str_at(&self, path: &[&str]) -> Option<&str> {
        self.get_path(path)?.as_str()
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .find(|e| e.key == key)
            .map(|e| &mut e.value)
    }
}

/// Parse a TOML document.
pub fn parse(text: &str) -> Result<Table, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        line: 1,
    };
    let mut root = Table::default();
    let mut current: Vec<String> = Vec::new();
    loop {
        p.skip_trivia(true);
        if p.eof() {
            break;
        }
        if p.peek() == Some(b'[') {
            let line = p.line;
            let array = p.peek_at(1) == Some(b'[');
            p.i += if array { 2 } else { 1 };
            p.skip_spaces();
            let path = p.key_path()?;
            p.skip_spaces();
            p.expect(b']')?;
            if array {
                p.expect(b']')?;
            }
            p.end_of_line()?;
            if path.is_empty() {
                return Err(p.err("empty table name"));
            }
            if array {
                let (last, parents) = path.split_last().expect("checked non-empty");
                let parent = table_at(&mut root, parents, line);
                match parent.get_mut(last) {
                    Some(Value::Array(items)) => items.push(Value::Table(Table::default())),
                    Some(_) => return Err(p.err(&format!("{last} is not an array of tables"))),
                    None => parent.entries.push(Entry {
                        key: last.clone(),
                        value: Value::Array(vec![Value::Table(Table::default())]),
                        line,
                    }),
                }
            } else {
                table_at(&mut root, &path, line);
            }
            current = path;
            continue;
        }
        let line = p.line;
        let path = p.key_path()?;
        if path.is_empty() {
            return Err(p.err("expected a key"));
        }
        p.skip_spaces();
        p.expect(b'=')?;
        p.skip_spaces();
        let value = p.value()?;
        p.end_of_line()?;
        let table = table_at(&mut root, &current, line);
        insert(table, &path, value, line);
    }
    Ok(root)
}

/// The table at `path`, created on the way; arrays of tables resolve to
/// their last element.
fn table_at<'a>(root: &'a mut Table, path: &[String], line: u32) -> &'a mut Table {
    let mut t = root;
    for seg in path {
        let pos = match t.entries.iter().position(|e| &e.key == seg) {
            Some(pos) => pos,
            None => {
                t.entries.push(Entry {
                    key: seg.clone(),
                    value: Value::Table(Table::default()),
                    line,
                });
                t.entries.len() - 1
            }
        };
        let v = &mut t.entries[pos].value;
        if !matches!(v, Value::Table(_) | Value::Array(_)) {
            *v = Value::Table(Table::default());
        }
        t = match v {
            Value::Table(inner) => inner,
            Value::Array(items) => {
                if !matches!(items.last(), Some(Value::Table(_))) {
                    items.push(Value::Table(Table::default()));
                }
                match items.last_mut() {
                    Some(Value::Table(inner)) => inner,
                    _ => unreachable!("pushed a table above"),
                }
            }
            _ => unreachable!("normalised above"),
        };
    }
    t
}

fn insert(table: &mut Table, path: &[String], value: Value, line: u32) {
    let (last, parents) = path.split_last().expect("non-empty key path");
    let t = table_at(table, parents, line);
    match t.entries.iter_mut().find(|e| &e.key == last) {
        Some(e) => {
            e.value = value;
            e.line = line;
        }
        None => t.entries.push(Entry {
            key: last.clone(),
            value,
            line,
        }),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    line: u32,
}

impl Parser<'_> {
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.s.get(self.i + n).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn err(&self, msg: &str) -> String {
        format!("line {}: {msg}", self.line)
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.bump();
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", c as char)))
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }

    /// Spaces, comments and (when `newlines`) line breaks.
    fn skip_trivia(&mut self, newlines: bool) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r') => self.i += 1,
                Some(b'\n') if newlines => {
                    self.bump();
                }
                Some(b'#') => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_trivia(false);
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.bump();
                Ok(())
            }
            Some(c) => Err(self.err(&format!("unexpected '{}'", c as char))),
        }
    }

    fn key_path(&mut self) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        loop {
            self.skip_spaces();
            let key = match self.peek() {
                Some(b'"') => {
                    self.bump();
                    self.basic_string()?
                }
                Some(b'\'') => {
                    self.bump();
                    self.literal_string()?
                }
                _ => {
                    let start = self.i;
                    while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    {
                        self.i += 1;
                    }
                    if start == self.i {
                        return Err(self.err("expected a key"));
                    }
                    String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
                }
            };
            path.push(key);
            self.skip_spaces();
            if self.peek() == Some(b'.') {
                self.i += 1;
                continue;
            }
            return Ok(path);
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => {
                if self.peek_at(1) == Some(b'"') && self.peek_at(2) == Some(b'"') {
                    self.i += 3;
                    self.multiline_basic().map(Value::Str)
                } else {
                    self.bump();
                    self.basic_string().map(Value::Str)
                }
            }
            Some(b'\'') => {
                if self.peek_at(1) == Some(b'\'') && self.peek_at(2) == Some(b'\'') {
                    self.i += 3;
                    self.multiline_literal().map(Value::Str)
                } else {
                    self.bump();
                    self.literal_string().map(Value::Str)
                }
            }
            Some(b'[') => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    self.skip_trivia(true);
                    if self.peek() == Some(b']') {
                        self.bump();
                        break;
                    }
                    items.push(self.value()?);
                    self.skip_trivia(true);
                    match self.peek() {
                        Some(b',') => {
                            self.bump();
                        }
                        Some(b']') => {
                            self.bump();
                            break;
                        }
                        _ => return Err(self.err("expected ',' or ']' in an array")),
                    }
                }
                Ok(Value::Array(items))
            }
            Some(b'{') => {
                self.bump();
                let mut t = Table::default();
                loop {
                    self.skip_trivia(true);
                    if self.peek() == Some(b'}') {
                        self.bump();
                        break;
                    }
                    let line = self.line;
                    let path = self.key_path()?;
                    self.skip_spaces();
                    self.expect(b'=')?;
                    self.skip_spaces();
                    let v = self.value()?;
                    insert(&mut t, &path, v, line);
                    self.skip_trivia(true);
                    match self.peek() {
                        Some(b',') => {
                            self.bump();
                        }
                        Some(b'}') => {
                            self.bump();
                            break;
                        }
                        _ => return Err(self.err("expected ',' or '}' in an inline table")),
                    }
                }
                Ok(Value::Table(t))
            }
            Some(_) => {
                let start = self.i;
                while !matches!(
                    self.peek(),
                    None | Some(b',' | b']' | b'}' | b'#' | b'\n' | b'\r')
                ) {
                    self.i += 1;
                }
                let raw = String::from_utf8_lossy(&self.s[start..self.i])
                    .trim()
                    .to_owned();
                if raw.is_empty() {
                    return Err(self.err("expected a value"));
                }
                Ok(scalar(&raw))
            }
            None => Err(self.err("expected a value")),
        }
    }

    fn escape(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let c = self.bump().ok_or_else(|| self.err("unterminated escape"))?;
        match c {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'e' => out.push(27),
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'u' | b'U' => {
                let n = if c == b'u' { 4 } else { 8 };
                let end = (self.i + n).min(self.s.len());
                let hex = String::from_utf8_lossy(&self.s[self.i..end]).into_owned();
                self.i = end;
                let ch = u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .unwrap_or('\u{fffd}');
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            b'\n' | b' ' | b'\t' | b'\r' => {
                // Line-ending backslash (multi-line strings): trim the
                // whitespace up to the next non-blank character.
                while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                    self.bump();
                }
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
        Ok(())
    }

    fn basic_string(&mut self) -> Result<String, String> {
        let mut out = Vec::new();
        loop {
            // Checked before consuming, so the error names this line.
            if matches!(self.peek(), None | Some(b'\n')) {
                return Err(self.err("unterminated string"));
            }
            match self.bump() {
                None | Some(b'\n') => return Err(self.err("unterminated string")),
                Some(b'"') => break,
                Some(b'\\') => self.escape(&mut out)?,
                Some(c) => out.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn literal_string(&mut self) -> Result<String, String> {
        let start = self.i;
        loop {
            if matches!(self.peek(), None | Some(b'\n')) {
                return Err(self.err("unterminated string"));
            }
            match self.bump() {
                None | Some(b'\n') => return Err(self.err("unterminated string")),
                Some(b'\'') => break,
                Some(_) => {}
            }
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i - 1]).into_owned())
    }

    fn multiline_basic(&mut self) -> Result<String, String> {
        // A newline right after the opening quotes is not part of the value.
        if self.peek() == Some(b'\r') {
            self.i += 1;
        }
        if self.peek() == Some(b'\n') {
            self.bump();
        }
        let mut out = Vec::new();
        loop {
            if self.peek() == Some(b'"')
                && self.peek_at(1) == Some(b'"')
                && self.peek_at(2) == Some(b'"')
            {
                self.i += 3;
                // Up to two quotes may close the content itself.
                while self.peek() == Some(b'"') {
                    out.push(b'"');
                    self.i += 1;
                }
                break;
            }
            match self.bump() {
                None => return Err(self.err("unterminated multi-line string")),
                Some(b'\\') => self.escape(&mut out)?,
                Some(b'\r') => {}
                Some(c) => out.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn multiline_literal(&mut self) -> Result<String, String> {
        if self.peek() == Some(b'\r') {
            self.i += 1;
        }
        if self.peek() == Some(b'\n') {
            self.bump();
        }
        let mut out = Vec::new();
        loop {
            if self.peek() == Some(b'\'')
                && self.peek_at(1) == Some(b'\'')
                && self.peek_at(2) == Some(b'\'')
            {
                self.i += 3;
                while self.peek() == Some(b'\'') {
                    out.push(b'\'');
                    self.i += 1;
                }
                break;
            }
            match self.bump() {
                None => return Err(self.err("unterminated multi-line string")),
                Some(b'\r') => {}
                Some(c) => out.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

fn scalar(raw: &str) -> Value {
    match raw {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    let digits = raw.replace('_', "");
    if let Ok(n) = digits.parse::<i64>() {
        return Value::Int(n);
    }
    if let Some(hex) = digits.strip_prefix("0x")
        && let Ok(n) = i64::from_str_radix(hex, 16)
    {
        return Value::Int(n);
    }
    if digits.contains(['.', 'e', 'E'])
        && !digits.contains(':')
        && let Ok(f) = digits.parse::<f64>()
    {
        return Value::Float(f);
    }
    Value::Other(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_manifest() {
        let text = r#"
# A comment
[package]
name = "hadion-core"   # trailing comment
version.workspace = true
edition = '2024'

[dependencies]
serde = { version = "1", features = ["derive"] }

[[bin]]
name = "first"
path = "src/bin/first.rs"

[[bin]]
name = "second"

[workspace]
members = [
    "crates/a",   # one
    "crates/*",
]
"#;
        let t = parse(text).unwrap();
        assert_eq!(t.str_at(&["package", "name"]), Some("hadion-core"));
        assert_eq!(t.str_at(&["package", "edition"]), Some("2024"));
        assert_eq!(
            t.get_path(&["package", "version", "workspace"]),
            Some(&Value::Bool(true))
        );
        assert_eq!(t.entry("package").unwrap().line, 3);
        let bins = t.get("bin").unwrap().as_array().unwrap();
        assert_eq!(bins.len(), 2);
        assert_eq!(
            bins[1].as_table().unwrap().str_at(&["name"]),
            Some("second")
        );
        assert_eq!(bins[0].as_table().unwrap().entry("name").unwrap().line, 12);
        assert_eq!(
            t.get_path(&["workspace", "members"]).unwrap().strings(),
            vec!["crates/a", "crates/*"]
        );
        let serde = t.table(&["dependencies", "serde"]).unwrap();
        assert_eq!(serde.get("features").unwrap().strings(), vec!["derive"]);
    }

    #[test]
    fn strings_and_scalars() {
        let text = "a = \"tab\\tquote\\\" \\u00e9\"\nb = 'C:\\path'\nc = \"\"\"\nline one\nline two\"\"\"\nd = '''\nraw \\n'''\ne = 1_000\nf = 3.5\ng = 1979-05-27\nh = [1, [2, 3], { x = 'y' }]\n\"quoted key\" = 1\n";
        let t = parse(text).unwrap();
        assert_eq!(t.str_at(&["a"]), Some("tab\tquote\" é"));
        assert_eq!(t.str_at(&["b"]), Some("C:\\path"));
        assert_eq!(t.str_at(&["c"]), Some("line one\nline two"));
        assert_eq!(t.str_at(&["d"]), Some("raw \\n"));
        assert_eq!(t.get("e"), Some(&Value::Int(1000)));
        assert_eq!(t.get("f"), Some(&Value::Float(3.5)));
        assert_eq!(t.get("g"), Some(&Value::Other("1979-05-27".into())));
        let h = t.get("h").unwrap().as_array().unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].as_table().unwrap().str_at(&["x"]), Some("y"));
        assert_eq!(t.get("quoted key"), Some(&Value::Int(1)));
    }

    #[test]
    fn pyproject_and_aliases() {
        let text = r#"
[project]
name = "demo"

[project.scripts]
demo-cli = "demo.cli:main"

[tool.poe.tasks]
lint = "ruff check ."
test = { cmd = "pytest -q", help = "Run the tests" }
all = ["lint", "test"]

[tool.poe.tasks.serve]
cmd = "uvicorn app:app"
help = "Dev server"
"#;
        let t = parse(text).unwrap();
        assert_eq!(
            t.str_at(&["project", "scripts", "demo-cli"]),
            Some("demo.cli:main")
        );
        let tasks = t.table(&["tool", "poe", "tasks"]).unwrap();
        let keys: Vec<&str> = tasks.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["lint", "test", "all", "serve"]);
        assert_eq!(tasks.entry("test").unwrap().line, 10);
        assert_eq!(
            tasks.table(&["serve"]).unwrap().str_at(&["help"]),
            Some("Dev server")
        );
    }

    #[test]
    fn errors_name_the_line() {
        let e = parse("[package]\nname = \"x\nother = 1\n").unwrap_err();
        assert!(e.starts_with("line 2"), "{e}");
        assert!(parse("a = ").is_err());
    }
}
