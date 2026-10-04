//! The demo exchange: the matching engine driven by simulated order flow,
//! streaming its book over WebSocket, packaged to deploy; and the docs
//! checks that keep the repository's front door honest.
//!
//! Curriculum: Weeks 47, 51 (see <https://tepy.dev/hft>).
//!
//! # Week 47 — production deployment
//!
//! One process, four kinds of thread (see [`Demo`]):
//!
//! ```text
//!             ┌──────────── engine thread ─────────────┐
//!  seed ────▶ │ LiveSession (flow + engine) at --rate  │
//!             │   events ─▶ L2Publisher ─▶ RingPublisher ──▶ BroadcastRing ──▶ ws client threads ──▶ browsers
//!             │              └─ periodic snapshots ─────────▶ SnapshotCell ──┘   (one per client,
//!             │   counters, publish latency ─▶ Registry ──┐                      conflated batches)
//!             └─────────────────────────────────────────┘ │
//!  ws accept thread:   cap clients, spawn a client thread │
//!  http accept thread: /, /health, /metrics, /stats, POST /shutdown
//! ```
//!
//! * [`Config`] (flags and `HFT_DEMO_*` variables), [`Stats`] and the page
//!   ([`render_page`]) are provided; the wiring in [`Demo`] is the
//!   exercise, along with `Dockerfile.demo` and `fly.toml` at the root.
//! * `/health` answers 200 only once the feed is publishing and 503 while
//!   starting or stopping, so a load balancer never routes to a demo that
//!   cannot serve.
//! * The public surface is read-only market data. `POST /shutdown` exists
//!   only when a token is configured.
//!
//! ```no_run
//! use demo::{Config, Demo};
//!
//! let config = Config { http_addr: "127.0.0.1:0".parse().unwrap(), ws_addr: "127.0.0.1:0".parse().unwrap(), ..Config::default() };
//! let demo = Demo::start(config).unwrap();
//! println!("open http://{}", demo.http_addr());
//! let stats = demo.stop();
//! println!("{} commands", stats.commands);
//! ```
//!
//! # Week 51 — documentation you can test
//!
//! [`docs`] reads the README and `docs/ARCHITECTURE.md` as data — mermaid
//! flowcharts, sections, links, week lists — so the week's tests can hold
//! the docs to the code: every crate on the diagram and in the
//! architecture overview, the order path drawn as built, every relative
//! link resolving.

pub mod client;
pub mod config;
pub mod docs;
pub mod page;
pub mod server;
pub mod stats;

pub use config::{Config, ConfigError, Invocation, USAGE};
pub use page::{render_page, PAGE_HTML};
pub use server::Demo;
pub use stats::{Stats, TradePrint};
