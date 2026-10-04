# Conventions

How the exchange, its tests, its benchmarks and the learner template fit
together. The curriculum spine lives in the site repo
(`app/hft/curriculum.ts` in github.com/TepyThai/tepy.dev): every week's
crate, test prefix and benchmark ids come from there.

## Branches

* `reference` — the complete implementation. Every test passes.
* `main` — the learner template, generated with
  `cargo xtask strip --out <dir> --check` from `reference`. Students fork this.

## Week tags

* **Tests** for week N are named `wNN_<what_it_checks>` (two digits), e.g.
  `w06_cancel_removes_from_level`. Put them in `#[cfg(test)] mod tests` in
  the file they test, or in `crates/<crate>/tests/wNN_<topic>.rs`.
  `cargo xtask test --week N` runs exactly these.
* A week's tests use only the **public API** of the crates, are
  **deterministic** (use `types::Rng` with a fixed seed), and finish in
  seconds under the `xtest` profile (optimized, no LTO). Timing assertions
  use generous bounds (10× headroom) — benchmarks are where speed is judged.
* 4–8 tests per week: the main behaviour, edge cases (empty, one element,
  full, boundary values), and at least one randomized/differential test where
  it makes sense.
* Later weeks extend earlier code. **Earlier weeks' tests must keep
  passing** — that is the regression suite.

## Benchmarks

* Each crate has one bench target, `benches/hft.rs`, calling
  `benchkit::run(&[(id, label, fn), ...])`.
* Ids are `wNN.<name>` and must match `curriculum.ts` exactly.
* Use `benchkit::throughput` for rates, `benchkit::latency` /
  `latency_batched` for per-operation distributions (reported in ns or µs
  with p50/p99/p99.9), `benchkit::ratio` for speedups, and
  `benchkit::Histogram` for custom multi-threaded measurements.
* A benchmark must panic only through unimplemented student code (the
  template), never because of the harness.
* Calibrate: run `cargo xtask bench --week N` on the reference and record
  the numbers. Curriculum targets are set at roughly 60% of the reference
  throughput / 1.5× the reference latency on the reference machine (AMD EPYC
  7543P), so a solid learner implementation hits them.

## Student markers (template generation)

Mark what the learner writes; everything else ships as-is:

```rust
// @student w06
pub struct Book { /* fields replaced by a placeholder */ }

// @student w06
impl Book { /* every fn body becomes todo!("w06: name") */ }

// @student w07
pub fn match_order(...) -> ... { /* body becomes todo!() */ }

// @student-begin w09
struct InternalHelper { ... }   // removed entirely
// @student-end
```

Rules that keep the template compiling (`cargo xtask strip --check`):

* Code that is **not** marked must not read or construct the fields of a
  marked struct, and must not call items inside a `@student-begin` region.
* Public signatures, trait definitions, error enums and doc comments stay
  unmarked — they are the contract the learner implements against.
* Prefer marking whole `impl` blocks for the week's main type, plus the
  struct itself when designing its representation is part of the lesson.
* When a later week changes a function the learner already wrote, mark it
  with the later week (the learner revisits it then).

## Code quality

* Idiomatic, documented Rust: every public item has a doc comment saying
  what it does and its cost (complexity, allocation).
* `unsafe` only when the week is about it, always with a `// SAFETY:`
  comment, and with a safe version or a test that exercises it under the
  same conditions.
* No external dependencies except where the week needs them (`tokio` for the
  gateway). Ask before adding one. The wasm crate uses a plain C ABI, and
  `apps/terminal` (GPUI) is a separate workspace, so neither adds
  dependencies to the curriculum crates.
* `cargo clippy --workspace --all-targets` clean; `cargo fmt`.
