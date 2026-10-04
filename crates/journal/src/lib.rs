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

pub mod codec;
pub mod crc;
pub mod frame;
pub mod memory;
pub mod replay;

pub use codec::{decode_command, encode_command, encoded_len, DecodeError, MAX_COMMAND_LEN};
pub use crc::{crc32, Crc32};
pub use frame::{
    Corruption, CorruptionKind, Frame, Frames, FIRST_SEQ, FRAME_HEADER_LEN, MAX_PAYLOAD_LEN,
};
pub use memory::Journal;
pub use replay::{replay, replay_frames, ReplayError, Replayed, StateHasher, StateMachine};
