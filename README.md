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

License: GPL-3.0-or-later, matching the original project.
