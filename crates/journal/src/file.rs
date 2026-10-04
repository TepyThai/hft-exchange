//! The file-backed journal (week 18): durable appends under a configurable
//! fsync policy, crash recovery that tells a torn last write from real
//! corruption, and a sparse index for reading from the middle.
//!
//! # On disk
//!
//! One file of back-to-back [frames](crate::frame), exactly what the
//! [in-memory journal](crate::Journal) holds, from the first sequence
//! number on. Nothing else: no header, no footer, no padding. The file only
//! grows; a snapshot never truncates its prefix (that would be compaction,
//! which needs segment files).
//!
//! # Durability
//!
//! An append lands in a user-space write buffer. It reaches the kernel
//! (and survives a *process* crash) when the buffer is written out, and
//! reaches the disk (and survives a *power loss*) when it is synced:
//!
//! | [`SyncPolicy`] | fsyncs | a power loss can lose |
//! | --- | --- | --- |
//! | `EveryN(1)` | one per command | nothing that `append` returned |
//! | `EveryN(n)` | one per `n` commands (group commit) | the last `< n` commands |
//! | `Interval(t)` | at most one per `t` | the last `t` of commands |
//! | `Never` | only on [`sync`](FileJournal::sync) | everything since the last sync |
//!
//! [`durable_seq`](FileJournal::durable_seq) says how far is safe — a
//! gateway that promises durability acknowledges a command only once it is
//! covered.
//!
//! # Recovery: torn tail or corruption?
//!
//! A crash in the middle of a write leaves the *last* frame incomplete:
//! cut short, or full length with garbage where the data never landed.
//! That is expected, and the fix is to truncate it — the command was never
//! acknowledged as durable. Damage anywhere *before* the end is a different
//! animal: bit rot, a bad disk, a buggy writer. Truncating there would
//! silently discard every command after it, including acknowledged ones,
//! so it must stop recovery instead.
//!
//! From the damaged frame alone the two look the same: a flipped bit in a
//! length field can make a mid-file frame appear to run past the end of the
//! file, exactly like a torn write. The difference is what comes *after*
//! it: [`classify_damage`] scans ahead for any self-consistent frame (sane
//! length, matching CRC). One exists → mid-file corruption; none → a torn
//! tail.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::clock::{Clock, SystemClock};
use crate::frame::{Corruption, Frames};
use crate::index::SeqIndex;
use crate::storage::{FileStorage, Storage};
use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;
use types::{Command, Seq};

// TODO(w18): implement — see the week guide on tepy.dev/hft.

/// When the journal fsyncs. See the [module docs](self) for what each
/// policy can lose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    /// Sync after every `n` appends (`n = 0` is treated as 1). `EveryN(1)`
    /// makes every command durable before `append` returns.
    EveryN(u64),
    /// Sync on the first append (or [`poll`](FileJournal::poll)) at least
    /// this long after the previous sync.
    Interval(Duration),
    /// Never sync on its own; only [`sync`](FileJournal::sync) does.
    Never,
}

/// Tuning of a [`FileJournal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalOptions {
    /// When to fsync.
    pub sync: SyncPolicy,
    /// Write the buffer out (without syncing) once it holds this many
    /// bytes. Bigger means fewer `write` calls and more lost on a process
    /// crash.
    pub buffer_bytes: usize,
    /// Keep the offset of every `index_stride`-th frame (see [`SeqIndex`]).
    pub index_stride: u64,
}

impl JournalOptions {
    /// The defaults with sync policy `sync`.
    pub fn with_sync(sync: SyncPolicy) -> JournalOptions {
        JournalOptions {
            sync,
            ..JournalOptions::default()
        }
    }
}

impl Default for JournalOptions {
    /// Sync every command, a 64 KiB write buffer, an index entry every 1024
    /// frames.
    fn default() -> Self {
        JournalOptions {
            sync: SyncPolicy::EveryN(1),
            buffer_bytes: 64 * 1024,
            index_stride: 1024,
        }
    }
}

/// Why a [`FileJournal`] operation failed.
#[derive(Debug)]
pub enum JournalError {
    /// The storage failed. The journal's state is unknown afterwards (a
    /// write may have landed partly): drop it and reopen, which truncates
    /// any partial frame.
    Io(io::Error),
    /// The journal is damaged *before* its end — valid frames follow the
    /// damage at `resumes_at`, so it is not a torn write. Nothing was
    /// changed; a human (or a replica) has to repair it.
    Corrupt {
        /// The first damaged frame.
        at: Corruption,
        /// Byte offset of the next self-consistent frame after it.
        resumes_at: u64,
    },
    /// A read asked for frames outside `first ..= next`.
    OutOfRange {
        /// The sequence number asked for.
        from: Seq,
        /// The journal's first sequence number.
        first: Seq,
        /// The sequence number the next append gets.
        next: Seq,
    },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Io(e) => write!(f, "journal i/o error: {e}"),
            JournalError::Corrupt { at, resumes_at } => {
                write!(f, "{at}; valid frames resume at byte {resumes_at}")
            }
            JournalError::OutOfRange { from, first, next } => write!(
                f,
                "seq {} is outside the journal ({} ..= {})",
                from.0, first.0, next.0
            ),
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalError::Io(e) => Some(e),
            JournalError::Corrupt { at, .. } => Some(at),
            JournalError::OutOfRange { .. } => None,
        }
    }
}

impl From<io::Error> for JournalError {
    fn from(e: io::Error) -> Self {
        JournalError::Io(e)
    }
}

/// An incomplete last frame that [`FileJournal::open`] cut off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TornTail {
    /// Where and how the last frame was damaged.
    pub at: Corruption,
    /// Bytes truncated away.
    pub dropped_bytes: u64,
}

/// What [`FileJournal::open`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    /// Valid frames in the journal.
    pub frames: u64,
    /// Size of the valid frames in bytes (the file size after opening).
    pub bytes: u64,
    /// The torn last write that was truncated, if any.
    pub torn_tail: Option<TornTail>,
}

/// What kind of damage stopped a scan of journal bytes (see
/// [`classify_damage`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Nothing valid follows: an interrupted last write. Safe to truncate.
    TornTail,
    /// A self-consistent frame starts at byte `resumes_at`, after the
    /// damage: corruption inside the journal. Not safe to truncate.
    MidFile { resumes_at: u64 },
}

/// Offset of the first self-consistent frame starting at or after `from`
/// in `bytes`: a header whose length is at most
/// `MAX_PAYLOAD_LEN`, whose
/// frame fits in `bytes`, and whose CRC matches. Sequence numbers are not
/// checked. Worst case O(bytes × frame length); in practice nearly every
/// position fails the length check and costs a few instructions.
#[allow(unused_variables)]
pub fn find_frame(bytes: &[u8], from: usize) -> Option<usize> {
    todo!("w18: find_frame")
}

/// Tells a torn last write from mid-file corruption, given the `bytes` of
/// a journal and the first damage a [`Frames`] scan of them reported.
///
/// * An intact frame with the wrong sequence number (`SeqMismatch`) is
///   never a torn write.
/// * Otherwise the damage is mid-file if [`find_frame`] finds a frame
///   anywhere after the damaged frame's first byte, and a torn tail if not.
///
/// O(bytes after the damage) in practice.
#[allow(unused_variables)]
pub fn classify_damage(bytes: &[u8], at: &Corruption) -> Damage {
    todo!("w18: classify_damage")
}

/// A sequenced journal in [`Storage`] (a file by default): the in-memory
/// [in-memory journal](crate::Journal)'s format and sequencing, made durable.
///
/// * [`open`](FileJournal::open) recovers whatever is in the storage,
///   truncating a torn last write and refusing mid-file corruption.
/// * [`append`](FileJournal::append) assigns the next sequence number,
///   buffers the frame and syncs as the [`SyncPolicy`] says.
/// * [`iter_from`](FileJournal::iter_from) reads frames back from any
///   sequence number, seeking through the [`SeqIndex`].
///
/// Not thread-safe by itself (`&mut self` everywhere): one sequencer thread
/// owns it. Dropping it discards the write buffer, as a crash would — call
/// [`sync`](FileJournal::sync) or [`flush`](FileJournal::flush) first.
#[derive(Debug)]
pub struct FileJournal<S = FileStorage, C = SystemClock> {
    // TODO(w18): design this type's fields.
    _todo: std::marker::PhantomData<(S, C,)>,
}

// TODO(w18): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl FileJournal<FileStorage, SystemClock> {
    /// Opens (creating if missing) the journal file at `path` with the
    /// system clock. See [`open`](FileJournal::open).
    pub fn open_path(
        path: impl AsRef<Path>,
        opts: JournalOptions,
    ) -> Result<(Self, Opened), JournalError> {
        todo!("w18: open_path")
    }
}

#[allow(unused_variables)]
impl<S: Storage, C: Clock> FileJournal<S, C> {
    /// Recovers the journal held in `storage` and readies it for appends.
    ///
    /// Verifies every frame (CRC and sequence), rebuilding the index on the
    /// way. A torn last write is truncated (and the truncation synced) and
    /// reported in [`Opened::torn_tail`]; mid-file damage fails with
    /// [`JournalError::Corrupt`] and leaves the storage untouched. On
    /// success everything in the storage is synced once, so
    /// [`durable_seq`](FileJournal::durable_seq) starts at the last frame.
    ///
    /// O(file) time; O(1) memory (it verifies 1 MiB at a time), except
    /// that damage is classified with everything after it in memory.
    pub fn open(
        storage: S,
        opts: JournalOptions,
        clock: C,
    ) -> Result<(Self, Opened), JournalError> {
        todo!("w18: open")
    }

    /// Appends `cmd` as the next frame and returns its sequence number,
    /// then syncs if the policy says so (or writes the buffer out if it is
    /// full).
    ///
    /// Cost: the in-memory append (~50 ns), a clock read under
    /// [`SyncPolicy::Interval`], and when due a `write` and an fsync.
    /// Allocation-free once the buffer has grown.
    #[inline]
    pub fn append(&mut self, cmd: &Command) -> Result<Seq, JournalError> {
        todo!("w18: append")
    }

    /// For [`SyncPolicy::Interval`]: syncs if there are unsynced appends
    /// and the interval has passed. Call it from a timer so a burst that
    /// ends is not left unsynced until the next append. Returns whether it
    /// synced. Other policies: does nothing.
    pub fn poll(&mut self) -> Result<bool, JournalError> {
        todo!("w18: poll")
    }

    /// Writes the buffered frames to the storage, without syncing: they
    /// survive a process crash, not a power loss. O(buffered bytes).
    pub fn flush(&mut self) -> Result<(), JournalError> {
        todo!("w18: flush")
    }

    /// Flushes, then fsyncs if anything is unsynced: afterwards
    /// [`durable_seq`](FileJournal::durable_seq) is the last frame.
    pub fn sync(&mut self) -> Result<(), JournalError> {
        todo!("w18: sync")
    }

    /// Frames with sequence number `>= from`, read back into `buf` (which
    /// is overwritten) — after flushing the write buffer.
    ///
    /// Starts at the indexed frame at or before `from`, so it reads (and
    /// verifies) at most `index_stride - 1` frames that are not wanted, not
    /// the whole prefix. O(bytes from there to the end).
    pub fn iter_from<'b>(
        &mut self,
        from: Seq,
        buf: &'b mut Vec<u8>,
    ) -> Result<Frames<'b>, JournalError> {
        todo!("w18: iter_from")
    }

    /// Sequence number of the first frame (`FIRST_SEQ`).
    pub fn first_seq(&self) -> Seq {
        todo!("w18: first_seq")
    }

    /// Sequence number the next append gets.
    pub fn next_seq(&self) -> Seq {
        todo!("w18: next_seq")
    }

    /// Sequence number of the last frame, if any.
    pub fn last_seq(&self) -> Option<Seq> {
        todo!("w18: last_seq")
    }

    /// Every frame up to this one is synced (`Seq(0)`: none yet).
    pub fn durable_seq(&self) -> Seq {
        todo!("w18: durable_seq")
    }

    /// Number of frames. O(1).
    pub fn len(&self) -> u64 {
        todo!("w18: len")
    }

    /// True if no frame was ever appended.
    pub fn is_empty(&self) -> bool {
        todo!("w18: is_empty")
    }

    /// Size of all frames, written or buffered, in bytes.
    pub fn byte_len(&self) -> u64 {
        todo!("w18: byte_len")
    }

    /// Number of fsyncs this journal issued since it was opened (the sync
    /// in `open` not included).
    pub fn syncs(&self) -> u64 {
        todo!("w18: syncs")
    }

    /// The sparse index of frame offsets.
    pub fn index(&self) -> &SeqIndex {
        todo!("w18: index")
    }

    /// The options it was opened with.
    pub fn options(&self) -> &JournalOptions {
        todo!("w18: options")
    }

    /// The underlying storage (e.g. to inspect a [`MemStorage`](crate::MemStorage)).
    pub fn storage(&self) -> &S {
        todo!("w18: storage")
    }
}
