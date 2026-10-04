//! What the demo reports about itself: [`Stats`] and its JSON forms for
//! `/stats` and `/health`. Provided contract for week 47 — the page reads
//! these shapes.

use marketdata::Level;
use std::fmt::Write as _;
use std::time::Duration;
use types::{Price, Qty, Side};

/// Trades kept for the page's tape.
pub const TAPE_LEN: usize = 32;

/// One print on the tape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TradePrint {
    /// 1-based trade number since start.
    pub n: u64,
    pub price: Price,
    pub qty: Qty,
    /// The aggressor's side.
    pub taker_side: Side,
}

/// A point-in-time view of a running demo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Time since [`Demo::start`](crate::Demo::start).
    pub uptime: Duration,
    /// The feed is publishing and the demo is not shutting down.
    pub ready: bool,
    /// A shutdown was requested.
    pub stopping: bool,
    /// Target commands per second (0 = unthrottled).
    pub rate: u64,
    /// Simulation seed.
    pub seed: u64,
    /// Commands the engine processed.
    pub commands: u64,
    /// Trades the engine printed.
    pub trades: u64,
    /// Total traded quantity.
    pub volume: u64,
    /// Sequence number of the last L2 update published to the feed.
    pub feed_seq: u64,
    /// Sequence number of the latest snapshot new clients start from.
    pub snapshot_seq: u64,
    /// WebSocket clients connected now.
    pub ws_clients: u64,
    /// WebSocket clients accepted since start.
    pub ws_connections: u64,
    /// WebSocket connections refused (over the client limit).
    pub ws_rejected: u64,
    pub best_bid: Option<Level>,
    pub best_ask: Option<Level>,
    /// The most recent trades, newest first (at most [`TAPE_LEN`]).
    pub last_trades: Vec<TradePrint>,
}

impl Stats {
    /// Commands per second averaged over the uptime.
    pub fn commands_per_sec(&self) -> f64 {
        let secs = self.uptime.as_secs_f64();
        if secs > 0.0 {
            self.commands as f64 / secs
        } else {
            0.0
        }
    }

    /// The `/stats` body. Prices in ticks, quantities in lots, sides as
    /// `"buy"`/`"sell"`:
    ///
    /// ```text
    /// {"uptime_ms":1200,"ready":true,"stopping":false,"rate":2000,"seed":42,
    ///  "commands":2400,"commands_per_sec":2000.0,"trades":310,"volume":4100,
    ///  "feed_seq":3900,"snapshot_seq":3072,"ws_clients":1,"ws_connections":1,
    ///  "ws_rejected":0,"best_bid":[9998,40],"best_ask":[10001,12],
    ///  "last_trades":[[310,10001,5,"buy"],…]}
    /// ```
    ///
    /// `best_bid`/`best_ask` are `[price, qty]` or `null`; `last_trades`
    /// entries are `[n, price, qty, taker_side]`, newest first. O(tape).
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(256 + 32 * self.last_trades.len());
        let level = |l: Option<Level>| match l {
            Some(l) => format!("[{},{}]", l.price.0, l.qty.0),
            None => "null".into(),
        };
        let _ = write!(
            out,
            "{{\"uptime_ms\":{},\"ready\":{},\"stopping\":{},\"rate\":{},\"seed\":{},\"commands\":{},\
             \"commands_per_sec\":{:.1},\"trades\":{},\"volume\":{},\"feed_seq\":{},\"snapshot_seq\":{},\
             \"ws_clients\":{},\"ws_connections\":{},\"ws_rejected\":{},\"best_bid\":{},\"best_ask\":{},\
             \"last_trades\":[",
            self.uptime.as_millis(),
            self.ready,
            self.stopping,
            self.rate,
            self.seed,
            self.commands,
            self.commands_per_sec(),
            self.trades,
            self.volume,
            self.feed_seq,
            self.snapshot_seq,
            self.ws_clients,
            self.ws_connections,
            self.ws_rejected,
            level(self.best_bid),
            level(self.best_ask),
        );
        for (i, t) in self.last_trades.iter().enumerate() {
            let side = match t.taker_side {
                Side::Buy => "buy",
                Side::Sell => "sell",
            };
            let comma = if i > 0 { "," } else { "" };
            let _ = write!(out, "{comma}[{},{},{},\"{side}\"]", t.n, t.price.0, t.qty.0);
        }
        out.push_str("]}");
        out
    }

    /// The `/health` body: `{"status":"ok"|"starting"|"stopping","ready":…,
    /// "uptime_ms":…,"seq":…,"commands":…}` where `seq` is the feed
    /// sequence number. Serve it with 200 when `ready`, 503 otherwise.
    pub fn health_json(&self) -> String {
        let status = if self.stopping {
            "stopping"
        } else if self.ready {
            "ok"
        } else {
            "starting"
        };
        format!(
            "{{\"status\":\"{status}\",\"ready\":{},\"uptime_ms\":{},\"seq\":{},\"commands\":{}}}",
            self.ready,
            self.uptime.as_millis(),
            self.feed_seq,
            self.commands
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shapes() {
        let mut s = Stats {
            uptime: Duration::from_millis(1_500),
            ready: true,
            rate: 2_000,
            seed: 42,
            commands: 3_000,
            feed_seq: 9,
            best_bid: Some(Level {
                price: Price(9_998),
                qty: Qty(40),
            }),
            last_trades: vec![TradePrint {
                n: 2,
                price: Price(10_001),
                qty: Qty(5),
                taker_side: Side::Buy,
            }],
            ..Stats::default()
        };
        let json = s.to_json();
        assert!(
            json.starts_with(
                "{\"uptime_ms\":1500,\"ready\":true,\"stopping\":false,\"rate\":2000,"
            ),
            "{json}"
        );
        assert!(json.contains("\"commands_per_sec\":2000.0,"), "{json}");
        assert!(
            json.ends_with(
                "\"best_bid\":[9998,40],\"best_ask\":null,\"last_trades\":[[2,10001,5,\"buy\"]]}"
            ),
            "{json}"
        );
        assert_eq!(
            s.health_json(),
            "{\"status\":\"ok\",\"ready\":true,\"uptime_ms\":1500,\"seq\":9,\"commands\":3000}"
        );
        s.stopping = true;
        assert!(s.health_json().starts_with("{\"status\":\"stopping\""));
    }
}
