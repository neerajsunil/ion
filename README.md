<div align="center">

<img src="crates/ion/resources/ion.png" width="112" alt="Ion logo: an ionizing atom">

# Ion

### The lightweight IDE for coding with AI agents

Your agent (Claude Code, Codex, Gemini CLI or whatever you use) writes the code.<br>
Ion is where you watch it, review it and steer it. Native, fast, and out of your way.

[![CI](https://github.com/neerajsunil/ion/actions/workflows/ci.yml/badge.svg)](https://github.com/neerajsunil/ion/actions/workflows/ci.yml)
[![License: MIT or Apache-2.0](https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-blue)](#license)
[![Rust](https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Windows 11](https://img.shields.io/badge/platform-Windows%2011-0078d4?logo=windows&logoColor=white)](#install)
[![Download](https://img.shields.io/github/v/release/neerajsunil/ion?filter=v*&label=download&logo=github)](https://github.com/neerajsunil/ion/releases/latest)
[![GitHub stars](https://img.shields.io/github/stars/neerajsunil/ion?style=flat&logo=github)](https://github.com/neerajsunil/ion/stargazers)
[![Last commit](https://img.shields.io/github/last-commit/neerajsunil/ion)](https://github.com/neerajsunil/ion/commits/main)

[Features](#features) · [Agents](#works-with-your-agent) · [Stats](#by-the-numbers) · [Install](#install) · [Shortcuts](docs/keybindings.md) · [Roadmap](docs/roadmap.md)

<img src="docs/images/hero.png" alt="Ion IDE showing a Rust file with multiple cursors, the file tree, and an integrated terminal running cargo test" width="100%">

</div>

## Why Ion

More and more code is written by agents running in a terminal. Your job moves
from typing code to directing and reviewing it, and most editors weren't built
for that. Cursor, Windsurf and Zed push their own built-in agent. VS Code works
with terminal agents, but it's heavy, and the terminal and change review come
second.

Ion is an open-source code editor built for **AI-assisted coding and vibe
coding**, made for that workflow:

- **Bring your own agent.** No built-in AI, no chat panel, no account, no
  second subscription. Ion works with any terminal agent and plugs into the
  ones that offer IDE integrations.
- **Terminal first.** A GPU-rendered terminal where your agent lives, with
  tabs, splits and badges when an agent finishes or needs you.
- **Review everything.** Every file the agent touched, as diffs. Revert a hunk
  or discard it all.
- **Native and light.** Written in Rust on [GPUI](https://www.gpui.rs) (the UI
  framework behind Zed). No Electron, no web view, no background polling, so
  your CPU goes to agents, builds and tests.

## Features

### Review what your agent changed

Open files reload live as the agent edits them. The file tree and gutter mark
what changed, and the Source Control view lists every modified file. Read the
diffs inline or side by side, revert a single hunk, or discard everything.

<img src="docs/images/side-by-side-diff.png" alt="Side-by-side diff of an agent's changes in Ion's Source Control view, with Revert buttons on each hunk" width="100%">

### Problems, sent straight back to the agent

Ion reads errors and warnings from build and test output in your terminals
(rustc and cargo, tsc, ESLint, gcc and clang, Go, Python, mypy, ruff, MSBuild
and more). They're underlined in the editor and listed in the Problems view,
and **Send to Agent** types them into your agent's prompt. No language server
setup needed.

<img src="docs/images/problems.png" alt="Ion's Problems view listing a Rust compile error, with a wavy underline and inline message on the failing line" width="100%">

### One-click Run

The ▶ button finds your project's tasks: `package.json` scripts (npm, pnpm,
yarn or bun, picked from your lockfile), Cargo, Go and Makefile targets. Each
runs in its own named terminal, and Ctrl+click opens `localhost:3000`-style
links from the output in your browser.

<img src="docs/images/run-tasks.png" alt="Ion's Run menu listing npm scripts, cargo commands and make targets detected from the project" width="100%">

### Docs, images and plans

A live Markdown preview beside the file (handy for an agent's `PLAN.md`), an
image viewer, and word wrap for prose.

<img src="docs/images/markdown-preview.png" alt="A Markdown file and its live preview side by side in Ion" width="100%">

### And the editor you'd expect

- Multiple cursors (Ctrl+D, Ctrl+Shift+L, Alt+click) and code folding
- Syntax highlighting for 20 languages with incremental tree-sitter parsing
- Quick open (Ctrl+P), command palette, find and replace, and project-wide search, all respecting `.gitignore`
- Tiling panes: split editors and terminals, and drag any tab anywhere
- Git: stage, commit, branches, blame, history, push, pull and sync
- SSH remote development over your own OpenSSH config ([docs](docs/remote-development.md))
- Dark, light and system themes, and session restore

## Works with your agent

| | |
|---|---|
| **Claude Code** | Ion speaks the `/ide` protocol. Agents started in Ion's terminals connect automatically; elsewhere, run `/ide`. Claude Code can then open files and diffs in Ion, read your selection and the Problems list, and propose edits you accept or reject in a diff tab. |
| **Codex, Gemini CLI, any CLI agent** | Run it in an Ion terminal (Claude Code and Codex launch from the terminal menu). Send a selection or `file:line` reference with **Ctrl+Alt+K**, drag files onto the terminal, or paste images. |
| **Every agent** | The Agents view (Ctrl+Shift+A) shows each agent's state and the files it changed. **Follow mode** keeps a live diff of the file it's editing. Get a desktop notification when a hidden agent finishes or needs input. |

## By the numbers

| | |
|---|---|
| **0** | built-in AI, accounts or required sign-ins |
| **0** | Electron, Node.js or web views |
| **~0%** | idle CPU target: no polling, no timers while nothing happens ([budgets](docs/performance.md)) |
| **36k** | lines of Rust, across 17 small crates |
| **160+** | tests, with clippy at `-D warnings` in CI |
| **20** | languages highlighted by tree-sitter |
| **1** | worker pool for background work (GPUI's executor): no Tokio, no Rayon |

## Install

**[Download the latest release](https://github.com/neerajsunil/ion/releases/latest)**
for Windows 11 (x64): extract the zip and run `ion.exe`. It's a single
portable file with no installer. macOS on Apple Silicon is planned.

Ion is in early development and builds aren't code-signed yet, so SmartScreen
may warn on first launch (**More info → Run anyway**).

### Build from source

1. Install [Rust](https://rustup.rs). The toolchain version is pinned and
   installs on the first build.
2. Install Visual Studio 2022 or the Build Tools with the **Desktop
   development with C++** workload.
3. Clone and run:

```bash
git clone https://github.com/neerajsunil/ion.git
```

```bash
cd ion
```

```bash
cargo dev
```

The first build compiles GPUI and takes a few minutes; after that only Ion's
own crates rebuild. See [docs/development.md](docs/development.md) for the fast
dev loop.

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| Ctrl+P | Go to file |
| Ctrl+Shift+P | Command palette |
| Ctrl+\` | Show or hide the terminal |
| Ctrl+Shift+G | Source Control |
| Ctrl+Shift+A | Agents view |
| Ctrl+Shift+M | Problems |
| Ctrl+Alt+K | Send selection to agent |
| Ctrl+D | Add the next occurrence as a cursor |
| Ctrl+Shift+[ / ] | Fold / unfold |
| Ctrl+Shift+V | Markdown preview |

The full list is in [docs/keybindings.md](docs/keybindings.md).

## How it's built

Ion is a Cargo workspace of small crates. The UI crates use GPUI; the logic
crates (buffer, project, git, syntax, the agent bridge) don't depend on GPUI,
and CI enforces it. That keeps rebuilds fast and the core testable.

```
ion (app) ─► workspace ─► editor, file_tree, terminal, ui, settings, theme
                 │
                 └─► text, project, syntax, git, fuzzy, bridge, remote   (no GPUI)
```

Read more in [architecture](docs/architecture.md), [vision](docs/vision.md)
and [performance](docs/performance.md).

## Roadmap

Most of the foundation, terminal, review and agent bridge work is done. Next
up are checkpoints (snapshot before a task, roll back after), staging single
hunks and macOS. See the [roadmap](docs/roadmap.md) and
[changelog](CHANGELOG.md).

## Star history

<a href="https://star-history.com/#neerajsunil/ion&Date">
  <img src="https://api.star-history.com/svg?repos=neerajsunil/ion&type=Date" alt="Star history chart for neerajsunil/ion" width="600">
</a>

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first, and
please keep changes in line with the [vision](docs/vision.md): every feature
has to earn its place.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit
for inclusion in Ion shall be dual licensed as above, without any additional
terms or conditions.
