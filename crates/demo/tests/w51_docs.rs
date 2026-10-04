//! Week 51: the documentation is held to the code — every crate on the
//! diagram and in the architecture overview with the weeks that build it,
//! the order path drawn as built, links that resolve, and a README whose
//! demo settings exist in the binary.

use demo::docs::{mermaid_blocks, parse_flowchart, relative_links, sections, week_list, Flowchart};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Every workspace member's directory name: `crates/*` and `xtask`.
fn workspace_crates() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root().join("crates"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.push("xtask".into());
    names.sort();
    names
}

/// The README's diagram: the mermaid block that names the most crates.
fn readme_chart() -> Flowchart {
    let readme = read("README.md");
    let crates = workspace_crates();
    mermaid_blocks(&readme)
        .into_iter()
        .map(parse_flowchart)
        .max_by_key(|c| crates.iter().filter(|k| c.node(k).is_some()).count())
        .expect("README.md has a ```mermaid block")
}

#[test]
fn w51_architecture_doc_covers_every_crate() {
    let doc = read("docs/ARCHITECTURE.md");
    let secs = sections(&doc);
    for name in workspace_crates() {
        let tag = format!("`{name}`");
        let section = secs
            .iter()
            .find(|s| s.title.contains(&tag))
            .unwrap_or_else(|| panic!("docs/ARCHITECTURE.md has no section titled with {tag}"));
        let words = section.body.split_whitespace().count();
        assert!(
            words >= 15,
            "the {tag} section is a sentence fragment ({words} words)"
        );
    }
    let order = secs
        .iter()
        .find(|s| s.title.contains("view") || s.body.contains("gateway → risk"));
    assert!(order.is_some(), "the overview states the order path");
}

#[test]
fn w51_architecture_weeks_match_crate_docs() {
    let doc = read("docs/ARCHITECTURE.md");
    let secs = sections(&doc);
    let mut checked = 0;
    for name in workspace_crates() {
        let dir = if name == "xtask" {
            root().join("xtask")
        } else {
            root().join("crates").join(&name)
        };
        let src = ["src/lib.rs", "src/main.rs"]
            .iter()
            .find_map(|f| std::fs::read_to_string(dir.join(f)).ok())
            .unwrap();
        let Some(line) = src
            .lines()
            .find(|l| l.starts_with("//!") && l.contains("Curriculum:"))
        else {
            continue; // provided crates name no weeks
        };
        let weeks = week_list(line);
        assert!(!weeks.is_empty(), "{name}: unreadable `{line}`");
        let section = secs
            .iter()
            .find(|s| s.title.contains(&format!("`{name}`")))
            .unwrap();
        assert_eq!(
            week_list(section.body),
            weeks,
            "docs/ARCHITECTURE.md lists other weeks for `{name}` than its crate docs"
        );
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} crates name their weeks");
}

#[test]
fn w51_readme_diagram_names_every_crate() {
    let readme = read("README.md");
    let section = sections(&readme)
        .into_iter()
        .find(|s| s.title == "Architecture")
        .expect("README has an Architecture section");
    assert_eq!(
        mermaid_blocks(section.body).len(),
        1,
        "the Architecture section holds the diagram"
    );
    let chart = readme_chart();
    let missing: Vec<String> = workspace_crates()
        .into_iter()
        .filter(|c| chart.node(c).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "crates missing from the README diagram: {missing:?}"
    );
    assert!(
        chart
            .node("terminal")
            .is_some_and(|n| n.label.contains("apps/terminal")),
        "the terminal is on the map"
    );
    // Every crate is connected to something, except the stand-alone tooling.
    for c in workspace_crates() {
        let linked = chart.edges.iter().any(|e| e.from == c || e.to == c);
        assert!(
            linked || ["types", "sync", "telemetry", "benchkit", "xtask"].contains(&c.as_str()),
            "`{c}` floats unconnected"
        );
    }
}

#[test]
fn w51_readme_diagram_draws_the_order_path() {
    let chart = readme_chart();
    assert_eq!(
        chart.path("gateway", "marketdata"),
        Some(vec!["gateway", "risk", "journal", "engine", "marketdata"]),
        "the order path is gateway → risk → journal → engine → marketdata"
    );
    let crates = workspace_crates();
    let consumers: Vec<&str> = chart
        .edges
        .iter()
        .filter(|e| e.from == "marketdata" && !crates.contains(&e.to))
        .map(|e| e.to.as_str())
        .collect();
    assert!(!consumers.is_empty(), "market data flows out to clients");
    assert!(
        chart
            .edges
            .iter()
            .any(|e| e.from == "engine" && e.to == "gateway"),
        "execution reports go back to the gateway"
    );
    assert!(
        chart.path("sim", "demo").is_some(),
        "the demo runs the simulated market"
    );
    // Arrows that cross a thread say how: every labelled order-path hop names its transport.
    let hop = chart
        .edges
        .iter()
        .find(|e| e.from == "gateway" && e.to == "risk")
        .unwrap();
    assert!(
        hop.label.as_deref().is_some_and(|l| l.contains("ring")),
        "gateway → risk is a queue: {hop:?}"
    );
}

#[test]
fn w51_relative_links_resolve() {
    let mut checked = 0;
    for doc in [
        "README.md",
        "CONVENTIONS.md",
        "docs/ARCHITECTURE.md",
        "apps/terminal/README.md",
    ] {
        let text = read(doc);
        let base = root().join(doc).parent().unwrap().to_path_buf();
        for link in relative_links(&text) {
            let target = base.join(link);
            assert!(target.exists(), "{doc}: broken link `{link}`");
            checked += 1;
        }
    }
    assert!(checked >= 4, "only {checked} relative links found");
    for file in ["Dockerfile.demo", "fly.toml", "docs/ARCHITECTURE.md"] {
        assert!(root().join(file).exists(), "{file} is missing");
    }
}

#[test]
fn w51_readme_demo_settings_exist_in_the_binary() {
    let readme = read("README.md");
    let section = sections(&readme)
        .into_iter()
        .find(|s| s.title.to_lowercase().contains("demo"))
        .expect("README has a section on running the demo");
    let help = Command::new(env!("CARGO_BIN_EXE_demo"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    let mut mentioned = 0;
    for word in section
        .body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        let is_var = word.starts_with("HFT_DEMO_") && word.len() > "HFT_DEMO_".len();
        let is_flag = word.starts_with("--")
            && word.len() > 2
            && !matches!(word, "--release" | "--rm" | "--init");
        if is_var || is_flag {
            assert!(
                help.contains(word),
                "README mentions `{word}` but `demo --help` does not"
            );
            mentioned += 1;
        }
    }
    assert!(
        mentioned >= 6,
        "the README documents the settings ({mentioned} found)"
    );
    for file in ["Dockerfile.demo", "fly.toml"] {
        assert!(
            section.body.contains(file),
            "the demo section points at {file}"
        );
    }
}

#[test]
fn w51_markdown_sections_links_and_weeks() {
    let md = "# A\nintro\n## B\nb text\n```sh\n# not a heading\n```\n## C\nc\n# D\n";
    let s = sections(md);
    let titles: Vec<_> = s.iter().map(|s| (s.level, s.title)).collect();
    assert_eq!(titles, [(1, "A"), (2, "B"), (2, "C"), (1, "D")]);
    assert_eq!(
        s[0].body,
        "intro\n## B\nb text\n```sh\n# not a heading\n```\n## C\nc\n"
    );
    assert_eq!(s[1].body, "b text\n```sh\n# not a heading\n```\n");
    assert_eq!(s[3].body, "");
    assert!(sections("").is_empty() && sections("#hashtag\n").is_empty());

    let md = "x\n```mermaid\nflowchart LR\n  a --> b\n```\n```rust\nfn f() {}\n```\n````mermaid\ngraph TD\n````\n";
    assert_eq!(
        mermaid_blocks(md),
        ["flowchart LR\n  a --> b\n", "graph TD\n"]
    );

    let md = "See [the guide](docs/A.md#x), ![shot](apps/t.png \"title\") and [web](https://x.y).\n\
              `[not](a.md)` [anchor](#top) [m](mailto:a@b)\n```\n[code](c.md)\n```\n[ref]: <docs/B.md>\n";
    assert_eq!(relative_links(md), ["docs/A.md", "apps/t.png", "docs/B.md"]);

    assert_eq!(
        week_list("Curriculum: Weeks 6, 27, 28 (see x)"),
        [6, 27, 28]
    );
    assert_eq!(week_list("**Week 14.**"), [14]);
    assert_eq!(week_list("Weekly. Weeks 7–8 and 12"), [7, 8, 12]);
    assert_eq!(week_list("no weeks here"), Vec::<u32>::new());
}

#[test]
fn w51_mermaid_flowchart_syntax() {
    let chart = parse_flowchart(
        "flowchart LR\n  %% comment\n  subgraph core[\"Core\"]\n    a[\"Alpha<br/>one\"] -->|\"via ring\"| b([Beta]) --> c\n  end\n  \
         a & c -.-> d((Delta)); d -- talks to --> a\n  e{Echo} === f[(Store)]\n  style a fill:#fff\n",
    );
    let ids: Vec<_> = chart.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        ["a", "b", "c", "d", "e", "f"],
        "subgraphs and style lines are not nodes"
    );
    assert_eq!(chart.node("a").unwrap().label, "Alpha<br/>one");
    assert_eq!(chart.node("b").unwrap().label, "Beta");
    assert_eq!(chart.node("c").unwrap().label, "c");
    assert_eq!(chart.node("d").unwrap().label, "Delta");
    assert_eq!(chart.node("f").unwrap().label, "Store");
    let edges: Vec<_> = chart
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str(), e.label.as_deref()))
        .collect();
    assert_eq!(
        edges,
        [
            ("a", "b", Some("via ring")),
            ("b", "c", None),
            ("a", "d", None),
            ("c", "d", None),
            ("d", "a", Some("talks to")),
            ("e", "f", None)
        ]
    );
    assert!(chart.has_edge("e", "f") && !chart.has_edge("f", "e"));
    assert_eq!(chart.path("a", "c"), Some(vec!["a", "b", "c"]));
    assert_eq!(chart.path("c", "b"), Some(vec!["c", "d", "a", "b"]));
    assert_eq!(chart.path("a", "a"), Some(vec!["a"]));
    assert_eq!(chart.path("a", "e"), None);
    assert_eq!(chart.path("zz", "a"), None);
    assert_eq!(
        parse_flowchart("graph TD\n  x -->"),
        Flowchart {
            nodes: vec![demo::docs::Node {
                id: "x".into(),
                label: "x".into()
            }],
            edges: vec![]
        }
    );
}
