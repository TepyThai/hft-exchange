//! WebAssembly bindings to the matching engine (live demo on tepy.dev/hft/live).
//!
//! A tiny C ABI with no JS glue, so `cargo xtask wasm` needs nothing beyond
//! the `wasm32-unknown-unknown` target: commands go in as scalars, state
//! comes out as JSON in a buffer inside wasm memory (`hx_state` returns its
//! length, `hx_buf` its address). The page's loader is
//! `app/hft/live/engineWasm.ts` in the tepy.dev repo.
//!
//! Built on [`sim::live::LiveSession`], so it runs whichever engine the
//! workspace has: the reference, or yours once weeks 6–8 pass.

use engine::OrderBook as _;
use sim::live::{LiveSession, UserKind};
use std::cell::RefCell;
use std::fmt::Write as _;
use types::{OrderId, Side};

thread_local! {
    static SESSION: RefCell<Option<LiveSession>> = const { RefCell::new(None) };
    static OUT: RefCell<String> = const { RefCell::new(String::new()) };
}

fn with<R>(f: impl FnOnce(&mut LiveSession) -> R) -> R {
    SESSION.with(|s| {
        let mut s = s.borrow_mut();
        f(s.get_or_insert_with(|| LiveSession::new(1)))
    })
}

/// Start a fresh session (new engine, new flow) from `seed`.
#[no_mangle]
pub extern "C" fn hx_init(seed: u64) {
    SESSION.with(|s| *s.borrow_mut() = Some(LiveSession::new(seed)));
}

/// Process `n` simulated commands; returns the number of trades.
#[no_mangle]
pub extern "C" fn hx_flow(n: u32) -> u32 {
    with(|s| s.run(n as usize))
}

/// Send a user order. side: 0 buy, 1 sell. kind: 0 limit, 1 IOC, 2 market.
/// Returns the order id.
#[no_mangle]
pub extern "C" fn hx_submit(side: u32, kind: u32, price: i64, qty: u64) -> u64 {
    let side = if side == 0 { Side::Buy } else { Side::Sell };
    let kind = match kind {
        0 => UserKind::Limit,
        1 => UserKind::Ioc,
        _ => UserKind::Market,
    };
    with(|s| s.submit(side, kind, price, qty).0)
}

/// Cancel a resting user order; 1 if it was cancelled.
#[no_mangle]
pub extern "C" fn hx_cancel(id: u64) -> u32 {
    with(|s| s.cancel(OrderId(id)) as u32)
}

/// Serialize the session (top `depth` levels per side, last `trades`
/// trades) as JSON into the output buffer; returns its length in bytes.
#[no_mangle]
pub extern "C" fn hx_state(depth: u32, trades: u32) -> u32 {
    OUT.with(|out| {
        let mut out = out.borrow_mut();
        out.clear();
        with(|s| write_state(s, depth as usize, trades as usize, &mut out));
        out.len() as u32
    })
}

/// Address of the output buffer written by [`hx_state`].
#[no_mangle]
pub extern "C" fn hx_buf() -> *const u8 {
    OUT.with(|out| out.borrow().as_ptr())
}

fn side_num(side: Side) -> u8 {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

/// The JSON shape is `EngineState` in engineWasm.ts.
pub fn write_state(s: &LiveSession, depth: usize, trades: usize, out: &mut String) {
    let snap = s.engine().snapshot(depth);
    let stats = s.stats();
    let user = s.user();
    let _ = write!(out, "{{\"seq\":{},\"last\":", snap.seq.0);
    match snap.last_price {
        Some(p) => {
            let _ = write!(out, "{}", p.0);
        }
        None => out.push_str("null"),
    }
    for (name, levels) in [("bids", &snap.bids), ("asks", &snap.asks)] {
        let _ = write!(out, ",\"{name}\":[");
        for (i, l) in levels.iter().enumerate() {
            let _ = write!(out, "{}[{},{},{}]", if i > 0 { "," } else { "" }, l.price.0, l.qty.0, l.orders);
        }
        out.push(']');
    }
    out.push_str(",\"trades\":[");
    for (i, t) in snap.trades.iter().rev().take(trades).enumerate() {
        let n = stats.trades.saturating_sub(i as u64);
        let _ = write!(out, "{}[{},{},{},{}]", if i > 0 { "," } else { "" }, n, t.price.0, t.qty.0, side_num(t.taker_side));
    }
    out.push_str("],\"mine\":{\"open\":[");
    for (i, o) in user.open_orders().iter().enumerate() {
        let _ = write!(out, "{}[{},{},{},{}]", if i > 0 { "," } else { "" }, o.id.0, side_num(o.side), o.price.0, o.remaining.0);
    }
    out.push_str("],\"fills\":[");
    for (i, f) in user.fills().take(20).enumerate() {
        let _ = write!(out, "{}[{},{},{},{}]", if i > 0 { "," } else { "" }, f.id.0, side_num(f.side), f.price.0, f.qty.0);
    }
    out.push_str("],\"rejects\":[");
    for (i, (_, reason)) in user.rejects().take(3).enumerate() {
        let _ = write!(out, "{}\"{reason:?}\"", if i > 0 { "," } else { "" });
    }
    let _ = write!(
        out,
        "],\"position\":{},\"cash\":{}}},\"stats\":{{\"commands\":{},\"trades\":{},\"volume\":{},\"resting\":{}}}}}",
        user.position,
        user.cash,
        stats.commands,
        stats.trades,
        stats.volume,
        s.engine().book().len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_json_shaped() {
        hx_init(5);
        assert!(hx_flow(3_000) > 0);
        hx_submit(0, 0, 9_000, 4);
        let len = hx_state(5, 10) as usize;
        let json = OUT.with(|o| o.borrow().clone());
        assert_eq!(json.len(), len);
        assert!(json.starts_with("{\"seq\":") && json.ends_with("}}"));
        assert!(json.contains("\"open\":[[") && json.contains("\"stats\":{\"commands\":3001"));
        // Balanced brackets as a cheap well-formedness check.
        let depth = json.chars().fold(0i32, |d, c| match c {
            '{' | '[' => d + 1,
            '}' | ']' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}
