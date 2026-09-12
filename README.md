# DOOM Fire in Rust

Dependency-free Rust port of the [DOOM-fire-zig](https://github.com/const-void/DOOM-fire-zig) terminal stress test.

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

```sh
cargo run --release
```

The binary uses only Rust's standard library plus small Unix/Windows FFI shims
for terminal size and console setup. Like the Zig original, it switches to the
alternate screen, shows a terminal capability preview, then renders the Doom
fire animation until the process is stopped.

Both stdin and stdout must be terminals. Press Return to advance past the
preview, or `q` followed by Return to quit at a prompt. Stop the animation with
Ctrl-C; on Unix, SIGTERM also restores the terminal before exiting.
On Unix, Ctrl-Z restores the terminal before suspending; use `fg` to resume and
redraw. Shutdown and suspension also work when Ctrl-S has paused terminal output.
Starting the application as a background job leaves the screen untouched until
you bring it to the foreground with `fg`.

The animation adapts to resizing and reserves the last row for frame statistics.
It requires at least 1 column and 2 rows; invalid dimensions or sizes above
1,000,000 terminal cells are rejected. A larger terminal (at least 120×22) is
recommended for the capability preview.

Unix terminal bindings cover Linux, Android, Apple platforms, FreeBSD, OpenBSD,
NetBSD, and DragonFly BSD. Other Unix targets require explicit ABI bindings.

## Checks

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

On Unix, `cargo test` also runs the Rust terminal integration tests. These use
`libc` as a development dependency; the application has no dependencies. The
suspend/resume test uses Bash and skips its coverage if Bash is unavailable.
To run just the terminal tests:

```sh
cargo test --locked --test terminal
```

License: GPL-3.0-or-later, matching the original project.
