//! The scenario runner: agents take turns, their commands go through a
//! [`Venue`], and what comes back is routed to them.

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
// @student w23
pub struct Runner<V: Venue = InProcess> {
    venue: V,
    seed: u64,
    instrument: InstrumentId,
    agents: Vec<Slot>,
    book: BookBuilder,
    now: u64,
    deliveries: Vec<Delivery>,
    updates: Vec<L2Update>,
    commands: u64,
    delivered: u64,
    trades: u64,
    volume: u64,
    md_updates: u64,
}

// @student-begin w23
/// One agent and what the runner keeps for it.
struct Slot {
    agent: Box<dyn Agent>,
    account: AccountId,
    rng: Rng,
    outbox: Outbox,
    inventory: Inventory,
    orders: u64,
    cancels: u64,
    rejects: u64,
    risk_rejects: u64,
}
// @student-end

// @student w23
impl Runner<InProcess> {
    /// A runner over a fresh in-process exchange built from `cfg`.
    pub fn new(cfg: ScenarioConfig) -> Runner<InProcess> {
        Runner::with_venue(InProcess::new(&cfg), &cfg)
    }
}

// @student w23
impl<V: Venue> Runner<V> {
    /// A runner over any venue (e.g. a network gateway client) trading
    /// `cfg.spec.id` with agents seeded from `cfg.seed` (the venue's own
    /// setup is the venue's business). Starts the book from the venue's
    /// market-data snapshot.
    pub fn with_venue(venue: V, cfg: &ScenarioConfig) -> Runner<V> {
        let mut venue = venue;
        let mut book = BookBuilder::new();
        book.apply_snapshot(&venue.market_snapshot());
        Runner {
            venue,
            seed: cfg.seed,
            instrument: cfg.spec.id,
            agents: Vec::new(),
            book,
            now: 0,
            deliveries: Vec::with_capacity(64),
            updates: Vec::with_capacity(64),
            commands: 0,
            delivered: 0,
            trades: 0,
            volume: 0,
            md_updates: 0,
        }
    }

    /// Adds an agent and returns its account. Its random number generator
    /// is seeded from the scenario seed and its position in the list.
    /// O(1) amortized.
    pub fn add_agent(&mut self, agent: Box<dyn Agent>) -> AccountId {
        let i = self.agents.len() as u64;
        let account = AccountId(i as u32 + 1);
        self.agents.push(Slot {
            agent,
            account,
            rng: Rng::new(self.seed ^ (i + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            outbox: Outbox::new(account, self.instrument, (i + 1) << AGENT_ID_SHIFT),
            inventory: Inventory::default(),
            orders: 0,
            cancels: 0,
            rejects: 0,
            risk_rejects: 0,
        });
        account
    }

    /// Runs one step: every agent gets one turn. O(agents + commands sent
    /// + their events); no allocation once buffers have grown.
    pub fn step(&mut self) {
        let n = self.agents.len();
        if n > 0 {
            let start = (self.now % n as u64) as usize;
            for k in 0..n {
                self.turn((start + k) % n);
            }
        }
        self.now += 1;
    }

    /// Runs `steps` steps (see [`step`](Runner::step)).
    pub fn run(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
    }

    /// Steps run so far (the `now` agents see).
    pub fn now(&self) -> u64 {
        self.now
    }

    /// The book as the market-data feed shows it.
    pub fn book(&self) -> &BookBuilder {
        &self.book
    }

    /// The venue (e.g. `InProcess::engine` for checks).
    pub fn venue(&self) -> &V {
        &self.venue
    }

    /// The venue, mutably (e.g. to set limits or engage a kill switch).
    pub fn venue_mut(&mut self) -> &mut V {
        &mut self.venue
    }

    /// Number of agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Fills, position and cash of the agent trading as `account`. O(1).
    pub fn inventory(&self, account: AccountId) -> Option<Inventory> {
        self.slot(account).map(|i| self.agents[i].inventory)
    }

    /// Counters so far. O(agents + book levels); allocates.
    pub fn report(&self) -> ScenarioReport {
        ScenarioReport {
            steps: self.now,
            commands: self.commands,
            deliveries: self.delivered,
            trades: self.trades,
            volume: self.volume,
            md_updates: self.md_updates,
            agents: self
                .agents
                .iter()
                .map(|s| AgentReport {
                    name: s.agent.name().to_string(),
                    account: s.account,
                    inventory: s.inventory,
                    orders: s.orders,
                    cancels: s.cancels,
                    rejects: s.rejects,
                    risk_rejects: s.risk_rejects,
                })
                .collect(),
            book: self.book.snapshot(),
        }
    }
}

// @student-begin w23
impl<V: Venue> Runner<V> {
    fn slot(&self, account: AccountId) -> Option<usize> {
        let i = (account.0 as usize).checked_sub(1)?;
        (i < self.agents.len()).then_some(i)
    }

    /// One agent's turn: act, send, flush, route.
    fn turn(&mut self, i: usize) {
        let slot = &mut self.agents[i];
        let mut ctx = AgentCtx {
            now: self.now,
            account: slot.account,
            book: &self.book,
            inventory: &slot.inventory,
            rng: &mut slot.rng,
            out: &mut slot.outbox,
        };
        slot.agent.act(&mut ctx);
        for cmd in slot.outbox.drain() {
            match cmd {
                Command::New(_) => slot.orders += 1,
                _ => slot.cancels += 1,
            }
            self.commands += 1;
            self.venue.send(slot.account, &cmd);
        }
        self.venue.flush(&mut self.deliveries, &mut self.updates);
        self.route();
    }

    /// Delivers what the venue returned and applies the market data.
    fn route(&mut self) {
        for k in 0..self.deliveries.len() {
            let d = self.deliveries[k];
            self.delivered += 1;
            let Some(i) = self.slot(d.account) else {
                continue;
            };
            let slot = &mut self.agents[i];
            if let Some(leg) = d.fill() {
                slot.inventory.apply(leg.side, leg.price, leg.qty);
                if !leg.maker {
                    self.trades += 1;
                    self.volume += leg.qty.0;
                }
            }
            if let Event::Rejected { reason, .. } = d.event {
                slot.rejects += 1;
                if matches!(reason, RejectReason::Risk(_)) {
                    slot.risk_rejects += 1;
                }
            }
            slot.agent.on_event(&d);
        }
        self.deliveries.clear();
        for k in 0..self.updates.len() {
            self.md_updates += 1;
            if let UpdateOutcome::Gap { .. } = self.book.apply_update(&self.updates[k]) {
                let snap = self.venue.market_snapshot();
                self.book.apply_snapshot(&snap);
            }
        }
        self.updates.clear();
    }
}
// @student-end
