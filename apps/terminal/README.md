# hft-terminal

A native trading terminal for your exchange, built with [GPUI](https://www.gpui.rs/) (the GPU-accelerated UI framework behind the Zed editor) and [gpui-component](https://github.com/longbridge/gpui-component).

![hft-terminal](screenshot.png)

It runs one in-process `sim::live::LiveSession`: the workspace's matching engine (yours, once weeks 6–8 pass), simulated order flow from `sim::flow`, and you as a trader.

- **Order book ladder**: top 14 levels per side, with depth bars, order counts and your resting orders marked.
- **Charts**: one-second candles, and cumulative depth on a shared price axis.
- **Engine latency**: every command is timed with `Instant`. The panel shows p50, p99, p99.9 and max over rolling two-second windows, plus a log2 histogram. Compare *Calm* (cold caches between frames) with *Flood* (hot loop).
- **Order entry**: limit, IOC or market orders. A blank price joins your side's touch (limit) or takes the other side's (IOC). The panel also shows position, mark-to-market P&L, fills and cancels.
- **Speeds**: *Calm*, *Busy*, *Open* and *Flood* send 2, 40, 800 and 12 000 commands per 16 ms frame.

## Run

```sh
cd apps/terminal
cargo run --release
```

The terminal is a separate Cargo workspace, so `cargo test` and CI for the curriculum crates never build GPUI.

On Linux, GPUI needs Vulkan and the X11 and Wayland development headers. On Debian or Ubuntu:

```sh
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libx11-xcb-dev libxcb1-dev libfontconfig-dev mesa-vulkan-drivers
```

macOS and Windows need nothing extra.

Quit with `ctrl-q` (`cmd-q` on macOS).
