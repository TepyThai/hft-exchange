//! Concurrency primitives: channels, ring buffers, MPSC queues, cache padding, cancellation, task sets, wait strategies, seqlocks.
//!
//! Curriculum: Weeks 1, 3, 4, 5, 30, 34 (see https://tepy.dev/hft).
//!
//! Everything here is `std`-only. The exchange's threads talk through these
//! types, so each one documents who may call what (single vs multi
//! producer/consumer) and what an operation costs.
//!
//! | Week | Module | What |
//! | --- | --- | --- |
//! | 1 | [`spmc`] | Blocking SPMC channel (Mutex + Condvar) |
//! | 1 | [`spsc`] | Bounded lock-free SPSC ring buffer |
//! | 3 | [`cancel`], [`taskset`] | Hierarchical cancellation, deadline-bounded task sets |
//! | 4 | [`pad`] | `CachePadded<T>`, padded counters; the ring's indices on separate lines |
//! | 5 | [`spsc`], [`mpsc`] | Cached indices + batching; bounded MPSC queue |
//! | 30 | [`wait`], [`affinity`] | Busy-spin / spin-yield / spin-park wait strategies, thread pinning |
//! | 34 | [`seqlock`], [`snapshot`] | Seqlock and an epoch-reclaimed snapshot cell |
//!
//! # Which queue?
//!
//! * One producer thread, one consumer thread, hot path: [`spsc`]. The
//!   engine → market-data and engine → journal hand-offs.
//! * Many producers (gateway sessions), one consumer (the engine): [`mpsc`].
//! * Rare control messages where blocking is fine: [`spmc`].

pub mod affinity;
pub mod cancel;
pub mod mpsc;
pub mod pad;
pub mod seqlock;
pub mod snapshot;
pub mod spmc;
pub mod spsc;
pub mod taskset;
pub mod wait;

pub use affinity::{current_affinity, pin_current_thread};
pub use cancel::CancelToken;
pub use pad::{CachePadded, PaddedCounters, UnpaddedCounters, CACHE_LINE};
pub use seqlock::SeqLock;
pub use snapshot::{SnapshotCell, SnapshotReader};
pub use taskset::{ShutdownReport, TaskSet};
pub use wait::{BusySpin, SpinPark, SpinYield, WaitStrategy};
