# Ion

Lightweight Rust + GPUI IDE (Windows 11 first). Resource efficiency is a core goal: see `docs/architecture.md` (threading rules, memory) and `docs/performance.md` before touching background work, watchers or rendering.

## Crate map (`crates/<folder>` → package `ion_<folder>`)

UI (GPUI): `ion` (binary, keymap, window) → `workspace` (panes, tabs, sidebar, palette, git panel, SSH/remote windows, persistence) → `editor` (editor view, text element, git gutter), `file_tree`, `terminal` (PTY + alacritty view), `ui`, `settings`, `theme`.

Logic (no GPUI, CI-enforced): `text` (rope buffer, selections, undo), `project` (worktree, file index, watcher, search; `remote` feature), `syntax` (tree-sitter), `fuzzy`, `git` (git CLI wrapper), `bridge` (MCP server for `/ide`), `remote` (SSH auth, server deploy, RPC mux), `remote_protocol` (wire format), `server` (`ion-server` Linux binary, built without GPUI/SSH).

Dependencies point down only. Never add GPUI to a logic crate. Shared dep versions and lints live only in the root `Cargo.toml`.

## Commands

Prefer the narrowest command that answers the question. Logic crates don't build GPUI, so `-p` on them is fast.

| Need | Command |
|---|---|
| Type-check one crate | `cargo check -q -p ion_<crate>` |
| Test one crate | `cargo test -q -p ion_<crate>` |
| Everything compiles | `cargo check -q --workspace --all-targets` |
| Lint (CI-equivalent) | `cargo lint` (clippy, `-D warnings`) |
| All tests | `cargo t` |
| Format | `cargo fmt --all` |
| Run the app | `cargo dev` (GUI; only when asked) |
| Server protocol test | `cargo build -p ion_server` then `python scripts/test-server.py target/debug/ion-server.exe` |
| SSH integration | `python scripts/test-ssh.py` (needs paramiko; slow, only for remote changes) |

Keep output small:
- Always pass `-q` to cargo check/test/build. On failure read only the errors: pipe through `2>&1 | grep -E -A5 '^(error|warning)' | head -80`, or `| tail -40` for test failures.
- Iterate with per-crate check/test; run `cargo lint` and workspace-wide tests once at the end, not after each edit.
- Never run `cargo clean` or `cargo build --release` unless asked (full GPUI rebuild takes minutes).
- Remote projects use the system OpenSSH client (`ssh`); no SSH/OpenSSL build step.

Before finishing a change: `cargo fmt --all`, `cargo lint`, tests for the affected crates (and `cargo t` for cross-crate changes).

## Conventions

- No `mod.rs`; use `foo.rs` next to `foo/`.
- Unit tests next to the code in logic crates; integration tests in `crates/<c>/tests/`.
- `unsafe_code` is denied. Sole exception: `ScanningPty::register` in `crates/terminal/src/pty.rs` (forwards alacritty's unsafe trait method). No `dbg!`, `todo!`, `println!` (clippy warns).
- One worker pool (GPUI's executor): no tokio/rayon, no polling or idle timers; background jobs take snapshots, not locks.
- Commit messages in imperative mood ("Add file watcher").

## Docs

`docs/architecture.md`, `docs/development.md`, `docs/performance.md`, `docs/remote-development.md`, `docs/roadmap.md`, `docs/vision.md`. Read the relevant one instead of re-deriving design from code.
