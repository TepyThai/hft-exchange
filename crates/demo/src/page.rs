//! The demo's web page: one self-contained HTML file (no external assets)
//! that connects to the WebSocket feed, keeps the L2 book from its snapshot
//! and update messages, renders the ladder once per animation frame, and
//! polls `/stats` for the counters and the trade tape. Provided contract
//! for week 47.

/// The page template; [`render_page`] fills in where the feed is.
pub const PAGE_HTML: &str = include_str!("page.html");

/// The placeholder in [`PAGE_HTML`] replaced by the page's settings.
const CONFIG_SLOT: &str = "__DEMO_CONFIG__";

/// The page to serve at `/`, configured to connect to `public_ws_url`, or
/// else to `ws://<the page's host>:<ws_port>/` (`wss` on an https page).
/// A `?ws=URL` query parameter overrides both in the browser. O(page size).
pub fn render_page(ws_port: u16, public_ws_url: Option<&str>) -> String {
    let url = match public_ws_url {
        // Config validation restricts this to a ws(s):// URL; escape the
        // characters that could end the JSON string or the script.
        Some(u) => format!(
            "\"{}\"",
            u.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('<', "\\u003c")
        ),
        None => "null".into(),
    };
    PAGE_HTML.replacen(
        CONFIG_SLOT,
        &format!("{{\"wsPort\":{ws_port},\"wsUrl\":{url}}}"),
        1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_injected_once() {
        let page = render_page(8081, None);
        assert!(page.contains("const CFG = {\"wsPort\":8081,\"wsUrl\":null};"));
        assert!(!page.contains(CONFIG_SLOT));
        let page = render_page(1, Some("wss://demo.example/feed\"</script>"));
        assert!(page.contains("\"wsUrl\":\"wss://demo.example/feed\\\"\\u003c/script>\""));
    }
}
