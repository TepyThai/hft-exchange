//! Week 37: compile-time specialization — risk policies and capacities as
//! type parameters.
//!
//! * [`RiskPolicy`] — the pre-trade check the order path runs before the
//!   matching engine. [`NoRisk`] (nothing), [`MaxOrderQty`] (a limit fixed
//!   at compile time) and [`risk::RiskEngine`] (the w15 pre-trade engine)
//!   implement it.
//! * [`Guarded`] — an [`Engine`] behind a policy, generic over both the book
//!   and the policy: with concrete types, every book call and policy hook
//!   is a direct (inlinable) call and `NoRisk` compiles to nothing;
//!   [`DynGuarded`] is the same thing through `Box<dyn …>`.
//! * [`TapeRing`] — a trade tape whose capacity is a const generic: the
//!   capacity check is a compile-time assertion and wrap-around a mask.
//!
//! The w37 benchmark runs the same flow through `Guarded<Book, _>` and
//! `DynGuarded` and reports the throughput ratio.

use crate::tape::TradePrint;
use crate::Engine;
use book::{Book, OrderBook};
#[allow(unused_imports)] // used by the reference implementation
use types::RiskReason;
use types::{Command, Event, NewOrder, Price, RejectReason};

/// A pre-trade risk policy: consulted for every new order **before** the
/// matching engine sees it, and told what happened afterwards.
///
/// A rejected order never reaches the engine (it gets no engine sequence
/// number); its only event is `Rejected { reason }`, and the policy is not
/// shown that event. Cancels and reduces are never checked.
///
/// Implement it for a concrete type and use it as a type parameter
/// ([`Guarded<B, R>`](Guarded)) to have the compiler inline it, or box it
/// (`Box<dyn RiskPolicy>`) to choose a policy at run time.
pub trait RiskPolicy {
    /// May `order` go to the engine? `reference` is the price to value it
    /// at (the engine's band reference — last trade, else the configured
    /// reference — else the order's own limit, else 0). Read-only.
    fn check(&self, order: &NewOrder, reference: Price) -> Result<(), RejectReason>;

    /// `order` passed [`check`](RiskPolicy::check) and is being sent to the
    /// engine (count it as open). Default: nothing.
    #[inline]
    fn on_accept(&mut self, order: &NewOrder) {
        let _ = order;
    }

    /// One event the engine produced, in order. Default: nothing.
    #[inline]
    fn on_event(&mut self, event: &Event) {
        let _ = event;
    }
}

/// No pre-trade checks at all. As a type parameter every hook compiles to
/// nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoRisk;

impl RiskPolicy for NoRisk {
    #[inline(always)]
    fn check(&self, _order: &NewOrder, _reference: Price) -> Result<(), RejectReason> {
        Ok(())
    }
}

/// A fat-finger limit known at compile time: orders for more than `MAX`
/// lots are rejected with `Risk(MaxOrderQty)`. Monomorphized, the check is
/// one compare against an immediate. Stateless; O(1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaxOrderQty<const MAX: u64>;

// @student w37
impl<const MAX: u64> RiskPolicy for MaxOrderQty<MAX> {
    #[inline]
    fn check(&self, order: &NewOrder, _reference: Price) -> Result<(), RejectReason> {
        if order.qty.0 > MAX {
            Err(RejectReason::Risk(RiskReason::MaxOrderQty))
        } else {
            Ok(())
        }
    }
}

/// The w15 pre-trade risk engine as a policy: `check` is
/// [`RiskEngine::check`](risk::RiskEngine::check), `on_accept` records the
/// order as open ([`on_new`](risk::RiskEngine::on_new)) and `on_event`
/// keeps exposure in sync ([`on_event`](risk::RiskEngine::on_event); the
/// fills it returns are dropped — position keeping is w45's).
// @student w37
impl RiskPolicy for risk::RiskEngine {
    #[inline]
    fn check(&self, order: &NewOrder, reference: Price) -> Result<(), RejectReason> {
        risk::RiskEngine::check(self, order, reference)
    }

    #[inline]
    fn on_accept(&mut self, order: &NewOrder) {
        self.on_new(order);
    }

    #[inline]
    fn on_event(&mut self, event: &Event) {
        risk::RiskEngine::on_event(self, event);
    }
}

/// Boxed policies are policies (dynamic dispatch: one indirect call per
/// hook).
impl<R: RiskPolicy + ?Sized> RiskPolicy for Box<R> {
    #[inline]
    fn check(&self, order: &NewOrder, reference: Price) -> Result<(), RejectReason> {
        (**self).check(order, reference)
    }

    #[inline]
    fn on_accept(&mut self, order: &NewOrder) {
        (**self).on_accept(order)
    }

    #[inline]
    fn on_event(&mut self, event: &Event) {
        (**self).on_event(event)
    }
}

/// A matching engine behind a pre-trade risk policy — the order path from
/// the risk check to the events — generic over the book `B` and the policy
/// `R`.
///
/// For a new order: `risk.check` (a failure is the single event
/// `Rejected`, the engine is untouched), then `risk.on_accept`, then
/// [`Engine::process`], then `risk.on_event` for each event the engine
/// appended. Cancels and reduces go straight to the engine, and their
/// events to `risk.on_event`.
///
/// `Guarded<Book, NoRisk>` behaves exactly like `Engine<Book>`. Cost: the
/// engine's, plus the policy's hooks — nothing at all for `NoRisk`.
// @student w37
#[derive(Clone, Debug)]
pub struct Guarded<B: OrderBook = Book, R: RiskPolicy = NoRisk> {
    engine: Engine<B>,
    risk: R,
}

/// The fully dynamic configuration: book and policy behind trait objects.
pub type DynGuarded = Guarded<Box<dyn OrderBook>, Box<dyn RiskPolicy>>;

// @student w37
impl<B: OrderBook, R: RiskPolicy> Guarded<B, R> {
    /// Puts `engine` behind `risk`.
    pub fn new(engine: Engine<B>, risk: R) -> Guarded<B, R> {
        Guarded { engine, risk }
    }

    /// Processes one command, **appending** its events to `out` (see the
    /// type docs for the order of hooks). Never allocates beyond `out`'s
    /// growth and the policy's own.
    #[inline]
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        if let Command::New(order) = cmd {
            let reference = self
                .engine
                .reference_price()
                .or(order.limit_price())
                .unwrap_or_default();
            if let Err(reason) = self.risk.check(order, reference) {
                out.push(Event::Rejected {
                    id: order.id,
                    reason,
                });
                return;
            }
            self.risk.on_accept(order);
        }
        let start = out.len();
        self.engine.process(cmd, out);
        for event in &out[start..] {
            self.risk.on_event(event);
        }
    }

    /// The engine (read-only).
    pub fn engine(&self) -> &Engine<B> {
        &self.engine
    }

    /// The policy.
    pub fn risk(&self) -> &R {
        &self.risk
    }

    /// The policy, mutably (e.g. to change limits or flip a kill switch).
    pub fn risk_mut(&mut self) -> &mut R {
        &mut self.risk
    }

    /// Takes the engine and the policy apart.
    pub fn into_parts(self) -> (Engine<B>, R) {
        (self.engine, self.risk)
    }
}

/// A ring of the last `N` trades, `N` fixed at compile time.
///
/// The same behaviour as [`TradeTape`](crate::TradeTape) (oldest evicted
/// when full), but `N` must be a power of two — checked when the type is
/// instantiated, not at run time — so the write position wraps with a mask,
/// and the buffer is an inline array: no allocation, no capacity field.
/// Push is O(1) and branch-free.
///
/// ```
/// # use engine::TapeRing;
/// let tape = TapeRing::<64>::new();
/// assert_eq!(TapeRing::<64>::CAPACITY, 64);
/// assert!(tape.is_empty());
/// ```
///
/// ```compile_fail
/// # use engine::TapeRing;
/// let n = TapeRing::<48>::CAPACITY; // not a power of two: compile error
/// ```
// @student w37
#[derive(Clone, Debug)]
pub struct TapeRing<const N: usize> {
    buf: [TradePrint; N],
    /// Total trades ever pushed; the next write goes to `pushed & (N - 1)`.
    pushed: u64,
}

impl<const N: usize> TapeRing<N> {
    /// The ring's capacity, `N`. Using it with an `N` that is not a power
    /// of two (or zero) is a compile error.
    pub const CAPACITY: usize = {
        assert!(
            N.is_power_of_two(),
            "TapeRing capacity must be a power of two"
        );
        N
    };
}

// @student w37
impl<const N: usize> TapeRing<N> {
    /// An empty ring.
    pub fn new() -> TapeRing<N> {
        // Checked when `new` is instantiated for this `N`, not at run time.
        const {
            assert!(
                N.is_power_of_two(),
                "TapeRing capacity must be a power of two"
            )
        };
        TapeRing {
            buf: [TradePrint {
                seq: types::Seq(0),
                taker: types::OrderId(0),
                maker: types::OrderId(0),
                taker_side: types::Side::Buy,
                price: Price(0),
                qty: types::Qty(0),
            }; N],
            pushed: 0,
        }
    }

    /// Appends a trade, overwriting the oldest when full. O(1), no branch
    /// on fullness.
    #[inline]
    pub fn push(&mut self, print: TradePrint) {
        self.buf[(self.pushed as usize) & (N - 1)] = print;
        self.pushed += 1;
    }

    /// Number of trades held (≤ `N`).
    #[inline]
    pub fn len(&self) -> usize {
        self.pushed.min(N as u64) as usize
    }

    /// True if no trade was ever pushed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pushed == 0
    }

    /// The most recent trade.
    pub fn last(&self) -> Option<&TradePrint> {
        (self.pushed > 0).then(|| &self.buf[((self.pushed - 1) as usize) & (N - 1)])
    }

    /// The trades held, oldest first: the ring as its two contiguous runs
    /// (from the oldest to the end of the array, then from the start).
    pub fn iter(&self) -> TapeIter<'_> {
        let len = self.len();
        let head = if len < N {
            0
        } else {
            (self.pushed as usize) & (N - 1)
        };
        let (wrapped, oldest) = self.buf[..len].split_at(head);
        oldest.iter().chain(wrapped.iter())
    }

    /// Forgets every trade.
    pub fn clear(&mut self) {
        self.pushed = 0;
    }
}

/// Iterator over a [`TapeRing`], oldest trade first (double-ended).
pub type TapeIter<'a> =
    std::iter::Chain<std::slice::Iter<'a, TradePrint>, std::slice::Iter<'a, TradePrint>>;

impl<const N: usize> Default for TapeRing<N> {
    fn default() -> Self {
        TapeRing::new()
    }
}
