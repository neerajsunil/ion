# Architecture

Ion is a Cargo workspace of small crates built on [GPUI](https://gpui.rs), the
GPU-accelerated UI framework from Zed.

## Crates

| Crate (folder) | Package | Role | Uses GPUI |
|---|---|---|---|
| `crates/ion` | `ion` | Binary: startup, keymap, window | yes |
| `crates/workspace` | `ion_workspace` | Window layout: pane tree (editor area and terminal dock), tabs, sidebar, status bar | yes |
| `crates/editor` | `ion_editor` | Code editor view and custom text element | yes |
| `crates/file_tree` | `ion_file_tree` | Sidebar file tree | yes |
| `crates/ui` | `ion_ui` | UI kit: icons, buttons, tooltips, menus | yes |
| `crates/settings` | `ion_settings` | settings.json, applied live | yes |
| `crates/terminal` | `ion_terminal` | Integrated terminal: PTY, emulation, view | yes |
| `crates/theme` | `ion_theme` | Colors, fonts, metrics | yes |
| `crates/project` | `ion_project` | Worktree, file index, file system watcher | **no** |
| `crates/text` | `ion_text` | Rope buffer, selections, undo history | **no** |
| `crates/syntax` | `ion_syntax` | Language detection, tree-sitter parsing, highlight queries | **no** |
| `crates/fuzzy` | `ion_fuzzy` | File-name matching and ranking | **no** |
| `crates/git` | `ion_git` | Git status, diffs, blame, history and commits via the `git` CLI | **no** |
| `crates/bridge` | `ion_bridge` | Local MCP server (WebSocket, JSON-RPC) for harness IDE integrations such as Claude Code's `/ide`, and the socket (named pipe on Windows) Codex's `/ide` reads context from | **no** |
| `crates/remote` | `ion_remote` | System `ssh` process, server deployment and the multiplexed protocol | **no** |
| `crates/remote_protocol` | `ion_remote_protocol` | Versioned messages and bounded stdio framing | **no** |
| `crates/server` | `ion_server` | Remote server for Linux (static) and macOS: file operations, search and native watching | **no** |

Planned for later phases (see [roadmap.md](roadmap.md)): `review` and
`bridge`. They are created when their phase starts, not before.

### Dependency rules

```
ion ─▶ workspace ─▶ editor ─────▶ text, project, syntax, git
                 ├▶ file_tree ──▶ project
                 ├▶ ui, settings
                 ├▶ terminal ───▶ alacritty_terminal
                 ├▶ git
                 └▶ theme
```

- Dependencies only point downwards. UI crates may depend on logic crates, never
  the reverse.
- `text`, `project`, `syntax`, `fuzzy` and `git` must never depend on GPUI. CI enforces this.
  That keeps the core logic fast to compile, easy to test and reusable.
- Shared dependency versions and lint settings live only in the root
  `Cargo.toml` (`[workspace.dependencies]`, `[workspace.lints]`).

The server uses `project` without its `remote` feature. Its dependency graph
contains neither GPUI nor any SSH code: it runs behind the host's SSH daemon.
The client embeds packaged Linux assets when available (macOS servers are
downloaded from the matching release when needed) and uploads the matching
version over the SSH connection. Each remote project has one system `ssh`
process (jump hosts come from the user's SSH config) whose stdio becomes
`ion-server --stdio`. File requests, searches, the watcher, Git and terminals
are streams multiplexed over it, and the server spawns commands and ptys. The
server never listens on a port.

### Conventions

- Folder names are short `snake_case` (`crates/editor`), and package names are
  prefixed (`ion_editor`). Inside the workspace, crates refer to each other by
  the short name (`editor.workspace = true`).
- No `mod.rs` files: use `foo.rs` alongside a `foo/` folder.
- Logic crates get unit tests next to the code. Integration tests go in the
  crate's `tests/` folder.

## Threading model

| Thread | Role | When idle |
|---|---|---|
| Main (UI) | Owns all state, handles input, renders frames | Sleeps on OS events |
| GPUI background pool | The **only** worker pool: indexing, search, file I/O | Threads parked |
| File system watcher | One thread receiving OS change notifications (one more watches `.git`) | Blocked in the kernel |
| Terminal I/O (one per terminal) | Reads the PTY and parses output (`alacritty_terminal`) | Blocked on the PTY |
| SSH (four per remote connection) | The `ssh` process plus a reader (dispatches frames to requests and streams), a writer (sends queued frames), a stderr collector and a deadline ticker | Blocked on pipes or a condvar; `ssh` itself sends keepalives |
| Agent bridge | One thread accepting `/ide` connections, plus one per connected agent reading its socket; one accepting Codex's context requests, plus one per request; per remote project, one per agent connection forwarded from the server | Blocked in `accept` or on the socket |
| Burst workers | Parallel directory walk (indexing) and project search | Exist only while working |

Rules:

1. **One pool.** No tokio runtime or rayon pool next to GPUI's executor. Every
   extra runtime adds threads that sit around idle. Bursty work that wants
   every core (indexing, project search) uses scoped threads that exit as soon
   as the job is done. Project search uses at most 8: past that, reading files
   gets no faster, because the OS serializes much of the work.
2. **Snapshots, not locks.** Background jobs get immutable snapshots (cloning a
   rope is O(1)) and send results back to the UI thread. No locks are held
   across threads.
3. **Burst, then sleep.** Heavy work (for example a parallel directory walk)
   uses every core and then releases them. Long jobs are cancelled when their
   result is no longer needed, for example when the finder query changes.
4. **No idle timers.** No polling, no periodic rescans, and cursor blink stops
   after a few seconds without input. Changes come from OS events.

## Memory

- Only the visible lines are shaped and laid out.
- Directories in the file tree load when they're expanded.
- The file index is one string arena plus a 16-byte entry per file, not one
  `PathBuf` per file. Lowercase copies are stored only for paths with
  uppercase letters.
- Closing a tab drops its buffer, its diff base and its blame.
- Syntax trees (many times the file's size) exist only for tabs on screen:
  a tab parses on its first render, and drops its tree when it's no longer
  its pane's active tab. Restored sessions parse only the tabs they show.
- A tab hidden for 30 seconds keeps its text and its HEAD text (git gutter)
  lz4-compressed (`text::Packed`, about 2.7x smaller for source). Reads
  decompress on demand; showing the tab decompresses it. Tabs with undo
  history keep their text as is, since the undo snapshots share it.
- Image tabs decode their image only while shown.
- The file tree keeps listings only for expanded folders.
- Nothing is cached "in case": hold data while it's visible or in use, and
  rebuild it when it's needed again.

## Platforms

| Target | CPU baseline | Status |
|---|---|---|
| `x86_64-pc-windows-msvc` | `x86-64-v2` | Supported |
| `aarch64-apple-darwin` | `apple-m1` | Supported |
| `aarch64-pc-windows-msvc` | default | Possible |

`x86-64-v2` (SSE4.2, POPCNT) is required by Windows 11 24H2. `x86-64-v3` (AVX2)
is not used, because some Windows 11 certified Celeron and Pentium CPUs lack
it. CPU flags are set in `.cargo/config.toml`.
