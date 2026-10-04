//! Week 12: engine v1 — the sequenced pipeline.
//!
//! ```text
//!  Command ─▶ journal.append ─▶ Engine::process ─▶ L2Publisher::on_event ─▶ Output
//!             (assigns Seq)      (events)            (L2 updates)            { seq, events, updates }
//! ```
//!
//! The order is the point. A command is journaled *before* it is applied,
//! so the journal holds every command the engine ever saw — rejected ones
//! included, since they too advance [`Engine::seq`] — and replaying it
//! rebuilds the engine exactly ([`journal::replay()`], with the
//! [`StateMachine`] impl below). Market data is derived from the events
//! only, so a client applying the L2 updates holds the engine's book by
//! price.
//!
//! The journal is any [`CommandLog`]: the in-memory [`Journal`] here, the
//! durable [`FileJournal`](journal::FileJournal) in week 18 ([`recover`](crate::recover)), a
//! replicating [`Primary`](journal::Primary) in week 20.

use crate::{Engine, EngineConfig};
use book::{Book, OrderBook};
use journal::{CommandLog, Journal, StateMachine};
use marketdata::{L2Publisher, L2Update};
use std::fmt;
use types::{Command, Event, Seq};

/// What one command produced. Borrowed from the pipeline's reusable
/// buffers: valid until the next [`Pipeline::process`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output<'a> {
    /// The sequence number the journal assigned (= [`Engine::seq`] after
    /// the command).
    pub seq: Seq,
    /// The engine's events, in the documented order.
    pub events: &'a [Event],
    /// The L2 changes those events caused, in order (feed sequence numbers
    /// are the publisher's own, not `seq`).
    pub updates: &'a [L2Update],
}

/// A journal and an engine that disagree about where they are: the
/// journal's next sequence number must be the engine's plus one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqMismatch {
    /// The sequence number the journal would assign next.
    pub journal_next: Seq,
    /// The engine's sequence number (commands it has processed).
    pub engine_seq: Seq,
}

impl fmt::Display for SeqMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "journal would assign seq {} but the engine is at seq {}",
            self.journal_next.0, self.engine_seq.0
        )
    }
}

impl std::error::Error for SeqMismatch {}

/// Engine v1: one instrument's sequencer, matching engine and L2 market
/// data publisher, wired in that order.
///
/// Single-threaded and allocation-free in steady state: the event and
/// update buffers are reused, the in-memory journal grows (or is drained,
/// see [`journal_mut`](Pipeline::journal_mut)).
// @student w12
#[derive(Debug)]
pub struct Pipeline<J = Journal, B = Book> {
    journal: J,
    engine: Engine<B>,
    md: L2Publisher,
    events: Vec<Event>,
    updates: Vec<L2Update>,
}

// @student w12
impl Pipeline<Journal, Book> {
    /// A fresh engine for `config` behind a fresh in-memory journal.
    pub fn new(config: EngineConfig) -> Pipeline<Journal, Book> {
        Pipeline::with_journal(Journal::new(), Engine::with_config(config))
            .expect("a fresh journal starts where a fresh engine is")
    }
}

// @student w12
impl<J: CommandLog, B: OrderBook> Pipeline<J, B> {
    /// Wires `journal` in front of `engine`. The journal's next sequence
    /// number must follow the engine's (both fresh, or both recovered to
    /// the same point). The publisher starts from the engine's current
    /// book: every resting order is announced as `Rested`, best first and
    /// in time priority, so its first updates rebuild the book for a
    /// listener starting from empty. O(resting orders).
    pub fn with_journal(journal: J, engine: Engine<B>) -> Result<Pipeline<J, B>, SeqMismatch> {
        let journal_next = journal.next_seq();
        if journal_next.0 != engine.seq().0 + 1 {
            return Err(SeqMismatch {
                journal_next,
                engine_seq: engine.seq(),
            });
        }
        let mut md = L2Publisher::new();
        for side in [Side::Buy, Side::Sell] {
            engine.book().visit_orders(side, &mut |o| {
                md.on_event(&Event::Rested {
                    id: o.id,
                    side: o.side,
                    price: o.price,
                    qty: o.qty,
                });
                true
            });
        }
        Ok(Pipeline {
            journal,
            engine,
            md,
            events: Vec::with_capacity(64),
            updates: Vec::with_capacity(64),
        })
    }

    /// Sequences one command: appends it to the journal, then matches it,
    /// then publishes the resulting L2 changes. If the journal refuses
    /// (I/O error, not the leader), nothing else happens and the error is
    /// returned.
    ///
    /// Cost: a journal append + [`Engine::process`] + one publisher update
    /// per event. No allocation in steady state.
    #[inline]
    pub fn process(&mut self, cmd: &Command) -> Result<Output<'_>, J::Error> {
        let seq = self.journal.append(cmd)?;
        self.events.clear();
        self.updates.clear();
        self.engine.process(cmd, &mut self.events);
        debug_assert_eq!(self.engine.seq(), seq, "journal and engine out of step");
        for ev in &self.events {
            if let Some(u) = self.md.on_event(ev) {
                self.updates.push(u);
            }
        }
        Ok(Output {
            seq,
            events: &self.events,
            updates: &self.updates,
        })
    }

    /// The engine (read-only: all changes go through commands).
    pub fn engine(&self) -> &Engine<B> {
        &self.engine
    }

    /// The journal.
    pub fn journal(&self) -> &J {
        &self.journal
    }

    /// The journal, mutably — to sync or drain it (e.g. `clear()` an
    /// in-memory journal whose frames were persisted elsewhere). Appending
    /// through it would put journal and engine out of step.
    pub fn journal_mut(&mut self) -> &mut J {
        &mut self.journal
    }

    /// The market data publisher (L2 book, snapshots).
    pub fn publisher(&self) -> &L2Publisher {
        &self.md
    }

    /// Takes the pipeline apart.
    pub fn into_parts(self) -> (J, Engine<B>, L2Publisher) {
        (self.journal, self.engine, self.md)
    }
}

// @student-begin w12
use types::Side;

thread_local! {
    /// Event buffer for `StateMachine::apply`, which has nowhere to put
    /// events: reused, so replay does not allocate per command.
    static SCRATCH: std::cell::RefCell<Vec<Event>> = const { std::cell::RefCell::new(Vec::new()) };
}
// @student-end

/// The engine as a journal state machine: applying a command is processing
/// it (the events are discarded); the state hash is
/// [`Engine::state_hash`]. This is what lets [`journal::replay()`] rebuild an
/// engine from its journal.
// @student w12
impl<B: OrderBook> StateMachine for Engine<B> {
    fn apply(&mut self, cmd: &Command) {
        SCRATCH.with(|s| {
            let mut events = s.borrow_mut();
            events.clear();
            self.process(cmd, &mut events);
        });
    }

    fn state_hash(&self) -> u64 {
        Engine::state_hash(self)
    }
}
