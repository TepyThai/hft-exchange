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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
#[allow(unused_variables)]
pub fn parse_partial(text: &str) -> (Document, Vec<ParseError>) {
    todo!("w22: parse_partial")
}

// TODO(w22): implement — see the week guide on tepy.dev/hft.

/// Parses a TOML integer literal: optional sign, decimal digits with single
/// underscores between digits, or `0x` hex. `None` if malformed or out of
/// `i64` range. Also used for environment overrides.
#[allow(unused_variables)]
pub fn parse_int(raw: &str) -> Option<i64> {
    todo!("w22: parse_int")
}
