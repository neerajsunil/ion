# Development

## Setup

1. Install [Rust](https://rustup.rs). The exact toolchain version is pinned in
   `rust-toolchain.toml` and installed automatically on first build.
2. Platform tools:
   - **Windows 11:** Visual Studio 2022 (or Build Tools) with the **Desktop
     development with C++** workload. It provides the Windows SDK.
   - **macOS (Apple Silicon):** the Command Line Tools
     (`xcode-select --install`). GPUI compiles its Metal shaders when Ion
     starts (`runtime_shaders`), so full Xcode isn't needed.
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

## macOS app bundle

`scripts/bundle-macos.sh` builds a release `Ion.app` (icon from
`crates/ion/resources/ion.icns`, metadata from `Info.plist`) and zips it into
`dist/`. It signs ad hoc unless `ION_SIGN_IDENTITY` names a Developer ID
identity. Regenerate the icons with `crates/ion/resources/make_icon.py`.

An app started from Finder or the Dock gets launchd's minimal `PATH`, so Ion
reads `PATH` from your login shell and restarts itself with it
(`crates/ion/src/login_path.rs`). Started from a terminal, it keeps that
terminal's environment.
