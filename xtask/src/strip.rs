//! Learner template generation.
//!
//! The `reference` branch is the source of truth: a complete, tested
//! implementation. `cargo xtask strip --out DIR` copies the workspace and
//! removes the parts a learner writes, marked in the source with:
//!
//! * `// @student wNN` above a `fn` — its body becomes `todo!("wNN: name")`
//! * `// @student wNN` above an `impl` block — every fn body inside does
//! * `// @student wNN` above a `struct` with named fields — the fields are
//!   replaced by a placeholder (the learner designs the representation)
//! * `// @student-begin wNN` … `// @student-end` — the region is removed
//!   (private helpers, internal types)
//!
//! Tests, benchmarks and everything unmarked are kept verbatim, so the
//! template compiles and every week's tests fail with a clear `todo!()`
//! until implemented. `--check` builds the result to prove it.

use crate::Args;
use std::path::{Path, PathBuf};

pub fn cmd_strip(args: &Args) -> Result<bool, String> {
    let out = args.value("out").ok_or("strip needs --out DIR")?;
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map_err(|_| "run via `cargo xtask strip`")?
        .parent()
        .ok_or("no workspace root")?
        .to_path_buf();
    let out = PathBuf::from(out);
    if out.exists() {
        return Err(format!("{} already exists", out.display()));
    }
    copy_tree(&root, &out)?;
    let mut stripped = 0;
    for file in rust_sources(&out.join("crates")) {
        let src = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let (next, n) = strip_source(&src).map_err(|e| format!("{}: {e}", file.display()))?;
        if n > 0 {
            std::fs::write(&file, next).map_err(|e| e.to_string())?;
            stripped += n;
        }
    }
    println!("template written to {} ({stripped} items stripped)", out.display());
    if args.flag("check") {
        let status = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["check", "--workspace", "--all-targets", "--quiet"])
            .current_dir(&out)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("template does not compile".into());
        }
        println!("template compiles ✓");
    }
    Ok(true)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), "target" | ".git" | "hft-report.json") {
            continue;
        }
        let src = entry.path();
        let dst = to.join(name.as_ref());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // Only `src/` trees are stripped; tests/ and benches/ stay intact.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "tests" || name == "benches" {
                continue;
            }
            out.extend(rust_sources(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Rust-aware scanning

/// Index just past a string / char literal / comment starting at `i`, or
/// `i` itself if none starts there. Handles lifetimes (`'a`) vs chars.
fn skip_literal(b: &[u8], i: usize) -> usize {
    let c = b[i];
    let next = b.get(i + 1).copied().unwrap_or(0);
    if c == b'/' && next == b'/' {
        return b[i..].iter().position(|&x| x == b'\n').map_or(b.len(), |p| i + p);
    }
    if c == b'/' && next == b'*' {
        let mut depth = 1;
        let mut j = i + 2;
        while j + 1 < b.len() && depth > 0 {
            if b[j] == b'/' && b[j + 1] == b'*' {
                depth += 1;
                j += 2;
            } else if b[j] == b'*' && b[j + 1] == b'/' {
                depth -= 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        return j;
    }
    if c == b'r' && (next == b'"' || next == b'#') && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
        let mut j = i + 1;
        let mut hashes = 0;
        while j < b.len() && b[j] == b'#' {
            hashes += 1;
            j += 1;
        }
        if j < b.len() && b[j] == b'"' {
            j += 1;
            while j < b.len() {
                if b[j] == b'"' && b[j + 1..].iter().take(hashes).filter(|&&h| h == b'#').count() == hashes {
                    return j + 1 + hashes;
                }
                j += 1;
            }
            return b.len();
        }
        return i;
    }
    if c == b'"' {
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'"' => return j + 1,
                _ => j += 1,
            }
        }
        return b.len();
    }
    if c == b'\'' {
        // char literal: 'x', '\n', '\u{..}' — otherwise a lifetime.
        if next == b'\\' {
            let mut j = i + 2;
            while j < b.len() && b[j] != b'\'' {
                j += 1;
            }
            return j + 1;
        }
        // Multi-byte UTF-8 char or ASCII followed by closing quote.
        let mut j = i + 1;
        if j < b.len() {
            j += utf8_len(b[j]);
        }
        if b.get(j) == Some(&b'\'') {
            return j + 1;
        }
        return i;
    }
    i
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn match_brace(b: &[u8], open: usize) -> Result<usize, String> {
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        let j = skip_literal(b, i);
        if j != i {
            i = j;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Err("unbalanced braces".into())
}

/// First `{` at paren/bracket depth 0 at or after `from`, or a `;` first
/// (no body).
fn body_open(b: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    while i < b.len() {
        let j = skip_literal(b, i);
        if j != i {
            i = j;
            continue;
        }
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth == 0 => return Some(i),
            b';' if depth == 0 => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

#[derive(Debug, PartialEq)]
enum ItemKind {
    Fn(String),
    Impl,
    Struct { generics: String },
    Other,
}

fn item_kind(header: &str) -> ItemKind {
    let words: Vec<&str> = header.split(|c: char| c.is_whitespace() || c == '(' || c == '<').filter(|w| !w.is_empty()).collect();
    if let Some(pos) = words.iter().position(|w| *w == "fn") {
        return ItemKind::Fn(words.get(pos + 1).copied().unwrap_or("?").to_string());
    }
    if words.iter().take(3).any(|w| *w == "impl") {
        return ItemKind::Impl;
    }
    if let Some(pos) = header.find("struct ") {
        let after = &header[pos + 7..];
        let generics = after
            .find('<')
            .and_then(|start| {
                let mut depth = 0;
                for (k, ch) in after[start..].char_indices() {
                    match ch {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(after[start + 1..start + k].to_string());
                            }
                        }
                        _ => {}
                    }
                }
                None
            })
            .unwrap_or_default();
        return ItemKind::Struct { generics };
    }
    ItemKind::Other
}

/// `T: Bound, 'a, const N: usize` → `PhantomData<(T, &'a ())>` or `()`.
fn phantom_for(generics: &str) -> String {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for ch in generics.chars() {
        match ch {
            '<' => {
                depth += 1;
                cur.push(ch)
            }
            '>' => {
                depth -= 1;
                cur.push(ch)
            }
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    let mut tys = Vec::new();
    for p in parts {
        let p = p.trim();
        if p.starts_with("const ") {
            continue;
        }
        let name = p.split([':', '=']).next().unwrap_or("").trim();
        if name.starts_with('\'') {
            tys.push(format!("&{name} ()"));
        } else if !name.is_empty() {
            tys.push(name.to_string());
        }
    }
    if tys.is_empty() {
        "()".into()
    } else {
        format!("std::marker::PhantomData<({},)>", tys.join(", "))
    }
}

fn indent_of(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

/// Returns the stripped source and how many items were stripped.
pub fn strip_source(src: &str) -> Result<(String, usize), String> {
    let mut out = src.to_string();
    let mut count = 0;
    // Process from the bottom so earlier byte offsets stay valid.
    while let Some((marker_start, marker_end, week, region)) = last_marker(&out) {
        let b = out.as_bytes();
        if region {
            let end_tag = "// @student-end";
            let end = out[marker_end..].find(end_tag).ok_or("@student-begin without @student-end")? + marker_end;
            let line_end = out[end..].find('\n').map_or(out.len(), |p| end + p);
            let indent = indent_of(&out[out[..marker_start].rfind('\n').map_or(0, |p| p + 1)..]);
            let replacement = format!("// TODO({week}): implement — see the week guide on tepy.dev/hft.");
            let line_start = out[..marker_start].rfind('\n').map_or(0, |p| p + 1);
            out.replace_range(line_start..line_end, &format!("{indent}{replacement}"));
            count += 1;
            continue;
        }
        // Item header starts after the marker line, skipping attributes,
        // doc comments and blank lines.
        let mut pos = marker_end;
        loop {
            let line_end = out[pos..].find('\n').map_or(out.len(), |p| pos + p);
            let line = out[pos..line_end].trim();
            if line.is_empty() || line.starts_with("#[") || line.starts_with("///") || (line.starts_with("//") && !line.starts_with("// @student")) {
                pos = (line_end + 1).min(out.len());
                if pos >= out.len() {
                    return Err(format!("marker {week} not followed by an item"));
                }
                continue;
            }
            break;
        }
        let open = body_open(b, pos).ok_or_else(|| format!("marker {week}: item has no body"))?;
        let header = out[pos..open].to_string();
        let close = match_brace(out.as_bytes(), open)?;
        let item_indent = indent_of(&out[out[..pos].rfind('\n').map_or(0, |p| p + 1)..pos + 1]);
        match item_kind(&header) {
            ItemKind::Fn(name) => {
                let body = format!("{{\n{item_indent}    todo!(\"{week}: {name}\")\n{item_indent}}}");
                out.replace_range(open..=close, &body);
            }
            ItemKind::Impl => {
                let inner = out[open + 1..close].to_string();
                let stripped = strip_impl_bodies(&inner, &week)?;
                out.replace_range(open + 1..close, &stripped);
            }
            ItemKind::Struct { generics } => {
                let phantom = phantom_for(&generics);
                let body = format!(
                    "{{\n{item_indent}    // TODO({week}): design this type's fields.\n{item_indent}    _todo: {phantom},\n{item_indent}}}"
                );
                out.replace_range(open..=close, &body);
            }
            ItemKind::Other => return Err(format!("marker {week}: unsupported item `{}`", header.trim())),
        }
        // Remove the marker line itself.
        let line_start = out[..marker_start].rfind('\n').map_or(0, |p| p + 1);
        out.replace_range(line_start..marker_end, "");
        count += 1;
    }
    Ok((out, count))
}

/// Replace every fn body inside an impl block.
fn strip_impl_bodies(inner: &str, week: &str) -> Result<String, String> {
    let mut out = inner.to_string();
    let mut search = 0;
    while let Some(rel) = find_fn_keyword(&out[search..]) {
        let fn_pos = search + rel;
        let b = out.as_bytes();
        let Some(open) = body_open(b, fn_pos) else {
            search = fn_pos + 2;
            continue;
        };
        let name = item_kind(&out[fn_pos..open]);
        let ItemKind::Fn(name) = name else {
            search = fn_pos + 2;
            continue;
        };
        let close = match_brace(b, open)?;
        let indent = indent_of(&out[out[..fn_pos].rfind('\n').map_or(0, |p| p + 1)..fn_pos + 1]);
        let body = format!("{{\n{indent}    todo!(\"{week}: {name}\")\n{indent}}}");
        out.replace_range(open..=close, &body);
        search = open + body.len();
    }
    Ok(out)
}

fn find_fn_keyword(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 3 <= b.len() {
        let j = skip_literal(b, i);
        if j != i {
            i = j;
            continue;
        }
        if &b[i..i + 3] == b"fn " && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Last marker in the source: (line start, end of marker line, "wNN", is_region).
fn last_marker(src: &str) -> Option<(usize, usize, String, bool)> {
    let mut best = None;
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim();
        let parsed = t
            .strip_prefix("// @student-begin ")
            .map(|w| (w, true))
            .or_else(|| t.strip_prefix("// @student ").map(|w| (w, false)));
        if let Some((rest, region)) = parsed {
            let week = rest.split_whitespace().next().unwrap_or("").to_string();
            best = Some((offset, offset + line.len(), week, region));
        }
        offset += line.len();
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fn_bodies_and_markers() {
        let src = "pub struct S;\n\n// @student w07\n#[inline]\npub fn add(a: u32, b: u32) -> u32 {\n    let s = \"}{\";\n    a + b\n}\n\nfn keep() -> u8 { 1 }\n";
        let (out, n) = strip_source(src).unwrap();
        assert_eq!(n, 1);
        assert!(out.contains("todo!(\"w07: add\")"));
        assert!(!out.contains("a + b"));
        assert!(!out.contains("@student"));
        assert!(out.contains("fn keep() -> u8 { 1 }"));
    }

    #[test]
    fn strips_impl_and_struct() {
        let src = "// @student w06\npub struct Book<'a, T: Copy> {\n    levels: Vec<T>,\n    name: &'a str,\n}\n\n// @student w06\nimpl<'a, T: Copy> Book<'a, T> {\n    pub fn new() -> Self { unimplemented!() }\n    fn len(&self) -> usize { self.levels.len() }\n}\n";
        let (out, n) = strip_source(src).unwrap();
        assert_eq!(n, 2);
        assert!(out.contains("_todo: std::marker::PhantomData<(&'a (), T,)>"));
        assert!(out.contains("todo!(\"w06: new\")"));
        assert!(out.contains("todo!(\"w06: len\")"));
        assert!(!out.contains("levels: Vec<T>"));
    }

    #[test]
    fn strips_regions() {
        let src = "fn a() {}\n// @student-begin w09\nstruct Helper;\nimpl Helper { fn x() {} }\n// @student-end\nfn b() {}\n";
        let (out, n) = strip_source(src).unwrap();
        assert_eq!(n, 1);
        assert!(out.contains("// TODO(w09)"));
        assert!(!out.contains("Helper"));
        assert!(out.contains("fn b() {}"));
    }

    #[test]
    fn lifetimes_are_not_chars() {
        let src = "// @student w01\nfn f<'a>(x: &'a str) -> &'a str {\n    let c = '{';\n    x\n}\n";
        let (out, _) = strip_source(src).unwrap();
        assert!(out.contains("todo!(\"w01: f\")"));
        assert!(!out.contains("let c"));
    }
}
