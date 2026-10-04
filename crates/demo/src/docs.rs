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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
#[allow(unused_variables)]
pub fn sections(markdown: &str) -> Vec<Section<'_>> {
    todo!("w51: sections")
}

/// The contents of every fenced block whose info string is `mermaid`, in
/// order (without the fences). O(n).
#[allow(unused_variables)]
pub fn mermaid_blocks(markdown: &str) -> Vec<&str> {
    todo!("w51: mermaid_blocks")
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
#[allow(unused_variables)]
pub fn parse_flowchart(source: &str) -> Flowchart {
    todo!("w51: parse_flowchart")
}

// TODO(w51): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl Flowchart {
    /// The node with this id. O(nodes).
    pub fn node(&self, id: &str) -> Option<&Node> {
        todo!("w51: node")
    }

    /// Whether some edge goes from `from` to `to`. O(edges).
    pub fn has_edge(&self, from: &str, to: &str) -> bool {
        todo!("w51: has_edge")
    }

    /// A shortest path (fewest edges) from `from` to `to`, following edges
    /// in their direction, as node ids including both ends; among equally
    /// short paths, the one using earlier edges. `Some([from])` when
    /// `from == to` and it is a node; `None` if unreachable. BFS,
    /// O(nodes × edges).
    pub fn path(&self, from: &str, to: &str) -> Option<Vec<&str>> {
        todo!("w51: path")
    }
}

/// Targets of inline links and images (`[text](target)`, `![alt](target)`)
/// and reference definitions (`[label]: target`) outside code blocks and
/// code spans, in order, that point into the repository: no URL scheme
/// (`https:`, `mailto:`), not a bare `#anchor`. A `#fragment` is cut off;
/// `<target>` brackets and `"titles"` are removed. O(n).
#[allow(unused_variables)]
pub fn relative_links(markdown: &str) -> Vec<&str> {
    todo!("w51: relative_links")
}

// TODO(w51): implement — see the week guide on tepy.dev/hft.

/// The week numbers of the first "Week N" / "Weeks N, M and P" phrase in
/// `text` (ranges `7–8` and `7-8` expand), in order; empty if there is
/// none. O(n).
#[allow(unused_variables)]
pub fn week_list(text: &str) -> Vec<u32> {
    todo!("w51: week_list")
}
