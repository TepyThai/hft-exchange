//! JSON encoding of the L2 feed for browsers (the WebSocket adapter's
//! message format). Integers only — prices in ticks, quantities in lots —
//! so nothing is lost in a JavaScript `number` below 2⁵³.
//!
//! ```text
//! update    {"seq":7,"side":"buy","price":10000,"qty":25}
//! snapshot  {"type":"snapshot","seq":6,"bids":[[10000,15],[9999,40]],"asks":[[10001,8]]}
//! batch     {"type":"updates","seq":9,"updates":[<update>,<update>,…]}
//! ```
//!
//! * A **snapshot** replaces the client's book: levels best first, as
//!   `[price, qty]` pairs. It reflects every update up to `seq`.
//! * A **batch** carries updates to apply in order (`qty` 0 removes the
//!   level). Batches may be conflated — not every sequence number appears
//!   — but after applying a whole batch the client's book equals the
//!   publisher's as of the batch's `seq` (the highest in it). Only trust
//!   the book between batches.
//!
//! Writing appends to a `String` and allocates only when it grows.

use crate::{L2Snapshot, L2Update, Level};
use std::fmt::Write as _;
use types::{Seq, Side};

/// Appends one update as a JSON object. O(1).
pub fn write_update_json(u: &L2Update, out: &mut String) {
    let side = match u.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    // Writing into a String cannot fail.
    let _ = write!(
        out,
        "{{\"seq\":{},\"side\":\"{side}\",\"price\":{},\"qty\":{}}}",
        u.seq.0, u.price.0, u.qty.0
    );
}

/// Appends a snapshot message. O(levels).
pub fn write_snapshot_json(snap: &L2Snapshot, out: &mut String) {
    fn levels(ls: &[Level], out: &mut String) {
        out.push('[');
        for (i, l) in ls.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "[{},{}]", l.price.0, l.qty.0);
        }
        out.push(']');
    }
    let _ = write!(
        out,
        "{{\"type\":\"snapshot\",\"seq\":{},\"bids\":",
        snap.seq.0
    );
    levels(&snap.bids, out);
    out.push_str(",\"asks\":");
    levels(&snap.asks, out);
    out.push('}');
}

/// Appends a batch message whose book is consistent as of `seq` once all
/// of `updates` are applied. O(updates).
pub fn write_batch_json(seq: Seq, updates: &[L2Update], out: &mut String) {
    let _ = write!(
        out,
        "{{\"type\":\"updates\",\"seq\":{},\"updates\":[",
        seq.0
    );
    for (i, u) in updates.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_update_json(u, out);
    }
    out.push_str("]}");
}
