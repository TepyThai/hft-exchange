//! Week 22: a parser for the TOML subset the exchange configuration uses.
//!
//! Supported (everything else is a syntax error with a line number):
//!
//! * comments (`# …` to end of line) and blank lines;
//! * `[table]` and `[[array-of-tables]]` headers (bare names, dots allowed:
//!   `[a.b]` is just the name `"a.b"`);
//! * `key = value` with bare keys (`A-Z a-z 0-9 _ -`);
//! * values: basic strings `"…"` (escapes `\"`, `\\`, `\n`, `\t`, `\r`,
//!   `\uXXXX`), literal strings `'…'`, integers (`-42`, `1_000_000`,
//!   `0x1f`), booleans, and arrays `[v, v, …]` of those, which may span
//!   lines and end with a trailing comma.
//!
//! Not supported on purpose: floats (nothing on the order path is a float,
//! so neither is any setting), dates, inline tables, dotted keys,
//! multi-line strings.
//!
//! The parser keeps going after an error (it skips to the next line), so
//! one pass reports every syntax error in the file. It also rejects a key
//! given twice in one table and a `[table]` header given twice — "last one
//! wins" hides merge mistakes. Unknown keys are the caller's business (it
//! knows the schema).

use std::fmt;

/// A parsed value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// A string (basic or literal).
    Str(String),
    /// A 64-bit signed integer.
    Int(i64),
    /// `true` / `false`.
    Bool(bool),
    /// `[v, …]`.
    Array(Vec<Value>),
}

impl Value {
    /// The value's type, as error messages name it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Str(_) => "string",
            Value::Int(_) => "integer",
            Value::Bool(_) => "boolean",
            Value::Array(_) => "array",
        }
    }
}

impl fmt::Display for Value {
    /// TOML syntax (strings quoted and escaped).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\t' => f.write_str("\\t")?,
                        '\r' => f.write_str("\\r")?,
                        c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Value::Int(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Array(items) => {
                f.write_str("[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
        }
    }
}

/// `key = value` and where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The key.
    pub key: String,
    /// The value.
    pub value: Value,
    /// 1-based line of the key (0 for entries not from a file, e.g. an
    /// environment override).
    pub line: usize,
}

/// One table: the root (name `""`), a `[name]` or one `[[name]]` element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// The header name (`""` for keys before any header).
    pub name: String,
    /// True for an `[[name]]` element.
    pub array: bool,
    /// 1-based line of the header (0 for the root).
    pub line: usize,
    /// Entries in file order (keys are unique within a table).
    pub entries: Vec<Entry>,
}

impl Table {
    /// The entry for `key`, if present. O(entries).
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

/// A parsed file: the root table first, then every header in file order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// Tables in file order; `tables[0]` is the root.
    pub tables: Vec<Table>,
}

impl Document {
    /// The single `[name]` table, if present.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name && !t.array)
    }

    /// Every `[[name]]` element, in file order.
    pub fn array<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Table> + 'a {
        self.tables
            .iter()
            .filter(move |t| t.name == name && t.array)
    }
}

/// A syntax error (or a duplicate key / table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line.
    pub line: usize,
    /// What is wrong.
    pub kind: ParseErrorKind,
}

/// What a [`ParseError`] is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// Malformed syntax.
    Syntax(String),
    /// A key given twice in one table (`first_line` is the first one).
    DuplicateKey {
        /// Table name (`""` for the root).
        table: String,
        /// The key.
        key: String,
        /// Line of the first occurrence.
        first_line: usize,
    },
    /// A `[name]` header given twice.
    DuplicateTable {
        /// The table name.
        name: String,
        /// Line of the first header.
        first_line: usize,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: ", self.line)?;
        match &self.kind {
            ParseErrorKind::Syntax(msg) => f.write_str(msg),
            ParseErrorKind::DuplicateKey {
                table,
                key,
                first_line,
            } => {
                write!(
                    f,
                    "duplicate key `{key}` in [{table}] (first on line {first_line})"
                )
            }
            ParseErrorKind::DuplicateTable { name, first_line } => {
                write!(f, "duplicate table [{name}] (first on line {first_line})")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses `text`. Returns the document, or every error found (in line
/// order). O(len); allocates the document.
pub fn parse(text: &str) -> Result<Document, Vec<ParseError>> {
    let (doc, errors) = parse_partial(text);
    if errors.is_empty() {
        Ok(doc)
    } else {
        Err(errors)
    }
}

/// Parses as much of `text` as it can: every line that parses is in the
/// document, every line that does not is an error (in line order). Lets a
/// validator report schema errors in the rest of the file along with the
/// syntax errors.
// @student w22
pub fn parse_partial(text: &str) -> (Document, Vec<ParseError>) {
    let mut p = Parser {
        src: text.as_bytes(),
        pos: 0,
        line: 1,
        errors: Vec::new(),
    };
    let mut doc = Document {
        tables: vec![Table {
            name: String::new(),
            array: false,
            line: 0,
            entries: Vec::new(),
        }],
    };
    loop {
        p.skip_blank();
        if p.at_end() {
            break;
        }
        let line = p.line;
        let result = if p.peek() == Some(b'[') {
            p.header(&mut doc)
        } else {
            p.key_value().and_then(|entry| {
                let table = doc.tables.last_mut().expect("root table");
                if let Some(first) = table.get(&entry.key) {
                    return Err(ParseError {
                        line,
                        kind: ParseErrorKind::DuplicateKey {
                            table: table.name.clone(),
                            key: entry.key.clone(),
                            first_line: first.line,
                        },
                    });
                }
                table.entries.push(entry);
                Ok(())
            })
        };
        match result.and_then(|()| p.end_of_line()) {
            Ok(()) => {}
            Err(e) => {
                p.errors.push(e);
                p.skip_line();
            }
        }
    }
    (doc, p.errors)
}

// @student-begin w22
struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    errors: Vec<ParseError>,
}

fn is_bare(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

impl Parser<'_> {
    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn err(&self, msg: impl Into<String>) -> ParseError {
        ParseError {
            line: self.line,
            kind: ParseErrorKind::Syntax(msg.into()),
        }
    }

    /// Spaces and tabs on the current line.
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.pos += 1;
            }
        }
    }

    /// Whitespace, comments and newlines (between items, and inside
    /// arrays).
    fn skip_blank(&mut self) {
        loop {
            self.skip_ws();
            self.skip_comment();
            match self.peek() {
                Some(b'\n') => {
                    self.bump();
                }
                Some(b'\r') if self.src.get(self.pos + 1) == Some(&b'\n') => {
                    self.pos += 1;
                }
                _ => return,
            }
        }
    }

    /// Recovery: drop the rest of the current line.
    fn skip_line(&mut self) {
        while let Some(c) = self.bump() {
            if c == b'\n' {
                return;
            }
        }
    }

    /// After an item: only whitespace and a comment may follow on the line.
    fn end_of_line(&mut self) -> Result<(), ParseError> {
        self.skip_ws();
        self.skip_comment();
        if self.peek() == Some(b'\r') {
            self.pos += 1;
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.bump();
                Ok(())
            }
            Some(c) => Err(self.err(format!("unexpected `{}` after the value", c as char))),
        }
    }

    fn bare_key(&mut self, what: &str, dots: bool) -> Result<String, ParseError> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| is_bare(c) || (dots && c == b'.'))
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err(format!("expected {what}")));
        }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned())
    }

    fn header(&mut self, doc: &mut Document) -> Result<(), ParseError> {
        let line = self.line;
        self.bump(); // '['
        let array = self.peek() == Some(b'[');
        if array {
            self.bump();
        }
        self.skip_ws();
        let name = self.bare_key("a table name", true)?;
        if name.starts_with('.') || name.ends_with('.') || name.contains("..") {
            return Err(self.err(format!("bad table name `{name}`")));
        }
        self.skip_ws();
        let close: &[u8] = if array { b"]]" } else { b"]" };
        if !self.src[self.pos..].starts_with(close) {
            return Err(self.err(format!(
                "expected `{}` after the table name",
                String::from_utf8_lossy(close)
            )));
        }
        self.pos += close.len();
        if !array {
            if let Some(first) = doc.tables.iter().find(|t| t.name == name) {
                return Err(ParseError {
                    line,
                    kind: ParseErrorKind::DuplicateTable {
                        name,
                        first_line: first.line,
                    },
                });
            }
        } else if doc.tables.iter().any(|t| t.name == name && !t.array) {
            return Err(self.err(format!("[[{name}]] after [{name}]")));
        }
        doc.tables.push(Table {
            name,
            array,
            line,
            entries: Vec::new(),
        });
        Ok(())
    }

    fn key_value(&mut self) -> Result<Entry, ParseError> {
        let line = self.line;
        let key = self.bare_key("a key", false)?;
        self.skip_ws();
        if self.peek() == Some(b'.') {
            return Err(self.err("dotted keys are not supported"));
        }
        if self.bump() != Some(b'=') {
            return Err(self.err(format!("expected `=` after `{key}`")));
        }
        self.skip_ws();
        let value = self.value()?;
        Ok(Entry { key, value, line })
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        match self.peek() {
            Some(b'"') => self.basic_string().map(Value::Str),
            Some(b'\'') => self.literal_string().map(Value::Str),
            Some(b'[') => self.array(),
            Some(b't' | b'f') => {
                let word = self.bare_key("a value", false)?;
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    _ => Err(self.err(format!("unknown value `{word}` (strings need quotes)"))),
                }
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' => self.integer(),
            Some(b'\n') | None => Err(self.err("missing value")),
            Some(_) => {
                let start = self.pos;
                while !matches!(
                    self.peek(),
                    None | Some(b'\n' | b' ' | b'\t' | b'#' | b',' | b']')
                ) {
                    self.pos += 1;
                }
                let word = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                Err(self.err(format!("unknown value `{word}` (strings need quotes)")))
            }
        }
    }

    fn integer(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+' | b'.'))
        {
            self.pos += 1;
        }
        let raw = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        parse_int(raw).map(Value::Int).ok_or_else(|| {
            if raw.contains('.') || (raw.contains(['e', 'E']) && !raw.starts_with("0x")) {
                self.err(format!(
                    "`{raw}`: floats are not supported (use integer ticks and lots)"
                ))
            } else {
                self.err(format!("`{raw}` is not a valid integer"))
            }
        })
    }

    fn basic_string(&mut self) -> Result<String, ParseError> {
        self.bump(); // '"'
        let mut out = Vec::new();
        loop {
            if matches!(self.peek(), None | Some(b'\n')) {
                return Err(self.err("unterminated string"));
            }
            match self.bump() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => break,
                Some(b'\\') => match self.bump() {
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'n') => out.push(b'\n'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b'u') => {
                        let hex = self.src.get(self.pos..self.pos + 4).unwrap_or(&[]);
                        let c = std::str::from_utf8(hex)
                            .ok()
                            .and_then(|h| u32::from_str_radix(h, 16).ok())
                            .and_then(char::from_u32)
                            .ok_or_else(|| self.err("bad \\u escape"))?;
                        self.pos += 4;
                        let mut b = [0; 4];
                        out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    }
                    _ => return Err(self.err("unknown escape in string")),
                },
                Some(c) => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("string is not valid UTF-8"))
    }

    fn literal_string(&mut self) -> Result<String, ParseError> {
        self.bump(); // '\''
        let start = self.pos;
        loop {
            match self.peek() {
                None | Some(b'\n') => return Err(self.err("unterminated string")),
                Some(b'\'') => break,
                Some(_) => self.pos += 1,
            }
        }
        let s = String::from_utf8(self.src[start..self.pos].to_vec())
            .map_err(|_| self.err("string is not valid UTF-8"));
        self.pos += 1;
        s
    }

    fn array(&mut self) -> Result<Value, ParseError> {
        self.bump(); // '['
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            if self.peek() == Some(b']') {
                self.bump();
                return Ok(Value::Array(items));
            }
            if self.at_end() {
                return Err(self.err("unterminated array"));
            }
            items.push(self.value()?);
            self.skip_blank();
            match self.bump() {
                Some(b',') => {}
                Some(b']') => return Ok(Value::Array(items)),
                _ => return Err(self.err("expected `,` or `]` in array")),
            }
        }
    }
}
// @student-end

/// Parses a TOML integer literal: optional sign, decimal digits with single
/// underscores between digits, or `0x` hex. `None` if malformed or out of
/// `i64` range. Also used for environment overrides.
// @student w22
pub fn parse_int(raw: &str) -> Option<i64> {
    let (neg, body) = match raw.as_bytes().first()? {
        b'-' => (true, &raw[1..]),
        b'+' => (false, &raw[1..]),
        _ => (false, raw),
    };
    let (digits, radix) = match body.strip_prefix("0x") {
        Some(hex) if !neg => (hex, 16),
        Some(_) => return None,
        None => (body, 10),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    if radix == 10 && digits.len() > 1 && digits.starts_with('0') {
        return None; // TOML forbids leading zeros
    }
    let clean: String = digits.chars().filter(|&c| c != '_').collect();
    let magnitude = u64::from_str_radix(&clean, radix).ok()?;
    if neg {
        0i64.checked_sub_unsigned(magnitude)
    } else {
        i64::try_from(magnitude).ok()
    }
}
