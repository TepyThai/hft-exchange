//! Benchmarks for the `journal` crate. Run with `cargo xtask bench --week N`.

use benchkit::{black_box, Report, Unit};
use journal::{encoded_len, Journal, FRAME_HEADER_LEN};
use types::{Command, NewOrder, Side};

/// Journal working set: big enough to amortize `clear()`, small enough to
/// stay cache-resident like a real journal's write head.
const JOURNAL_BYTES: usize = 256 * 1024;

const APPEND_LABEL: &str =
    "Append a NewOrder command (16 B header + 34 B payload) to an in-memory journal";

fn bench_append() -> Report {
    let mut j = Journal::with_capacity(JOURNAL_BYTES);
    let template = NewOrder::limit(0, Side::Buy, 10_000, 100);
    let frame = FRAME_HEADER_LEN + encoded_len(&Command::New(template));
    let mut id = 0u64;
    benchkit::latency_batched(
        "w09.journal_append",
        APPEND_LABEL,
        Unit::Ns,
        200_000,
        32,
        || {
            if j.byte_len() + frame > JOURNAL_BYTES {
                // O(1), keeps the allocation: appends never reallocate.
                j.clear();
            }
            id += 1;
            let mut o = template;
            o.id.0 = id;
            o.side = if id & 1 == 0 { Side::Buy } else { Side::Sell };
            black_box(j.append(black_box(&Command::New(o))));
        },
    )
}

fn main() {
    benchkit::run(&[("w09.journal_append", APPEND_LABEL, bench_append)]);
}
