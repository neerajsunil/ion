# Changelog

All notable changes to Ion are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Ion uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Project search (Ctrl+Shift+F) shows results as they're found instead of
  after the whole project is read, and looks first where matches are most
  likely: open tabs, then recent, agent-changed and Git-changed files, then
  source code, then other text files, then everything else. It also searches
  unsaved changes in open tabs. On the Linux kernel (96k files) the first
  result appears in milliseconds instead of after about 1.7 seconds.
- The status bar shows what Ion is doing while a project opens: indexing
  files (with a running count for local projects) and reading Git status.
- Go to File (Ctrl+P) remembers the files you use in each project across
  restarts and ranks the ones you use most and most recently first. Words
  separated by spaces match in any order (`e1000 main` finds
  `e1000/e1000_main.c`), and typing more letters only re-checks the files
  that already matched, so each keystroke stays fast in huge projects.

- A new Settings page with a side menu and a search box that looks through
  every setting and shortcut.
- Change any keyboard shortcut: click it and press new keys, or remove it.
  Reset one or all of them anytime (Ctrl+K Ctrl+S).
- New settings: line spacing, relative line numbers or none, cursor style,
  highlight the current line, indent with tabs or spaces, turn off bracket
  closing, trim trailing whitespace and add a final newline on save, terminal
  copy on select, sidebar on the right, and whether to reopen your last
  project.
- Choose which shells and agents appear in the terminal **+** menu, and pick
  the default shell from a dropdown.
- More shortcuts: Ctrl+J toggles the terminal, Ctrl+PageDown / Ctrl+PageUp
  switch tabs, and Ctrl+K Ctrl+W closes all tabs in a pane.
- Dragging a tab shows where it will land. Split dividers are thinner and
  easier to grab, and on a Mac double-clicking the title bar zooms the window.
- macOS on Apple Silicon: native menu bar, title bar laid out around the
  window buttons, desktop notifications, an `Ion.app` bundle in releases, and
  your login shell's `PATH` when started from Finder or the Dock.
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
  from `~/.ion/server`. Server release 0.4.0 (protocol 4).
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
- Codex `/ide` integration: type `/ide` in Codex and it sees Ion's open
  file, selection and open tabs.
- In remote projects, Claude Code's and Codex's `/ide` work from agents
  running on the server, carried over the project's SSH connection (no port
  forwarding needed). Protocol 4.
- Go to Symbol in File (Ctrl+Shift+O, or `@` in the palette), format on
  save with the language's usual formatter, and auto-indent on Enter.
- Send to Agent (Ctrl+Alt+K), drop files onto a terminal, paste images into
  one, and desktop notifications when a hidden agent finishes or needs input.
- Revert any hunk from a diff or the gutter, and Discard All.
- Problems (Ctrl+Shift+M): errors and warnings read from build and test
  output, underlined in the editor, and sent to an agent in one click. They
  open in a tab in the bottom panel that can be dragged to any pane, like a
  terminal.
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
- Remote projects use much less bandwidth (protocol 3): after the first
  load, the file list sends only added and removed paths instead of every
  path on each change, and file contents, file lists and command output of
  4 KiB or more are LZ4-compressed (about 8× smaller for file lists).
  `ION_REMOTE_TRACE=1` logs each remote request's size and time to stderr.
- Remote projects on a Mac: open a folder over SSH on an Apple Silicon Mac,
  with files, search, Git, live updates and terminals, as on Linux.
- Go to File (Ctrl+P) lists recently used and agent-changed files first
  (labeled "changed by agent" or "open") and ranks them ahead of other
  equally good matches. Paste `src/a.rs:12:5`, `a.rs(12,5)` or an absolute
  path in the project to open it at that line.
- Less background work when files change. Creating, deleting or renaming
  files updates the file list in place instead of re-reading the whole
  project (9 ms instead of 99 ms on a 27k-file project, with no disk reads).
  The watcher skips everything the file list skips (`.gitignore` in every
  folder, `.git/info/exclude` and the global excludes file), so builds in
  ignored folders cost nothing; `.gitignore` now also applies outside Git
  repositories. Saving a file whose Git status didn't change no longer
  redraws the tree and Git panel.
- Go to File matches large projects (over 20k files) in the background, so
  typing never waits on it, and project search is about a third faster on
  files without a match. Change markers copy less text on each re-diff and
  are skipped for files over 8 MB.

### Fixed

- In remote projects, files an agent changed weren't noticed unless they
  were open: the check for which changed paths are files looked at the local
  disk. It now asks the server, in one request per batch of changes.
- In remote projects, the terminal **+** menu, the command palette, the
  Agents view and Settings listed this machine's shells and agents instead of
  the server's. They now list and run the server's.
- Toggle Terminal focused the bottom panel instead of hiding it when the
  panel was showing but not focused.
- Deleting to the Recycle Bin crashed Ion.
- The file tree stopped highlighting the open file.
