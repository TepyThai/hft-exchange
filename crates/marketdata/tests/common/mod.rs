//! Helpers shared by the week 16, 29 and 46 tests: a deterministic,
//! realistic L2 feed and a reference book.
#![allow(dead_code)]

use marketdata::{L2Publisher, L2Snapshot, L2Update, Level};
use std::collections::BTreeMap;
use types::{Event, OrderId, Price, Qty, Rng, Seq, Side};

/// Generates a consistent stream of book-changing engine events for one
/// instrument: orders resting near a drifting mid (geometric-ish distance
/// from the touch, with a thin far tail), cancels, partial and full fills,
/// reduces. Keeps its own model of resting orders so every event refers
/// to a live order with its true remaining quantity.
pub struct EventGen {
    rng: Rng,
    live: Vec<(OrderId, Side, Price, Qty)>,
    /// Orders the mid moved through, to be cancelled (keeps the book
    /// uncrossed, as a matcher would).
    crossed: Vec<(OrderId, Qty)>,
    next_id: u64,
    target: usize,
    pub mid: i64,
    /// Probability per event that the mid moves by ±1.
    pub drift: f64,
    /// Probability that a new order lands far (100–5000 ticks) away.
    pub far: f64,
}

impl EventGen {
    pub fn new(seed: u64, target: usize) -> EventGen {
        EventGen {
            rng: Rng::new(seed),
            live: Vec::with_capacity(target * 2),
            crossed: Vec::new(),
            next_id: 0,
            target,
            mid: 10_000,
            drift: 0.0,
            far: 0.0,
        }
    }

    pub fn next(&mut self) -> Event {
        if let Some((id, remaining)) = self.crossed.pop() {
            return Event::Cancelled { id, remaining };
        }
        if self.drift > 0.0 && self.rng.chance(self.drift) {
            self.mid += if self.rng.next_u64() & 1 == 0 { 1 } else { -1 };
            let mid = self.mid;
            let crossed = &mut self.crossed;
            self.live.retain(|&(id, side, price, qty)| {
                let cross = match side {
                    Side::Buy => price.0 >= mid,
                    Side::Sell => price.0 <= mid,
                };
                if cross {
                    crossed.push((id, qty));
                }
                !cross
            });
            if let Some((id, remaining)) = self.crossed.pop() {
                return Event::Cancelled { id, remaining };
            }
        }
        let r = self.rng.range(0, 99);
        let add_threshold = if self.live.len() < self.target {
            55
        } else {
            40
        };
        if self.live.is_empty() || r < add_threshold {
            self.next_id += 1;
            let id = OrderId(self.next_id);
            let side = self.rng.side();
            let dist = if self.far > 0.0 && self.rng.chance(self.far) {
                self.rng.range(100, 5_000) as i64
            } else {
                self.rng.range(0, 29).min(self.rng.range(0, 29)) as i64
            };
            let price = match side {
                Side::Buy => Price(self.mid - 1 - dist),
                Side::Sell => Price(self.mid + 1 + dist),
            };
            let qty = Qty(self.rng.range(1, 200));
            self.live.push((id, side, price, qty));
            return Event::Rested {
                id,
                side,
                price,
                qty,
            };
        }
        let i = self.rng.range(0, self.live.len() as u64 - 1) as usize;
        let (id, side, price, qty) = self.live[i];
        if r < 85 {
            self.live.swap_remove(i);
            Event::Cancelled { id, remaining: qty }
        } else if r < 95 || qty.0 < 2 {
            let fill = Qty(self.rng.range(1, qty.0));
            self.next_id += 1;
            if fill == qty {
                self.live.swap_remove(i);
            } else {
                self.live[i].3 = qty - fill;
            }
            Event::Trade {
                taker: OrderId(self.next_id),
                maker: id,
                taker_side: side.opposite(),
                price,
                qty: fill,
            }
        } else {
            let remaining = Qty(self.rng.range(1, qty.0 - 1));
            self.live[i].3 = remaining;
            Event::Reduced { id, remaining }
        }
    }
}

/// A recorded feed: every update in order, plus a full snapshot after
/// every `snapshot_every` updates (and the publisher's final book).
pub struct Feed {
    pub updates: Vec<L2Update>,
    /// `(index into updates after which it was taken, snapshot)`.
    pub snapshots: Vec<(usize, L2Snapshot)>,
    pub last: L2Snapshot,
}

/// Runs `gen` until `n` updates were published.
pub fn feed_from(mut gen: EventGen, n: usize, snapshot_every: u64) -> Feed {
    let mut p = L2Publisher::with_snapshot_interval(snapshot_every);
    let mut updates = Vec::with_capacity(n);
    let mut snapshots = Vec::new();
    while updates.len() < n {
        if let Some(u) = p.on_event(&gen.next()) {
            updates.push(u);
            if let Some(s) = p.periodic_snapshot() {
                snapshots.push((updates.len(), s));
            }
        }
    }
    Feed {
        updates,
        snapshots,
        last: p.snapshot(),
    }
}

/// `n` updates from a book of ~`target` orders around 10,000.
pub fn feed(seed: u64, n: usize, target: usize, snapshot_every: u64) -> Feed {
    feed_from(EventGen::new(seed, target), n, snapshot_every)
}

pub fn upd(seq: u64, side: Side, price: i64, qty: u64) -> L2Update {
    L2Update {
        seq: Seq(seq),
        side,
        price: Price(price),
        qty: Qty(qty),
    }
}

pub fn level(price: i64, qty: u64) -> Level {
    Level {
        price: Price(price),
        qty: Qty(qty),
    }
}

/// The obvious book: an ordered map per side, updates set levels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefBook {
    pub seq: u64,
    pub bids: BTreeMap<i64, u64>,
    pub asks: BTreeMap<i64, u64>,
}

impl RefBook {
    pub fn side_mut(&mut self, side: Side) -> &mut BTreeMap<i64, u64> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    pub fn set(&mut self, side: Side, price: i64, qty: u64) {
        if qty == 0 {
            self.side_mut(side).remove(&price);
        } else {
            self.side_mut(side).insert(price, qty);
        }
    }

    pub fn apply(&mut self, u: &L2Update) {
        self.set(u.side, u.price.0, u.qty.0);
        self.seq = self.seq.max(u.seq.0);
    }

    pub fn load(&mut self, s: &L2Snapshot) {
        self.bids = s.bids.iter().map(|l| (l.price.0, l.qty.0)).collect();
        self.asks = s.asks.iter().map(|l| (l.price.0, l.qty.0)).collect();
        self.seq = s.seq.0;
    }

    pub fn snapshot(&self) -> L2Snapshot {
        L2Snapshot {
            seq: Seq(self.seq),
            bids: self.bids.iter().rev().map(|(&p, &q)| level(p, q)).collect(),
            asks: self.asks.iter().map(|(&p, &q)| level(p, q)).collect(),
        }
    }
}
