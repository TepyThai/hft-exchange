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

use crate::cancel::CancelToken;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// @student-begin w03
struct Task {
    name: String,
    handle: JoinHandle<()>,
    /// Set (Release) by the task's exit guard, even when it panics.
    finished: Arc<AtomicBool>,
}

/// Counts finished tasks; `shutdown` waits on the condvar for the count to
/// reach the number spawned.
struct Completion {
    finished: Mutex<usize>,
    cond: Condvar,
}

/// Lives on the task's stack; its `Drop` runs when the task returns *or*
/// unwinds, so a panicking task still counts as finished.
struct ExitGuard {
    flag: Arc<AtomicBool>,
    completion: Arc<Completion>,
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Release);
        *self
            .completion
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += 1;
        self.completion.cond.notify_all();
    }
}

impl Drop for TaskSet {
    fn drop(&mut self) {
        self.token.cancel();
        for task in self.tasks.drain(..) {
            let _ = task.handle.join();
        }
    }
}
// @student-end

/// A set of named threads that share a cancellation subtree and are
/// shut down together.
// @student w03
pub struct TaskSet {
    token: CancelToken,
    tasks: Vec<Task>,
    completion: Arc<Completion>,
    /// Tasks spawned over the set's lifetime (including reaped ones).
    spawned: usize,
    /// Names of tasks found panicked by `reap`, reported at shutdown.
    panicked: Vec<String>,
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

// @student w03
impl TaskSet {
    /// An empty set with its own root token.
    pub fn new() -> TaskSet {
        TaskSet::with_token(CancelToken::new())
    }

    /// An empty set whose token is a child of `parent`: cancelling the
    /// parent shuts the set's tasks down too (e.g. a supervisor's token).
    pub fn with_parent(parent: &CancelToken) -> TaskSet {
        TaskSet::with_token(parent.child())
    }

    fn with_token(token: CancelToken) -> TaskSet {
        TaskSet {
            token,
            tasks: Vec::new(),
            completion: Arc::new(Completion {
                finished: Mutex::new(0),
                cond: Condvar::new(),
            }),
            spawned: 0,
            panicked: Vec::new(),
        }
    }

    /// The set's token. Every task's token is a child of it, so cancelling
    /// it cancels all tasks (without joining them).
    pub fn token(&self) -> &CancelToken {
        &self.token
    }

    /// Spawns a named OS thread running `f(token)`, where `token` is a new
    /// child of the set's token. Costs a thread spawn (~10–50 µs) and two
    /// small allocations. Fails only if the OS refuses to create a thread.
    pub fn spawn<F>(&mut self, name: impl Into<String>, f: F) -> std::io::Result<()>
    where
        F: FnOnce(CancelToken) + Send + 'static,
    {
        let name = name.into();
        let token = self.token.child();
        let finished = Arc::new(AtomicBool::new(false));
        let guard = ExitGuard {
            flag: Arc::clone(&finished),
            completion: Arc::clone(&self.completion),
        };
        // Counted before spawning: if the OS refuses the thread, the closure
        // (and with it the guard) is dropped, marking the task finished.
        self.spawned += 1;
        let handle = std::thread::Builder::new()
            .name(name.clone())
            .spawn(move || {
                let _guard = guard;
                f(token);
            })?;
        self.tasks.push(Task {
            name,
            handle,
            finished,
        });
        Ok(())
    }

    /// Tasks spawned and not yet joined.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// True if no task is waiting to be joined.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Tasks that have not finished yet.
    pub fn running(&self) -> usize {
        self.spawned
            - *self
                .completion
                .finished
                .lock()
                .unwrap_or_else(|e| e.into_inner())
    }

    /// Joins every task that has already finished (without blocking on
    /// running ones) and returns how many were joined. Long-lived sets —
    /// one task per gateway session — call this periodically so finished
    /// threads don't accumulate. O(len).
    pub fn reap(&mut self) -> usize {
        let mut joined = 0;
        let mut i = 0;
        while i < self.tasks.len() {
            if self.tasks[i].finished.load(Ordering::Acquire) {
                let task = self.tasks.swap_remove(i);
                if task.handle.join().is_err() {
                    self.panicked.push(task.name);
                }
                joined += 1;
            } else {
                i += 1;
            }
        }
        joined
    }

    /// Cancels every task, waits up to `timeout` for them to exit, and
    /// joins those that did. Tasks still running at the deadline are
    /// reported in [`ShutdownReport::missed`]; their handles stay in the
    /// report. Returns as soon as all tasks exit, so a cooperative set
    /// shuts down in roughly the time its slowest task takes to notice.
    pub fn shutdown(mut self, timeout: Duration) -> ShutdownReport {
        let start = Instant::now();
        let deadline = start.checked_add(timeout);
        self.token.cancel();
        {
            let mut finished = self
                .completion
                .finished
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            while *finished < self.spawned {
                let remaining = match deadline {
                    Some(d) => d.saturating_duration_since(Instant::now()),
                    None => Duration::from_secs(3600),
                };
                if remaining.is_zero() {
                    break;
                }
                finished = self
                    .completion
                    .cond
                    .wait_timeout(finished, remaining)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        let mut report = ShutdownReport {
            elapsed: Duration::ZERO,
            joined: 0,
            panicked: std::mem::take(&mut self.panicked),
            missed: Vec::new(),
            stragglers: Vec::new(),
        };
        for task in std::mem::take(&mut self.tasks) {
            if task.finished.load(Ordering::Acquire) {
                // The exit guard has run: the thread is already returning,
                // so this join blocks only for thread teardown.
                if task.handle.join().is_err() {
                    report.panicked.push(task.name);
                }
                report.joined += 1;
            } else {
                report.missed.push(task.name.clone());
                report.stragglers.push((task.name, task.handle));
            }
        }
        report.elapsed = start.elapsed();
        report
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
