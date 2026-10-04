//! Sequenced append-only journal, replay, snapshots, recovery and replication.
//!
//! Curriculum: Weeks 9, 18, 20 (see <https://tepy.dev/hft>).
//!
//! # Week 9 — deterministic event sourcing
//!
//! Every inbound [`Command`](types::Command) is assigned a sequence number
//! and appended to the [`Journal`] *before* it is applied. Because the
//! engine is a deterministic function of that sequence, the journal is the
//! source of truth: [`replay()`] rebuilds any [`StateMachine`] from it, and
//! the [`state_hash`](StateMachine::state_hash) of the result is
//! identical, bit for bit, to the live one.
//!
//! * [`codec`] — the compact binary encoding of a command
//!   ([`encode_command`] / [`decode_command`], layout documented there).
//! * [`crc`] — CRC-32/IEEE, table driven ([`crc32`], [`Crc32`]).
//! * [`frame`] — the frame format (`[len u32][seq u64][crc u32][payload]`,
//!   little-endian), the verifying [`Frames`] iterator and corruption
//!   reports.
//! * [`memory`] — the in-memory [`Journal`] (the sequencer's log).
//! * [`replay()`](mod@replay) — the [`StateMachine`] trait, [`replay()`] and the
//!   deterministic [`StateHasher`]; [`replay_frames`] replays raw bytes.
//! * [`log`] — [`CommandLog`], the sequencer's view of any journal (the
//!   engine pipeline of week 12 appends through it).
//!
//! # Week 18 — persistence and recovery
//!
//! * [`file`](mod@file) — [`FileJournal`]: the same frames in a file, with a
//!   [`SyncPolicy`] (every N commands, every T, never) and recovery that
//!   truncates a torn last write but refuses mid-file corruption
//!   ([`classify_damage`] scans ahead for a valid frame to tell them apart).
//! * [`storage`] — the [`Storage`] it writes to: [`FileStorage`], or
//!   [`MemStorage`], which counts fsyncs and simulates power loss.
//! * [`index`] — [`SeqIndex`], a sparse seq → offset index, so reading
//!   from the middle does not scan from the start.
//! * [`snapshot`] — [`Snapshottable`] state machines and the atomic,
//!   checksummed [`SnapshotStore`].
//! * [`recovery`] — [`recover`]: newest valid snapshot + journal tail.
//!
//! # Week 20 — failover and hot standby
//!
//! * [`replication`] — a [`Primary`] streams frames to a [`Standby`], which
//!   validates them ([`Journal::append_frame`]: CRC, sequence continuity),
//!   applies them to its own state machine and acknowledges; leases on an
//!   injected [`Clock`] ([`ManualClock`] in tests) decide who leads.
//!
//! ```
//! use journal::{replay, Journal, StateMachine};
//! use types::{Command, NewOrder, Seq, Side};
//!
//! #[derive(Default)]
//! struct Count(u64);
//! impl StateMachine for Count {
//!     fn apply(&mut self, _: &Command) { self.0 += 1 }
//!     fn state_hash(&self) -> u64 { self.0 }
//! }
//!
//! let mut j = Journal::new();
//! assert_eq!(j.append(&Command::New(NewOrder::limit(1, Side::Buy, 100, 5))), Seq(1));
//! assert_eq!(j.append(&Command::Cancel { id: types::OrderId(1) }), Seq(2));
//! let mut c = Count::default();
//! let done = replay(&j, j.first_seq(), &mut c).unwrap();
//! assert_eq!((done.applied, c.0), (2, 2));
//! ```

pub mod clock;
pub mod codec;
pub mod crc;
pub mod file;
pub mod frame;
pub mod index;
pub mod log;
pub mod memory;
pub mod recovery;
pub mod replay;
pub mod replication;
pub mod snapshot;
pub mod storage;

pub use clock::{Clock, ManualClock, SystemClock};
pub use codec::{decode_command, encode_command, encoded_len, DecodeError, MAX_COMMAND_LEN};
pub use crc::{crc32, Crc32};
pub use file::{
    classify_damage, find_frame, Damage, FileJournal, JournalError, JournalOptions, Opened,
    SyncPolicy, TornTail,
};
pub use frame::{
    Corruption, CorruptionKind, Frame, FrameError, Frames, FIRST_SEQ, FRAME_HEADER_LEN,
    MAX_PAYLOAD_LEN,
};
pub use index::SeqIndex;
pub use log::CommandLog;
pub use memory::Journal;
pub use recovery::{recover, Recovered, RecoveryError};
pub use replay::{replay, replay_frames, ReplayError, Replayed, StateHasher, StateMachine};
pub use replication::{
    link, FrameBuf, Handled, Primary, PrimaryLink, ReplicationError, Standby, StandbyLink,
    StandbyStats, ToPrimary, ToStandby, MAX_FRAME_LEN,
};
pub use snapshot::{LoadedSnapshot, SnapshotError, SnapshotStore, Snapshottable};
pub use storage::{FileStorage, MemStorage, Storage};
