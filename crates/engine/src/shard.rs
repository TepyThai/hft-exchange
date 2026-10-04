//! Week 36: the multi-instrument engine — an instrument registry, a router
//! that shards instruments over engine threads, and per-shard sequencing.
//!
//! ```text
//!               ┌───────────── router (caller's thread) ─────────────┐
//! Command ───▶  │ New: instrument → shard (registry)                  │
//!               │ Cancel/Reduce: order id → shard (id map)            │
//!               │ per-shard seq, batch of commands per shard          │
//!               └──────┬──────────────┬──────────────┬────────────────┘
//!                 mpsc │         mpsc │         mpsc │      (batches)
//!               ┌──────▼─────┐ ┌──────▼─────┐ ┌──────▼─────┐
//!               │ shard 0    │ │ shard 1    │ │ shard 2    │  one thread each,
//!               │ Engine/inst│ │ Engine/inst│ │ Engine/inst│  one Engine per
//!               └──────┬─────┘ └──────┬─────┘ └──────┬─────┘  instrument
//!                      └──── events + dead ids ──────┘
//!                              (one shared mpsc back)
//! ```
//!
//! Each shard is a deterministic function of the commands routed to it, in
//! its own sequence order: same input stream (and flush points) → same
//! per-shard event streams. There is no global order across shards — that
//! is the point: instruments never wait for each other.
//!
//! **Where the SPSC ring (weeks 1 and 5) slots in.** The router → shard links are
//! single-producer single-consumer, and so is each shard → router path if
//! it gets its own queue: replacing `std::sync::mpsc` with the `sync`
//! crate's SPSC ring (busy-polled, no syscalls, no allocation per message)
//! removes the channel's locking and wake-ups from the hot path. The
//! batching and buffer recycling here stay as they are.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::Engine;
#[allow(unused_imports)] // used by the reference implementation
use crate::EngineConfig;
#[allow(unused_imports)] // used by the reference implementation
use book::IdBuildHasher;
#[allow(unused_imports)] // used by the reference implementation
use book::{IdMap, OrderBook};
#[allow(unused_imports)] // used by the reference implementation
use std::collections::HashMap;
#[allow(unused_imports)] // used by the reference implementation
use std::sync::mpsc::{self, Receiver, Sender};
#[allow(unused_imports)] // used by the reference implementation
use std::thread::JoinHandle;
use types::{Command, Event, InstrumentId, InstrumentSpec, OrderId, Seq};

/// Index of an engine shard (thread), `0..shard_count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardId(pub u16);

/// Why an instrument could not be registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// This instrument id is already registered.
    Duplicate(InstrumentId),
    /// The requested shard does not exist.
    NoSuchShard(ShardId),
}

/// Why the router refused a command (it never reached an engine, has no
/// sequence number and produces no event — the gateway answers it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// A new order for an instrument that is not registered.
    UnknownInstrument(InstrumentId),
    /// A cancel or reduce for an id the router has no live order for.
    UnknownOrder(OrderId),
    /// A new order reusing the id of an order live on another instrument.
    DuplicateOrderId(OrderId),
}

/// One engine event, tagged with where and when it happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShardEvent {
    pub shard: ShardId,
    /// The shard's sequence number of the command that produced it
    /// (1-based, gap-free per shard).
    pub seq: Seq,
    pub instrument: InstrumentId,
    pub event: Event,
}

/// Reference data of every tradable instrument and the shard that owns it.
///
/// Lookups are O(1) (one hash probe); registering is O(1) amortized.
#[derive(Clone, Debug)]
pub struct InstrumentRegistry {
    // TODO(w36): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl InstrumentRegistry {
    /// An empty registry for `shards` engine threads.
    ///
    /// # Panics
    /// If `shards` is 0 or more than `u16::MAX`.
    pub fn new(shards: usize) -> InstrumentRegistry {
        todo!("w36: new")
    }

    /// Registers `spec` on the shard with the fewest instruments (the
    /// lowest-numbered on a tie — round robin for a fresh registry) and
    /// returns that shard.
    pub fn register(&mut self, spec: InstrumentSpec) -> Result<ShardId, RegistryError> {
        todo!("w36: register")
    }

    /// Registers `spec` on `shard` (to co-locate instruments that must
    /// share a sequence, or to balance by hand).
    pub fn register_on(
        &mut self,
        spec: InstrumentSpec,
        shard: ShardId,
    ) -> Result<ShardId, RegistryError> {
        todo!("w36: register_on")
    }

    /// The shard that owns `instrument`.
    pub fn shard_of(&self, instrument: InstrumentId) -> Option<ShardId> {
        todo!("w36: shard_of")
    }

    /// The instrument's reference data.
    pub fn spec(&self, instrument: InstrumentId) -> Option<&InstrumentSpec> {
        todo!("w36: spec")
    }

    /// Number of shards.
    pub fn shard_count(&self) -> usize {
        todo!("w36: shard_count")
    }

    /// Number of registered instruments.
    pub fn len(&self) -> usize {
        todo!("w36: len")
    }

    /// True if nothing is registered.
    pub fn is_empty(&self) -> bool {
        todo!("w36: is_empty")
    }

    /// The instruments on `shard`, in registration order.
    pub fn instruments_on(&self, shard: ShardId) -> Vec<InstrumentSpec> {
        todo!("w36: instruments_on")
    }
}

// TODO(w36): implement — see the week guide on tepy.dev/hft.

/// A multi-instrument engine: one thread per shard, each running one
/// [`Engine`] per instrument the [`InstrumentRegistry`] put on it, fed by a
/// router on the caller's thread.
///
/// * [`send`](ShardedEngine::send) routes a command — a new order by its
///   instrument, a cancel or reduce by the id → shard map built from the
///   new orders it routed — gives it the shard's next sequence number and
///   appends it to that shard's batch; full batches go out at once.
/// * [`flush`](ShardedEngine::flush) sends the partial batches, waits until
///   every shard has processed everything sent so far, and returns the
///   events. It also prunes the id map: shards report which orders ended
///   (filled, cancelled, expired, rejected), and those ids are forgotten —
///   **only at flush points**, so routing is a deterministic function of
///   the commands and the flush points, never of thread timing.
/// * [`shutdown`](ShardedEngine::shutdown) flushes, stops the threads and
///   returns the engines.
///
/// Cost on the router: one or two hash probes and one copy of the command
/// per command, a channel send per batch, and one copy of each event at
/// flush.
/// Batches (with their command, event and dead-id buffers) are pooled, so
/// once the pool holds as many batches as one flush interval needs, nothing
/// allocates.
pub struct ShardedEngine {
    // TODO(w36): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl ShardedEngine {
    /// Starts one thread per shard of `registry`, each with a fresh
    /// [`Engine`] (`EngineConfig::new(spec)`) per instrument on it. Commands
    /// travel in batches of up to `batch_size` (≥ 1; 64–1024 amortizes the
    /// channel well).
    pub fn new(registry: InstrumentRegistry, batch_size: usize) -> ShardedEngine {
        todo!("w36: new")
    }

    /// Routes `cmd` to its shard and queues it there; returns the shard and
    /// the shard sequence number it was given. Errors leave everything
    /// unchanged. O(1) amortized.
    pub fn send(&mut self, cmd: &Command) -> Result<(ShardId, Seq), RouteError> {
        todo!("w36: send")
    }

    /// Sends every partial batch, waits for all shards to finish all work
    /// sent so far, appends the events to `out` (per shard in sequence
    /// order; shards interleaved in batch arrival order), and forgets the
    /// ids of orders that ended. Blocks for as long as the shards need.
    ///
    /// Until a flush, returned batches (and their events) wait in the reply
    /// channel: flush regularly — the w36 benchmark flushes every 64k
    /// commands.
    pub fn flush(&mut self, out: &mut Vec<ShardEvent>) {
        todo!("w36: flush")
    }

    /// The last sequence number given to a command on `shard`.
    pub fn shard_seq(&self, shard: ShardId) -> Seq {
        todo!("w36: shard_seq")
    }

    /// Number of order ids the router can route cancels for (live orders,
    /// plus orders that ended since the last flush).
    pub fn routable_orders(&self) -> usize {
        todo!("w36: routable_orders")
    }

    /// The registry the engine was started with.
    pub fn registry(&self) -> &InstrumentRegistry {
        todo!("w36: registry")
    }

    /// Flushes (events into `out`), stops every shard thread and returns
    /// the engines, sorted by instrument id.
    ///
    /// # Panics
    /// If a shard thread panicked.
    pub fn shutdown(mut self, out: &mut Vec<ShardEvent>) -> Vec<(InstrumentId, Engine)> {
        todo!("w36: shutdown")
    }
}

// TODO(w36): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl Drop for ShardedEngine {
    /// Hangs up on the shards and waits for their threads (dropping the
    /// engines). Use [`shutdown`](ShardedEngine::shutdown) to keep them.
    fn drop(&mut self) {
        todo!("w36: drop")
    }
}
