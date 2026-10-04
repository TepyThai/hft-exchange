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

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
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
#[derive(Debug)]
pub struct Pipeline<J = Journal, B = Book> {
    // TODO(w12): design this type's fields.
    _todo: std::marker::PhantomData<(J, B,)>,
}

#[allow(unused_variables)]
impl Pipeline<Journal, Book> {
    /// A fresh engine for `config` behind a fresh in-memory journal.
    pub fn new(config: EngineConfig) -> Pipeline<Journal, Book> {
        todo!("w12: new")
    }
}

#[allow(unused_variables)]
impl<J: CommandLog, B: OrderBook> Pipeline<J, B> {
    /// Wires `journal` in front of `engine`. The journal's next sequence
    /// number must follow the engine's (both fresh, or both recovered to
    /// the same point). The publisher is primed from the engine's current
    /// book (every resting order fed as `Rested`, best first, in time
    /// priority) so its depth matches the engine. The priming updates are
    /// discarded, not published: after a restore or promotion, send
    /// listeners `publisher().snapshot()` so they resync. O(resting orders).
    pub fn with_journal(journal: J, engine: Engine<B>) -> Result<Pipeline<J, B>, SeqMismatch> {
        todo!("w12: with_journal")
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
        todo!("w12: process")
    }

    /// The engine (read-only: all changes go through commands).
    pub fn engine(&self) -> &Engine<B> {
        todo!("w12: engine")
    }

    /// The journal.
    pub fn journal(&self) -> &J {
        todo!("w12: journal")
    }

    /// The journal, mutably — to sync or drain it (e.g. `clear()` an
    /// in-memory journal whose frames were persisted elsewhere). Appending
    /// through it would put journal and engine out of step.
    pub fn journal_mut(&mut self) -> &mut J {
        todo!("w12: journal_mut")
    }

    /// The market data publisher (L2 book, snapshots).
    pub fn publisher(&self) -> &L2Publisher {
        todo!("w12: publisher")
    }

    /// Takes the pipeline apart.
    pub fn into_parts(self) -> (J, Engine<B>, L2Publisher) {
        todo!("w12: into_parts")
    }
}

// TODO(w12): implement — see the week guide on tepy.dev/hft.

/// The engine as a journal state machine: applying a command is processing
/// it (the events are discarded); the state hash is
/// [`Engine::state_hash`]. This is what lets [`journal::replay()`] rebuild an
/// engine from its journal.
#[allow(unused_variables)]
impl<B: OrderBook> StateMachine for Engine<B> {
    fn apply(&mut self, cmd: &Command) {
        todo!("w12: apply")
    }

    fn state_hash(&self) -> u64 {
        todo!("w12: state_hash")
    }
}
