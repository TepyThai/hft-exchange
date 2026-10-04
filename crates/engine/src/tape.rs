//! The recent-trades tape.
//!
//! Provided infrastructure (not part of a week's exercise): the engine
//! pushes every trade here so snapshots, market data and the visualizer can
//! show the last trades without replaying events.

use types::{OrderId, Price, Qty, Seq, Side};

/// One executed trade as kept on the tape — the fields of
/// [`Event::Trade`](types::Event::Trade) plus the engine sequence number of
/// the command that caused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TradePrint {
    /// [`Engine::seq`](crate::Engine::seq) of the taker's command.
    pub seq: Seq,
    pub taker: OrderId,
    pub maker: OrderId,
    pub taker_side: Side,
    /// The maker's resting price.
    pub price: Price,
    pub qty: Qty,
}

/// Fixed-capacity ring of the most recent trades; the oldest is overwritten
/// when full. Push is O(1) and never allocates after construction. A
/// capacity of 0 disables the tape.
#[derive(Clone, Debug)]
pub struct TradeTape {
    buf: Vec<TradePrint>,
    cap: usize,
    /// Index of the oldest print once the ring is full.
    head: usize,
}

impl TradeTape {
    /// An empty tape keeping the last `capacity` trades (allocates once).
    pub fn new(capacity: usize) -> TradeTape {
        TradeTape {
            buf: Vec::with_capacity(capacity),
            cap: capacity,
            head: 0,
        }
    }

    /// Appends a trade, evicting the oldest when full. O(1).
    #[inline]
    pub fn push(&mut self, print: TradePrint) {
        if self.buf.len() < self.cap {
            self.buf.push(print);
        } else if self.cap > 0 {
            self.buf[self.head] = print;
            self.head += 1;
            if self.head == self.cap {
                self.head = 0;
            }
        }
    }

    /// Trades on the tape, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &TradePrint> + '_ {
        self.buf[self.head..]
            .iter()
            .chain(self.buf[..self.head].iter())
    }

    /// The most recent trade.
    pub fn last(&self) -> Option<&TradePrint> {
        if self.head == 0 {
            self.buf.last()
        } else {
            self.buf.get(self.head - 1)
        }
    }

    /// Number of trades on the tape (≤ capacity).
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Forgets every trade, keeping the allocation.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.head = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print(n: u64) -> TradePrint {
        TradePrint {
            seq: Seq(n),
            taker: OrderId(n),
            maker: OrderId(0),
            taker_side: Side::Buy,
            price: Price(n as i64),
            qty: Qty(1),
        }
    }

    #[test]
    fn ring_keeps_the_last_n_in_order() {
        let mut t = TradeTape::new(3);
        assert!(t.last().is_none());
        for n in 1..=5 {
            t.push(print(n));
            assert_eq!(t.last().unwrap().seq, Seq(n));
        }
        let seqs: Vec<u64> = t.iter().map(|p| p.seq.0).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        assert_eq!(t.len(), 3);
        let mut off = TradeTape::new(0);
        off.push(print(1));
        assert!(off.is_empty());
    }
}
