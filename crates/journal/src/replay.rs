//! Deterministic replay: rebuild any state machine from the journal.
//!
//! The exchange is a pure function of its sequenced input: the same
//! commands, applied in the same order to the same initial state, produce
//! the same state — bit for bit. Replay is how that property is used
//! (recovery, standbys, debugging) and [`StateMachine::state_hash`] is how
//! it is checked.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::codec::DecodeError;
use crate::frame::{Corruption, Frames};
use crate::memory::Journal;
use std::fmt;
use types::{Command, Seq};

/// Anything rebuilt from the command journal: the matching engine, a risk
/// view, the toy machines in tests.
///
/// `apply` must be deterministic — a function of the current state and
/// `cmd` only: no clocks, no randomness, no iteration over `HashMap`s
/// whose order differs between runs or processes.
pub trait StateMachine {
    /// Applies one sequenced command.
    fn apply(&mut self, cmd: &Command);

    /// A digest of the complete state. Equal states must hash equal across
    /// runs and processes (use [`StateHasher`], never `RandomState`).
    fn state_hash(&self) -> u64;
}

/// What a replay applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Replayed {
    /// Number of commands applied.
    pub applied: u64,
    /// Sequence number of the last command applied.
    pub last_seq: Option<Seq>,
}

/// Why a replay stopped early. Every variant that can occur mid-replay
/// carries the progress made before stopping: the state machine reflects
/// exactly the commands up to `progress.last_seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// `from` precedes the journal's first frame, so commands between them
    /// are missing (the journal is a tail: replay its snapshot first).
    /// Nothing was applied.
    BeforeStart { from: Seq, first: Seq },
    /// A damaged frame; replay stopped at the last good one.
    Corrupt { at: Corruption, progress: Replayed },
    /// A frame passed its checksum but does not decode — the writer is
    /// broken, not the storage.
    Decode {
        seq: Seq,
        error: DecodeError,
        progress: Replayed,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::BeforeStart { from, first } => {
                write!(
                    f,
                    "replay from seq {} but the journal starts at {}",
                    from.0, first.0
                )
            }
            ReplayError::Corrupt { at, progress } => {
                write!(f, "{at} (after {} commands)", progress.applied)
            }
            ReplayError::Decode {
                seq,
                error,
                progress,
            } => {
                write!(
                    f,
                    "seq {}: {error} (after {} commands)",
                    seq.0, progress.applied
                )
            }
        }
    }
}

impl std::error::Error for ReplayError {}

/// Applies every journaled command with sequence number `>= from`, in
/// order, to `sm`. `from` must be within the journal
/// (`journal.first_seq() ..= journal.next_seq()`); starting past the end
/// applies nothing.
///
/// Stops at the first damaged or undecodable frame and reports how far it
/// got. O(journal bytes), no allocation.
#[allow(unused_variables)]
pub fn replay<S: StateMachine + ?Sized>(
    journal: &Journal,
    from: Seq,
    sm: &mut S,
) -> Result<Replayed, ReplayError> {
    todo!("w09: replay")
}

/// Applies every frame `frames` yields, in order, to `sm` — replay straight
/// from raw journal bytes (`Frames::new(bytes, first_seq)`), e.g. a file
/// read back after a crash or a stream received from a primary, without
/// building a [`Journal`] first.
///
/// Stops at the first damaged or undecodable frame; `sm` then reflects
/// exactly the frames before it. O(bytes), no allocation.
#[allow(unused_variables)]
pub fn replay_frames<S: StateMachine + ?Sized>(
    frames: Frames<'_>,
    sm: &mut S,
) -> Result<Replayed, ReplayError> {
    todo!("w09: replay_frames")
}

/// A deterministic 64-bit [`Hasher`](std::hash::Hasher) for
/// [`StateMachine::state_hash`]: FNV-1a, with no random keys, so the same
/// state hashes the same in every run and every process.
///
/// Feed it state in a deterministic order (sorted maps, price levels best
/// first, FIFO queues front to back). FNV-1a is ~1 ns per byte — fine for
/// verification, not for hash tables on the hot path.
///
/// ```
/// use std::hash::{Hash, Hasher};
/// let mut a = journal::StateHasher::new();
/// let mut b = journal::StateHasher::new();
/// (1u64, 2u32).hash(&mut a);
/// (1u64, 2u32).hash(&mut b);
/// assert_eq!(a.finish(), b.finish());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateHasher {
    // TODO(w09): design this type's fields.
    _todo: (),
}

/// FNV-1a 64-bit offset basis.
pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[allow(unused_variables)]
impl StateHasher {
    /// A hasher in its initial state ([`FNV_OFFSET`]).
    pub fn new() -> StateHasher {
        todo!("w09: new")
    }
}

impl Default for StateHasher {
    fn default() -> Self {
        StateHasher::new()
    }
}

#[allow(unused_variables)]
impl std::hash::Hasher for StateHasher {
    fn write(&mut self, bytes: &[u8]) {
        todo!("w09: write")
    }

    fn finish(&self) -> u64 {
        todo!("w09: finish")
    }
}
