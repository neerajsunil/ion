# Development

## Setup (Windows 11)

1. Install [Rust](https://rustup.rs). The exact toolchain version is pinned in
   `rust-toolchain.toml` and installed automatically on first build.
2. Install Visual Studio 2022 (or Build Tools) with the **Desktop development
   with C++** workload. It provides the Windows SDK.
3. Optional: `cargo install --locked bacon` for watch mode.

```bash
cargo dev
```

The first build compiles GPUI and takes a few minutes. After that, rebuilds only
touch Ion's own crates.

## Fast loop

| Command | What it does |
|---|---|
| `cargo dev` | Build and run Ion |
| `bacon` | Re-check on every save (fastest feedback) |
| `bacon run` | Rebuild and relaunch Ion on every save |
| `cargo t` | Run all tests |
| `cargo test -p ion_text` | Test one crate (logic crates don't build GPUI) |
| `cargo lint` | Clippy with warnings as errors, same as CI |

Builds never run tests. Tests only run when you ask for them, and in CI.

### Why rebuilds are fast

- **Small crates.** Changing the editor doesn't recompile the buffer, and vice versa.
- **Optimized dependencies, unoptimized Ion.** In the `dev` profile, GPUI and
  other dependencies are built with `opt-level = 3` once and cached, while Ion's
  crates compile with no optimization. The app stays smooth in development and
  rebuilds stay quick.
- **Minimal debug info.** `debug = "line-tables-only"` keeps panics and
  backtraces useful while cutting link time.
- **`rust-lld`.** Configured in `.cargo/config.toml`, and much faster than the
  default MSVC linker.

### Hot reload

Rust and GPUI have no built-in hot reload. Today the loop is `bacon run`:
save, an incremental rebuild of a few seconds, and Ion relaunches. Hot-patching
with [subsecond](https://crates.io/crates/subsecond) is something we may try once
there's real UI to iterate on.

## Profiles

| Profile | Use |
|---|---|
| `dev` (default) | Daily development |
| `release` | Shipping: fat LTO, `panic = "abort"`, stripped |
| `profiling` | Release speed plus symbols: `cargo build --profile profiling` |

## Before opening a PR

```bash
cargo fmt --all
cargo lint
cargo t
```
