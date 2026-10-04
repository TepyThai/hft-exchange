//! Week 30: pinning threads to CPUs.
//!
//! A pinned thread keeps its caches warm and never migrates mid-burst; two
//! pinned threads on cores that share an L3 hand cache lines to each other
//! in tens of nanoseconds. On Linux this calls `sched_setaffinity(2)`
//! through a two-line FFI declaration (no `libc` crate). Elsewhere pinning
//! is a no-op that returns `Ok(())`.
//!
//! Pair it with isolated cores (`isolcpus=`, `nohz_full=` on the kernel
//! command line) so nothing else is scheduled where the engine spins.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use std::io;

// TODO(w30): implement — see the week guide on tepy.dev/hft.

/// Restricts the **calling thread** to run only on CPU `cpu` (as numbered
/// by the OS, e.g. `0..nproc`). Threads it spawns afterwards inherit the
/// mask.
///
/// Errors if the CPU does not exist, is offline, or is outside the
/// process's allowed set (cgroups / `taskset`). On non-Linux targets this
/// does nothing and returns `Ok(())`. Cost: one syscall; call it once at
/// thread start, not on the hot path.
#[allow(unused_variables)]
pub fn pin_current_thread(cpu: usize) -> io::Result<()> {
    todo!("w30: pin_current_thread")
}

/// The CPUs the calling thread may currently run on, ascending. On
/// non-Linux targets: `0..available_parallelism`. One syscall.
#[allow(unused_variables)]
pub fn current_affinity() -> io::Result<Vec<usize>> {
    todo!("w30: current_affinity")
}
