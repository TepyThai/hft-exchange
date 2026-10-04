//! Week 47 — the server wiring: an engine thread stepping the simulated
//! market at a fixed rate and publishing L2 data, a WebSocket feed, and a
//! small HTTP server for the page, health, metrics and stats.
//!
//! # Threads and what crosses between them
//!
//! | Thread | Owns | Talks to others through |
//! | --- | --- | --- |
//! | `demo-engine` | `LiveSession`, `L2Publisher`, the ring's writer, its metric handles | the broadcast ring (updates), the `SnapshotCell`, atomics and a small mutex-guarded view for `/stats` |
//! | `demo-ws-accept` | the feed listener, the list of client sockets | spawns one `ws-client` thread per client (`marketdata::serve_ws_client`, reading the ring) |
//! | `demo-http-accept` | the HTTP listener | spawns one short-lived thread per request |
//!
//! The engine never waits on a client: the ring's writer never blocks, a
//! slow client lags, gets conflated batches or a fresh snapshot, and is
//! disconnected if its socket stays full (week 16's policy).
//!
//! # Pacing
//!
//! The engine thread runs on a schedule, not a sleep per command: by time
//! `t` it should have processed `t × rate` commands, so each wake-up runs
//! whatever is due (capped per batch) and then sleeps about a millisecond.
//! If it falls more than a second behind (a stalled VM), it drops the
//! backlog rather than bursting. `rate == 0` runs flat out.
//!
//! # Shutdown
//!
//! [`Demo::shutdown`], `POST /shutdown` or the end of `--duration` set one
//! flag. The engine thread finishes its batch and exits; [`Demo::wait`]
//! then wakes both acceptors (a connection to their own port), closes every
//! feed client's socket so its thread ends, joins the threads and returns
//! the final [`Stats`]. `/health` turns 503 as soon as the flag is set.

#![allow(unused_imports, unused_mut, dead_code)] // template: imports and helpers are hints for what you'll need
use crate::config::Config;
use crate::stats::Stats;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use telemetry::Registry;

// TODO(w47): implement — see the week guide on tepy.dev/hft.

/// Most commands the engine thread runs between two looks at the clock and
/// the stop flag.
pub const MAX_BATCH: u64 = 1_024;

/// Longest the engine sleeps when it is ahead of schedule.
pub const IDLE_SLEEP: Duration = Duration::from_millis(1);

/// HTTP requests served at once; more get 503.
pub const MAX_HTTP_CONNECTIONS: u64 = 64;

/// Read and write timeout of one HTTP connection.
pub const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest HTTP request head the server reads.
pub const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// The demo exchange, running. Created by [`Demo::start`]; stop it with
/// [`shutdown`](Demo::shutdown) then [`wait`](Demo::wait), or
/// [`stop`](Demo::stop). Dropping it requests a shutdown but does not
/// wait for the threads.
pub struct Demo {
    // TODO(w47): design this type's fields.
    _todo: (),
}

#[allow(unused_variables)]
impl Demo {
    /// Binds both listeners (so the ports are known and in use before it
    /// returns), then starts the engine, feed and HTTP threads. Errors are
    /// bind or spawn failures. Allocates the ring (`ring_capacity × 32`
    /// bytes) and the session.
    pub fn start(config: Config) -> io::Result<Demo> {
        todo!("w47: start")
    }

    /// The HTTP server's bound address (the real port when configured
    /// with port 0).
    pub fn http_addr(&self) -> SocketAddr {
        todo!("w47: http_addr")
    }

    /// The WebSocket feed's bound address.
    pub fn ws_addr(&self) -> SocketAddr {
        todo!("w47: ws_addr")
    }

    /// The metrics registry behind `/metrics`.
    pub fn registry(&self) -> &Registry {
        todo!("w47: registry")
    }

    /// A consistent-enough view of the counters (each value is exact; they
    /// are not read atomically together). Takes the view lock briefly;
    /// allocates the tape.
    pub fn stats(&self) -> Stats {
        todo!("w47: stats")
    }

    /// Whether a shutdown was requested (by [`shutdown`](Self::shutdown),
    /// `POST /shutdown` or the configured duration).
    pub fn is_stopping(&self) -> bool {
        todo!("w47: is_stopping")
    }

    /// Requests a shutdown and returns at once; idempotent.
    pub fn shutdown(&self) {
        todo!("w47: shutdown")
    }

    /// Blocks until a shutdown is requested, then drains: joins the engine
    /// thread, wakes and joins both acceptors (which close every feed
    /// client), and returns the final stats. Re-raises a panic of the
    /// engine thread after cleaning up.
    pub fn wait(mut self) -> Stats {
        todo!("w47: wait")
    }

    /// [`shutdown`](Self::shutdown) then [`wait`](Self::wait).
    pub fn stop(self) -> Stats {
        todo!("w47: stop")
    }
}

#[allow(unused_variables)]
impl Drop for Demo {
    fn drop(&mut self) {
        todo!("w47: drop")
    }
}

// TODO(w47): implement — see the week guide on tepy.dev/hft.
