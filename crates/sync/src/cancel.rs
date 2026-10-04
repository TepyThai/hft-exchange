//! Week 3: hierarchical cancellation.
//!
//! A [`CancelToken`] is a node in a cancellation tree:
//!
//! * [`CancelToken::child`] creates a child node;
//! * [`CancelToken::cancel`] cancels a node **and its whole subtree** — never
//!   its parent or siblings;
//! * [`CancelToken::is_cancelled`] is a single atomic load, cheap enough to
//!   check on every iteration of a hot loop;
//! * [`CancelToken::wait_timeout`] blocks until cancelled or timed out.
//!
//! Cloning a token shares the **same** node (handing it to another thread),
//! it does not create a child.
//!
//! # Design: push cancellation down eagerly
//!
//! Each node keeps weak references to its children, and a strong reference
//! to its parent — so an intermediate token can be dropped without cutting
//! its descendants off from cancellation, while a dropped leaf is freed at
//! once. `cancel` walks the subtree once, setting every node's flag and
//! waking its waiters, so a check never has to look at ancestors. The alternative — each token
//! checking every ancestor's flag — makes `is_cancelled` O(depth) on the
//! hot path to save work on the (rare) cancel path; we pay on the rare
//! path instead.
//!
//! A child created from an already-cancelled parent is born cancelled (the
//! check and the registration happen under the parent's lock, so a
//! concurrent `cancel` cannot slip in between).
//!
//! ```
//! use sync::CancelToken;
//! let root = CancelToken::new();
//! let child = root.child();
//! child.cancel();
//! assert!(!root.is_cancelled()); // cancellation does not propagate up
//! root.cancel();
//! assert!(root.child().is_cancelled()); // ...but always down
//! ```

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

// @student-begin w03
struct Node {
    cancelled: AtomicBool,
    /// Children and the waiters' condvar share this lock: a waiter checks
    /// the flag under it, and `cancel` notifies under it, so no wakeup is
    /// lost.
    inner: Mutex<Children>,
    cond: Condvar,
    /// Keeps the path from the root alive while this node lives.
    parent: Option<Arc<Node>>,
}

struct Children {
    /// Weak, so dropping a child token frees its node; dead entries are
    /// pruned when the list doubles (amortized O(1) per `child`).
    list: Vec<Weak<Node>>,
    prune_at: usize,
    /// Threads blocked in `wait_timeout` on this node. `cancel` only pays
    /// for `notify_all` (a futex syscall, ~0.3 µs) when this is non-zero —
    /// without it, cancelling 1,000 idle children costs 1,000 syscalls.
    waiters: usize,
}

impl Node {
    fn new(cancelled: bool, parent: Option<Arc<Node>>) -> Arc<Node> {
        Arc::new(Node {
            cancelled: AtomicBool::new(cancelled),
            inner: Mutex::new(Children {
                list: Vec::new(),
                prune_at: 8,
                waiters: 0,
            }),
            cond: Condvar::new(),
            parent,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Children> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Node {
    /// Releases the chain of parents iteratively: dropping the last token
    /// of a 100k-deep chain must not recurse 100k times.
    fn drop(&mut self) {
        let mut next = self.parent.take();
        while let Some(parent) = next {
            next = match Arc::into_inner(parent) {
                // We held the last reference: unlink its parent before it
                // is dropped (with `parent == None`, so no recursion).
                Some(mut node) => node.parent.take(),
                None => None,
            };
        }
    }
}
// @student-end

/// A handle to one node of a cancellation tree. `Clone` shares the node;
/// [`child`](CancelToken::child) makes a new node below it.
#[derive(Clone)]
// @student w03
pub struct CancelToken {
    node: Arc<Node>,
}

// @student w03
impl CancelToken {
    /// A new, uncancelled root token. Allocates one node.
    pub fn new() -> CancelToken {
        CancelToken {
            node: Node::new(false, None),
        }
    }

    /// A new token below this one: cancelled whenever this one (or any
    /// ancestor) is cancelled, but cancelling it leaves this one alone.
    /// Born cancelled if this token already is. O(1) amortized: one lock
    /// and one allocation. The child keeps this node alive (dropping the
    /// parent handle never detaches its descendants).
    pub fn child(&self) -> CancelToken {
        let mut children = self.node.lock();
        // Checked under the lock: `cancel` sets the flag before taking the
        // lock, so either we see the flag, or `cancel` sees our child.
        if self.node.cancelled.load(Ordering::Acquire) {
            return CancelToken {
                node: Node::new(true, None),
            };
        }
        if children.list.len() >= children.prune_at {
            children.list.retain(|w| w.strong_count() > 0);
            children.prune_at = (children.list.len() * 2).max(8);
        }
        let node = Node::new(false, Some(Arc::clone(&self.node)));
        children.list.push(Arc::downgrade(&node));
        CancelToken { node }
    }

    /// Cancels this token and every descendant, waking all their waiters.
    /// Idempotent. O(subtree size); iterative, so deep trees cannot
    /// overflow the stack. When it returns, every descendant reports
    /// cancelled (if another thread is cancelling an overlapping subtree
    /// at the same moment, that thread finishes its part).
    pub fn cancel(&self) {
        let mut stack = vec![Arc::clone(&self.node)];
        while let Some(node) = stack.pop() {
            if node.cancelled.swap(true, Ordering::AcqRel) {
                // Already cancelled: whoever did it propagates below it.
                continue;
            }
            let mut children = node.lock();
            if children.waiters > 0 {
                node.cond.notify_all();
            }
            // A cancelled node never gains children again (`child` checks
            // the flag), so the list can be emptied.
            stack.extend(children.list.drain(..).filter_map(|w| w.upgrade()));
        }
    }

    /// True if this token or any ancestor has been cancelled. One
    /// `Acquire` load: anything the canceller did before `cancel` is
    /// visible once this returns true.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.node.cancelled.load(Ordering::Acquire)
    }

    /// Blocks until cancelled or until `timeout` elapses; returns whether
    /// the token is cancelled. Returns immediately if it already is.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_cancelled() {
            return true;
        }
        let deadline = Instant::now().checked_add(timeout);
        let mut children = self.node.lock();
        loop {
            if self.is_cancelled() {
                return true;
            }
            let remaining = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => Duration::from_secs(3600),
            };
            if remaining.is_zero() {
                return false;
            }
            children.waiters += 1;
            children = self
                .node
                .cond
                .wait_timeout(children, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            children.waiters -= 1;
        }
    }

    /// Blocks until cancelled.
    pub fn wait(&self) {
        while !self.wait_timeout(Duration::from_secs(3600)) {}
    }

    /// True if `self` and `other` are handles to the same node.
    pub fn same_node(&self, other: &CancelToken) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        CancelToken::new()
    }
}

impl fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
