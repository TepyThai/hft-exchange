# Benchmark report

| | |
| --- | --- |
| Commit | `983f8f548f20` on `reference` |
| Machine | AMD EPYC 7543P 32-Core Processor · 64 cores · linux |
| Toolchain | rustc 1.97.1 (8bab26f4f 2026-07-14) |
| Run | 2026-10-04 16:47 UTC · local · hft-exchange xtask 0.1.0 |

Benchmarks depend on the machine, its load and its tuning: compare runs from the same machine and profile only.

## Tests

| Week | Passed | Failed |
| ---: | ---: | --- |
| 1 | 8 | 0 |
| 2 | 7 | 0 |
| 3 | 8 | 0 |
| 4 | 6 | 0 |
| 5 | 8 | 0 |
| 6 | 8 | 0 |
| 7 | 8 | 0 |
| 8 | 7 | 0 |
| 9 | 7 | 0 |
| 10 | 7 | 0 |
| 11 | 8 | 0 |
| 12 | 10 | 0 |
| 13 | 7 | 0 |
| 14 | 8 | 0 |
| 15 | 8 | 0 |
| 16 | 7 | 0 |
| 17 | 9 | 0 |
| 18 | 7 | 0 |
| 19 | 7 | 0 |
| 20 | 6 | 0 |
| 21 | 6 | 0 |
| 22 | 6 | 0 |
| 23 | 6 | 0 |
| 24 | 8 | 0 |
| 25 | 7 | 0 |
| 26 | 6 | 0 |
| 27 | 7 | 0 |
| 28 | 6 | 0 |
| 29 | 6 | 0 |
| 30 | 5 | 0 |
| 31 | 4 | 0 |
| 32 | 4 | 0 |
| 34 | 7 | 0 |
| 35 | 7 | 0 |
| 36 | 6 | 0 |
| 37 | 6 | 0 |
| 38 | 6 | 0 |
| 39 | 6 | 0 |
| 40 | 6 | 0 |
| 41 | 5 | 0 |
| 42 | 5 | 0 |
| 43 | 5 | 0 |
| 44 | 5 | 0 |
| 45 | 8 | 0 |
| 46 | 7 | 0 |
| 47 | 7 | 0 |
| 48 | 8 | 0 |
| 51 | 8 | 0 |
| **all** | **324** | **0** |

## Latency

| Week | Id | Benchmark | p50 | p99 | p99.9 | max | samples |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: |
| 6 | `w06.add_cancel` | Add then cancel, 100k resting orders | 130 ns | 550 ns | 850 ns | 148 µs | 2,000,000 |
| 6 | `w06.best_price` | Best bid/ask query | 2 ns | 5 ns | 8 ns | 279 ns | 200,000 |
| 27 | `w27.add_cancel_slab` | Add then cancel with slab storage | 130 ns | 251 ns | 501 ns | 115 µs | 2,000,000 |
| 28 | `w28.ladder_add_cancel` | Ladder book add then cancel | 120 ns | 190 ns | 341 ns | 112 µs | 2,000,000 |
| 7 | `w07.match_latency` | Submit→events, mixed flow (70% passive) | 70 ns | 261 ns | 391 ns | 53.4 µs | 1,000,000 |
| 8 | `w08.fok_check` | FOK feasibility check, deep book | 80 ns | 241 ns | 301 ns | 43.0 µs | 1,000,000 |
| 18 | `w18.recovery` | Recover a 1M-command journal: snapshot at 750k + replay 250k tail (verify whole file, warm page cache) | 56.4 ms | 62.4 ms | 62.4 ms | 62.4 ms | 15 |
| 20 | `w20.replication_lag` | Primary append -> standby applied (validated, journaled, matched), in-process, 100k cmds/s, spinning standby | 1.03 µs | 6.44 µs | 13.6 µs | 253 µs | 200,000 |
| 25 | `w25.engine_open_loop` | Engine under open-loop load at 50% utilization (mixed flow), response time from intended start | 95 ns | 31.9 µs | 162 µs | 195 µs | 1,000,000 |
| 38 | `w38.match_latency` | Submit→events after hot/cold split | 70 ns | 240 ns | 361 ns | 21.7 µs | 1,000,000 |
| 40 | `w40.match_latency` | Final submit→events latency | 50 ns | 190 ns | 301 ns | 34.2 µs | 1,000,000 |
| 14 | `w14.parse_nos` | Parse NewOrderSingle | 158 ns | 313 ns | 389 ns | 775 ns | 20,000 |
| 13 | `w13.roundtrip` | Client order → ack over loopback TCP | 42.1 µs | 80.1 µs | 159 µs | 1.44 ms | 20,000 |
| 31 | `w31.tcp_pingpong` | TCP ping-pong, loopback | 27.5 µs | 47.2 µs | 112 µs | 1.32 ms | 20,000 |
| 24 | `w24.e2e_latency` | Order in → ack out, loopback, under load | 59.6 µs | 2.52 ms | 4.11 ms | 5.40 ms | 65,189 |
| 39 | `w39.tick_to_ack` | Order in → ack, in-process pipeline | 631 ns | 2.92 µs | 7.39 µs | 350 µs | 129,966 |
| 9 | `w09.journal_append` | Append a NewOrder command (16 B header + 34 B payload) to an in-memory journal | 50 ns | 65 ns | 321 ns | 2.77 µs | 200,000 |
| 10 | `w10.l2_update` | Engine event → L2 incremental (steady-state book, ~5k orders on ~60 levels) | 45 ns | 64 ns | 96 ns | 6.59 µs | 200,000 |
| 15 | `w15.risk_check` | Full pre-trade check | 19 ns | 49 ns | 209 ns | 39.3 µs | 200,000 |
| 17 | `w17.throttle_check` | Token bucket check | 13 ns | 24 ns | 129 ns | 1.17 µs | 200,000 |
| 45 | `w45.fill_update` | Apply fill to positions + PnL | 38 ns | 65 ns | 283 ns | 1.86 µs | 200,000 |
| 42 | `w42.strategy_decision` | Market data → quote decision | 110 ns | 201 ns | 321 ns | 108 µs | 200,000 |
| 3 | `w03.cancel_fanout` | Cancel 1,000 child tokens, observed by all | 17.6 µs | 26.9 µs | 38.2 µs | 73.0 µs | 5,000 |
| 30 | `w30.wakeup_busy` | Cross-thread wakeup, busy polling | 90 ns | 150 ns | 22.0 µs | 290 µs | 200,000 |
| 30 | `w30.wakeup_park` | Cross-thread wakeup, park/unpark | 5.32 µs | 73.4 µs | 1.65 ms | 3.34 ms | 20,000 |
| 34 | `w34.seqlock_read` | Seqlock read under a concurrent writer | 1 ns | 3 ns | 149 ns | 1.29 µs | 200,000 |
| 2 | `w02.histogram_record` | Histogram::record cost | 1 ns | 3 ns | 9 ns | 31 ns | 20,000 |
| 19 | `w19.metric_overhead` | Hot-path counter + histogram record | 3 ns | 7 ns | 18 ns | 37 ns | 20,000 |
| 21 | `w21.clock_now` | Clock::now cost | 10 ns | 19 ns | 53 ns | 122 ns | 20,000 |
| 11 | `w11.decode` | Decode one 40-byte order message | 9 ns | 16 ns | 29 ns | 182 ns | 20,000 |
| 35 | `w35.decode` | Generated decoder, one message | 5 ns | 11 ns | 22 ns | 70 ns | 20,000 |

## Throughput and ratios

| Week | Id | Benchmark | Result |
| ---: | --- | --- | ---: |
| 28 | `w28.speedup` | Ladder vs w06 book | 1.24× |
| 7 | `w07.engine_throughput` | Orders per second, single thread | 17.0 M ops/s |
| 12 | `w12.engine_v1_throughput` | Engine v1 end to end (journal + match + md) | 8.02 M ops/s |
| 26 | `w26.engine_throughput` | Engine throughput after profiling fixes | 18.8 M ops/s |
| 36 | `w36.sharded_throughput` | 4 shards, uniform symbols | 11.9 M ops/s |
| 37 | `w37.static_vs_dyn` | Generic vs dyn engine | 1.20× |
| 40 | `w40.engine_throughput` | Final engine throughput | 26.7 M ops/s |
| 32 | `w32.batched_io` | Messages per wakeup-batched loop | 4.44 M msgs/s |
| 16 | `w16.fanout` | Publish to 16 readers (L2Update, 4096-slot ring, every reader drains every round) | 31.8 M msgs/s |
| 29 | `w29.depth_sum_speedup` | SIMD vs scalar depth aggregation (depth over N ticks, sweep-to-fill, qty at or better; 4096-tick ladder) | 2.16× |
| 46 | `w46.pcap_replay` | Capture replay into books (pcap → UDP → feed packets → reorder buffer → book, 32 updates/packet) | 30.9 M msgs/s |
| 23 | `w23.sim_throughput` | Simulated orders through the full pipeline | 4.56 M ops/s |
| 44 | `w44.replay_speed` | Backtest replay speed | 6.57 M msgs/s |
| 1 | `w01.spsc_throughput` | SPSC ring, producer→consumer across two threads | 179 M ops/s |
| 1 | `w01.mutex_channel_throughput` | Mutex channel baseline, two threads | 30.0 M ops/s |
| 4 | `w04.padded_counters` | 8 threads incrementing padded counters | 2.51 G ops/s |
| 4 | `w04.unpadded_counters` | 8 threads incrementing adjacent counters (baseline) | 106 M ops/s |
| 5 | `w05.spsc_batched_throughput` | SPSC ring with batching, two threads | 1.68 G ops/s |
| 5 | `w05.mpsc_throughput` | MPSC, 4 producers → 1 consumer | 13.2 M ops/s |
| 11 | `w11.decode_throughput` | Decode stream of mixed messages | 73.4 M msgs/s |
