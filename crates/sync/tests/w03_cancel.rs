//! Week 3: hierarchical cancellation and deadline-bounded task sets.

mod common;

use common::within;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sync::{CancelToken, TaskSet};

#[test]
fn w03_cancel_propagates_down_not_up() {
    let root = CancelToken::new();
    let a = root.child();
    let b = root.child();
    let a1 = a.child();
    let a2 = a.child();
    let a1x = a1.child();
    assert!(![&root, &a, &b, &a1, &a2, &a1x]
        .iter()
        .any(|t| t.is_cancelled()));

    a.cancel();
    assert!(a.is_cancelled() && a1.is_cancelled() && a2.is_cancelled() && a1x.is_cancelled());
    assert!(!root.is_cancelled(), "cancellation must not propagate up");
    assert!(!b.is_cancelled(), "cancellation must not reach siblings");

    root.cancel();
    assert!(root.is_cancelled() && b.is_cancelled());
}

#[test]
fn w03_clone_shares_node_and_late_children_are_born_cancelled() {
    let root = CancelToken::new();
    let handle = root.clone();
    assert!(handle.same_node(&root));
    let child = root.child();
    assert!(!child.same_node(&root), "child() creates a new node");

    handle.cancel(); // cancelling through a clone cancels the shared node
    assert!(root.is_cancelled() && child.is_cancelled());
    root.cancel(); // idempotent
    assert!(root.is_cancelled());

    let late = root.child();
    assert!(
        late.is_cancelled(),
        "a child of a cancelled token is born cancelled"
    );
    assert!(late.child().is_cancelled());
    assert!(late.wait_timeout(Duration::ZERO));
}

#[test]
fn w03_wait_timeout_times_out_then_wakes_on_cancel() {
    within(30, || {
        let root = CancelToken::new();
        let leaf = root.child().child();

        let t = Instant::now();
        assert!(!leaf.wait_timeout(Duration::from_millis(20)));
        assert!(
            t.elapsed() >= Duration::from_millis(20),
            "returned before the timeout"
        );

        let waiter = {
            let leaf = leaf.clone();
            std::thread::spawn(move || {
                let t = Instant::now();
                let cancelled = leaf.wait_timeout(Duration::from_secs(20));
                (cancelled, t.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        root.cancel();
        let (cancelled, waited) = waiter.join().unwrap();
        assert!(cancelled);
        assert!(
            waited < Duration::from_secs(5),
            "waiter was not woken by cancel (waited {waited:?})"
        );
        leaf.wait(); // returns at once now
    });
}

#[test]
fn w03_deep_and_wide_trees() {
    // A 100k-deep chain: cancel must not recurse on the stack.
    let root = CancelToken::new();
    let mut leaf = root.clone();
    let mut chain = Vec::new();
    for _ in 0..100_000 {
        let next = leaf.child();
        chain.push(std::mem::replace(&mut leaf, next));
    }
    root.cancel();
    assert!(leaf.is_cancelled());
    assert!(chain.iter().all(|t| t.is_cancelled()));
    drop(chain);

    // 10k children, half dropped early: the rest are all cancelled.
    let root = CancelToken::new();
    let mut kids: Vec<CancelToken> = (0..10_000).map(|_| root.child()).collect();
    kids.truncate(5_000);
    let more: Vec<CancelToken> = (0..5_000).map(|_| root.child()).collect();
    root.cancel();
    assert!(kids.iter().chain(&more).all(|t| t.is_cancelled()));
}

#[test]
fn w03_child_creation_racing_cancel_never_escapes() {
    // Threads create children (and grandchildren) while another thread
    // cancels the root. Every token created must end up cancelled.
    for round in 0..30 {
        let root = CancelToken::new();
        let made = Arc::new(Mutex::new(Vec::new()));
        let creators: Vec<_> = (0..4)
            .map(|_| {
                let root = root.clone();
                let made = Arc::clone(&made);
                std::thread::spawn(move || {
                    let mut mine = Vec::new();
                    for i in 0..2_000 {
                        let c = root.child();
                        if i % 3 == 0 {
                            mine.push(c.child());
                        }
                        mine.push(c);
                    }
                    made.lock().unwrap().extend(mine);
                })
            })
            .collect();
        std::thread::sleep(Duration::from_micros(50 * (round % 5)));
        root.cancel();
        for c in creators {
            c.join().unwrap();
        }
        let made = made.lock().unwrap();
        assert!(
            made.iter().all(|t| t.is_cancelled()),
            "a child escaped cancellation in round {round}"
        );
    }
}

/// Counts live tasks: +1 when created on the task's thread, -1 when dropped.
struct Alive(Arc<AtomicUsize>);

impl Alive {
    fn enter(count: &Arc<AtomicUsize>) -> Alive {
        count.fetch_add(1, Ordering::SeqCst);
        Alive(Arc::clone(count))
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn w03_taskset_shutdown_joins_every_task() {
    let alive = Arc::new(AtomicUsize::new(0));
    let work = Arc::new(AtomicUsize::new(0));
    let mut set = TaskSet::new();
    for i in 0..64 {
        let (alive, work) = (Arc::clone(&alive), Arc::clone(&work));
        set.spawn(format!("worker-{i}"), move |token| {
            let _alive = Alive::enter(&alive);
            while !token.wait_timeout(Duration::from_millis(1)) {
                work.fetch_add(1, Ordering::Relaxed);
            }
        })
        .unwrap();
    }
    assert_eq!(set.len(), 64);
    let t = Instant::now();
    while alive.load(Ordering::SeqCst) < 64 {
        assert!(t.elapsed() < Duration::from_secs(10), "tasks did not start");
        std::thread::yield_now();
    }
    assert_eq!(set.running(), 64);

    let report = set.shutdown(Duration::from_secs(10));
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.joined, 64);
    assert!(
        report.elapsed < Duration::from_secs(2),
        "cooperative tasks drained slowly: {:?}",
        report.elapsed
    );
    assert_eq!(alive.load(Ordering::SeqCst), 0, "a task outlived shutdown");
}

#[test]
fn w03_taskset_reports_missed_deadline_and_panics() {
    let alive = Arc::new(AtomicUsize::new(0));
    let mut set = TaskSet::new();
    {
        let alive = Arc::clone(&alive);
        set.spawn("stubborn", move |_token| {
            let _alive = Alive::enter(&alive);
            std::thread::sleep(Duration::from_millis(1_000)); // ignores cancellation
        })
        .unwrap();
    }
    set.spawn("boom", |token| {
        token.wait();
        panic!("task failure (expected by the test)");
    })
    .unwrap();
    {
        let alive = Arc::clone(&alive);
        set.spawn("good", move |token| {
            let _alive = Alive::enter(&alive);
            token.wait();
        })
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(20));

    let mut report = set.shutdown(Duration::from_millis(150));
    assert_eq!(report.missed, vec!["stubborn".to_string()]);
    assert_eq!(report.panicked, vec!["boom".to_string()]);
    assert_eq!(report.joined, 2);
    assert!(!report.is_clean());
    assert!(
        report.elapsed >= Duration::from_millis(150) && report.elapsed < Duration::from_millis(900),
        "{:?}",
        report.elapsed
    );
    assert_eq!(report.stragglers(), 1);
    assert_eq!(report.join_stragglers(), 1);
    assert_eq!(alive.load(Ordering::SeqCst), 0);
}

#[test]
fn w03_taskset_parent_cancel_reap_and_drop_join() {
    let supervisor = CancelToken::new();
    let alive = Arc::new(AtomicUsize::new(0));
    let mut set = TaskSet::with_parent(&supervisor);
    for i in 0..8 {
        let alive = Arc::clone(&alive);
        set.spawn(format!("session-{i}"), move |token| {
            let _alive = Alive::enter(&alive);
            token.wait();
        })
        .unwrap();
    }
    assert!(!set.token().is_cancelled());
    assert_eq!(set.reap(), 0, "nothing has finished yet");

    // The supervisor's cancel reaches the set's tasks through the tree.
    supervisor.cancel();
    assert!(set.token().is_cancelled());
    within(20, {
        let alive = Arc::clone(&alive);
        move || {
            while alive.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });
    let t = Instant::now();
    let mut reaped = 0;
    while reaped < 8 {
        reaped += set.reap();
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "finished tasks were not reaped"
        );
        std::thread::yield_now();
    }
    assert!(set.is_empty());

    // Dropping a set without shutdown cancels and joins what is left.
    let alive = Arc::new(AtomicUsize::new(0));
    let mut set = TaskSet::new();
    for i in 0..4 {
        let alive = Arc::clone(&alive);
        set.spawn(format!("t{i}"), move |token| {
            let _alive = Alive::enter(&alive);
            token.wait();
            std::thread::sleep(Duration::from_millis(5));
        })
        .unwrap();
    }
    within(20, move || drop(set));
    assert_eq!(alive.load(Ordering::SeqCst), 0, "drop must join every task");
}
