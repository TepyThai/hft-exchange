//! hft-terminal: a native trading terminal over the hft-exchange engine.
//!
//! One window, one in-process [`LiveSession`]: the engine (yours or the
//! reference), simulated order flow from `sim::flow`, and you as a trader.
//! Every engine command is timed, so the latency panel is a live histogram
//! of your matching engine under the selected load.
//!
//! Built with GPUI (Zed's GPU-accelerated UI framework) and
//! gpui-component. Run from this directory:
//!
//! ```text
//! cargo run --release
//! ```

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use benchkit::Histogram;
use engine::{BookSnapshot, LevelInfo};
use gpui_kit::component::{
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    chart::{BarChart, CandlestickChart},
    h_flex,
    input::{Input, InputState},
    tab::{Tab, TabBar},
    v_flex, ActiveTheme, Sizable, Theme, ThemeMode, TitleBar,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use sim::live::{LiveSession, UserKind, USER_ID_BASE};
use types::Side;

actions!(hft_terminal, [Quit]);

const TICK: Duration = Duration::from_millis(16);
/// Simulated commands per 16 ms frame.
const SPEEDS: [(&str, usize); 4] = [("Calm", 2), ("Busy", 40), ("Open", 800), ("Flood", 12_000)];
const CANDLE: Duration = Duration::from_secs(1);
const MAX_CANDLES: usize = 60;
const LADDER_DEPTH: usize = 14;
const DEPTH_LEVELS: usize = 60;
const LATENCY_WINDOW: Duration = Duration::from_secs(2);
/// Upper edges of the latency histogram buckets (ns); the last bucket is open.
const LATENCY_EDGES: [u64; 9] = [64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384];

// Categorical slots 1 and 2 (dark), validated for CVD separation: bids and
// buys are blue, asks and sells orange, everywhere.
fn bid_color() -> Hsla {
    rgb(0x3987e5).into()
}
fn ask_color() -> Hsla {
    rgb(0xd95926).into()
}

fn price(ticks: i64) -> String {
    format!("{:.2}", ticks as f64 / 100.0)
}

fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.2}M", n as f64 / 1e6),
        _ => format!("{:.2}G", n as f64 / 1e9),
    }
}

fn nanos(ns: u64) -> String {
    if ns >= 10_000 {
        format!("{:.1} µs", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

#[derive(Clone)]
struct Candle {
    label: SharedString,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

#[derive(Clone)]
struct LatencyBar {
    label: SharedString,
    count: f64,
}

#[derive(Clone, Default)]
struct LatencySummary {
    bars: Vec<LatencyBar>,
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    per_sec: f64,
}

/// Engine command latency over rolling two-second windows.
struct LatencyWindow {
    hist: Histogram,
    counts: [u64; LATENCY_EDGES.len() + 1],
    max: u64,
    started: Instant,
    shown: LatencySummary,
}

impl LatencyWindow {
    fn new() -> LatencyWindow {
        LatencyWindow {
            hist: Histogram::new(),
            counts: [0; LATENCY_EDGES.len() + 1],
            max: 0,
            started: Instant::now(),
            shown: LatencySummary::default(),
        }
    }

    fn record(&mut self, ns: u64) {
        self.hist.record(ns);
        let i = LATENCY_EDGES.iter().position(|&edge| ns < edge).unwrap_or(LATENCY_EDGES.len());
        self.counts[i] += 1;
        self.max = self.max.max(ns);
    }

    fn roll(&mut self) {
        let elapsed = self.started.elapsed();
        if elapsed < LATENCY_WINDOW {
            return;
        }
        let bars = self
            .counts
            .iter()
            .enumerate()
            .map(|(i, &count)| LatencyBar {
                label: match i {
                    0 => format!("<{}", LATENCY_EDGES[0]).into(),
                    i if i == LATENCY_EDGES.len() => format!("≥{}k", LATENCY_EDGES[i - 1] / 1024).into(),
                    i if LATENCY_EDGES[i] >= 2048 => format!("{}k", LATENCY_EDGES[i] / 1024).into(),
                    i => LATENCY_EDGES[i].to_string().into(),
                },
                count: count as f64,
            })
            .collect();
        self.shown = if self.hist.is_empty() {
            LatencySummary { bars, ..LatencySummary::default() }
        } else {
            LatencySummary {
                bars,
                p50: self.hist.quantile(0.50),
                p99: self.hist.quantile(0.99),
                p999: self.hist.quantile(0.999),
                max: self.max,
                per_sec: self.hist.len() as f64 / elapsed.as_secs_f64(),
            }
        };
        self.hist = Histogram::new();
        self.counts = [0; LATENCY_EDGES.len() + 1];
        self.max = 0;
        self.started = Instant::now();
    }
}

struct Terminal {
    session: LiveSession,
    seed: u64,
    running: bool,
    speed: usize,
    kind: UserKind,
    qty: Entity<InputState>,
    price: Entity<InputState>,
    snap: BookSnapshot,
    depth_bids: Vec<LevelInfo>,
    depth_asks: Vec<LevelInfo>,
    candles: VecDeque<Candle>,
    candle: Option<Candle>,
    candle_started: Instant,
    candle_count: u64,
    latency: LatencyWindow,
    message: Option<SharedString>,
}

impl Terminal {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Terminal {
        let qty = cx.new(|cx| InputState::new(window, cx).placeholder("Quantity").default_value("10"));
        let price = cx.new(|cx| InputState::new(window, cx).placeholder("Price (blank: touch)"));
        let seed = 0x5eed;
        let mut terminal = Terminal {
            session: LiveSession::new(seed),
            seed,
            running: true,
            speed: 1,
            kind: UserKind::Limit,
            qty,
            price,
            snap: BookSnapshot::default(),
            depth_bids: Vec::new(),
            depth_asks: Vec::new(),
            candles: VecDeque::with_capacity(MAX_CANDLES),
            candle: None,
            candle_started: Instant::now(),
            candle_count: 0,
            latency: LatencyWindow::new(),
            message: None,
        };
        terminal.reset();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let alive = this.update(cx, |this, cx| {
                this.tick();
                cx.notify();
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
        terminal
    }

    fn reset(&mut self) {
        self.session = LiveSession::new(self.seed);
        self.session.run(3_000); // a resting book to start from
        self.candles.clear();
        self.candle = None;
        self.candle_count = 0;
        self.latency = LatencyWindow::new();
        self.message = None;
        self.refresh_views();
    }

    fn tick(&mut self) {
        if self.running {
            for _ in 0..SPEEDS[self.speed].1 {
                let t0 = Instant::now();
                self.session.step();
                self.latency.record(t0.elapsed().as_nanos() as u64);
            }
        }
        self.latency.roll();
        self.update_candles();
        self.refresh_views();
    }

    fn refresh_views(&mut self) {
        let engine = self.session.engine();
        engine.snapshot_into(LADDER_DEPTH, &mut self.snap);
        engine.levels(Side::Buy, DEPTH_LEVELS, &mut self.depth_bids);
        engine.levels(Side::Sell, DEPTH_LEVELS, &mut self.depth_asks);
    }

    fn update_candles(&mut self) {
        let Some(last) = self.session.engine().last_trade_price() else { return };
        let p = last.0 as f64 / 100.0;
        let c = self.candle.get_or_insert_with(|| Candle { label: "0".into(), open: p, high: p, low: p, close: p });
        c.high = c.high.max(p);
        c.low = c.low.min(p);
        c.close = p;
        if self.candle_started.elapsed() >= CANDLE {
            let done = c.clone();
            if self.candles.len() == MAX_CANDLES {
                self.candles.pop_front();
            }
            self.candles.push_back(done);
            self.candle_count += 1;
            self.candle = Some(Candle {
                label: self.candle_count.to_string().into(),
                open: p,
                high: p,
                low: p,
                close: p,
            });
            self.candle_started = Instant::now();
        }
    }

    fn send(&mut self, side: Side, cx: &mut Context<Self>) {
        let qty: u64 = self.qty.read(cx).value().trim().parse().unwrap_or(0);
        let text = self.price.read(cx).value();
        let touch = match (side, self.kind) {
            // Blank price: limits join their own side, IOCs take the other.
            (Side::Buy, UserKind::Limit) => self.snap.best_bid(),
            (Side::Sell, UserKind::Limit) => self.snap.best_ask(),
            (Side::Buy, _) => self.snap.best_ask(),
            (Side::Sell, _) => self.snap.best_bid(),
        };
        let limit = match text.trim() {
            "" => touch.map_or(10_000, |l| l.price.0),
            t => match t.parse::<f64>() {
                Ok(v) => (v * 100.0).round() as i64,
                Err(_) => {
                    self.message = Some(format!("Not a price: {t}").into());
                    return;
                }
            },
        };
        let id = self.session.submit(side, self.kind, limit, qty);
        let rejected = self.session.user().rejects().next().filter(|(r, _)| *r == id).map(|(_, why)| *why);
        self.message = Some(match rejected {
            Some(why) => format!("Order {} rejected: {why:?}", id.0 - USER_ID_BASE + 1).into(),
            None => format!(
                "Order {} sent: {} {qty}{}",
                id.0 - USER_ID_BASE + 1,
                if side == Side::Buy { "buy" } else { "sell" },
                if self.kind == UserKind::Market { " at market".to_string() } else { format!(" @ {}", price(limit)) }
            )
            .into(),
        });
        self.refresh_views();
        cx.notify();
    }

    // ---- views -------------------------------------------------------------

    fn panel_title(&self, title: &str, cx: &Context<Self>) -> impl IntoElement {
        div().text_xs().text_color(cx.theme().muted_foreground).child(title.to_uppercase())
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_3()
                .gap_3()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().text_sm().child("hft-exchange"))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground).child("terminal")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            TabBar::new("speed")
                                .segmented()
                                .small()
                                .selected_index(self.speed)
                                .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                    this.speed = *ix;
                                    cx.notify();
                                }))
                                .children(SPEEDS.iter().map(|(label, _)| Tab::new().label(*label))),
                        )
                        .child(
                            Button::new("pause")
                                .small()
                                .outline()
                                .label(if self.running { "Pause" } else { "Resume" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.running = !this.running;
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("reset").small().ghost().label("New session").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.seed = this.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                                this.reset();
                                cx.notify();
                            },
                        ))),
                ),
        )
    }

    fn render_ladder(&self, cx: &Context<Self>) -> impl IntoElement {
        let max = self.snap.bids.iter().chain(&self.snap.asks).map(|l| l.qty.0).max().unwrap_or(1).max(1);
        let mine: Vec<i64> = self.session.user().open_orders().iter().map(|o| o.price.0).collect();
        let row = |l: &LevelInfo, color: Hsla| {
            let frac = l.qty.0 as f32 / max as f32;
            div()
                .relative()
                .h(px(22.))
                .w_full()
                .child(div().absolute().top(px(2.)).bottom(px(2.)).right_0().w(relative(frac)).bg(color.opacity(0.2)))
                .child(
                    h_flex()
                        .relative()
                        .size_full()
                        .px_3()
                        .gap_2()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(
                            h_flex()
                                .w(px(84.))
                                .gap_1()
                                .text_color(color)
                                .child(price(l.price.0))
                                .when(mine.contains(&l.price.0), |d| d.child(div().text_color(cx.theme().foreground).child("●"))),
                        )
                        .child(h_flex().flex_1().justify_end().child(l.qty.0.to_string()))
                        .child(h_flex().w(px(32.)).justify_end().text_color(cx.theme().muted_foreground).child(l.orders.to_string())),
                )
        };
        let spread = self.snap.spread().map_or("—".to_string(), |s| format!("{s} ticks"));
        v_flex()
            .w(px(260.))
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().w(px(84.)).child("PRICE"))
                    .child(h_flex().flex_1().justify_end().child("QTY"))
                    .child(h_flex().w(px(32.)).justify_end().child("#")),
            )
            .child(
                v_flex()
                    .flex_1()
                    .justify_end()
                    .children(self.snap.asks.iter().rev().map(|l| row(l, ask_color()))),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .justify_between()
                    .text_xs()
                    .bg(cx.theme().secondary)
                    .child(div().text_color(cx.theme().muted_foreground).child("spread"))
                    .child(div().font_family(cx.theme().mono_font_family.clone()).child(spread))
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(self.snap.last_price.map_or("—".to_string(), |p| format!("last {}", price(p.0)))),
                    ),
            )
            .child(v_flex().flex_1().children(self.snap.bids.iter().map(|l| row(l, bid_color()))))
    }

    fn render_candles(&self, cx: &Context<Self>) -> impl IntoElement {
        let data: Vec<Candle> = self.candles.iter().cloned().chain(self.candle.clone()).collect();
        v_flex()
            .flex_1()
            .min_h(px(220.))
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(self.panel_title("Price · 1 s candles", cx))
                    .child(
                        div()
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(self.snap.last_price.map_or("—".to_string(), |p| price(p.0))),
                    ),
            )
            .child(
                CandlestickChart::new(data)
                    .x(|c| c.label.clone())
                    .open(|c| c.open)
                    .high(|c| c.high)
                    .low(|c| c.low)
                    .close(|c| c.close)
                    .bullish(bid_color())
                    .bearish(ask_color())
                    .tick_margin(10),
            )
    }

    fn render_depth(&self, cx: &Context<Self>) -> impl IntoElement {
        let bids = self.depth_bids.clone();
        let asks = self.depth_asks.clone();
        let (lo, hi) = match (bids.last(), asks.last()) {
            (Some(b), Some(a)) => (b.price.0, a.price.0),
            _ => (0, 1),
        };
        v_flex()
            .h(px(190.))
            .p_3()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.panel_title("Depth · cumulative quantity", cx))
            .child(
                div().flex_1().child(
                    canvas(
                        |_, _, _| (),
                        move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                            paint_depth(bounds, &bids, &asks, window);
                        },
                    )
                    .size_full(),
                ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(price(lo))
                    .child(price((lo + hi) / 2))
                    .child(price(hi)),
            )
    }

    fn render_latency(&self, cx: &Context<Self>) -> impl IntoElement {
        let s = self.latency.shown.clone();
        let stat = |label: &str, value: String| {
            v_flex()
                .gap_0p5()
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_string()))
                .child(div().text_sm().font_family(cx.theme().mono_font_family.clone()).child(value))
        };
        let color = bid_color();
        v_flex()
            .h(px(230.))
            .p_3()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.panel_title("Engine latency per command · last 2 s (ns)", cx))
            .child(
                h_flex()
                    .gap_6()
                    .child(stat("p50", nanos(s.p50)))
                    .child(stat("p99", nanos(s.p99)))
                    .child(stat("p99.9", nanos(s.p999)))
                    .child(stat("max", nanos(s.max)))
                    .child(stat("commands/s", compact(s.per_sec as u64))),
            )
            .child(
                div().flex_1().child(
                    BarChart::new(s.bars)
                        .band(|b| b.label.clone())
                        .value(|b| b.count)
                        .fill(move |_, _, _, _| color)
                        .tick_margin(1),
                ),
            )
    }

    fn render_order_entry(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let kind_ix = match self.kind {
            UserKind::Limit => 0,
            UserKind::Ioc => 1,
            UserKind::Market => 2,
        };
        let solid = |color: Hsla, cx: &App| {
            ButtonCustomVariant::new(cx).color(color).foreground(white()).hover(color.opacity(0.85)).active(color.opacity(0.7))
        };
        let user = self.session.user();
        let pnl = user.pnl(self.snap.last_price);
        v_flex()
            .gap_3()
            .child(self.panel_title("Order entry", cx))
            .child(
                TabBar::new("kind")
                    .segmented()
                    .small()
                    .selected_index(kind_ix)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.kind = [UserKind::Limit, UserKind::Ioc, UserKind::Market][*ix];
                        cx.notify();
                    }))
                    .child(Tab::new().label("Limit"))
                    .child(Tab::new().label("IOC"))
                    .child(Tab::new().label("Market")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(110.)).child(Input::new(&self.qty).small()))
                    .child(div().flex_1().child(Input::new(&self.price).small())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("buy")
                            .custom(solid(bid_color(), cx))
                            .label("Buy")
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, cx| this.send(Side::Buy, cx))),
                    )
                    .child(
                        Button::new("sell")
                            .custom(solid(ask_color(), cx))
                            .label("Sell")
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, cx| this.send(Side::Sell, cx))),
                    ),
            )
            .when_some(self.message.clone(), |d, m| d.child(div().text_xs().text_color(cx.theme().muted_foreground).child(m)))
            .child(
                h_flex()
                    .justify_between()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(format!("Position {}", user.position))
                    .child(format!("P&L {}", price(pnl))),
            )
    }

    fn render_my_orders(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.session.user().open_orders();
        v_flex()
            .gap_1()
            .child(self.panel_title("Your resting orders", cx))
            .when(open.is_empty(), |d| d.child(div().text_xs().text_color(cx.theme().muted_foreground).child("None")))
            .children(open.into_iter().take(6).map(|o| {
                let id = o.id;
                h_flex()
                    .gap_2()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(
                        div()
                            .w(px(36.))
                            .text_color(if o.side == Side::Buy { bid_color() } else { ask_color() })
                            .child(if o.side == Side::Buy { "BUY" } else { "SELL" }),
                    )
                    .child(div().flex_1().child(format!("{} @ {}", o.remaining.0, price(o.price.0))))
                    .child(
                        Button::new(SharedString::from(format!("cancel-{}", id.0)))
                            .xsmall()
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.session.cancel(id);
                                this.refresh_views();
                                cx.notify();
                            })),
                    )
            }))
            .children(self.session.user().fills().take(4).map(|f| {
                div()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} {} @ {} ({})",
                        if f.side == Side::Buy { "bought" } else { "sold" },
                        f.qty.0,
                        price(f.price.0),
                        if f.maker { "maker" } else { "taker" }
                    ))
            }))
    }

    fn render_tape(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .gap_1()
            .overflow_hidden()
            .child(self.panel_title("Trade tape", cx))
            .children(self.snap.trades.iter().rev().take(18).map(|t| {
                let color = if t.taker_side == Side::Buy { bid_color() } else { ask_color() };
                h_flex()
                    .justify_between()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(div().text_color(color).child(format!(
                        "{} {}",
                        if t.taker_side == Side::Buy { "▲" } else { "▼" },
                        price(t.price.0)
                    )))
                    .child(div().text_color(cx.theme().muted_foreground).child(t.qty.0.to_string()))
            }))
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let st = self.session.stats();
        let resting = engine::OrderBook::len(self.session.engine().book());
        h_flex()
            .h(px(28.))
            .px_3()
            .gap_6()
            .items_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(format!("{} commands", compact(st.commands)))
            .child(format!("{} trades", compact(st.trades)))
            .child(format!("{} volume", compact(st.volume)))
            .child(format!("{} resting orders", compact(resting as u64)))
            .child(format!("seq {}", self.snap.seq.0))
            .child(div().flex_1())
            .child(if self.running { format!("{} flow", SPEEDS[self.speed].0) } else { "paused".to_string() })
    }
}

/// Cumulative depth as step areas: bids grow leftward from the best bid,
/// asks rightward from the best ask, on one shared price axis.
fn paint_depth(bounds: Bounds<Pixels>, bids: &[LevelInfo], asks: &[LevelInfo], window: &mut Window) {
    let (Some(lo), Some(hi)) = (bids.last().map(|l| l.price.0), asks.last().map(|l| l.price.0)) else { return };
    if hi <= lo {
        return;
    }
    let total = |levels: &[LevelInfo]| levels.iter().map(|l| l.qty.0).sum::<u64>();
    let max = total(bids).max(total(asks)).max(1) as f32;
    let (x0, w) = (bounds.origin.x.as_f32(), bounds.size.width.as_f32());
    let (y0, h) = (bounds.origin.y.as_f32(), bounds.size.height.as_f32());
    let x = |p: i64| x0 + (p - lo) as f32 / (hi - lo) as f32 * w;
    let y = |q: u64| y0 + h - q as f32 / max * h;
    let bottom = y0 + h;

    for (levels, color, toward) in [(bids, bid_color(), lo), (asks, ask_color(), hi)] {
        let mut cum = 0u64;
        let mut edge = PathBuilder::stroke(px(2.));
        for (i, l) in levels.iter().enumerate() {
            cum += l.qty.0;
            let next = levels.get(i + 1).map_or(toward, |n| n.price.0);
            let (xa, xb) = (x(l.price.0), x(next));
            let (left, right) = if xa < xb { (xa, xb) } else { (xb, xa) };
            let top = y(cum);
            window.paint_quad(fill(
                Bounds::from_corners(point(px(left), px(top)), point(px(right), px(bottom))),
                color.opacity(0.22),
            ));
            if i == 0 {
                edge.move_to(point(px(xa), px(bottom)));
            }
            edge.line_to(point(px(xa), px(top)));
            edge.line_to(point(px(xb), px(top)));
        }
        if let Ok(path) = edge.build() {
            window.paint_path(path, color);
        }
    }
}

impl Render for Terminal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(self.render_ladder(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .child(self.render_candles(cx))
                            .child(self.render_depth(cx))
                            .child(self.render_latency(cx)),
                    )
                    .child(
                        v_flex()
                            .w(px(320.))
                            .h_full()
                            .p_3()
                            .gap_4()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .child(self.render_order_entry(cx))
                            .child(self.render_my_orders(cx))
                            .child(self.render_tape(cx)),
                    ),
            )
            .child(self.render_status_bar(cx))
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        cx.bind_keys([
            #[cfg(target_os = "macos")]
            KeyBinding::new("cmd-q", Quit, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1400.), px(900.)), cx)),
            ..TitleBar::window_options()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            window.activate_window();
            window.set_window_title("hft-exchange terminal");
            Theme::change(ThemeMode::Dark, Some(window), cx);
            cx.new(|cx| Terminal::new(window, cx))
        })
        .expect("failed to open window");
    });
}
