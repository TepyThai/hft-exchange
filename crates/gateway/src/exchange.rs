//! Week 24: exchange v2 — every component of Phase 2 in one pipeline,
//! measured end to end.
//!
//! ```text
//!  clients ─TCP─▶ gateway readers (Tokio): decode, session, busy reject at the edge
//!                     │ bounded MPSC (week 5)
//!                     ▼
//!  engine thread ── Sequencer, one batch of ≤ 256 messages at a time:
//!    PreTrade      TimedPreTrade<RiskGate>      stamps Gateway (picked up), Risk
//!    on_command    journal append (buffered)    the write-ahead point
//!    Engine::process
//!    on_events     L2Publisher, trade ticks     stamps Match; market data held back
//!    … rest of the batch …
//!    flush         group commit (fsync)         stamps Journal (durable)
//!                  md → broadcast rings         stamps Publish
//!                  reports → session queues     stamps Ack
//!                     │                               │
//!  writers ◀── per-session queues      L2 + trade rings, SnapshotCell
//!                                       → in-process readers, WebSocket feed
//! ```
//!
//! # Output gating
//!
//! The journal records *inputs*, so a command is appended before it is
//! matched, and matching runs ahead of the disk. What waits is everything
//! **visible**: a batch's execution reports and its market data are
//! released only after [`Stage::flush`] has made the batch durable. One
//! fsync covers the whole batch (group commit), so under load the cost of
//! durability is shared by every order in it. A client that has an ack, and
//! a subscriber that has seen a trade, can rely on a restart replaying it.
//!
//! What "durable" means is the [`Fsync`] policy:
//!
//! | policy | per batch | an ack survives |
//! | --- | --- | --- |
//! | `Always` | write + fsync, then release | power loss |
//! | `IntervalMs(t)` | write, release; fsync at most every `t` (also when idle) | a process crash; power loss after ≤ `t` |
//! | `Never` | write, release | a process crash |
//!
//! The engine thread never waits for a consumer: market data goes into
//! broadcast rings that overwrite the oldest value (a slow reader gets
//! `Lagged` and resyncs from the [`SnapshotCell`]), reports into bounded
//! per-session queues (a session that stops reading is cut). A journal that
//! cannot write stops the exchange (the engine thread panics): it must not
//! acknowledge orders it cannot replay.
//!
//! # Restart
//!
//! One journal per instrument ([`journal_path`]): each engine is recovered
//! on its own with `engine::recover`, and the sequencer resumes exchange
//! order ids after the largest one any journal holds. Resting orders
//! survive a restart (with `cancel_on_disconnect` off — on, a clean
//! shutdown cancels them through the journal); their sessions do not, so
//! reports about them are dropped. Pre-trade risk state (positions, open
//! orders) starts empty: rebuilding it from the journal is left for later.
//!
//! # Where the time goes
//!
//! Every new order that passes pre-trade risk carries [`StageStamps`] on
//! `telemetry::Clock::global()`, the clock the gateway reads `rx_ns` from:
//!
//! | stamp | when | the interval ending there is |
//! | --- | --- | --- |
//! | `Ingress` | the session's socket read returned | — |
//! | `Gateway` | the engine thread picked the order up | decode + inbound queue wait |
//! | `Risk` | the pre-trade check passed | risk and throttle |
//! | `Match` | the engine produced the events | journal append + matching |
//! | `Journal` | the batch's group commit returned | waiting for durability |
//! | `Publish` | the batch's market data was in the rings | md fan-out |
//! | `Ack` | the reports were handed to the sessions | release |
//!
//! (Stamps are kept in the order taken, so `Journal` follows `Match`.) The
//! stamps go into a [`StageLog`]; `Attribution` (week 39) turns them into
//! per-stage distributions and says which stage the slowest orders spent
//! their time in. The client-side view — intended send to ack received,
//! sockets included — is [`loadgen`](crate::loadgen)'s.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::config::{ExchangeConfig, Fsync};
use crate::hooks::{PreTrade, Stage};
use crate::sequencer::Sequencer;
use crate::server::{GatewayConfig, GatewayStats, ShutdownReport};
use journal::{
    FileJournal, FileStorage, MemStorage, RecoveryError, Storage, SystemClock, TornTail,
};
use marketdata::{BroadcastRing, L2Publisher, L2Update, RingValue, SnapshotCell};
use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;
use telemetry::{Attribution, Clock, StageStamps};
use types::{InstrumentId, OrderId, Price, Qty, Seq, Side};

#[allow(unused_imports)] // used by the implementation the learner writes
use {
    crate::hooks::Hooks,
    crate::server::Gateway,
    crate::session::SessionId,
    engine::{Engine, EngineConfig},
    journal::{JournalOptions, SyncPolicy, FIRST_SEQ},
    marketdata::RingPublisher,
    std::net::{TcpListener, TcpStream},
    std::thread::JoinHandle,
    telemetry::Stage as Stamp,
    types::{Command, Event, NewOrder, RejectReason},
};

// ---------------------------------------------------------------------------
// Configuration (provided)

/// Where the journals live.
#[derive(Clone, Debug)]
pub enum JournalBackend {
    /// One file per instrument, named by [`journal_path`] from this base
    /// path (its directory must exist).
    File(PathBuf),
    /// In memory, for tests and benchmarks: syncs are counted
    /// ([`MemStorage::syncs`]) and power loss can be simulated, nothing
    /// touches a disk. Start again with the same [`MemJournals`] to
    /// restart from it.
    Memory(MemJournals),
}

/// In-memory journal storage, one [`MemStorage`] per instrument. Clones
/// share everything: keep one to inspect the journals while the exchange
/// writes them.
#[derive(Clone, Debug, Default)]
pub struct MemJournals {
    inner: Arc<Mutex<Vec<(InstrumentId, MemStorage)>>>,
}

impl MemJournals {
    /// No journals yet.
    pub fn new() -> MemJournals {
        MemJournals::default()
    }

    /// The storage of `instrument`'s journal, created empty on first use.
    /// The handle shares its bytes with the journal. O(instruments).
    pub fn storage(&self, instrument: InstrumentId) -> MemStorage {
        let mut all = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, s)) = all.iter().find(|(id, _)| *id == instrument) {
            return s.clone();
        }
        let s = MemStorage::new();
        all.push((instrument, s.clone()));
        s
    }
}

/// The journal file of `instrument`: `base` with the instrument id before
/// the extension — `journal.bin` → `journal.1.bin`, `log` → `log.1`.
pub fn journal_path(base: &Path, instrument: InstrumentId) -> PathBuf {
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match base.extension() {
        Some(ext) => format!("{stem}.{}.{}", instrument.0, ext.to_string_lossy()),
        None => format!("{stem}.{}", instrument.0),
    };
    base.with_file_name(name)
}

/// Market data fan-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedConfig {
    /// Values each broadcast ring holds — how far a reader may fall behind
    /// before it is `Lagged`. Rounded up to a power of two and to at least
    /// twice `snapshot_interval` (a reader that lags must find a snapshot
    /// newer than what it lost).
    pub ring_capacity: usize,
    /// L2 updates between the snapshots stored in each [`SnapshotCell`]
    /// (taken at the end of a batch, so possibly a few more).
    pub snapshot_interval: u64,
    /// Serve the L2 feed over WebSocket (week 16's format): instrument `k`
    /// (in configuration order) on this port + `k`; port 0 gives each an
    /// ephemeral port (see [`ExchangeV2::md_addrs`]). `None`: in-process
    /// readers only.
    pub listen: Option<SocketAddr>,
}

impl Default for FeedConfig {
    /// 64 Ki values per ring, a snapshot every 1000 updates, no WebSocket.
    fn default() -> Self {
        FeedConfig {
            ring_capacity: 1 << 16,
            snapshot_interval: 1000,
            listen: None,
        }
    }
}

/// Everything exchange v2 needs to start.
#[derive(Clone, Debug)]
pub struct V2Config {
    /// Sessions, queues, busy rejects, the engines (one per instrument).
    pub gateway: GatewayConfig,
    /// Where the journals are.
    pub journal: JournalBackend,
    /// When they are fsynced (see the module docs).
    pub fsync: Fsync,
    /// Market data.
    pub feed: FeedConfig,
    /// Stage-stamp samples the [`StageLog`] keeps (the most recent).
    pub stage_samples: usize,
}

impl V2Config {
    /// In-memory journals synced every batch, the default feed, 64 Ki
    /// samples: the configuration tests start from.
    pub fn in_memory(gateway: GatewayConfig, journals: MemJournals) -> V2Config {
        V2Config {
            gateway,
            journal: JournalBackend::Memory(journals),
            fsync: Fsync::Always,
            feed: FeedConfig::default(),
            stage_samples: 1 << 16,
        }
    }

    /// The configuration file's view (week 22): `[gateway]` and the
    /// instruments, `[journal] path` and `fsync`, `[marketdata]`
    /// `subscriber_queue` (ring capacity), `snapshot_interval` and
    /// `listen`.
    pub fn from_config(cfg: &ExchangeConfig) -> V2Config {
        V2Config {
            gateway: cfg.gateway_config(),
            journal: JournalBackend::File(cfg.journal.path.clone()),
            fsync: cfg.journal.fsync,
            feed: FeedConfig {
                ring_capacity: cfg.marketdata.subscriber_queue,
                snapshot_interval: cfg.marketdata.snapshot_interval,
                listen: cfg.marketdata.listen,
            },
            stage_samples: 1 << 16,
        }
    }
}

// ---------------------------------------------------------------------------
// Market data handles (provided)

/// One trade on the public tape: no order ids (an exchange does not
/// disclose who traded), a dense per-instrument sequence number starting
/// at 1 in each run of the exchange. Packs into three ring words like an
/// [`L2Update`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TradeTick {
    /// Tape sequence number: 1, 2, 3, … in execution order.
    pub seq: Seq,
    /// The side of the incoming (aggressive) order.
    pub taker_side: Side,
    /// The maker's price.
    pub price: Price,
    /// Quantity traded.
    pub qty: Qty,
}

/// `[seq | side << 63, price, qty]`.
impl RingValue for TradeTick {
    const WORDS: usize = 3;

    #[inline]
    fn pack(&self, words: &mut [u64]) {
        debug_assert!(self.seq.0 < 1 << 63, "tape seq out of range");
        let side = match self.taker_side {
            Side::Buy => 0,
            Side::Sell => 1u64 << 63,
        };
        words[0] = self.seq.0 | side;
        words[1] = self.price.0 as u64;
        words[2] = self.qty.0;
    }

    #[inline]
    fn unpack(words: &[u64]) -> Self {
        TradeTick {
            seq: Seq(words[0] & !(1 << 63)),
            taker_side: if words[0] >> 63 == 0 {
                Side::Buy
            } else {
                Side::Sell
            },
            price: Price(words[1] as i64),
            qty: Qty(words[2]),
        }
    }
}

/// One instrument's market data, for any number of readers on any thread.
/// Cheap to clone (three `Arc`s).
#[derive(Clone)]
pub struct Feed {
    /// The instrument.
    pub instrument: InstrumentId,
    /// L2 incrementals (week 10's feed contract), in feed sequence order.
    pub l2: Arc<BroadcastRing<L2Update>>,
    /// The trade tape.
    pub trades: Arc<BroadcastRing<TradeTick>>,
    /// The latest L2 snapshot: where a new or lagging reader starts.
    pub snapshots: SnapshotCell,
}

impl fmt::Debug for Feed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Feed")
            .field("instrument", &self.instrument)
            .field("l2_published", &self.l2.published())
            .field("trades_published", &self.trades.published())
            .field("snapshot_seq", &self.snapshots.seq())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Stage stamps: hand-off, log and counters (provided)

/// The stamps of the order the engine thread is working on, handed from
/// the pre-trade hook (which sees `rx_ns` and times the check) to the
/// pipeline stage (which sees the rest). Both run on the engine thread, one
/// right after the other, so relaxed atomics are enough; clones share the
/// slot. Holds at most one order.
#[derive(Clone, Debug, Default)]
pub struct StampSlot {
    inner: Arc<SlotInner>,
}

#[derive(Debug, Default)]
struct SlotInner {
    full: AtomicBool,
    ingress: AtomicU64,
    gateway: AtomicU64,
    risk: AtomicU64,
}

impl StampSlot {
    /// An empty slot.
    pub fn new() -> StampSlot {
        StampSlot::default()
    }

    /// Stores an order's `Ingress`, `Gateway` and `Risk` times (ns),
    /// replacing whatever was there. Four relaxed stores.
    #[inline]
    pub fn put(&self, ingress: u64, gateway: u64, risk: u64) {
        let s = &self.inner;
        s.ingress.store(ingress, Ordering::Relaxed);
        s.gateway.store(gateway, Ordering::Relaxed);
        s.risk.store(risk, Ordering::Relaxed);
        s.full.store(true, Ordering::Relaxed);
    }

    /// Empties the slot: the stored times as stamps, or `None` if nothing
    /// was put since the last take (a cancel, a reduce, a disconnect).
    #[inline]
    pub fn take(&self) -> Option<StageStamps> {
        let s = &self.inner;
        if !s.full.swap(false, Ordering::Relaxed) {
            return None;
        }
        let mut stamps = StageStamps::new();
        stamps.stamp(telemetry::Stage::Ingress, s.ingress.load(Ordering::Relaxed));
        stamps.stamp(telemetry::Stage::Gateway, s.gateway.load(Ordering::Relaxed));
        stamps.stamp(telemetry::Stage::Risk, s.risk.load(Ordering::Relaxed));
        Some(stamps)
    }
}

/// The most recent orders' [`StageStamps`], shared between the engine
/// thread (which adds a batch at a time and never waits) and whoever
/// reports (which copies them out). Clones share the log.
#[derive(Clone, Debug)]
pub struct StageLog {
    inner: Arc<Mutex<LogState>>,
    capacity: usize,
    /// Samples dropped while a reader held the lock (counted without it).
    busy_dropped: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct LogState {
    samples: VecDeque<StageStamps>,
    recorded: u64,
    dropped: u64,
}

/// Per-stage and end-to-end p50/p99/p99.9 of `samples`, one line each, in
/// nanoseconds. Built only on week 2's histogram and week 21's stamps, so
/// it works from week 24 on; week 39's `Attribution` adds tail ownership
/// and budgets on top. O(samples).
pub fn summarize(samples: &[StageStamps]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let line = |out: &mut String, name: &str, h: &telemetry::Histogram| {
        let _ = writeln!(
            out,
            "{name:>12} {:>9} {:>9} {:>9}",
            h.percentile(0.50),
            h.percentile(0.99),
            h.percentile(0.999)
        );
    };
    let _ = writeln!(
        out,
        "{:>12} {:>9} {:>9} {:>9}  ({} orders)",
        "stage",
        "p50",
        "p99",
        "p99.9",
        samples.len()
    );
    for stage in telemetry::Stage::ALL {
        let mut h = telemetry::Histogram::new();
        samples
            .iter()
            .filter_map(|s| s.interval(stage))
            .for_each(|v| h.record(v));
        if h.count() > 0 {
            line(&mut out, stage.name(), &h);
        }
    }
    let mut total = telemetry::Histogram::new();
    samples.iter().for_each(|s| total.record(s.total()));
    if total.count() > 0 {
        line(&mut out, "end to end", &total);
    }
    out
}

impl StageLog {
    /// A log keeping the last `capacity` samples (at least 1); 64 bytes
    /// each, allocated as they arrive.
    pub fn new(capacity: usize) -> StageLog {
        StageLog {
            inner: Arc::new(Mutex::new(LogState::default())),
            capacity: capacity.max(1),
            busy_dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Engine side: moves every sample in `batch` into the log if its lock
    /// is free, and returns at once otherwise — the samples stay in `batch`
    /// for the next call (a reader copying the log must not stall
    /// matching). Either way at most `capacity` samples are kept, the
    /// oldest dropped first and counted in [`dropped`](Self::dropped).
    /// O(batch).
    pub fn record(&self, batch: &mut Vec<StageStamps>) {
        if batch.is_empty() {
            return;
        }
        match self.inner.try_lock() {
            Ok(mut log) => self.append(&mut log, batch),
            Err(TryLockError::Poisoned(e)) => self.append(&mut e.into_inner(), batch),
            Err(TryLockError::WouldBlock) => {
                if batch.len() > self.capacity {
                    let excess = batch.len() - self.capacity;
                    batch.drain(..excess);
                    // Never wait for the reader: count without the lock.
                    self.busy_dropped
                        .fetch_add(excess as u64, Ordering::Relaxed);
                }
            }
        }
    }

    fn append(&self, log: &mut LogState, batch: &mut Vec<StageStamps>) {
        log.recorded += batch.len() as u64;
        log.samples.extend(batch.drain(..));
        if log.samples.len() > self.capacity {
            let excess = log.samples.len() - self.capacity;
            log.samples.drain(..excess);
            log.dropped += excess as u64;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LogState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A copy of the samples, oldest first. O(samples), allocates.
    pub fn samples(&self) -> Vec<StageStamps> {
        self.lock().samples.iter().copied().collect()
    }

    /// The samples whose first stamp (their `Ingress`) is at or after
    /// `t_ns` on `Clock::global()` — e.g. a load test's measured phase.
    pub fn samples_since(&self, t_ns: u64) -> Vec<StageStamps> {
        self.lock()
            .samples
            .iter()
            .filter(|s| s.at(0).is_some_and(|(_, t)| t >= t_ns))
            .copied()
            .collect()
    }

    /// Week 39's attribution of [`samples`](Self::samples).
    pub fn attribution(&self) -> Attribution {
        Attribution::from_samples(&self.samples())
    }

    /// Empties the log (the counters keep counting).
    pub fn clear(&self) {
        self.lock().samples.clear();
    }

    /// Samples recorded since the log was created.
    pub fn recorded(&self) -> u64 {
        self.lock().recorded
    }

    /// Samples dropped because the log was full (or busy and full).
    pub fn dropped(&self) -> u64 {
        self.lock().dropped + self.busy_dropped.load(Ordering::Relaxed)
    }

    /// [`summarize`] over the samples kept.
    pub fn summary(&self) -> String {
        summarize(&self.samples())
    }
}

/// Live counters of the exchange v2 stage. The engine thread updates them
/// with relaxed stores once per batch; read them from any thread with
/// [`snapshot`](Self::snapshot).
#[derive(Debug, Default)]
pub struct PipelineCounters {
    /// Engine batches flushed.
    pub batches: AtomicU64,
    /// Commands appended to the journals.
    pub journaled: AtomicU64,
    /// fsyncs issued (all journals).
    pub syncs: AtomicU64,
    /// L2 updates published.
    pub md_updates: AtomicU64,
    /// Trades published on the tape.
    pub trades: AtomicU64,
    /// Snapshots stored in the snapshot cells.
    pub snapshots: AtomicU64,
}

impl PipelineCounters {
    /// The counters now (each field read on its own, not atomically
    /// together).
    pub fn snapshot(&self) -> PipelineStats {
        let r = |c: &AtomicU64| c.load(Ordering::Relaxed);
        PipelineStats {
            batches: r(&self.batches),
            journaled: r(&self.journaled),
            syncs: r(&self.syncs),
            md_updates: r(&self.md_updates),
            trades: r(&self.trades),
            snapshots: r(&self.snapshots),
        }
    }
}

/// A reading of [`PipelineCounters`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Engine batches flushed.
    pub batches: u64,
    /// Commands appended to the journals.
    pub journaled: u64,
    /// fsyncs issued (all journals).
    pub syncs: u64,
    /// L2 updates published.
    pub md_updates: u64,
    /// Trades published on the tape.
    pub trades: u64,
    /// Snapshots stored.
    pub snapshots: u64,
}

// ---------------------------------------------------------------------------
// Start-up results (provided)

/// What starting found in one instrument's journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// The instrument.
    pub instrument: InstrumentId,
    /// Commands replayed into the engine (the whole journal: no snapshots).
    pub replayed: u64,
    /// The engine's sequence number afterwards (`Seq(0)`: empty journal).
    pub applied: Seq,
    /// A torn last write that was cut off, if any.
    pub torn_tail: Option<TornTail>,
    /// The recovered engine's [`Engine::state_hash`].
    pub state_hash: u64,
    /// The largest exchange order id in the journal (`OrderId(0)`: none).
    pub max_order_id: OrderId,
}

/// Why exchange v2 did not start.
#[derive(Debug)]
pub enum StartError {
    /// A socket, a thread or a journal file could not be opened.
    Io(io::Error),
    /// A journal is damaged (or unreadable): do not start, repair it.
    Recovery {
        /// Whose journal.
        instrument: InstrumentId,
        /// What recovery said.
        error: RecoveryError,
    },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Io(e) => write!(f, "exchange v2: {e}"),
            StartError::Recovery { instrument, error } => {
                write!(
                    f,
                    "exchange v2: journal of instrument {}: {error}",
                    instrument.0
                )
            }
        }
    }
}

impl std::error::Error for StartError {}

impl From<io::Error> for StartError {
    fn from(e: io::Error) -> Self {
        StartError::Io(e)
    }
}

/// The engine-thread half of exchange v2, without the network: what
/// [`assemble`] builds and [`ExchangeV2::start`] runs behind the gateway.
/// Drive [`sequencer`](Self::sequencer) directly for in-process
/// measurements (`command`, then `flush`).
pub struct Assembled {
    /// Engines (recovered), id map and the exchange v2 hooks.
    pub sequencer: Sequencer,
    /// One per instrument, in configuration order.
    pub feeds: Vec<Feed>,
    /// Stage stamps of recent orders.
    pub log: StageLog,
    /// The stage's counters.
    pub counters: Arc<PipelineCounters>,
    /// What each journal held at start.
    pub recovery: Vec<Recovery>,
}

/// What [`ExchangeV2::shutdown`] did.
#[derive(Clone, Debug)]
pub struct V2Report {
    /// The gateway's shutdown (drain, writers, engine thread).
    pub gateway: ShutdownReport,
    /// The stage's counters at the end, the final sync included.
    pub pipeline: PipelineStats,
}

impl V2Report {
    /// True if the gateway shut down cleanly (and so the journals were
    /// synced by the exiting engine thread).
    pub fn is_clean(&self) -> bool {
        self.gateway.is_clean()
    }
}

// ---------------------------------------------------------------------------
// Week 24: the learner's wiring

/// Week 24: a pre-trade check that also stamps the order — `Gateway` when
/// the engine thread picks it up (just before the check), `Risk` when the
/// check passes — and hands both, with the receive time `rx_ns` as
/// `Ingress`, to the stage through a [`StampSlot`]. Rejected orders leave
/// the slot empty. Two clock reads per order on top of `P`'s check.
pub struct TimedPreTrade<P> {
    // TODO(w24): design this type's fields.
    _todo: std::marker::PhantomData<(P,)>,
}

#[allow(unused_variables)]
impl<P: PreTrade> TimedPreTrade<P> {
    /// Wraps `inner`, handing stamps to whoever holds a clone of `slot`.
    pub fn new(inner: P, slot: StampSlot) -> TimedPreTrade<P> {
        todo!("w24: new")
    }

    /// The wrapped check.
    pub fn inner(&self) -> &P {
        todo!("w24: inner")
    }
}

#[allow(unused_variables)]
impl<P: PreTrade> PreTrade for TimedPreTrade<P> {
    fn check(
        &mut self,
        session: SessionId,
        order: &NewOrder,
        rx_ns: u64,
        reference: Option<Price>,
    ) -> Result<(), RejectReason> {
        todo!("w24: check")
    }

    fn on_event(&mut self, event: &Event) {
        todo!("w24: on_event")
    }

    fn on_close(&mut self, session: SessionId) {
        todo!("w24: on_close")
    }
}

/// One instrument's recovered state, handed to [`ExchangeStage::new`].
pub struct LaneParts<S> {
    /// The instrument.
    pub instrument: InstrumentId,
    /// Its journal, positioned right after the engine's state.
    pub journal: FileJournal<S, SystemClock>,
    /// Its L2 publisher, primed with the engine's book.
    pub publisher: L2Publisher,
}

/// Week 24: the exchange v2 [`Stage`] — the journal as the write-ahead
/// point, market-data fan-out, durable-before-visible output gating and
/// stage stamps (see the module docs).
///
/// Per command: one buffered journal append, one publisher update per
/// event, a clock read for sampled orders. Per batch: a write and (under
/// `Fsync::Always`) an fsync per instrument that had commands, one ring
/// publish per update and trade, and occasionally a snapshot (O(levels),
/// allocates). No allocation per command once buffers have grown.
pub struct ExchangeStage<S: Storage = FileStorage> {
    // TODO(w24): design this type's fields.
    _todo: std::marker::PhantomData<(S,)>,
}

#[allow(unused_variables)]
impl<S: Storage> ExchangeStage<S> {
    /// A stage over `lanes` (one per instrument), committing under `fsync`,
    /// publishing per `feed` (its `listen` is not used here), taking
    /// `Ingress`/`Gateway`/`Risk` stamps from `slot` and recording samples
    /// in `log`. Returns the stage and one [`Feed`] per lane, whose
    /// snapshot cells already hold each book as recovered. Allocates the
    /// rings.
    pub fn new(
        lanes: Vec<LaneParts<S>>,
        fsync: Fsync,
        feed: &FeedConfig,
        slot: StampSlot,
        log: StageLog,
        counters: Arc<PipelineCounters>,
    ) -> (ExchangeStage<S>, Vec<Feed>) {
        todo!("w24: new")
    }

    /// Instruments, in lane order.
    pub fn instruments(&self) -> Vec<InstrumentId> {
        todo!("w24: instruments")
    }

    /// `instrument`'s journal.
    pub fn journal(&self, instrument: InstrumentId) -> Option<&FileJournal<S, SystemClock>> {
        todo!("w24: journal")
    }
}

#[allow(unused_variables)]
impl<S: Storage + Send> Stage for ExchangeStage<S> {
    fn on_command(&mut self, instrument: InstrumentId, cmd: &Command) {
        todo!("w24: on_command")
    }

    fn on_events(&mut self, instrument: InstrumentId, _cmd: &Command, events: &[Event]) {
        todo!("w24: on_events")
    }

    fn flush(&mut self) {
        todo!("w24: flush")
    }

    fn on_idle(&mut self) {
        todo!("w24: on_idle")
    }
}

#[allow(unused_variables)]
impl<S: Storage> Drop for ExchangeStage<S> {
    /// The engine thread is exiting (shutdown): sync every journal, so a
    /// clean shutdown leaves nothing to a later power cut.
    fn drop(&mut self) {
        todo!("w24: drop")
    }
}

/// Week 24: recovers every instrument's engine from its journal, wires the
/// exchange v2 hooks (`pretrade` behind a [`TimedPreTrade`], an
/// [`ExchangeStage`]) and returns them around a [`Sequencer`] whose
/// exchange ids continue after the journals' largest. Reads every journal
/// in full (O(journal bytes) time and memory, once).
#[allow(unused_variables)]
pub fn assemble(
    config: &V2Config,
    pretrade: impl PreTrade + 'static,
) -> Result<Assembled, StartError> {
    todo!("w24: assemble")
}

/// Week 24: exchange v2, running — the gateway around the assembled engine
/// thread, plus a WebSocket feed server per instrument when configured.
/// Start it with [`ExchangeV2::start`], stop it with
/// [`ExchangeV2::shutdown`] (dropping it shuts down too). `start`,
/// `shutdown` and `drop` block; call them outside any async runtime.
pub struct ExchangeV2 {
    // TODO(w24): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl ExchangeV2 {
    /// Recovers the journals, assembles the pipeline ([`assemble`]),
    /// starts the WebSocket servers (if `config.feed.listen` is set) and
    /// the gateway. Fails on a damaged journal or a socket that cannot be
    /// bound.
    pub fn start(
        config: V2Config,
        pretrade: impl PreTrade + 'static,
    ) -> Result<ExchangeV2, StartError> {
        todo!("w24: start")
    }

    /// The order-entry address.
    pub fn local_addr(&self) -> SocketAddr {
        todo!("w24: local_addr")
    }

    /// Every instrument's feed, in configuration order.
    pub fn feeds(&self) -> &[Feed] {
        todo!("w24: feeds")
    }

    /// `instrument`'s feed.
    pub fn feed(&self, instrument: InstrumentId) -> Option<&Feed> {
        todo!("w24: feed")
    }

    /// Where each instrument's WebSocket feed is served (empty when not
    /// configured).
    pub fn md_addrs(&self) -> Vec<(InstrumentId, SocketAddr)> {
        todo!("w24: md_addrs")
    }

    /// The stage stamps of recent orders.
    pub fn stage_log(&self) -> &StageLog {
        todo!("w24: stage_log")
    }

    /// What each journal held at start.
    pub fn recovery(&self) -> &[Recovery] {
        todo!("w24: recovery")
    }

    /// The gateway's counters (busy rejects included).
    pub fn stats(&self) -> GatewayStats {
        todo!("w24: stats")
    }

    /// The pipeline stage's counters.
    pub fn pipeline(&self) -> PipelineStats {
        todo!("w24: pipeline")
    }

    /// Stops order entry and drains it (every accepted order is sequenced,
    /// journaled and reported, see [`Gateway::shutdown`]); the exiting
    /// engine thread syncs the journals. Then stops the WebSocket servers.
    pub fn shutdown(self, timeout: Duration) -> V2Report {
        todo!("w24: shutdown")
    }
}

// TODO(w24): implement — see the week guide on tepy.dev/hft.
