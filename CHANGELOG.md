# Changelog

All notable changes to Ion are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Ion uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Workspace and crate structure, project docs and CI.
- App icon: an ionizing atom (flat, ink and orange), shown in the title bar,
  start page, taskbar and on `ion.exe`.
- SSH remote development, rebuilt: one login per connection shared by files,
  search, Git, the watcher and every terminal; `~/.ssh/config` hosts, users,
  ports, keys and ProxyJump; password, key-passphrase and one-time-code
  prompts in the dialog; keepalives with automatic reconnect and a status bar
  indicator; recent remote projects; Open Folder browses the server in remote
  windows; Tab completion of remote folders.
- Remote servers update themselves: a mismatched server is replaced with the
  matching release (downloaded only when needed) and old versions are removed
  from `~/.ion/server`. Server release 0.2.0 (protocol 2).
- Editor: toggle comment (Ctrl+/), move, duplicate and delete lines, indent
  and outdent selections (Tab / Shift+Tab), auto-closing brackets and quotes.
- Go to line (Ctrl+G), reopen closed tab (Ctrl+Shift+T), Close All tabs, and
  zoom (Ctrl+= / Ctrl+- / Ctrl+0).
- Git: fetch, pull, push, and Publish Branch / Sync buttons.
- Ctrl+click file references in terminal output to open them.
- Tab and Shift+Tab move between fields in every dialog.
- Open a folder (Ctrl+O or `ion <path>`) and browse it in the file tree.
- Editor with tabs: editing, selection, clipboard, undo/redo, save (Ctrl+S).
- Prompts before closing tabs or the window with unsaved changes.
- Integrated terminal (Ctrl+`): PowerShell 7 by default, tabs (Ctrl+Shift+`),
  resizable panel, 256-color and truecolor, mouse selection, scrollback,
  clipboard, and a badge when a background terminal rings its bell.
- Syntax highlighting (tree-sitter) for Rust, JavaScript/JSX, TypeScript/TSX,
  Python, Go, C, C++, C#, Java, JSON, HTML, CSS, Shell, TOML, YAML, Markdown,
  PHP, Ruby, Lua and Swift. Parsing is incremental and runs in the background.
- Live reload: files changed on disk (by an agent, git, a formatter) update in
  place, undoable with Ctrl+Z. Unsaved edits are never overwritten without asking.
- File tree updates automatically when files are added, removed or renamed.
- Quick open (Ctrl+P), find and replace (Ctrl+F / Ctrl+H) and project-wide
  search (Ctrl+Shift+F), all respecting `.gitignore`.
- File tree context menu: new file/folder, rename, delete to Recycle Bin, copy
  path, reveal in File Explorer.
- New file (Ctrl+N) and Save As (Ctrl+Shift+S).
- Restores the last folder, open files and terminal on startup; recent folders
  on the welcome screen.
- Git (Ctrl+Shift+G): changes list with stage, unstage and discard, commit box
  (Ctrl+Enter), commit history, and diff tabs for files and commits
  (double-click a line to open it in the file).
- Changed-line markers in the editor gutter and change colors in the file tree.
- Blame for the cursor line in the status bar; click it to open the commit.
- Current branch in the status bar; click it to switch or create a branch.
- Tiling panes: split the editor area or the terminal dock (Ctrl+\,
  Ctrl+Shift+\), drag tabs between panes or onto an edge to split, resize
  with dividers, maximize a pane (Ctrl+Shift+Enter). Terminals are tabs that
  can live anywhere, and can be renamed.
- Resizable sidebar, with a toggle in the status bar.
- The pane layout, terminals and sidebar width are restored on startup.
- File tree: multi-select (Ctrl/Shift+click), keyboard navigation, Delete and
  F2, and more context menu actions (open to the side, open in terminal,
  cut/copy/paste, duplicate, collapse all).
- Git view: select several files to stage, unstage or discard them together,
  plus a right-click menu.

- Redesigned interface: a custom title bar with the Ion menu, project
  switcher, search box and panel toggles; icon tabs for Explorer, Search and
  Source Control; file-type icons on tabs; a terminal toolbar with new
  terminal, shell picker, split, maximize and hide; tooltips with shortcuts
  on every button.
- Command palette (Ctrl+Shift+P): every command by name, with its shortcut.
- Settings (Ctrl+,): dark, light or system theme, interface, editor and
  terminal text sizes, editor font, tab size, auto save, default shell, and
  git blame and gutter toggles. Saved to settings.json, which can also be
  edited by hand.
- New terminals can use any installed shell (PowerShell 7, Windows
  PowerShell, Command Prompt, Git Bash, WSL).
- Multi-line commit messages.
- Claude Code `/ide` integration: agents open files and diffs in Ion, read
  the selection and diagnostics, and propose edits you accept or reject.
- Send to Agent (Ctrl+Alt+K), drop files onto a terminal, paste images into
  one, and desktop notifications when a hidden agent finishes or needs input.
- Revert any hunk from a diff or the gutter, and Discard All.
- Problems (Ctrl+Shift+M): errors and warnings read from build and test
  output, underlined in the editor, and sent to an agent in one click.
- Run button: `package.json` scripts, Cargo, Go and Makefile targets, each in
  its own named terminal. Ctrl+click `localhost:PORT` links in output.
- Side-by-side or inline diffs.
- Multiple cursors (Ctrl+D, Ctrl+Shift+L, Alt+click) and code folding.
- Markdown preview (Ctrl+Shift+V), image viewer and word wrap (Alt+Z).

### Changed

- SSH uses the system OpenSSH client instead of a bundled SSH library: your
  `~/.ssh/config` (including `ProxyJump` and `ProxyCommand`), agent and
  `known_hosts` apply as they do for `ssh`, and building Ion no longer needs
  OpenSSL or Perl. On Windows, Ion needs the OpenSSH Client optional feature.

### Fixed

- Deleting to the Recycle Bin crashed Ion.
- The file tree stopped highlighting the open file.
