//! Week 3: structured concurrency with threads.
//!
//! A [`TaskSet`] owns every thread it spawns. Each task gets its own child
//! [`CancelToken`] of the set's token; [`TaskSet::shutdown`] cancels them
//! all, waits for them up to a deadline, joins the ones that finished, and
//! reports the ones that did not. Dropping a set without calling
//! `shutdown` cancels and joins everything (the set never outlives its
//! tasks — no leaked threads).
//!
//! Threads cannot be killed, so a task that ignores cancellation past the
//! deadline is *reported* in [`ShutdownReport::missed`] and its handle kept
//! in the report: call [`ShutdownReport::join_stragglers`] to wait for it,
//! or escalate (log, abort the process) — the policy is the caller's.
//!
//! ```
//! use std::time::Duration;
//! use sync::TaskSet;
//!
//! let mut set = TaskSet::new();
//! set.spawn("worker", |token| {
//!     while !token.is_cancelled() {
//!         std::thread::sleep(Duration::from_millis(1));
//!     }
//! })
//! .unwrap();
//! let report = set.shutdown(Duration::from_secs(1));
//! assert!(report.is_clean());
//! ```

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::cancel::CancelToken;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// TODO(w03): implement — see the week guide on tepy.dev/hft.

/// A set of named threads that share a cancellation subtree and are
/// shut down together.
pub struct TaskSet {
    // TODO(w03): design this type's fields.
    _todo: (),
}

/// What happened during [`TaskSet::shutdown`].
pub struct ShutdownReport {
    /// From the cancel to the end of the last in-deadline join.
    pub elapsed: Duration,
    /// Tasks that exited by the deadline and were joined (including ones
    /// that panicked).
    pub joined: usize,
    /// Names of tasks that panicked (during shutdown or earlier, as seen by
    /// [`TaskSet::reap`]).
    pub panicked: Vec<String>,
    /// Names of tasks still running at the deadline.
    pub missed: Vec<String>,
    stragglers: Vec<(String, JoinHandle<()>)>,
}

#[allow(unused_variables)]
impl TaskSet {
    /// An empty set with its own root token.
    pub fn new() -> TaskSet {
        todo!("w03: new")
    }

    /// An empty set whose token is a child of `parent`: cancelling the
    /// parent shuts the set's tasks down too (e.g. a supervisor's token).
    pub fn with_parent(parent: &CancelToken) -> TaskSet {
        todo!("w03: with_parent")
    }

    fn with_token(token: CancelToken) -> TaskSet {
        todo!("w03: with_token")
    }

    /// The set's token. Every task's token is a child of it, so cancelling
    /// it cancels all tasks (without joining them).
    pub fn token(&self) -> &CancelToken {
        todo!("w03: token")
    }

    /// Spawns a named OS thread running `f(token)`, where `token` is a new
    /// child of the set's token. Costs a thread spawn (~10–50 µs) and two
    /// small allocations. Fails only if the OS refuses to create a thread.
    pub fn spawn<F>(&mut self, name: impl Into<String>, f: F) -> std::io::Result<()>
    where
        F: FnOnce(CancelToken) + Send + 'static,
    {
        todo!("w03: spawn")
    }

    /// Tasks spawned and not yet joined.
    pub fn len(&self) -> usize {
        todo!("w03: len")
    }

    /// True if no task is waiting to be joined.
    pub fn is_empty(&self) -> bool {
        todo!("w03: is_empty")
    }

    /// Tasks that have not finished yet.
    pub fn running(&self) -> usize {
        todo!("w03: running")
    }

    /// Joins every task that has already finished (without blocking on
    /// running ones) and returns how many were joined. Long-lived sets —
    /// one task per gateway session — call this periodically so finished
    /// threads don't accumulate. O(len).
    pub fn reap(&mut self) -> usize {
        todo!("w03: reap")
    }

    /// Cancels every task, waits up to `timeout` for them to exit, and
    /// joins those that did. Tasks still running at the deadline are
    /// reported in [`ShutdownReport::missed`]; their handles stay in the
    /// report. Returns as soon as all tasks exit, so a cooperative set
    /// shuts down in roughly the time its slowest task takes to notice.
    pub fn shutdown(mut self, timeout: Duration) -> ShutdownReport {
        todo!("w03: shutdown")
    }
}

impl Default for TaskSet {
    fn default() -> Self {
        TaskSet::new()
    }
}

impl fmt::Debug for TaskSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskSet")
            .field("len", &self.len())
            .field("running", &self.running())
            .finish()
    }
}

impl ShutdownReport {
    /// True if every task exited by the deadline and none panicked.
    pub fn is_clean(&self) -> bool {
        self.missed.is_empty() && self.panicked.is_empty()
    }

    /// Tasks that missed the deadline and have not been joined yet.
    pub fn stragglers(&self) -> usize {
        self.stragglers.len()
    }

    /// Blocks until every task that missed the deadline exits, joins it,
    /// and returns how many were joined. Panicked stragglers are added to
    /// [`panicked`](Self::panicked). (Dropping the report instead detaches
    /// them.)
    pub fn join_stragglers(&mut self) -> usize {
        let n = self.stragglers.len();
        for (name, handle) in self.stragglers.drain(..) {
            if handle.join().is_err() {
                self.panicked.push(name);
            }
        }
        n
    }
}

impl fmt::Debug for ShutdownReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShutdownReport")
            .field("elapsed", &self.elapsed)
            .field("joined", &self.joined)
            .field("panicked", &self.panicked)
            .field("missed", &self.missed)
            .field("stragglers", &self.stragglers.len())
            .finish()
    }
}
