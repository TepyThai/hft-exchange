# Architecture

A one-page map of the exchange: where an order goes, which thread owns
what, what must always be true, and which crate implements each box. It
describes the system the 52 weeks build; each crate section names the weeks
that build it. The [README](../README.md) has the diagram.

## Bird's-eye view

The exchange is one process per instrument group. Members connect over TCP
and speak FIX 4.4 or the binary protocol. Their orders flow along one line:

**gateway → risk → journal → engine → market data**

1. A **gateway** I/O thread decodes a frame into a `Command`, checks the
   session's throttle and pushes the command onto an MPSC ring.
2. The **engine thread** pops it. Pre-trade **risk** checks it against the
   account's limits; the **journal** gives it the next sequence number and
   appends it (and, with a standby, replicates it); the **engine** matches
   it and emits `Event`s; the **L2 publisher** turns events into price-level
   updates. All four are direct calls on that one thread.
3. Events go back to the owning session as execution reports over an SPSC
   ring, and L2 updates go to every market-data client through a broadcast
   ring that never waits for a reader.

The **demo** runs the same engine and market data with simulated members
instead of a gateway, and serves the feed over WebSocket. **wasm** and
**apps/terminal** run the engine with the same simulated flow in a browser
tab and in a native window.

## Threads and queues

| Thread | Owns | In | Out |
| --- | --- | --- | --- |
| gateway I/O (Tokio) | sessions, FIX/binary codecs, throttles | TCP | commands: MPSC ring |
| engine | risk state, journal writer, order book, L2 publisher | MPSC ring | execution reports: SPSC ring; L2 updates: broadcast ring |
| market-data clients | one ring cursor and conflation buffer each | broadcast ring | WebSocket / UDP |
| replication | the standby connection | journal | TCP |

Every arrow that crosses a thread is a queue from `sync` or `marketdata`,
and every queue is bounded. Nothing on the engine thread takes a lock or
waits on a consumer.

## Invariants

- **Determinism.** The engine is a pure function of the sequenced command
  stream: no clocks, no randomness, fixed hashers. Replaying the journal
  rebuilds the same book and the same `state_hash`.
- **Journal before effect.** A command is sequenced and appended before the
  engine applies it, rejected commands included, so the journal is the
  source of truth for recovery and failover.
- **One writer per piece of state.** Each structure above has exactly one
  owning thread; others see copies (snapshots, ring entries, metric cells).
- **Price-time priority.** Better price first; at one price, earlier first.
  A partial fill keeps its place.
- **Integers on the order path.** Prices are ticks, quantities are lots.
- **The feed contract.** L2 update `n` sets one level's total; a snapshot
  at `seq = s` reflects every update up to `s`; applying a whole conflated
  batch gives the publisher's book as of the batch's `seq`.
- **Slow consumers never slow the engine.** They lag, get conflated
  batches or a fresh snapshot, or are disconnected.

## Decisions

**Single-threaded matching.** One thread owns each book: no locks, no
cache-line traffic on the hot path, and determinism for free. Throughput
scales by sharding instruments across engine threads (week 36), not by
sharing a book.

**Acknowledge after the journal.** An execution report leaves only after
the command is in the journal (and on the standby when replication is on),
so an acknowledged order survives a crash or failover. It costs a few
microseconds of order-to-ack, measured in weeks 24 and 39.

**Fixed-layout binary wire format.** Every message type has a fixed size
and fixed offsets, so decoding is a bounds check and a few loads with no
allocation, and the schema (week 35) generates the codecs. FIX stays at
the edge for members that need it.

## Crates

### `types`

The shared vocabulary: `Price`, `Qty`, `OrderId`, `NewOrder`, `Command`,
`Event`, `RejectReason`, `InstrumentSpec` and the seeded `Rng` that keeps
tests and simulations reproducible. Every other crate depends on it and it
depends on nothing. Provided, not built in a week.

### `sync`

The queues and primitives the threads talk through: a blocking SPMC
channel and a lock-free SPSC ring, cache-padded counters, cancellation
tokens and task sets with deadline-bounded shutdown, the optimized SPSC and
a bounded MPSC queue, pinned threads with spin-then-park waiting, and
seqlocks and epoch-reclaimed snapshot cells. Weeks 1, 3, 4, 5, 30, 34.

### `telemetry`

Measurement: a log-linear latency histogram, per-thread counters and
histograms merged off the hot path with Prometheus exposition, a calibrated
TSC clock and stage timestamps, an open-loop load harness that corrects for
coordinated omission, and per-stage latency attribution against budgets.
Weeks 2, 19, 21, 25, 39.

### `book`

The limit order book behind the `OrderBook` trait: price levels with FIFO
queues and an order index for O(1) cancel, then an allocation-free slab
variant and a cache-friendly ladder variant that the engine can swap in.
Weeks 6, 27, 28.

### `engine`

Price-time matching over any `OrderBook`: limit and market orders, partial
fills, IOC/FOK/post-only, validation with typed rejects, a seeded
differential test against a naive matcher, then profiling, sharding across
instruments, static specialization, hot/cold path splits and the final
performance sprint. Weeks 7, 8, 12, 26, 36, 37, 38, 40.

### `journal`

The sequenced, CRC-checked, append-only command log and replay to an
identical state hash; then a file-backed journal with snapshots and
recovery from torn writes, and primary-to-standby replication with a
leased leader. Weeks 9, 18, 20.

### `marketdata`

Engine events to L2: sequenced incrementals and snapshots, a client book
builder with gap recovery, the single-writer broadcast ring with lag
detection and conflation, the WebSocket feed, SIMD depth aggregation over a
dense ladder, and pcap capture and replay. Weeks 10, 16, 29, 46.

### `wire`

The binary protocol: fixed-layout codecs, zero-copy message views and
length-prefixed framing, then schema-driven, versioned codecs generated
with `macro_rules!`. Weeks 11, 35.

### `fix`

FIX 4.4 without allocation: parsing and encoding NewOrderSingle,
OrderCancelRequest and ExecutionReport with exact BodyLength and CheckSum,
and a sans-IO session layer with sequence numbers, heartbeats and gap
recovery. Week 14.

### `risk`

Pre-trade checks per account (order size, notional, projected position,
price band) and kill switches; token-bucket and sliding-window throttles
per session and account on an injected clock; positions and PnL with
post-trade limits that feed the kill switch. Weeks 15, 17, 45.

### `gateway`

The front door: Tokio TCP sessions with logon and order entry routed to the
engine thread, typed configuration and the release build, the end-to-end
exchange binary with its load test, socket tuning, and a batched I/O event
loop. Weeks 13, 22, 24, 31, 32.

### `sim`

Everything that trades against the exchange: synthetic flow and the live
session (provided), then simulated members and scenario tests,
microstructure analytics, a market-making strategy, pairs trading, an
event-driven backtester, and fault-injection scenarios.
Weeks 23, 41, 42, 43, 44, 48.

### `demo`

The deployable demo: the engine and simulated flow on one thread at a fixed
rate, the L2 feed over WebSocket, a small HTTP server (page, health,
Prometheus metrics, stats), `Dockerfile.demo` and `fly.toml`; then the
tests that hold this document and the README to the code.
Weeks 47, 51.

### `wasm`

The engine and the live session compiled to WebAssembly with a plain C ABI
and no JavaScript glue, for the live page on tepy.dev. `cargo xtask wasm`
builds it. Provided; it runs whichever engine the workspace has.

### `benchkit`

The benchmark harness: latency distributions, throughput and ratios,
emitted as `BENCHJSON` lines that `cargo xtask bench` collects one process
per benchmark. Provided.

### `xtask`

Workspace tooling: week-scoped tests and benchmarks, the JSON report and
its Markdown rendering, upload to tepy.dev, learner-template generation
(`strip`) and the WebAssembly build. Provided.

### `apps/terminal`

A native trading terminal built with GPUI, outside the workspace so its
dependencies never reach the curriculum crates. It runs the live session
and shows the ladder, candles, depth and a per-command latency histogram.
See [its README](../apps/terminal/README.md).
