# DOOM Fire RS

Dependency-free Rust port of the parent `DOOM-fire-zig` terminal stress test.

```sh
cargo run --release
```

The binary uses only Rust's standard library plus small Unix/Windows FFI shims
for terminal size and console setup. Like the Zig original, it switches to the
alternate screen, shows a terminal capability preview, then renders the Doom
fire animation until the process is stopped.

License: GPL-3.0-or-later, matching the original project.
# doom-fire-rs
