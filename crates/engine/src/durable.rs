//! Week 18: a durable engine — the pipeline over a [`FileJournal`], with
//! snapshots and crash recovery.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use engine::{recover, EngineConfig};
//! use journal::{FileStorage, JournalOptions, SnapshotStore, SyncPolicy, SystemClock};
//! use types::{Command, NewOrder, Side};
//!
//! let dir = std::env::temp_dir().join(format!("engine-durable-doc-{}", std::process::id()));
//! let store = SnapshotStore::open(dir.join("snapshots"))?;
//! let opts = JournalOptions::with_sync(SyncPolicy::EveryN(1));
//! let open = || -> Result<_, Box<dyn std::error::Error>> {
//!     let storage = FileStorage::open(dir.join("journal.log"))?;
//!     Ok(recover(EngineConfig::default(), storage, Some(&store), opts, SystemClock::new())?)
//! };
//!
//! let (mut exchange, _) = open()?;
//! exchange.process(&Command::New(NewOrder::limit(1, Side::Buy, 100, 5)))?;
//! exchange.checkpoint(&store)?; // snapshot at seq 1
//! exchange.process(&Command::New(NewOrder::limit(2, Side::Sell, 101, 5)))?;
//! let live = exchange.engine().state_hash();
//! drop(exchange); // EveryN(1): both commands are already durable
//!
//! let (restarted, report) = open()?;
//! assert_eq!((report.snapshot, report.replayed), (Some(types::Seq(1)), 1));
//! assert_eq!(restarted.engine().state_hash(), live);
//! # std::fs::remove_dir_all(&dir)?;
//! # Ok(())
//! # }
//! ```

use crate::{EngineConfig, Pipeline};
use book::{Book, OrderBook};
use journal::{
    Clock, FileJournal, FileStorage, JournalError, JournalOptions, Recovered, RecoveryError,
    SnapshotError, SnapshotStore, Storage, SystemClock,
};
use std::fmt;
use types::Seq;

// @student-begin w18
use crate::Engine;
use journal::Snapshottable;
// @student-end

/// Engine v1 over a durable [`FileJournal`] — what [`recover`] returns.
pub type DurablePipeline<S = FileStorage, C = SystemClock, B = Book> =
    Pipeline<FileJournal<S, C>, B>;

/// Why [`Pipeline::checkpoint`] failed.
#[derive(Debug)]
pub enum CheckpointError {
    /// Syncing the journal failed (the snapshot was not written).
    Journal(JournalError),
    /// Writing the snapshot failed.
    Snapshot(SnapshotError),
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointError::Journal(e) => write!(f, "checkpoint: {e}"),
            CheckpointError::Snapshot(e) => write!(f, "checkpoint: {e}"),
        }
    }
}

impl std::error::Error for CheckpointError {}

/// Rebuilds a durable pipeline after a (re)start: opens the journal in
/// `storage` (truncating a torn last write, refusing mid-file corruption),
/// restores the newest valid snapshot in `snapshots` and replays the
/// journal tail after it. An empty storage gives a fresh engine.
///
/// See [`journal::recover`] for the steps and their cost.
// @student w18
pub fn recover<S: Storage, C: Clock>(
    config: EngineConfig,
    storage: S,
    snapshots: Option<&SnapshotStore>,
    opts: JournalOptions,
    clock: C,
) -> Result<(DurablePipeline<S, C>, Recovered), RecoveryError> {
    let mut engine = Engine::with_config(config);
    let (journal, report) = journal::recover(storage, snapshots, opts, clock, &mut engine)?;
    let pipeline = Pipeline::with_journal(journal, engine)
        .expect("recovery leaves the journal right after the engine");
    Ok((pipeline, report))
}

// @student w18
impl<S: Storage, C: Clock, B: OrderBook> DurablePipeline<S, C, B> {
    /// Takes a snapshot: syncs the journal first (a snapshot must never be
    /// ahead of the durable journal), then writes the engine's state at its
    /// current sequence number to `store`, atomically. Returns that
    /// sequence number.
    ///
    /// Cost: an fsync, O(resting orders) to encode, and the snapshot file's
    /// write + two fsyncs. Runs on the sequencer thread, so the engine is
    /// paused meanwhile — fine every few seconds, not per command.
    pub fn checkpoint(&mut self, store: &SnapshotStore) -> Result<Seq, CheckpointError> {
        self.journal_mut()
            .sync()
            .map_err(CheckpointError::Journal)?;
        let mut state = Vec::new();
        self.engine().save_snapshot(&mut state);
        let seq = self.engine().seq();
        store.save(seq, &state).map_err(CheckpointError::Snapshot)?;
        Ok(seq)
    }
}
