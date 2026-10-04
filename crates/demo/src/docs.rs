//! Week 51 — documentation as data.
//!
//! The README and `docs/ARCHITECTURE.md` make claims about the code: which
//! crates exist, how an order flows, which weeks build what, where things
//! live. These functions read Markdown and the Mermaid flowcharts in it so
//! tests can check those claims against the workspace, the way the week's
//! `arch-doc` drill lints an architecture written as data.
//!
//! They handle the subset of CommonMark and Mermaid that the docs use and
//! say so; anything they do not understand is ignored rather than guessed.

/// A heading and the Markdown under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section<'a> {
    /// 1 for `#`, up to 6.
    pub level: usize,
    /// The heading text, trimmed, closing `#`s removed.
    pub title: &'a str,
    /// Everything after the heading line up to the next heading of the same
    /// or a higher level (or the end), subsections included.
    pub body: &'a str,
}

/// A node of a [`Flowchart`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    /// The label from its shape (`id["label"]`), or the id if it has none.
    /// Quotes are removed; `<br/>` and other markup are kept.
    pub label: String,
}

/// A link between two nodes. `a --- b` and `a -.- b` (no arrowhead) are
/// still recorded from `a` to `b`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// `a -->|label| b` or `a -- label --> b`, quotes removed.
    pub label: Option<String>,
}

/// The nodes and edges of a Mermaid `flowchart` / `graph`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flowchart {
    /// In order of first appearance; each id once (a later label replaces
    /// an earlier bare id).
    pub nodes: Vec<Node>,
    /// In source order.
    pub edges: Vec<Edge>,
}

/// Every ATX heading (`#` … `######` then a space) outside fenced code
/// blocks, in order, with its body. O(n).
// @student w51
pub fn sections(markdown: &str) -> Vec<Section<'_>> {
    // (level, title, start of heading line, end of heading line)
    let mut heads: Vec<(usize, &str, usize, usize)> = Vec::new();
    let mut fence: Option<&str> = None;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            continue;
        }
        let level = trimmed.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with([' ', '\t']) {
            let title = trimmed[level..].trim().trim_end_matches('#').trim_end();
            heads.push((level, title, start, offset));
        }
    }
    heads
        .iter()
        .enumerate()
        .map(|(i, &(level, title, _, body_start))| {
            let end = heads[i + 1..]
                .iter()
                .find(|h| h.0 <= level)
                .map_or(markdown.len(), |h| h.2);
            Section {
                level,
                title,
                body: &markdown[body_start..end],
            }
        })
        .collect()
}

/// The contents of every fenced block whose info string is `mermaid`, in
/// order (without the fences). O(n).
// @student w51
pub fn mermaid_blocks(markdown: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut open: Option<(&str, bool, usize)> = None; // (fence, is mermaid, body start)
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        match open {
            Some((fence, mermaid, body))
                if trimmed.starts_with(fence) && trimmed[fence.len()..].trim().is_empty() =>
            {
                if mermaid {
                    blocks.push(&markdown[body..start]);
                }
                open = None;
            }
            Some(_) => {}
            None => {
                let fence = trimmed
                    .bytes()
                    .take_while(|&b| b == b'`' || b == b'~')
                    .count();
                if fence >= 3 {
                    let info = trimmed[fence..].trim();
                    open = Some((&trimmed[..fence], info == "mermaid", offset));
                }
            }
        }
    }
    blocks
}

/// Parses a Mermaid flowchart: a `flowchart`/`graph` header, then
/// statements separated by newlines or `;`:
///
/// * nodes `id`, `id[label]`, `id(label)`, `id([label])`, `id[(label)]`,
///   `id((label))`, `id{label}`, `id>label]`, labels optionally quoted;
/// * links `-->`, `---`, `-.->`, `-.-`, `==>`, `===` (any length), with
///   `|label|` after the link or `-- label -->` around it; chains
///   `a --> b --> c`; `a & b --> c`;
/// * `subgraph id[title]` / `end` (the subgraph is not a node), `%%`
///   comments; `direction`, `classDef`, `class`, `style`, `linkStyle` and
///   `click` lines are skipped.
///
/// O(n). Malformed statements are skipped.
// @student w51
pub fn parse_flowchart(source: &str) -> Flowchart {
    let mut chart = Flowchart::default();
    for statement in source.lines().flat_map(|l| l.split(';')) {
        let s = statement.split("%%").next().unwrap_or("").trim();
        let first = s.split_whitespace().next().unwrap_or("");
        if s.is_empty()
            || matches!(
                first,
                "flowchart"
                    | "graph"
                    | "subgraph"
                    | "end"
                    | "direction"
                    | "classDef"
                    | "class"
                    | "style"
                    | "linkStyle"
                    | "click"
            )
        {
            continue;
        }
        parse_statement(s, &mut chart);
    }
    chart
}

// @student-begin w51
/// One statement: node groups joined by links.
fn parse_statement(s: &str, chart: &mut Flowchart) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut previous: Option<Vec<String>> = None;
    let mut pending_label: Option<String> = None;
    loop {
        // A group: node (& node)*.
        let mut group = Vec::new();
        loop {
            skip_ws(b, &mut i);
            let Some(node) = parse_node(s, &mut i) else {
                return;
            };
            add_node(chart, node.0.clone(), node.1);
            group.push(node.0);
            skip_ws(b, &mut i);
            if b.get(i) == Some(&b'&') {
                i += 1;
            } else {
                break;
            }
        }
        if let Some(from) = previous.take() {
            for f in &from {
                for t in &group {
                    chart.edges.push(Edge {
                        from: f.clone(),
                        to: t.clone(),
                        label: pending_label.clone(),
                    });
                }
            }
        }
        skip_ws(b, &mut i);
        if i >= b.len() {
            return;
        }
        let Some(label) = parse_link(s, &mut i) else {
            return;
        };
        pending_label = label;
        previous = Some(group);
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while b.get(*i).is_some_and(|c| c.is_ascii_whitespace()) {
        *i += 1;
    }
}

/// `id` plus an optional shape; returns (id, label if a shape gave one).
fn parse_node(s: &str, i: &mut usize) -> Option<(String, Option<String>)> {
    let b = s.as_bytes();
    let start = *i;
    while b
        .get(*i)
        .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_')
    {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    let id = s[start..*i].to_string();
    let open = match b.get(*i) {
        Some(b'[' | b'(' | b'{' | b'>') => *i,
        _ => return Some((id, None)),
    };
    // Find the matching close of the outermost bracket, skipping quotes.
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut j = open;
    while j < b.len() {
        let c = b[j];
        if in_quote {
            in_quote = c != b'"';
        } else {
            match c {
                b'"' => in_quote = true,
                b'[' | b'(' | b'{' => depth += 1,
                b']' | b')' | b'}' => {
                    depth -= 1;
                    if depth <= 0 {
                        break;
                    }
                }
                b'>' if j == open => depth += 1,
                _ => {}
            }
        }
        j += 1;
    }
    let inner_end = j.min(b.len());
    *i = (j + 1).min(b.len());
    let label = s[open + 1..inner_end]
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | '{' | '}' | '/' | '\\'))
        .trim()
        .trim_matches('"')
        .to_string();
    Some((id, Some(label)))
}

/// A link at `i`; returns its label (if any). `None` if there is no link.
fn parse_link(s: &str, i: &mut usize) -> Option<Option<String>> {
    let b = s.as_bytes();
    let start = *i;
    while b
        .get(*i)
        .is_some_and(|c| matches!(c, b'-' | b'=' | b'.' | b'<' | b'>'))
    {
        *i += 1;
    }
    let op = &s[start..*i];
    if op.len() < 2 {
        return None;
    }
    // `-- label -->`: a two-character opener, then text up to the rest.
    if matches!(op, "--" | "==" | "-.") {
        let rest = &s[*i..];
        let close = ["-->", "---", "==>", "===", ".->", ".-"]
            .iter()
            .filter_map(|c| rest.find(c))
            .min()?;
        let label = rest[..close].trim().trim_matches('"').to_string();
        *i += close;
        while b
            .get(*i)
            .is_some_and(|c| matches!(c, b'-' | b'=' | b'.' | b'>'))
        {
            *i += 1;
        }
        return Some(Some(label));
    }
    skip_ws(b, i);
    if b.get(*i) == Some(&b'|') {
        let close = s[*i + 1..].find('|')? + *i + 1;
        let label = s[*i + 1..close].trim().trim_matches('"').to_string();
        *i = close + 1;
        return Some(Some(label));
    }
    Some(None)
}

fn add_node(chart: &mut Flowchart, id: String, label: Option<String>) {
    match chart.nodes.iter_mut().find(|n| n.id == id) {
        Some(node) => {
            if let Some(label) = label {
                node.label = label;
            }
        }
        None => {
            let label = label.unwrap_or_else(|| id.clone());
            chart.nodes.push(Node { id, label });
        }
    }
}
// @student-end

// @student w51
impl Flowchart {
    /// The node with this id. O(nodes).
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Whether some edge goes from `from` to `to`. O(edges).
    pub fn has_edge(&self, from: &str, to: &str) -> bool {
        self.edges.iter().any(|e| e.from == from && e.to == to)
    }

    /// A shortest path (fewest edges) from `from` to `to`, following edges
    /// in their direction, as node ids including both ends; among equally
    /// short paths, the one using earlier edges. `Some([from])` when
    /// `from == to` and it is a node; `None` if unreachable. BFS,
    /// O(nodes × edges).
    pub fn path(&self, from: &str, to: &str) -> Option<Vec<&str>> {
        self.node(from)?;
        let mut parent: Vec<(&str, Option<&str>)> = vec![(self.node(from)?.id.as_str(), None)];
        let mut frontier = 0;
        while frontier < parent.len() {
            let here = parent[frontier].0;
            if here == to {
                let mut path = vec![here];
                let mut at = parent[frontier].1;
                while let Some(p) = at {
                    path.push(p);
                    at = parent.iter().find(|(n, _)| *n == p).and_then(|(_, up)| *up);
                }
                path.reverse();
                return Some(path);
            }
            for e in self.edges.iter().filter(|e| e.from == here) {
                if !parent.iter().any(|(n, _)| *n == e.to) {
                    parent.push((e.to.as_str(), Some(here)));
                }
            }
            frontier += 1;
        }
        None
    }
}

/// Targets of inline links and images (`[text](target)`, `![alt](target)`)
/// and reference definitions (`[label]: target`) outside code blocks and
/// code spans, in order, that point into the repository: no URL scheme
/// (`https:`, `mailto:`), not a bare `#anchor`. A `#fragment` is cut off;
/// `<target>` brackets and `"titles"` are removed. O(n).
// @student w51
pub fn relative_links(markdown: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            continue;
        }
        // Reference definition: `[label]: target`.
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some((_, target)) = rest.split_once("]:") {
                push_target(target, &mut out);
                continue;
            }
        }
        let b = line.as_bytes();
        let mut i = 0;
        let mut in_code = false;
        while i < b.len() {
            match b[i] {
                b'`' => in_code = !in_code,
                b']' if !in_code && b.get(i + 1) == Some(&b'(') => {
                    if let Some(len) = line[i + 2..].find(')') {
                        push_target(&line[i + 2..i + 2 + len], &mut out);
                        i += 2 + len;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    out
}

// @student-begin w51
fn push_target<'a>(raw: &'a str, out: &mut Vec<&'a str>) {
    let raw = raw.trim();
    let target = match raw.strip_prefix('<') {
        Some(r) => r.split('>').next().unwrap_or(""),
        None => raw.split_whitespace().next().unwrap_or(""),
    };
    let target = target.split('#').next().unwrap_or("");
    let scheme = target.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
    });
    if !target.is_empty() && !scheme {
        out.push(target);
    }
}
// @student-end

/// The week numbers of the first "Week N" / "Weeks N, M and P" phrase in
/// `text` (ranges `7–8` and `7-8` expand), in order; empty if there is
/// none. O(n).
// @student w51
pub fn week_list(text: &str) -> Vec<u32> {
    let mut search = 0;
    while let Some(found) = text[search..].find("Week") {
        let mut rest = &text[search + found + 4..];
        rest = rest.strip_prefix('s').unwrap_or(rest);
        let mut weeks = Vec::new();
        loop {
            rest = rest
                .trim_start_matches([' ', ',', '&'])
                .trim_start_matches("and ");
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                break;
            }
            let n: u32 = rest[..digits].parse().unwrap_or(0);
            rest = &rest[digits..];
            let dash = ['–', '-'].iter().find_map(|d| rest.strip_prefix(*d));
            match dash.map(|r| (r, r.bytes().take_while(u8::is_ascii_digit).count())) {
                Some((r, len)) if len > 0 => {
                    let hi: u32 = r[..len].parse().unwrap_or(n);
                    weeks.extend(n..=hi);
                    rest = &r[len..];
                }
                _ => weeks.push(n),
            }
        }
        if !weeks.is_empty() {
            return weeks;
        }
        search += found + 4;
    }
    Vec::new()
}
