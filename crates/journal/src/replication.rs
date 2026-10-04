//! Hot standby (week 20): replicate the sequenced journal to a standby that
//! applies it to its own state machine, and fail over to it safely.
//!
//! ```text
//!   Primary                                   Standby
//!   append(cmd) → journal ─ Frame(seq) ────▶ append_frame (CRC, seq == next)
//!                          ◀── Ack(seq, t) ─  apply to its machine; grant lease at t
//!   lease_until = t + lease                   promise: no promotion before t + lease
//!               ◀─ Resend(from) ─ on a gap or a damaged frame
//! ```
//!
//! # What the standby checks
//!
//! Frames are self-validating (week 9): the CRC catches damage and the
//! sequence number catches loss and duplication. A frame for `next_seq` is
//! appended and applied; an older one is a duplicate (ignored, re-acked); a
//! newer one or a damaged one means frames are missing, so the standby asks
//! for a resend from `next_seq` — once, not for every early frame — and
//! drops frames until it arrives. If the request itself is lost, it asks
//! again once a quarter of the lease has passed without the resend
//! arriving, so a steady stream of new frames cannot stall replication.
//! Heartbeats carry the primary's `next_seq`, which exposes a lost *last*
//! frame.
//!
//! # Leases: one leader at a time
//!
//! Each acknowledgement carries the standby's clock reading `t` and is a
//! promise: "I will not take over before `t + lease`". The primary may
//! accept commands only while it holds such a promise
//! ([`Primary::is_leader`]); the standby may promote itself only once its
//! newest promise has expired ([`Standby::can_promote`]). Because both
//! sides compare against the same promise, their leadership intervals never
//! overlap — no split brain — even if the primary is merely slow or cut
//! off, not dead. (Across machines, clocks drift: the primary would
//! subtract a safety margin. In-process, both read one [`Clock`].)
//!
//! A primary starts *without* a lease: the standby grants the first one
//! when it is created, so whichever side starts first, nothing overlaps.
//!
//! # What "no acknowledged command lost" takes
//!
//! A client may be told its command is done only once the standby has
//! acknowledged it ([`Primary::wait_acked`]): then every acknowledged
//! command is in the standby's journal when the primary dies. Commands the
//! primary sent but that were not acknowledged may or may not have made it;
//! their clients never heard back and must ask.
//!
//! Time comes from an injected [`Clock`], so tests drive lease expiry with
//! a [`ManualClock`](crate::ManualClock), deterministically. Blocking waits
//! ([`Primary::wait_acked`], [`Standby::recv`]) use real time: they only
//! decide how long to wait, never who leads.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::clock::{Clock, SystemClock};
use crate::codec::MAX_COMMAND_LEN;
use crate::frame::{FrameError, FRAME_HEADER_LEN};
use crate::log::CommandLog;
use crate::memory::Journal;
use crate::replay::StateMachine;
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use types::{Command, Seq};

// TODO(w20): implement — see the week guide on tepy.dev/hft.

/// Largest frame: a header plus the largest command.
pub const MAX_FRAME_LEN: usize = FRAME_HEADER_LEN + MAX_COMMAND_LEN;

/// One frame held by value — what crosses the replication link. `Copy`,
/// 51 bytes, no allocation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FrameBuf {
    len: u8,
    bytes: [u8; MAX_FRAME_LEN],
}

impl FrameBuf {
    /// A copy of `frame`, or `None` if it is longer than [`MAX_FRAME_LEN`].
    pub fn copy_from(frame: &[u8]) -> Option<FrameBuf> {
        if frame.len() > MAX_FRAME_LEN {
            return None;
        }
        let mut bytes = [0u8; MAX_FRAME_LEN];
        bytes[..frame.len()].copy_from_slice(frame);
        Some(FrameBuf {
            len: frame.len() as u8,
            bytes,
        })
    }

    /// The frame's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// The frame's bytes, writable (tests damage frames in transit).
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[..self.len as usize]
    }

    /// The sequence number in the header — unverified, for logging.
    pub fn seq(&self) -> Option<Seq> {
        let b = self.as_bytes().get(4..12)?;
        Some(Seq(u64::from_le_bytes(b.try_into().ok()?)))
    }
}

impl fmt::Debug for FrameBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameBuf")
            .field("len", &self.len)
            .field("seq", &self.seq())
            .finish()
    }
}

/// Primary → standby.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToStandby {
    /// One journal frame.
    Frame(FrameBuf),
    /// "I am alive; my journal's next sequence number is `next_seq`."
    Heartbeat { next_seq: Seq },
}

/// Standby → primary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToPrimary {
    /// Every frame up to `applied` is in the standby's journal and applied,
    /// and the standby will not promote itself before `granted_at + lease`
    /// (its clock).
    Ack { applied: Seq, granted_at: u64 },
    /// Send every frame from `from` on again.
    Resend { from: Seq },
}

/// The primary's end of an in-process replication link.
#[derive(Debug)]
pub struct PrimaryLink {
    /// Frames and heartbeats to the standby.
    pub tx: Sender<ToStandby>,
    /// Acknowledgements and resend requests from the standby.
    pub rx: Receiver<ToPrimary>,
}

/// The standby's end of an in-process replication link.
#[derive(Debug)]
pub struct StandbyLink {
    /// Acknowledgements and resend requests to the primary.
    pub tx: Sender<ToPrimary>,
    /// Frames and heartbeats from the primary.
    pub rx: Receiver<ToStandby>,
}

/// A connected pair of link ends over two `std::sync::mpsc` channels (the
/// in-process transport; a network transport carries the same messages).
pub fn link() -> (PrimaryLink, StandbyLink) {
    let (to_standby, from_primary) = mpsc::channel();
    let (to_primary, from_standby) = mpsc::channel();
    (
        PrimaryLink {
            tx: to_standby,
            rx: from_standby,
        },
        StandbyLink {
            tx: to_primary,
            rx: from_primary,
        },
    )
}

/// Why a replication operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicationError {
    /// The primary holds no unexpired lease: it must not accept commands
    /// (the standby may already have taken over).
    NotLeader { now: u64, lease_until: u64 },
    /// The other end hung up.
    Disconnected,
    /// The standby had not acknowledged `wanted` in time.
    Timeout { acked: Seq, wanted: Seq },
    /// The standby asked for frames the primary no longer holds (its
    /// journal was cleared past them): it needs a snapshot instead.
    ResendUnavailable { from: Seq, first: Seq },
}

impl fmt::Display for ReplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplicationError::NotLeader { now, lease_until } => {
                write!(
                    f,
                    "not the leader: lease ended at {lease_until} (now {now})"
                )
            }
            ReplicationError::Disconnected => write!(f, "replication peer disconnected"),
            ReplicationError::Timeout { acked, wanted } => write!(
                f,
                "standby acknowledged up to seq {}, waited for {}",
                acked.0, wanted.0
            ),
            ReplicationError::ResendUnavailable { from, first } => write!(
                f,
                "resend from seq {} requested but the journal starts at {}",
                from.0, first.0
            ),
        }
    }
}

impl std::error::Error for ReplicationError {}

/// The primary: a sequencer whose journal streams every frame to a standby,
/// and who leads only while the standby's lease says so.
///
/// Implements [`CommandLog`], so the engine pipeline can sequence through
/// it unchanged. Single-threaded (`&mut self`); acknowledgements are
/// processed whenever it is called ([`poll`](Primary::poll) runs inside
/// [`append`](Primary::append)).
#[derive(Debug)]
pub struct Primary<C = SystemClock> {
    // TODO(w20): design this type's fields.
    _todo: std::marker::PhantomData<(C,)>,
}

// TODO(w20): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<C: Clock> Primary<C> {
    /// A primary over `journal`, which the standby must also hold (both
    /// empty, typically). It has no lease until the standby grants one.
    pub fn new(journal: Journal, link: PrimaryLink, clock: C, lease: Duration) -> Primary<C> {
        todo!("w20: new")
    }

    /// Sequences `cmd`: refuses with [`ReplicationError::NotLeader`] unless
    /// the lease is current (after processing pending acknowledgements),
    /// otherwise appends it to the local journal and sends the frame to the
    /// standby. Does not wait for the acknowledgement — see
    /// [`wait_acked`](Primary::wait_acked).
    ///
    /// A dead link does not fail the append (the lease will run out
    /// instead). Cost: a journal append, one clock read, one channel send.
    pub fn append(&mut self, cmd: &Command) -> Result<Seq, ReplicationError> {
        todo!("w20: append")
    }

    /// Processes every acknowledgement and resend request that has
    /// arrived, without blocking. Returns how many.
    pub fn poll(&mut self) -> Result<usize, ReplicationError> {
        todo!("w20: poll")
    }

    /// Blocks until the standby has acknowledged `seq` (then the command
    /// survives losing the primary), or `timeout` of real time passes.
    pub fn wait_acked(&mut self, seq: Seq, timeout: Duration) -> Result<(), ReplicationError> {
        todo!("w20: wait_acked")
    }

    /// Sends a heartbeat (renewing the lease once acknowledged, and
    /// exposing a lost last frame). Call it periodically, well within the
    /// lease — when idle, and also under load, since it is the cheapest way
    /// to expose missing frames quickly.
    pub fn heartbeat(&mut self) -> Result<(), ReplicationError> {
        todo!("w20: heartbeat")
    }

    /// True while the lease is current: the clock reads before
    /// [`lease_until`](Primary::lease_until). Does not process pending
    /// acknowledgements (call [`poll`](Primary::poll)).
    pub fn is_leader(&self) -> bool {
        todo!("w20: is_leader")
    }

    /// When the current lease ends (clock reading; 0 = never granted).
    pub fn lease_until(&self) -> u64 {
        todo!("w20: lease_until")
    }

    /// Highest sequence number the standby has acknowledged.
    pub fn acked_seq(&self) -> Seq {
        todo!("w20: acked_seq")
    }

    /// False once a send or receive found the standby gone.
    pub fn is_connected(&self) -> bool {
        todo!("w20: is_connected")
    }

    /// Frames sent again on request.
    pub fn resent(&self) -> u64 {
        todo!("w20: resent")
    }

    /// The local journal.
    pub fn journal(&self) -> &Journal {
        todo!("w20: journal")
    }

    /// Gives up the local journal.
    pub fn into_journal(self) -> Journal {
        todo!("w20: into_journal")
    }
}

impl<C: Clock> CommandLog for Primary<C> {
    type Error = ReplicationError;

    #[inline]
    fn append(&mut self, cmd: &Command) -> Result<Seq, ReplicationError> {
        Primary::append(self, cmd)
    }

    #[inline]
    fn next_seq(&self) -> Seq {
        self.journal().next_seq()
    }
}

/// What [`Standby::handle`] did with one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// The frame was appended and applied.
    Applied(Seq),
    /// A frame the standby already had; ignored.
    Duplicate(Seq),
    /// A damaged or early frame; dropped, and a resend requested.
    Rejected(FrameError),
    /// A heartbeat (a resend was requested if it revealed missing frames).
    Heartbeat,
}

/// Counters of what a standby has seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StandbyStats {
    /// Frames appended and applied.
    pub applied: u64,
    /// Frames already held (retransmissions), ignored.
    pub duplicates: u64,
    /// Frames that arrived early (frames before them missing).
    pub gaps: u64,
    /// Frames that failed verification.
    pub corrupt: u64,
    /// Resend requests sent to the primary.
    pub resend_requests: u64,
    /// Heartbeats received.
    pub heartbeats: u64,
}

/// A hot standby: validates the primary's frames, keeps its own journal and
/// applies every command to its own state machine `M`, acknowledging (and
/// granting the primary's lease) as it goes. Promotes itself once the
/// primary's lease has expired.
#[derive(Debug)]
pub struct Standby<M, C = SystemClock> {
    // TODO(w20): design this type's fields.
    _todo: std::marker::PhantomData<(M, C,)>,
}

// TODO(w20): implement — see the week guide on tepy.dev/hft.

#[allow(unused_variables)]
impl<M: StateMachine, C: Clock> Standby<M, C> {
    /// A standby over `journal` and `machine` — the machine must already
    /// reflect the journal (both empty, or replayed from it). Grants the
    /// primary its first lease, now.
    pub fn new(
        journal: Journal,
        machine: M,
        link: StandbyLink,
        clock: C,
        lease: Duration,
    ) -> Standby<M, C> {
        todo!("w20: new")
    }

    /// Validates and applies one message from the primary (see the
    /// [module docs](self)), then acknowledges. O(frame) plus one
    /// `apply`.
    pub fn handle(&mut self, msg: ToStandby) -> Handled {
        todo!("w20: handle")
    }

    /// Handles one message if one is waiting, without blocking.
    /// `Err(Disconnected)` once the primary is gone *and* every message it
    /// sent has been handled.
    pub fn poll_one(&mut self) -> Result<Option<Handled>, ReplicationError> {
        todo!("w20: poll_one")
    }

    /// Handles every waiting message, without blocking; returns how many.
    /// `Err(Disconnected)` as for [`poll_one`](Standby::poll_one), only
    /// when there was nothing to handle.
    pub fn poll(&mut self) -> Result<usize, ReplicationError> {
        todo!("w20: poll")
    }

    /// Waits up to `timeout` (real time) for one message and handles it.
    /// `Ok(None)` on timeout.
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<Handled>, ReplicationError> {
        todo!("w20: recv")
    }

    /// True once the newest lease granted has expired: the primary can no
    /// longer accept commands, so taking over cannot create two leaders.
    pub fn can_promote(&self) -> bool {
        todo!("w20: can_promote")
    }

    /// When the primary's newest lease ends (clock reading).
    pub fn lease_expires_at(&self) -> u64 {
        todo!("w20: lease_expires_at")
    }

    /// Becomes the primary: returns the journal and the machine — every
    /// command the old primary got acknowledged, applied — to carry on
    /// sequencing from [`applied_seq`](Standby::applied_seq) + 1. Refused
    /// (the standby is handed back) while the lease is current.
    pub fn promote(self) -> Result<(Journal, M), Box<Standby<M, C>>> {
        todo!("w20: promote")
    }

    /// Sequence number of the last frame applied (`Seq(0)`: none).
    pub fn applied_seq(&self) -> Seq {
        todo!("w20: applied_seq")
    }

    /// The standby's journal.
    pub fn journal(&self) -> &Journal {
        todo!("w20: journal")
    }

    /// The standby's state machine.
    pub fn machine(&self) -> &M {
        todo!("w20: machine")
    }

    /// What it has seen so far.
    pub fn stats(&self) -> StandbyStats {
        todo!("w20: stats")
    }
}
