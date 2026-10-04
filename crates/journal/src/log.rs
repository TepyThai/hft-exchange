//! The sequencer's view of a journal: [`CommandLog`].
//!
//! The engine pipeline (week 12) appends every command to "a journal"
//! before matching it. Which journal — the in-memory [`Journal`], the
//! durable [`FileJournal`] (week 18) or a replicating
//! [`Primary`](crate::replication::Primary) (week 20) — is the deployment's
//! choice, so the pipeline is generic over this trait.
//!
//! Provided glue (not part of a week's exercise).

use crate::clock::Clock;
use crate::file::{FileJournal, JournalError};
use crate::memory::Journal;
use crate::storage::Storage;
use std::convert::Infallible;
use std::fmt;
use types::{Command, Seq};

/// An append-only command log that assigns sequence numbers.
pub trait CommandLog {
    /// Why an append can fail ([`Infallible`] for memory).
    type Error: fmt::Debug + fmt::Display;

    /// Records `cmd` and returns the sequence number it was assigned: the
    /// previous one plus one, starting from [`next_seq`](CommandLog::next_seq).
    /// On error nothing was assigned.
    fn append(&mut self, cmd: &Command) -> Result<Seq, Self::Error>;

    /// The sequence number the next append will assign.
    fn next_seq(&self) -> Seq;
}

impl CommandLog for Journal {
    type Error = Infallible;

    #[inline]
    fn append(&mut self, cmd: &Command) -> Result<Seq, Infallible> {
        Ok(Journal::append(self, cmd))
    }

    #[inline]
    fn next_seq(&self) -> Seq {
        Journal::next_seq(self)
    }
}

impl<S: Storage, C: Clock> CommandLog for FileJournal<S, C> {
    type Error = JournalError;

    #[inline]
    fn append(&mut self, cmd: &Command) -> Result<Seq, JournalError> {
        FileJournal::append(self, cmd)
    }

    #[inline]
    fn next_seq(&self) -> Seq {
        FileJournal::next_seq(self)
    }
}

impl<L: CommandLog + ?Sized> CommandLog for &mut L {
    type Error = L::Error;

    #[inline]
    fn append(&mut self, cmd: &Command) -> Result<Seq, L::Error> {
        (**self).append(cmd)
    }

    #[inline]
    fn next_seq(&self) -> Seq {
        (**self).next_seq()
    }
}
