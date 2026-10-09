# Roadmap

Each phase ships something usable on its own.

## Phase 0: Repository setup ✅

- [x] Cargo workspace and crate layout
- [x] Licenses, contributing guide, CI
- [x] Vision, architecture and roadmap docs
- [x] Fast development loop (see [development.md](development.md))

## Phase 1: Foundation

Open a project, view and edit files, find files by name.

- [x] Open a folder (dialog or command-line argument)
- [x] File tree sidebar
- [x] Editor: view, edit, save, undo/redo, clipboard, mouse selection
- [x] Tabs, with an unsaved-changes indicator
- [x] File finder (Ctrl+P) matching file names, exact matches first
- [x] File watcher: reload open files and update the tree when files change on disk
- [x] Find and replace in file (Ctrl+F / Ctrl+H)
- [x] Project-wide search (Ctrl+Shift+F)
- [x] File operations: new file/folder, rename, delete to Recycle Bin
- [x] New file (Ctrl+N) and Save As
- [x] Session restore and recent folders
- [x] Status bar: file path, cursor position

## Phase 2: Terminal

- [x] Integrated terminal (VT parsing via `alacritty_terminal`)
- [x] Terminal tabs
- [x] Terminal splits, and tiling panes for any tab
- [x] Attention badges when a terminal rings the bell
- [x] Attention badges for notification escape codes (OSC 9, OSC 777, OSC 99)
- [x] Progress on terminal tabs (OSC 9;4)

## Phase 3: Review

- [x] Changed-file markers in the file tree and changed-line markers in the gutter
- [x] Changes panel: every modified file as a diff
- [x] Commit history and blame for the cursor line
- [x] Revert any hunk, and discard all changes
- [x] Side-by-side or inline diffs
- [ ] Checkpoints: snapshot before a task, diff and roll back afterwards
- [x] Syntax highlighting (tree-sitter)

## Phase 4: Bridge

- [x] Send a selection or `file:line` reference to an agent (Send to Agent), drop files on a terminal, paste images into one
- [x] Clickable file paths and web addresses (`localhost:3000`) in terminal output
- [x] Launch installed agent harnesses (Claude Code, Codex) from the terminal menu and palette
- [x] Agents view: every agent and shell with its state and the files it changed
- [x] Follow mode: a live diff tab of the file an agent is editing, on its latest edit
- [x] Harness IDE integrations where available (Claude Code's and Codex's `/ide`)
- [x] Desktop notifications when a hidden agent finishes or needs input
- [x] Problems: errors and warnings read from build and test output, underlined in editors, sent to an agent
- [x] Run button: `package.json` scripts, Cargo, Go and Makefile targets in named terminals

## Phase 5: Polish

- [x] Git: stage, commit, discard, switch branches
- [x] Git: push, pull and fetch buttons
- [ ] Git: stage or revert individual hunks
- [x] Settings and themes (dark, light, system)
- [x] Command palette
- [x] Markdown preview, image viewer, word wrap
- [x] Multiple cursors (Ctrl+D, Alt+click) and code folding
- [x] macOS (Apple Silicon)
