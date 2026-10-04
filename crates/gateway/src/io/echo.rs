//! The same session code on any backend: a framing-layer echo server with
//! natural batching.

#[allow(unused_imports)] // used by the implementation the learner writes
use super::{Completion, Submission};
use super::{ConnId, IoBackend, IoStats};
#[allow(unused_imports)] // used by the implementation the learner writes
use std::collections::{HashMap, VecDeque};
use std::io;
#[allow(unused_imports)] // used by the implementation the learner writes
use std::time::{Duration, Instant};
#[allow(unused_imports)] // used by the implementation the learner writes
use wire::FrameDecoder;

/// How [`echo_loop`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoOptions {
    /// Bytes per read buffer.
    pub read_size: usize,
    /// Completions handled per wakeup: `usize::MAX` takes everything ready
    /// (natural batching); `1` handles one completion per wakeup.
    pub max_batch: usize,
    /// Give up if no completion arrives for this long.
    pub idle_timeout: Duration,
}

impl Default for EchoOptions {
    /// 16 KiB reads, natural batching, 10 s idle timeout.
    fn default() -> Self {
        EchoOptions {
            read_size: 16 * 1024,
            max_batch: usize::MAX,
            idle_timeout: Duration::from_secs(10),
        }
    }
}

/// What one [`echo_loop`] run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EchoStats {
    /// Frames received (and echoed).
    pub messages: u64,
    /// Bytes received (and echoed).
    pub bytes: u64,
    /// Completions handled.
    pub completions: u64,
    /// Loop iterations, one `submit_and_wait` each.
    pub wakeups: u64,
    /// From the first submission to the last connection's end of stream.
    pub elapsed: Duration,
    /// The backend's counters at the end.
    pub io: IoStats,
}

impl EchoStats {
    /// Messages handled per wakeup.
    pub fn per_wakeup(&self) -> f64 {
        self.messages as f64 / self.wakeups.max(1) as f64
    }

    /// Messages per second over the run.
    pub fn msgs_per_sec(&self) -> f64 {
        self.messages as f64 / self.elapsed.as_secs_f64().max(1e-9)
    }
}

/// Echoes every frame received on `conns` back to its sender until every
/// connection reaches end of stream (or fails) and every echo is written.
///
/// Each loop iteration is one wakeup: wait for at least one completion
/// (without waiting when some are already in hand), handle up to
/// `max_batch` of them — count the frames of each read, queue its bytes for
/// echo, re-post the read with the same buffer — then push **one write per
/// connection** with everything that connection's reads produced in this
/// batch. The submit for those writes and the re-posted reads happens at
/// the start of the next iteration: never inside the per-completion loop.
///
/// Buffers are recycled: a read's buffer is re-posted; a write's comes back
/// in its completion and goes to a pool. Allocates only to grow buffers.
// @student w32
pub fn echo_loop<B: IoBackend + ?Sized>(
    io: &mut B,
    conns: &[ConnId],
    opts: &EchoOptions,
) -> io::Result<EchoStats> {
    const READ: u64 = 0;
    const WRITE: u64 = 1;
    let tag = |k: usize, op: u64| ((k as u64) << 1) | op;
    let mut stats = EchoStats::default();
    let mut decoders: Vec<FrameDecoder> = conns.iter().map(|_| FrameDecoder::default()).collect();
    let mut pending: Vec<Vec<u8>> = conns.iter().map(|_| Vec::new()).collect();
    let mut dirty: Vec<usize> = Vec::with_capacity(conns.len());
    let mut pool: Vec<Vec<u8>> = Vec::new();
    let mut ready: VecDeque<Completion> = VecDeque::new();
    let mut fresh: Vec<Completion> = Vec::new();
    let mut batch: Vec<Completion> = Vec::new();
    let mut open = conns.len();
    let mut writing = 0usize;
    let start = Instant::now();

    for (k, &conn) in conns.iter().enumerate() {
        let sub = Submission::Read {
            conn,
            buf: vec![0; opts.read_size],
            user_data: tag(k, READ),
        };
        push(io, sub, &mut ready)?;
    }
    // Until every connection ended and every echo is written.
    while open > 0 || writing > 0 {
        let min = usize::from(ready.is_empty());
        io.submit_and_wait(min, Some(opts.idle_timeout))?;
        stats.wakeups += 1;
        io.drain(&mut fresh);
        ready.extend(fresh.drain(..));
        if ready.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "echo loop: no completion in time",
            ));
        }
        let take = ready.len().min(opts.max_batch);
        batch.extend(ready.drain(..take));
        for c in batch.drain(..) {
            stats.completions += 1;
            let k = (c.user_data >> 1) as usize;
            let buf = c.buf.unwrap_or_default();
            if c.user_data & 1 == WRITE {
                writing -= 1;
                if c.result < 0 {
                    return Err(io::Error::other(format!(
                        "echo write failed: errno {}",
                        -c.result
                    )));
                }
                pool.push(buf);
                continue;
            }
            if c.result <= 0 {
                // End of stream (or an error): this connection is done.
                open -= 1;
                continue;
            }
            stats.bytes += c.result as u64;
            let mut input = &buf[..];
            while decoders[k]
                .next_frame(&mut input)
                .map_err(io::Error::other)?
                .is_some()
            {
                stats.messages += 1;
            }
            if pending[k].is_empty() {
                dirty.push(k);
            }
            pending[k].extend_from_slice(&buf);
            let mut buf = buf;
            buf.resize(opts.read_size, 0);
            push(
                io,
                Submission::Read {
                    conn: conns[k],
                    buf,
                    user_data: tag(k, READ),
                },
                &mut ready,
            )?;
        }
        // One write per connection per batch.
        for k in dirty.drain(..) {
            let mut spare = pool.pop().unwrap_or_default();
            spare.clear();
            let data = std::mem::replace(&mut pending[k], spare);
            push(
                io,
                Submission::Write {
                    conn: conns[k],
                    buf: data,
                    user_data: tag(k, WRITE),
                },
                &mut ready,
            )?;
            writing += 1;
        }
    }
    stats.elapsed = start.elapsed();
    stats.io = io.stats();
    Ok(stats)
}

// @student-begin w32
/// Pushes, submitting (without waiting) whenever the queue is full.
fn push<B: IoBackend + ?Sized>(
    io: &mut B,
    mut sub: Submission,
    ready: &mut VecDeque<Completion>,
) -> io::Result<()> {
    let mut scratch = Vec::new();
    loop {
        match io.push(sub) {
            Ok(()) => return Ok(()),
            Err(back) => {
                sub = back;
                io.submit_and_wait(0, Some(Duration::ZERO))?;
                io.drain(&mut scratch);
                ready.extend(scratch.drain(..));
            }
        }
    }
}
// @student-end
