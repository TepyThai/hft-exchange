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
//! **Where the w30 SPSC ring slots in.** The router → shard links are
//! single-producer single-consumer, and so is each shard → router path if
//! it gets its own queue: replacing `std::sync::mpsc` with the `sync`
//! crate's SPSC ring (busy-polled, no syscalls, no allocation per message)
//! removes the channel's locking and wake-ups from the hot path. The
//! batching and buffer recycling here stay as they are.

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
// @student w36
#[derive(Clone, Debug)]
pub struct InstrumentRegistry {
    shards: usize,
    /// In registration order: the spec, its shard, its engine slot on that
    /// shard.
    entries: Vec<(InstrumentSpec, ShardId, u32)>,
    /// Instrument → entry. One probe per new order on the router: a
    /// one-multiply hasher, not SipHash (that was 6% of the router's time).
    index: HashMap<InstrumentId, usize, IdBuildHasher>,
    per_shard: Vec<u32>,
}

// @student w36
impl InstrumentRegistry {
    /// An empty registry for `shards` engine threads.
    ///
    /// # Panics
    /// If `shards` is 0 or more than `u16::MAX`.
    pub fn new(shards: usize) -> InstrumentRegistry {
        assert!(
            shards > 0 && shards <= u16::MAX as usize,
            "1..=65535 shards"
        );
        InstrumentRegistry {
            shards,
            entries: Vec::new(),
            index: HashMap::default(),
            per_shard: vec![0; shards],
        }
    }

    /// Registers `spec` on the shard with the fewest instruments (the
    /// lowest-numbered on a tie — round robin for a fresh registry) and
    /// returns that shard.
    pub fn register(&mut self, spec: InstrumentSpec) -> Result<ShardId, RegistryError> {
        let (shard, _) = self
            .per_shard
            .iter()
            .enumerate()
            .min_by_key(|&(i, &n)| (n, i))
            .expect("at least one shard");
        self.register_on(spec, ShardId(shard as u16))
    }

    /// Registers `spec` on `shard` (to co-locate instruments that must
    /// share a sequence, or to balance by hand).
    pub fn register_on(
        &mut self,
        spec: InstrumentSpec,
        shard: ShardId,
    ) -> Result<ShardId, RegistryError> {
        if shard.0 as usize >= self.shards {
            return Err(RegistryError::NoSuchShard(shard));
        }
        if self.index.contains_key(&spec.id) {
            return Err(RegistryError::Duplicate(spec.id));
        }
        let slot = self.per_shard[shard.0 as usize];
        self.per_shard[shard.0 as usize] += 1;
        self.index.insert(spec.id, self.entries.len());
        self.entries.push((spec, shard, slot));
        Ok(shard)
    }

    /// The shard that owns `instrument`.
    pub fn shard_of(&self, instrument: InstrumentId) -> Option<ShardId> {
        self.index.get(&instrument).map(|&i| self.entries[i].1)
    }

    /// The instrument's reference data.
    pub fn spec(&self, instrument: InstrumentId) -> Option<&InstrumentSpec> {
        self.index.get(&instrument).map(|&i| &self.entries[i].0)
    }

    /// Number of shards.
    pub fn shard_count(&self) -> usize {
        self.shards
    }

    /// Number of registered instruments.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The instruments on `shard`, in registration order.
    pub fn instruments_on(&self, shard: ShardId) -> Vec<InstrumentSpec> {
        self.entries
            .iter()
            .filter(|e| e.1 == shard)
            .map(|e| e.0)
            .collect()
    }
}

// @student-begin w36
/// A command on its way to a shard: the shard's sequence number and the
/// engine slot (instrument) on that shard.
#[derive(Clone, Copy, Debug)]
struct Routed {
    seq: u64,
    slot: u32,
    cmd: Command,
}

/// The unit of work and of reply: the router fills `cmds`, the shard fills
/// `events` and `dead` and sends the whole batch back, and the router
/// recycles the buffers — no allocation per batch in steady state.
#[derive(Debug, Default)]
struct Batch {
    shard: u16,
    cmds: Vec<Routed>,
    events: Vec<ShardEvent>,
    /// Orders that ended (filled, cancelled, expired, rejected) with the
    /// shard seq of the command that ended them.
    dead: Vec<(OrderId, u64)>,
}

/// Where the router sends cancels and reduces for a live order.
#[derive(Clone, Copy, Debug)]
struct Route {
    shard: u16,
    slot: u32,
    /// Shard seq of the latest `New` with this id: a death reported for an
    /// earlier seq belongs to an older order that reused the id.
    seq: u64,
}

/// The shard thread: process batches until the router hangs up, then hand
/// the engines back.
fn run_shard(
    shard: ShardId,
    mut engines: Vec<(InstrumentId, Engine)>,
    rx: Receiver<Batch>,
    tx: Sender<Batch>,
) -> Vec<(InstrumentId, Engine)> {
    let mut out = Vec::with_capacity(64);
    while let Ok(mut batch) = rx.recv() {
        for r in &batch.cmds {
            let (instrument, engine) = &mut engines[r.slot as usize];
            out.clear();
            engine.process(&r.cmd, &mut out);
            match r.cmd {
                Command::New(order) => {
                    let lives = out.iter().any(|e| matches!(e, Event::Rested { .. }))
                        || out.iter().any(|e| {
                            matches!(
                                e,
                                Event::Rejected {
                                    reason: types::RejectReason::DuplicateOrderId,
                                    ..
                                }
                            )
                        });
                    if !lives {
                        batch.dead.push((order.id, r.seq));
                    }
                }
                Command::Cancel { id } => batch.dead.push((id, r.seq)),
                Command::Reduce { .. } => {}
            }
            for &event in &out {
                if let Event::Trade { maker, .. } = event {
                    if !engine.book().contains(maker) {
                        batch.dead.push((maker, r.seq));
                    }
                }
                batch.events.push(ShardEvent {
                    shard,
                    seq: Seq(r.seq),
                    instrument: *instrument,
                    event,
                });
            }
        }
        if tx.send(batch).is_err() {
            break;
        }
    }
    engines
}
// @student-end

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
// @student w36
pub struct ShardedEngine {
    registry: InstrumentRegistry,
    batch_size: usize,
    senders: Vec<Sender<Batch>>,
    replies: Receiver<Batch>,
    handles: Vec<JoinHandle<Vec<(InstrumentId, Engine)>>>,
    /// Per shard: the batch being filled, the last seq assigned, the
    /// batches sent and not yet returned.
    pending: Vec<Batch>,
    seqs: Vec<u64>,
    in_flight: Vec<usize>,
    /// Empty batches ready for reuse.
    pool: Vec<Batch>,
    routes: IdMap<Route>,
}

// @student w36
impl ShardedEngine {
    /// Starts one thread per shard of `registry`, each with a fresh
    /// [`Engine`] (`EngineConfig::new(spec)`) per instrument on it. Commands
    /// travel in batches of up to `batch_size` (≥ 1; 64–1024 amortizes the
    /// channel well).
    pub fn new(registry: InstrumentRegistry, batch_size: usize) -> ShardedEngine {
        let batch_size = batch_size.max(1);
        let shards = registry.shard_count();
        let (reply_tx, replies) = mpsc::channel();
        let mut senders = Vec::with_capacity(shards);
        let mut handles = Vec::with_capacity(shards);
        for s in 0..shards {
            let shard = ShardId(s as u16);
            let engines = registry
                .instruments_on(shard)
                .into_iter()
                .map(|spec| (spec.id, Engine::with_config(EngineConfig::new(spec))))
                .collect();
            let (tx, rx) = mpsc::channel();
            let reply_tx = reply_tx.clone();
            senders.push(tx);
            handles.push(
                std::thread::Builder::new()
                    .name(format!("engine-shard-{s}"))
                    .spawn(move || run_shard(shard, engines, rx, reply_tx))
                    .expect("spawn shard thread"),
            );
        }
        let pending = (0..shards)
            .map(|s| Batch {
                shard: s as u16,
                cmds: Vec::with_capacity(batch_size),
                ..Batch::default()
            })
            .collect();
        ShardedEngine {
            registry,
            batch_size,
            senders,
            replies,
            handles,
            pending,
            seqs: vec![0; shards],
            in_flight: vec![0; shards],
            pool: Vec::new(),
            routes: IdMap::default(),
        }
    }

    /// Routes `cmd` to its shard and queues it there; returns the shard and
    /// the shard sequence number it was given. Errors leave everything
    /// unchanged. O(1) amortized.
    pub fn send(&mut self, cmd: &Command) -> Result<(ShardId, Seq), RouteError> {
        let route = match *cmd {
            Command::New(order) => {
                let i = *self
                    .registry
                    .index
                    .get(&order.instrument)
                    .ok_or(RouteError::UnknownInstrument(order.instrument))?;
                let (_, shard, slot) = self.registry.entries[i];
                let seq = self.seqs[shard.0 as usize] + 1;
                match self.routes.get_mut(&order.id) {
                    Some(r) if (r.shard, r.slot) != (shard.0, slot) => {
                        return Err(RouteError::DuplicateOrderId(order.id));
                    }
                    Some(r) => r.seq = seq,
                    None => {
                        self.routes.insert(
                            order.id,
                            Route {
                                shard: shard.0,
                                slot,
                                seq,
                            },
                        );
                    }
                }
                (shard.0, slot)
            }
            Command::Cancel { id } => {
                let r = self
                    .routes
                    .remove(&id)
                    .ok_or(RouteError::UnknownOrder(id))?;
                (r.shard, r.slot)
            }
            Command::Reduce { id, .. } => {
                let r = self.routes.get(&id).ok_or(RouteError::UnknownOrder(id))?;
                (r.shard, r.slot)
            }
        };
        let (shard, slot) = route;
        let s = shard as usize;
        self.seqs[s] += 1;
        let seq = self.seqs[s];
        self.pending[s].cmds.push(Routed {
            seq,
            slot,
            cmd: *cmd,
        });
        if self.pending[s].cmds.len() >= self.batch_size {
            self.dispatch(s);
        }
        Ok((ShardId(shard), Seq(seq)))
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
        for s in 0..self.pending.len() {
            if !self.pending[s].cmds.is_empty() {
                self.dispatch(s);
            }
        }
        while self.in_flight.iter().any(|&n| n > 0) {
            let batch = self.replies.recv().expect("a shard thread died");
            self.collect(batch, out);
        }
    }

    /// The last sequence number given to a command on `shard`.
    pub fn shard_seq(&self, shard: ShardId) -> Seq {
        Seq(self.seqs[shard.0 as usize])
    }

    /// Number of order ids the router can route cancels for (live orders,
    /// plus orders that ended since the last flush).
    pub fn routable_orders(&self) -> usize {
        self.routes.len()
    }

    /// The registry the engine was started with.
    pub fn registry(&self) -> &InstrumentRegistry {
        &self.registry
    }

    /// Flushes (events into `out`), stops every shard thread and returns
    /// the engines, sorted by instrument id.
    ///
    /// # Panics
    /// If a shard thread panicked.
    pub fn shutdown(mut self, out: &mut Vec<ShardEvent>) -> Vec<(InstrumentId, Engine)> {
        self.flush(out);
        self.senders.clear();
        let mut engines: Vec<(InstrumentId, Engine)> = std::mem::take(&mut self.handles)
            .into_iter()
            .flat_map(|h| h.join().expect("shard thread panicked"))
            .collect();
        engines.sort_by_key(|(id, _)| *id);
        engines
    }
}

// @student-begin w36
impl ShardedEngine {
    /// Sends shard `s`'s pending batch and starts a fresh one (pooled).
    fn dispatch(&mut self, s: usize) {
        let mut next = self.pool.pop().unwrap_or_default();
        next.shard = s as u16;
        let full = std::mem::replace(&mut self.pending[s], next);
        self.in_flight[s] += 1;
        self.senders[s].send(full).expect("a shard thread died");
    }

    /// Takes a returned batch (only ever inside `flush`): its events go to
    /// `out`, its dead ids leave the route map unless the id was reused
    /// since, and its buffers go back to the pool.
    fn collect(&mut self, mut batch: Batch, out: &mut Vec<ShardEvent>) {
        self.in_flight[batch.shard as usize] -= 1;
        out.extend_from_slice(&batch.events);
        for &(id, seq) in &batch.dead {
            if let Some(r) = self.routes.get(&id) {
                if r.shard == batch.shard && r.seq <= seq {
                    self.routes.remove(&id);
                }
            }
        }
        batch.cmds.clear();
        batch.events.clear();
        batch.dead.clear();
        self.pool.push(batch);
    }
}
// @student-end

// @student w36
impl Drop for ShardedEngine {
    /// Hangs up on the shards and waits for their threads (dropping the
    /// engines). Use [`shutdown`](ShardedEngine::shutdown) to keep them.
    fn drop(&mut self) {
        self.senders.clear();
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}
