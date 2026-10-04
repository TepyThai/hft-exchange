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

use std::io;

// @student-begin w30
#[cfg(target_os = "linux")]
mod ffi {
    /// Bits in glibc's / musl's `cpu_set_t`.
    pub const CPU_SETSIZE: usize = 1024;
    /// `cpu_set_t`: a 1024-bit mask, CPU `n` is bit `n % 64` of word `n / 64`.
    pub type CpuSet = [u64; CPU_SETSIZE / 64];

    extern "C" {
        /// `int sched_setaffinity(pid_t pid, size_t cpusetsize, const cpu_set_t *mask);`
        pub fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const CpuSet) -> i32;
        /// `int sched_getaffinity(pid_t pid, size_t cpusetsize, cpu_set_t *mask);`
        pub fn sched_getaffinity(pid: i32, cpusetsize: usize, mask: *mut CpuSet) -> i32;
    }
}
// @student-end

/// Restricts the **calling thread** to run only on CPU `cpu` (as numbered
/// by the OS, e.g. `0..nproc`). Threads it spawns afterwards inherit the
/// mask.
///
/// Errors if the CPU does not exist, is offline, or is outside the
/// process's allowed set (cgroups / `taskset`). On non-Linux targets this
/// does nothing and returns `Ok(())`. Cost: one syscall; call it once at
/// thread start, not on the hot path.
// @student w30
pub fn pin_current_thread(cpu: usize) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        if cpu >= ffi::CPU_SETSIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cpu {cpu} >= CPU_SETSIZE"),
            ));
        }
        let mut set: ffi::CpuSet = [0; ffi::CPU_SETSIZE / 64];
        set[cpu / 64] |= 1 << (cpu % 64);
        // SAFETY: pid 0 means the calling thread; `set` is a fully
        // initialized `cpu_set_t`-sized buffer that outlives the call, we
        // pass its exact size, and the kernel only reads from it.
        let rc = unsafe { ffi::sched_setaffinity(0, std::mem::size_of::<ffi::CpuSet>(), &set) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cpu;
        Ok(())
    }
}

/// The CPUs the calling thread may currently run on, ascending. On
/// non-Linux targets: `0..available_parallelism`. One syscall.
// @student w30
pub fn current_affinity() -> io::Result<Vec<usize>> {
    #[cfg(target_os = "linux")]
    {
        let mut set: ffi::CpuSet = [0; ffi::CPU_SETSIZE / 64];
        // SAFETY: pid 0 means the calling thread; `set` is a writable
        // `cpu_set_t`-sized buffer and we pass its exact size, so the
        // kernel writes within bounds.
        let rc = unsafe { ffi::sched_getaffinity(0, std::mem::size_of::<ffi::CpuSet>(), &mut set) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((0..ffi::CPU_SETSIZE)
            .filter(|&c| set[c / 64] & (1 << (c % 64)) != 0)
            .collect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok((0..std::thread::available_parallelism()?.get()).collect())
    }
}
