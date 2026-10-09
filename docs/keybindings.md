# Keyboard shortcuts

Windows shortcuts are shown; on macOS use ⌘ where Windows uses Ctrl.

## Change any shortcut

Open **Settings → Keyboard Shortcuts** (Ctrl+K Ctrl+S). Search for a
command, click its shortcut and press the keys you want. Each changed
shortcut has a button to reset it, and **Reset All** puts everything back.
You can also remove a shortcut you keep pressing by accident.

Your shortcuts are saved in `settings.json` under `keybindings`, so you can
edit them there too. An empty value turns a shortcut off, and two-step
shortcuts are written with a space:

```json
"keybindings": {
  "workspace::ToggleTerminal": "ctrl-j",
  "workspace::CloseAllTabs": "ctrl-k ctrl-w",
  "editor::DuplicateLine": ""
}
```

## Finding things

Everything Ion can do is in the **Ion menu** (top left), in the **command
palette**, and as a tooltip on each button.

| Shortcut | Action |
|---|---|
| Ctrl+P | Go to file (or click the search box in the title bar). Recent and agent-changed files come first, then the files you use most; words match in any order; `path:line:column` opens at that line |
| Ctrl+Shift+P or F1 | Command palette: type `>` then a command name |
| Ctrl+G | Go to line (`line` or `line:column`) |
| Ctrl+Shift+O | Go to symbol in file (or type `@` in the palette) |
| Ctrl+, | Settings (type to search every setting) |
| Ctrl+K Ctrl+S | Keyboard shortcuts |
| Ctrl+= / Ctrl+- / Ctrl+0 | Zoom in / out / reset (all text sizes) |
| Ctrl+Q | Quit (asks once about unsaved changes in every window) |

In dialogs and forms, **Tab** and **Shift+Tab** move between fields.

## Files and project

| Shortcut | Action |
|---|---|
| Ctrl+O | Open folder |
| Ctrl+N | New file |
| Ctrl+S | Save (asks for a name if untitled) |
| Ctrl+Shift+S | Save as |
| Ctrl+W | Close tab |
| Ctrl+K Ctrl+W | Close all tabs in the pane |
| Ctrl+Shift+T | Reopen the last closed file |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+PageDown / Ctrl+PageUp | Next / previous tab (on a Mac, also ⌘⌥→ / ⌘⌥←) |
| Ctrl+B | Toggle sidebar (or drag its edge to resize; double-click the edge to reset) |
| Ctrl+Shift+E | Show files in sidebar |
| Ctrl+Shift+F | Search in project |

The app menu and command palette also offer **Remote: Connect to SSH…**,
**Remote: Disconnect SSH**, and **File: Refresh Project**. In a remote window,
Ctrl+O browses folders on the server. See
[remote development](remote-development.md) for logging in, SSH config and
reconnecting.

### File tree

Click a file to open it (the tree keeps focus, so the keys below work);
double-click to open it and jump into the editor. Ctrl+click adds to the
selection and Shift+click selects a range.

| Key | Action |
|---|---|
| ↑ / ↓ | Move the selection |
| → / ← | Expand / collapse a folder (or go to the parent) |
| Enter | Open the file, or toggle the folder |
| Delete | Move the selected items to the Recycle Bin |
| F2 | Rename |

Right-click for Open to the Side, New File, New Folder, Open in Terminal,
Cut, Copy, Paste, Duplicate, Copy Path, Copy Relative Path, Rename, Delete,
Reveal in File Explorer and (on the root) Collapse All Folders. With several
items selected the menu acts on all of them.

## Panes and layout

The editor area and the terminal dock below it can each be split into panes.
Any tab (file, diff or terminal) can be dragged to another pane's tab bar, or
onto a pane's edge to split it. While you drag, the part of the pane the tab
will take is shaded. Drag a divider to resize; double-click it to make the
panes equal. The layout is restored on the next start.

The sidebar can sit on the left or the right (**Settings → General**). On a
Mac, double-click the title bar to zoom the window.

| Shortcut | Action |
|---|---|
| Ctrl+\ | Split right |
| Ctrl+Shift+\ | Split down |
| Ctrl+Alt+Arrow | Focus the pane in that direction |
| Ctrl+Shift+Enter | Maximize the pane, or restore it |
| Ctrl+W | Close the tab (middle-click a tab works too) |

Each tab bar has **+** (new file or terminal) and **⋯** (split, maximize,
close pane). Right-click a tab to close it, close the others or all of them,
move it to a new split, or rename a terminal (double-click a terminal tab
works too).

## Git

| Shortcut | Action |
|---|---|
| Ctrl+Shift+G | Show the Git view |
| Ctrl+Shift+A | Show the Agents view |
| Ctrl+Shift+M | Toggle the Problems tab in the bottom panel (errors and warnings from terminal output) |
| Ctrl+Alt+K | Send the selection (or `file:line`) to the agent |
| Ctrl+Enter or Enter | Commit (in the message box) |

Click a changed file for its diff, a commit in History for its changes, the
branch in the status bar to switch branches, and the blame text in the status
bar to open that commit. Double-click a line in a diff to open the file there.
Hover a changed file for open, discard (↺), stage (+) and unstage (−).
Under the commit box, **Publish Branch** pushes a new branch, and **Sync**
(with ↓ behind / ↑ ahead counts) pulls and then pushes. **Git: Fetch**,
**Git: Pull** and **Git: Push** are in the command palette.
Ctrl+click and Shift+click select several files; the hover buttons and the
right-click menu (Stage, Unstage, Discard Changes, Open, Copy Paths) then act
on all of them.

## Find and replace

| Shortcut | Action |
|---|---|
| Ctrl+F | Find in file |
| Ctrl+H | Find and replace |
| Enter / Shift+Enter | Next / previous match |
| Alt+C | Toggle case sensitivity |
| Ctrl+Alt+Enter | Replace all |
| Escape | Close |

## Editing

| Shortcut | Action |
|---|---|
| Ctrl+Z / Ctrl+Y | Undo / redo (also Ctrl+Shift+Z) |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste (whole line when nothing is selected) |
| Ctrl+A | Select all |
| Tab / Shift+Tab | Indent / outdent (the selected lines, if several) |
| Ctrl+/ | Comment or uncomment the selected lines |
| Alt+↑ / Alt+↓ | Move the selected lines up / down |
| Shift+Alt+↑ / Shift+Alt+↓ | Duplicate the selected lines |
| Ctrl+Shift+K | Delete the selected lines |
| Ctrl+← / Ctrl+→ | Move by word (add Shift to select) |
| Ctrl+Backspace / Ctrl+Delete | Delete word |
| Home / End | Line start (toggles with first non-blank) / line end |
| Ctrl+Home / Ctrl+End | Start / end of file |
| Double / triple click | Select word / line |
| Ctrl+D | Select the word, then add the next occurrence as another cursor |
| Ctrl+Shift+L | Select every occurrence |
| Alt+click | Add or remove a cursor |
| Escape | Back to one cursor |
| Ctrl+Shift+[ / Ctrl+Shift+] | Fold / unfold the block (or click the gutter chevron) |
| Ctrl+K Ctrl+0 / Ctrl+K Ctrl+J | Fold all / unfold all |
| Alt+Z | Toggle word wrap |
| Ctrl+Alt+Z | Revert the change at the cursor |
| Ctrl+Shift+V | Open a Markdown preview to the side |

Brackets and quotes close themselves: typing `(` gives `()`, typing `)` before
an auto-inserted `)` steps over it, Backspace between a pair removes both, and
typing a bracket or quote with text selected wraps the selection.

## Terminal

| Shortcut | Action |
|---|---|
| Ctrl+\` or Ctrl+J | Show or hide the terminal dock (shells keep running while hidden) |
| Ctrl+Shift+\` | New terminal tab |
| Ctrl+C | Copy if text is selected, otherwise interrupt |
| Ctrl+V / Ctrl+Shift+V | Paste |
| Right click | Copy selection, or paste |
| Select text | Copies it, if **Copy on select** is on in Settings |
| Ctrl+click | Open a file named in the output (`src/main.rs:12:5`, `Program.cs(10,4)`) or a web address (`localhost:3000`) |

The **+** menu lists your agents and shells. Hide the ones you don't use in
**Settings → Terminal**, where you also pick the default shell.

While the terminal is focused, Ctrl+W, Ctrl+B, Ctrl+O, Ctrl+F, Ctrl+H and
Ctrl+N go to the shell (readline and agent shortcuts), not to Ion.
