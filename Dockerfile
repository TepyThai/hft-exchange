# The exchange binary (crates/gateway, bin `exchange`) in a container (week 22).
#
#   docker build -t hft-exchange:$(git rev-parse --short HEAD) .
#   docker run --rm --network host --cpuset-cpus 2-5 \
#       -e HFT_ENGINE_CPU=3 -e HFT_JOURNAL_PATH=/data/journal.bin -v hft-data:/data \
#       hft-exchange:<sha>
#
# * Host networking: the default bridge puts a veth pair, a bridge and NAT
#   between the socket and the NIC — more work and jitter per packet.
# * --cpuset-cpus, not --cpus: a CFS quota throttles a busy-polling engine
#   thread for milliseconds at the end of every 100 ms period.
# * For a reproducible image, pin both base images by digest
#   (`FROM rust:1.97.1-slim-bookworm@sha256:…`); the tags below are the
#   readable version of that.

ARG RUST_VERSION=1.97.1

FROM rust:${RUST_VERSION}-slim-bookworm AS build
ARG RUST_VERSION
# rust-toolchain.toml asks for "stable"; build with the image's pinned
# toolchain instead of letting rustup download whatever stable is today.
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} \
    CARGO_TERM_COLOR=never
WORKDIR /src
COPY . .
# --locked: fail rather than resolve different dependency versions than
# the committed Cargo.lock. The release profile (fat LTO, one codegen
# unit, line tables) comes from the workspace Cargo.toml. (With BuildKit,
# `RUN --mount=type=cache,target=/usr/local/cargo/registry` keeps the
# crate downloads between builds.)
RUN cargo build --profile release --locked -p gateway --bin exchange \
    && cp target/release/exchange /usr/local/bin/exchange

# glibc runtime (not musl: its allocator is markedly slower under
# multithreaded allocation), no shell, runs as an unprivileged user.
# For a debuggable image use debian:bookworm-slim instead.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /usr/local/bin/exchange /usr/local/bin/exchange
COPY exchange.toml /etc/hft/exchange.toml
ENV HFT_JOURNAL_PATH=/tmp/hft-journal.bin
EXPOSE 9000
ENTRYPOINT ["/usr/local/bin/exchange", "--config", "/etc/hft/exchange.toml"]
