# DOOM Fire in Rust

Rust port of the [DOOM-fire-zig](https://github.com/const-void/DOOM-fire-zig) terminal stress test.

## Install

Install the latest published release from [crates.io](https://crates.io/crates/doom-fire-rs):

```sh
cargo install doom-fire-rs
```

Then run:

```sh
doom-fire-rs
```

## Run From Source

Requires Rust 1.100 or newer.

```sh
cargo run --release
```

The binary uses Rust's standard library, `libc` for portable Unix keyboard mode
bindings, and small Unix/Windows FFI shims. Like the Zig original, it switches to
the alternate screen, shows a terminal capability preview, then renders the Doom
fire animation until the process is stopped.

Both stdin and stdout must be terminals. Press Return to advance past the
preview, or `q` followed by Return to quit at a prompt. Stop the animation with
`q` (no Return needed) or Ctrl-C; on Unix, SIGTERM also restores the terminal
before exiting.
On Unix, Ctrl-Z restores the terminal before suspending; use `fg` to resume and
redraw. Shutdown and suspension also work when Ctrl-S has paused terminal output.
Starting the application as a background job leaves the screen untouched until
you bring it to the foreground with `fg`.

The animation adapts to resizing and reserves the last row for frame statistics.
The preview includes indexed colors, a truecolor gradient, text styles, block and
shading glyphs, and the selected renderer's output resolution. These are visual
checks, not automatic capability detection.
It requires at least 1 column and 2 rows; invalid dimensions or sizes above
1,000,000 terminal cells are rejected. A larger terminal (at least 120×22) is
recommended for the capability preview.

Unix terminal bindings cover Linux, Android, Apple platforms, FreeBSD, OpenBSD,
NetBSD, and DragonFly BSD. Other Unix targets require explicit ABI bindings.

## Kitty Graphics Benchmark

Run inside Kitty or another terminal that supports the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/):

```sh
cargo run --release -- --kitty
# Or, after installation:
doom-fire-rs --kitty
```

`--kitty` shows the same terminal capability preview; press Return to start.
It sends uncompressed RGB frames as chunked base64 over the terminal connection,
keeping one image and updating its root frame in place. This keeps the previous
frame visible while the next one is being uploaded. The stats row stays separate.

By default, the simulation has four times as many pixels on each axis as ANSI
output (16 times as many pixels total). Increase or decrease the resolution with:

```sh
cargo run --release -- --kitty --kitty-scale 8
```

`--kitty-scale` accepts 1 through 16; 1 uses the ANSI simulation resolution.
Cooling is adjusted to keep a similar flame height at higher resolutions. Images
are limited to 16,777,216 pixels. Colors use a fixed xterm palette rather than the
terminal's customized ANSI colors.

The benchmark runs without a frame-rate limit. The status bar reports:

- `res`: output width and height in pixels (the logical half-block grid for ANSI).
- `fps` and `ms/f`: completed frames per second and average elapsed milliseconds
  per frame, including loop overhead.
- `MPix/s`: millions of output pixels per second.
- `MiB/s`: bytes successfully written, including protocol and status text, in
  binary mebibytes per second.
- `sim/enc/write`: average milliseconds spent simulating, encoding the frame and
  status, and writing/flushing output. Write time includes terminal backpressure.
- `frame`: minimum, average, and maximum encoded frame sizes before status text,
  when the window is wide enough. These are not resident memory measurements.

Statistics are cumulative since animation startup or the last resize, exclude
Unix suspension time, and appear on the following frame. They measure producer
throughput, not the terminal's displayed refresh rate. Narrow windows clip the
status without wrapping. Keep terminal size and `--kitty-scale` fixed when
comparing terminals. Use `--kitty-scale 1` when comparing with the ANSI renderer
at the same simulation resolution.

Resizing and Unix suspend/resume work in both modes. Exiting or suspending removes
the Kitty image and restores keyboard settings. `q` or Ctrl-C stops the benchmark.
Protocol support is assumed when `--kitty` is selected; there is no automatic
fallback on unsupported terminals.
Use `--help` to list options.

## Checks

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

On Unix, `cargo test` also runs the Rust terminal integration tests. These use
the same `libc` bindings as the application. The suspend/resume test uses Bash
and skips its coverage if Bash is unavailable.
To run just the terminal tests:

```sh
cargo test --locked --test terminal
```

License: GPL-3.0-or-later, matching the original project.
