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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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

#[allow(unused_variables)]
impl<const MAX: u64> RiskPolicy for MaxOrderQty<MAX> {
    #[inline]
    fn check(&self, order: &NewOrder, _reference: Price) -> Result<(), RejectReason> {
        todo!("w37: check")
    }
}

/// The w15 pre-trade risk engine as a policy: `check` is
/// [`RiskEngine::check`](risk::RiskEngine::check), `on_accept` records the
/// order as open ([`on_new`](risk::RiskEngine::on_new)) and `on_event`
/// keeps exposure in sync ([`on_event`](risk::RiskEngine::on_event); the
/// fills it returns are dropped — position keeping is w45's).
#[allow(unused_variables)]
impl RiskPolicy for risk::RiskEngine {
    #[inline]
    fn check(&self, order: &NewOrder, reference: Price) -> Result<(), RejectReason> {
        todo!("w37: check")
    }

    #[inline]
    fn on_accept(&mut self, order: &NewOrder) {
        todo!("w37: on_accept")
    }

    #[inline]
    fn on_event(&mut self, event: &Event) {
        todo!("w37: on_event")
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
#[derive(Clone, Debug)]
pub struct Guarded<B: OrderBook = Book, R: RiskPolicy = NoRisk> {
    // TODO(w37): design this type's fields.
    _todo: std::marker::PhantomData<(B, R,)>,
}

/// The fully dynamic configuration: book and policy behind trait objects.
pub type DynGuarded = Guarded<Box<dyn OrderBook>, Box<dyn RiskPolicy>>;

#[allow(unused_variables)]
impl<B: OrderBook, R: RiskPolicy> Guarded<B, R> {
    /// Puts `engine` behind `risk`.
    pub fn new(engine: Engine<B>, risk: R) -> Guarded<B, R> {
        todo!("w37: new")
    }

    /// Processes one command, **appending** its events to `out` (see the
    /// type docs for the order of hooks). Never allocates beyond `out`'s
    /// growth and the policy's own.
    #[inline]
    pub fn process(&mut self, cmd: &Command, out: &mut Vec<Event>) {
        todo!("w37: process")
    }

    /// The engine (read-only).
    pub fn engine(&self) -> &Engine<B> {
        todo!("w37: engine")
    }

    /// The policy.
    pub fn risk(&self) -> &R {
        todo!("w37: risk")
    }

    /// The policy, mutably (e.g. to change limits or flip a kill switch).
    pub fn risk_mut(&mut self) -> &mut R {
        todo!("w37: risk_mut")
    }

    /// Takes the engine and the policy apart.
    pub fn into_parts(self) -> (Engine<B>, R) {
        todo!("w37: into_parts")
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
#[derive(Clone, Debug)]
pub struct TapeRing<const N: usize> {
    // TODO(w37): design this type's fields.
    _todo: (),
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

#[allow(unused_variables)]
impl<const N: usize> TapeRing<N> {
    /// An empty ring.
    pub fn new() -> TapeRing<N> {
        todo!("w37: new")
    }

    /// Appends a trade, overwriting the oldest when full. O(1), no branch
    /// on fullness.
    #[inline]
    pub fn push(&mut self, print: TradePrint) {
        todo!("w37: push")
    }

    /// Number of trades held (≤ `N`).
    #[inline]
    pub fn len(&self) -> usize {
        todo!("w37: len")
    }

    /// True if no trade was ever pushed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        todo!("w37: is_empty")
    }

    /// The most recent trade.
    pub fn last(&self) -> Option<&TradePrint> {
        todo!("w37: last")
    }

    /// The trades held, oldest first: the ring as its two contiguous runs
    /// (from the oldest to the end of the array, then from the start).
    pub fn iter(&self) -> TapeIter<'_> {
        todo!("w37: iter")
    }

    /// Forgets every trade.
    pub fn clear(&mut self) {
        todo!("w37: clear")
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
