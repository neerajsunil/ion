//! Pointing an agent at code: Send to Agent (the selection, or files from
//! the tree) and files dropped on a terminal.
//!
//! A connected Claude Code gets an `at_mentioned` notification, which puts
//! the reference in its prompt. Any other agent gets the reference typed
//! into its terminal, as `@path#L3-7` for Claude Code or `path:3-7`.

use std::path::{Path, PathBuf};

use gpui::{Context, Window};
use terminal::HarnessKind;

use crate::pane::{ItemKind, PaneId};
use crate::workspace::Workspace;

/// How a terminal names a file: relative to its folder (or the project)
/// when inside it. `lines` are one-based and inclusive.
fn reference(
    path: &Path,
    lines: Option<(usize, usize)>,
    cwd: Option<&Path>,
    root: Option<&Path>,
    harness: Option<HarnessKind>,
) -> String {
    let relative = [cwd, root]
        .into_iter()
        .flatten()
        .find_map(|base| path.strip_prefix(base).ok())
        .filter(|relative| !relative.as_os_str().is_empty());
    let shown = match relative {
        Some(relative) => PathBuf::from(relative.to_string_lossy().replace('\\', "/")),
        None => path.to_path_buf(),
    };
    let shown = terminal::quote_path(&shown);
    let claude = harness == Some(HarnessKind::ClaudeCode);
    match (claude, lines) {
        (true, Some((start, end))) if start == end => format!("@{shown}#L{start}"),
        (true, Some((start, end))) => format!("@{shown}#L{start}-{end}"),
        (true, None) => format!("@{shown}"),
        (false, Some((start, end))) if start == end => format!("{shown}:{start}"),
        (false, Some((start, end))) => format!("{shown}:{start}-{end}"),
        (false, None) => shown,
    }
}

impl Workspace {
    /// The terminal references go to: the focused one, else the agent being
    /// followed, else the first agent, else the first terminal.
    pub(crate) fn agent_terminal(&self, cx: &gpui::App) -> Option<(PaneId, usize)> {
        if let Some(item) = self.active_item()
            && item.terminal().is_some()
        {
            let pane = self.active_pane;
            return Some((pane, self.panes[&pane].active));
        }
        let position = |id| self.find_item(id);
        if let Some(found) = self
            .following
            .as_ref()
            .and_then(|following| position(following.terminal))
        {
            return Some(found);
        }
        let terminals: Vec<_> = self
            .items()
            .filter_map(|(_, item)| Some((item.id(), item.terminal()?.read(cx).harness())))
            .collect();
        let agent = terminals.iter().find(|(_, harness)| harness.is_some());
        agent
            .or(terminals.first())
            .and_then(|(id, _)| position(*id))
    }

    /// Types references to `files` into a terminal and focuses it.
    fn type_references(
        &mut self,
        (pane, ix): (PaneId, usize),
        files: &[(PathBuf, Option<(usize, usize)>)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ItemKind::Terminal { view, cwd, .. } = &self.panes[&pane].items[ix].kind else {
            return;
        };
        let (view, cwd) = (view.clone(), cwd.clone());
        let harness = view.read(cx).harness();
        let text: Vec<String> = files
            .iter()
            .map(|(path, lines)| {
                reference(path, *lines, cwd.as_deref(), self.root.as_deref(), harness)
            })
            .collect();
        let text = format!("{} ", text.join(" "));
        view.update(cx, |view, cx| view.insert_text(&text, cx));
        self.activate_item(pane, ix, window, cx);
    }

    /// Sends files (with optional one-based line ranges) to the agent.
    fn send_references(
        &mut self,
        files: Vec<(PathBuf, Option<(usize, usize)>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            return;
        }
        let target = self.agent_terminal(cx);
        // Claude Code connected over /ide takes the reference directly.
        let mentioned = files.iter().all(|(path, lines)| {
            let zero_based = lines.map(|(start, end)| (start - 1, end - 1));
            self.ide_mention(path, zero_based, cx)
        });
        if mentioned {
            if let Some((pane, ix)) = target {
                self.activate_item(pane, ix, window, cx);
            }
            self.status = Some("Sent to Claude Code".into());
            cx.notify();
            return;
        }
        match target {
            Some(target) => self.type_references(target, &files, window, cx),
            None => {
                self.status = Some("Start an agent or a terminal first".into());
                cx.notify();
            }
        }
    }

    /// Sends the active editor's selection (or its file) to the agent.
    pub(crate) fn send_selection_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        let editor = editor.read(cx);
        let Some(path) = editor.path().map(Path::to_path_buf) else {
            self.status = Some("Save the file first, so the agent can read it".into());
            cx.notify();
            return;
        };
        let ((start_row, _), (end_row, end_col)) = editor.selection_points();
        let lines = (!editor.selected_text().is_empty()).then(|| {
            // A selection ending at the start of a line doesn't include it.
            let end = if end_col == 0 && end_row > start_row {
                end_row - 1
            } else {
                end_row
            };
            (start_row + 1, end + 1)
        });
        self.send_references(vec![(path, lines)], window, cx);
    }

    /// Sends whole files or folders (from the tree) to the agent.
    pub(crate) fn send_paths_to_agent(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files = paths.into_iter().map(|path| (path, None)).collect();
        self.send_references(files, window, cx);
    }

    /// Files dropped on a pane: a terminal gets their paths, an editor pane
    /// opens them.
    pub(crate) fn drop_files(
        &mut self,
        pane: PaneId,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.panes.get(&pane) else {
            return;
        };
        let terminal = target
            .active_item()
            .is_some_and(|item| item.terminal().is_some());
        if terminal {
            let files: Vec<_> = paths.into_iter().map(|path| (path, None)).collect();
            let ix = target.active;
            self.type_references((pane, ix), &files, window, cx);
            return;
        }
        self.editor_pane = pane;
        for path in paths {
            if !path.is_dir() {
                self.open_file(path, window, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_files_the_way_agents_read_them() {
        let root = PathBuf::from("project");
        let file = root.join("src").join("main.rs");
        let claude = Some(HarnessKind::ClaudeCode);
        assert_eq!(
            reference(&file, Some((3, 7)), None, Some(&root), claude),
            "@src/main.rs#L3-7"
        );
        assert_eq!(
            reference(&file, Some((3, 3)), None, Some(&root), claude),
            "@src/main.rs#L3"
        );
        assert_eq!(
            reference(&file, None, Some(&root), None, None),
            "src/main.rs"
        );
        assert_eq!(
            reference(
                &file,
                Some((1, 2)),
                None,
                Some(&root),
                Some(HarnessKind::Codex)
            ),
            "src/main.rs:1-2"
        );
        let spaced = root.join("my file.rs");
        assert_eq!(
            reference(&spaced, None, None, Some(&root), None),
            "\"my file.rs\""
        );
    }
}
