//! The scenario runner: agents take turns, their commands go through a
//! [`Venue`], and what comes back is routed to them.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use super::agent::{Agent, Inventory};
use super::venue::{InProcess, Venue};
use marketdata::{BookBuilder, L2Snapshot};
use risk::Limits;
use types::{AccountId, InstrumentSpec, Price};
// Used only by the implementation (unused in the learner template).
#[allow(unused_imports)]
use {
    super::agent::{AgentCtx, Delivery, Outbox},
    marketdata::{L2Update, UpdateOutcome},
    types::{Command, Event, InstrumentId, RejectReason, Rng},
};

/// Bits of an order id below the agent's prefix: agent `i` (0-based) uses
/// ids `(i + 1) << AGENT_ID_SHIFT ..`, so ids never collide across agents.
pub const AGENT_ID_SHIFT: u32 = 40;

/// Static setup of a scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScenarioConfig {
    /// Seeds every agent's random number generator.
    pub seed: u64,
    /// The instrument traded (tick, lot, engine price band).
    pub spec: InstrumentSpec,
    /// Reference price for pre-trade risk and the engine's price band until
    /// the first trade.
    pub reference_price: Price,
    /// Pre-trade limits of every account without its own.
    pub default_limits: Limits,
    /// Capacity hint: resting orders to pre-allocate room for.
    pub expected_orders: usize,
}

impl Default for ScenarioConfig {
    /// Seed 1, the default instrument (tick 1, lot 1), reference 10 000,
    /// no limits, room for 4 096 orders.
    fn default() -> Self {
        ScenarioConfig {
            seed: 1,
            spec: InstrumentSpec::default(),
            reference_price: Price(10_000),
            default_limits: Limits::UNLIMITED,
            expected_orders: 4_096,
        }
    }
}

/// What one agent did in a run.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AgentReport {
    /// [`Agent::name`].
    pub name: String,
    /// The account it traded as.
    pub account: AccountId,
    /// Fills, position and cash.
    pub inventory: Inventory,
    /// New orders sent.
    pub orders: u64,
    /// Cancels and reduces sent.
    pub cancels: u64,
    /// Rejections received (engine and risk).
    pub rejects: u64,
    /// Of those, rejections by pre-trade risk (`Risk(_)`).
    pub risk_rejects: u64,
}

/// What a whole run did.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScenarioReport {
    /// Steps run.
    pub steps: u64,
    /// Commands sent by all agents.
    pub commands: u64,
    /// Deliveries routed to agents.
    pub deliveries: u64,
    /// Trades (each counted once).
    pub trades: u64,
    /// Traded quantity (each trade counted once).
    pub volume: u64,
    /// Market-data updates applied.
    pub md_updates: u64,
    /// Per agent, in the order they were added.
    pub agents: Vec<AgentReport>,
    /// The book as the market-data feed shows it at the end.
    pub book: L2Snapshot,
}

/// Runs agents against a [`Venue`] in deterministic turns.
///
/// A [`step`](Runner::step) gives every agent one turn, starting with agent
/// `step % n` so no agent always goes first. A turn is: the agent
/// [`act`](Agent::act)s on the current book, its commands are sent in
/// order, the venue is flushed, the deliveries are routed to their owners
/// (updating their [`Inventory`]) and the market data is applied to the
/// shared [`BookBuilder`] — so the next agent sees the book as it is now.
///
/// Agent `i` (in the order added) trades as `AccountId(i + 1)` with order
/// ids from `(i + 1) << AGENT_ID_SHIFT`. Everything is deterministic for a
/// seed. Cost per command: the venue's, plus O(1) routing.
pub struct Runner<V: Venue = InProcess> {
    // TODO(w23): design this type's fields.
    _todo: std::marker::PhantomData<(V,)>,
}

// TODO(w23): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl Runner<InProcess> {
    /// A runner over a fresh in-process exchange built from `cfg`.
    pub fn new(cfg: ScenarioConfig) -> Runner<InProcess> {
        todo!("w23: new")
    }
}

#[allow(unused_variables)]
impl<V: Venue> Runner<V> {
    /// A runner over any venue (e.g. a network gateway client) trading
    /// `cfg.spec.id` with agents seeded from `cfg.seed` (the venue's own
    /// setup is the venue's business). Starts the book from the venue's
    /// market-data snapshot.
    pub fn with_venue(venue: V, cfg: &ScenarioConfig) -> Runner<V> {
        todo!("w23: with_venue")
    }

    /// Adds an agent and returns its account. Its random number generator
    /// is seeded from the scenario seed and its position in the list.
    /// O(1) amortized.
    pub fn add_agent(&mut self, agent: Box<dyn Agent>) -> AccountId {
        todo!("w23: add_agent")
    }

    /// Runs one step: every agent gets one turn. O(agents + commands sent
    /// + their events); no allocation once buffers have grown.
    pub fn step(&mut self) {
        todo!("w23: step")
    }

    /// Runs `steps` steps (see [`step`](Runner::step)).
    pub fn run(&mut self, steps: u64) {
        todo!("w23: run")
    }

    /// Steps run so far (the `now` agents see).
    pub fn now(&self) -> u64 {
        todo!("w23: now")
    }

    /// The book as the market-data feed shows it.
    pub fn book(&self) -> &BookBuilder {
        todo!("w23: book")
    }

    /// The venue (e.g. `InProcess::engine` for checks).
    pub fn venue(&self) -> &V {
        todo!("w23: venue")
    }

    /// The venue, mutably (e.g. to set limits or engage a kill switch).
    pub fn venue_mut(&mut self) -> &mut V {
        todo!("w23: venue_mut")
    }

    /// Number of agents.
    pub fn agent_count(&self) -> usize {
        todo!("w23: agent_count")
    }

    /// Fills, position and cash of the agent trading as `account`. O(1).
    pub fn inventory(&self, account: AccountId) -> Option<Inventory> {
        todo!("w23: inventory")
    }

    /// Counters so far. O(agents + book levels); allocates.
    pub fn report(&self) -> ScenarioReport {
        todo!("w23: report")
    }
}

// TODO(w23): implement — see the week guide on tepy.dev/hft.
