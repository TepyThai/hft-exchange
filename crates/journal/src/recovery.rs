//! Recovery after a restart (week 18): newest snapshot + journal tail.
//!
//! ```text
//!  journal file   [1 ............................ s][s+1 ........ last][torn?]
//!  snapshot s     ^^^^^^^^^^ state at s ^^^^^^^^^^^
//!  recovery       load snapshot s, replay s+1 ..= last, truncate the torn part
//! ```
//!
//! The order of checks matters: the journal is opened (and verified)
//! first, because a snapshot is only usable if the journal still holds
//! everything up to its sequence number — a snapshot *ahead* of the journal
//! means synced journal data went missing, and recovery must not paper over
//! it.

use crate::clock::Clock;
use crate::file::{FileJournal, JournalError, JournalOptions, TornTail};
use crate::replay::ReplayError;
use crate::snapshot::{SnapshotError, SnapshotStore, Snapshottable};
use crate::storage::Storage;
use std::fmt;
use types::Seq;

// @student-begin w18
use crate::replay::replay_frames;
// @student-end

/// What [`recover`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Recovered {
    /// Sequence number of the snapshot loaded, if any.
    pub snapshot: Option<Seq>,
    /// Damaged snapshot files skipped in favour of an older one.
    pub skipped_snapshots: usize,
    /// Journal commands replayed on top of the snapshot.
    pub replayed: u64,
    /// The machine's sequence number afterwards (`Seq(0)`: empty).
    pub applied: Seq,
    /// The torn last write the journal truncated, if any.
    pub torn_tail: Option<TornTail>,
}

/// Why [`recover`] failed. Every variant means "do not start": the data on
/// disk needs attention (or a replica).
#[derive(Debug)]
pub enum RecoveryError {
    /// The journal could not be opened: I/O, or corruption before its end.
    Journal(JournalError),
    /// The snapshot store failed, or the machine rejected a snapshot that
    /// passed its checksum.
    Snapshot(SnapshotError),
    /// The snapshot includes commands the journal no longer has (journal
    /// data was lost after the snapshot was taken).
    SnapshotAhead { snapshot: Seq, journal_next: Seq },
    /// The machine's sequence number does not match the snapshot it loaded
    /// (a bug in its `load_snapshot`).
    SeqMismatch { expected: Seq, found: Seq },
    /// Replaying the tail failed (a frame that verifies but does not
    /// decode).
    Replay(ReplayError),
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecoveryError::Journal(e) => write!(f, "recovery: {e}"),
            RecoveryError::Snapshot(e) => write!(f, "recovery: {e}"),
            RecoveryError::SnapshotAhead {
                snapshot,
                journal_next,
            } => write!(
                f,
                "recovery: snapshot at seq {} but the journal ends before seq {}",
                snapshot.0, journal_next.0
            ),
            RecoveryError::SeqMismatch { expected, found } => write!(
                f,
                "recovery: loaded snapshot {} but the machine is at seq {}",
                expected.0, found.0
            ),
            RecoveryError::Replay(e) => write!(f, "recovery: {e}"),
        }
    }
}

impl std::error::Error for RecoveryError {}

impl From<JournalError> for RecoveryError {
    fn from(e: JournalError) -> Self {
        RecoveryError::Journal(e)
    }
}

impl From<SnapshotError> for RecoveryError {
    fn from(e: SnapshotError) -> Self {
        RecoveryError::Snapshot(e)
    }
}

impl From<ReplayError> for RecoveryError {
    fn from(e: ReplayError) -> Self {
        RecoveryError::Replay(e)
    }
}

/// Recovers `machine` (in its initial state) and the journal in `storage`:
///
/// 1. open the journal ([`FileJournal::open`]: verify every frame, truncate
///    a torn tail, refuse mid-file corruption);
/// 2. load the newest valid snapshot from `snapshots`, if given and any
///    exists, unless it is ahead of the journal (an error);
/// 3. replay the journal from the machine's sequence number + 1, seeking
///    through the journal's index instead of reading from the start.
///
/// Returns the journal, ready to append the next command, and a report.
/// Cost: O(journal bytes) to verify, plus O(snapshot) plus O(tail) to
/// replay.
// @student w18
pub fn recover<M, S, C>(
    storage: S,
    snapshots: Option<&SnapshotStore>,
    opts: JournalOptions,
    clock: C,
    machine: &mut M,
) -> Result<(FileJournal<S, C>, Recovered), RecoveryError>
where
    M: Snapshottable + ?Sized,
    S: Storage,
    C: Clock,
{
    let (mut journal, opened) = FileJournal::open(storage, opts, clock)?;
    let mut report = Recovered {
        torn_tail: opened.torn_tail,
        ..Recovered::default()
    };
    if let Some(snap) = snapshots
        .map(SnapshotStore::load_latest)
        .transpose()?
        .flatten()
    {
        if snap.seq >= journal.next_seq() {
            return Err(RecoveryError::SnapshotAhead {
                snapshot: snap.seq,
                journal_next: journal.next_seq(),
            });
        }
        machine.load_snapshot(&snap.state)?;
        if machine.applied_seq() != snap.seq {
            return Err(RecoveryError::SeqMismatch {
                expected: snap.seq,
                found: machine.applied_seq(),
            });
        }
        report.snapshot = Some(snap.seq);
        report.skipped_snapshots = snap.skipped;
    }
    let from = Seq(machine.applied_seq().0 + 1);
    let mut buf = Vec::new();
    let replayed = replay_frames(journal.iter_from(from, &mut buf)?, machine)?;
    report.replayed = replayed.applied;
    report.applied = machine.applied_seq();
    Ok((journal, report))
}
