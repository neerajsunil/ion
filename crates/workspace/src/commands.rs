//! Every command Ion offers, for the command palette and the app menu.

use gpui::{Action, App, SharedString};
use ui::IconName;

use crate::workspace::*;

pub(crate) struct Command {
    pub category: &'static str,
    pub label: &'static str,
    pub icon: Option<IconName>,
    pub action: Box<dyn Action>,
}

impl Command {
    pub fn title(&self) -> String {
        format!("{}: {}", self.category, self.label)
    }

    pub fn shortcut(&self, cx: &App) -> Option<SharedString> {
        ui::shortcut(self.action.as_ref(), cx)
    }
}

fn command(
    category: &'static str,
    label: &'static str,
    icon: Option<IconName>,
    action: impl Action,
) -> Command {
    Command {
        category,
        label,
        icon,
        action: Box::new(action),
    }
}

/// Every command, with one per installed agent (`agents`: on this machine,
/// or on the server for a remote project).
pub(crate) fn all_for_workspace(
    remote: bool,
    agents: &[terminal::HarnessKind],
    settings: &settings::Settings,
) -> Vec<Command> {
    let mut commands: Vec<_> = all()
        .into_iter()
        .filter(|command| remote || !command.action.as_any().is::<DisconnectSsh>())
        .collect();
    commands.extend(
        agents
            .iter()
            .filter(|kind| settings.shows_terminal(kind.name()))
            .map(|kind| agent_command(*kind)),
    );
    commands
}

fn agent_command(kind: terminal::HarnessKind) -> Command {
    let label = match kind {
        terminal::HarnessKind::ClaudeCode => "New Claude Code",
        terminal::HarnessKind::Codex => "New Codex",
    };
    command("Agent", label, Some(IconName::Bot), NewAgent(kind))
}

fn all() -> Vec<Command> {
    use IconName as I;
    vec![
        command("File", "New File", Some(I::FilePlus), NewFile),
        command("File", "Open Folder…", Some(I::FolderOpen), OpenFolder),
        command("Remote", "Connect to SSH…", Some(I::Monitor), ConnectSsh),
        command("Remote", "Disconnect SSH", Some(I::LogOut), DisconnectSsh),
        command(
            "File",
            "Refresh Project",
            Some(I::RefreshCw),
            RefreshProject,
        ),
        command("File", "Go to File…", Some(I::Search), ToggleFileFinder),
        command("File", "Save", Some(I::Save), editor::Save),
        command("File", "Save As…", None, SaveAs),
        command("File", "Close Tab", Some(I::X), CloseTab),
        command("File", "Close All Tabs", Some(I::X), CloseAllTabs),
        command("File", "Reopen Closed Tab", Some(I::Undo2), ReopenClosedTab),
        command("Go", "Go to Line…", None, GoToLine),
        command("Go", "Go to Symbol in File…", None, GoToSymbol),
        command("Git", "Fetch", Some(I::RefreshCw), GitFetch),
        command("Git", "Pull", Some(I::ArrowDown), GitPull),
        command("Git", "Push", Some(I::ArrowUp), GitPush),
        command(
            "Git",
            "Discard All Changes…",
            Some(I::Undo2),
            DiscardAllChanges,
        ),
        command(
            "Git",
            "Revert Change at Cursor",
            Some(I::Undo2),
            editor::RevertHunk,
        ),
        command(
            "Agent",
            "Send Selection to Agent",
            Some(I::Bot),
            SendToAgent,
        ),
        command("Edit", "Toggle Comment", None, editor::ToggleComment),
        command("View", "Toggle Word Wrap", None, editor::ToggleWordWrap),
        command(
            "Edit",
            "Add Next Occurrence",
            None,
            editor::AddNextOccurrence,
        ),
        command(
            "Edit",
            "Select All Occurrences",
            None,
            editor::SelectAllOccurrences,
        ),
        command("View", "Fold", Some(I::ChevronRight), editor::Fold),
        command("View", "Unfold", Some(I::ChevronDown), editor::Unfold),
        command("View", "Fold All", None, editor::FoldAll),
        command("View", "Unfold All", None, editor::UnfoldAll),
        command(
            "View",
            "Toggle Side-by-Side Diff",
            Some(I::Columns2),
            editor::ToggleDiffLayout,
        ),
        command(
            "Markdown",
            "Open Preview",
            Some(I::Eye),
            OpenMarkdownPreview,
        ),
        command(
            "Edit",
            "Duplicate Line",
            Some(I::Copy),
            editor::DuplicateLine,
        ),
        command("Edit", "Move Line Up", Some(I::ArrowUp), editor::MoveLineUp),
        command(
            "Edit",
            "Move Line Down",
            Some(I::ArrowDown),
            editor::MoveLineDown,
        ),
        command("Edit", "Delete Line", Some(I::Trash2), editor::DeleteLine),
        command("View", "Zoom In", None, ZoomIn),
        command("View", "Zoom Out", None, ZoomOut),
        command("View", "Reset Zoom", None, ResetZoom),
        command("Edit", "Find", Some(I::Search), Find),
        command("Edit", "Find and Replace", None, FindReplace),
        command(
            "Edit",
            "Find in Files",
            Some(I::FolderSearch),
            SearchProject,
        ),
        command("View", "Command Palette", Some(I::Command), CommandPalette),
        command("View", "Explorer", Some(I::Files), ShowFiles),
        command("View", "Source Control", Some(I::GitBranch), ShowGit),
        command("View", "Toggle Sidebar", Some(I::PanelLeft), ToggleSidebar),
        command(
            "View",
            "Toggle Terminal",
            Some(I::PanelBottom),
            ToggleTerminal,
        ),
        command("View", "Split Right", Some(I::Columns2), SplitRight),
        command("View", "Split Down", Some(I::Rows2), SplitDown),
        command(
            "View",
            "Maximize Pane",
            Some(I::Maximize2),
            ToggleMaximizePane,
        ),
        command("View", "Next Tab", None, NextTab),
        command("View", "Previous Tab", None, PreviousTab),
        command("View", "Focus Pane Left", None, FocusPaneLeft),
        command("View", "Focus Pane Right", None, FocusPaneRight),
        command("View", "Focus Pane Above", None, FocusPaneUp),
        command("View", "Focus Pane Below", None, FocusPaneDown),
        command(
            "Terminal",
            "New Terminal",
            Some(I::SquareTerminal),
            NewTerminal,
        ),
        command("View", "Show Agents", Some(I::Bot), ShowAgents),
        command(
            "View",
            "Show Problems",
            Some(I::TriangleAlert),
            ShowProblems,
        ),
        command("Run", "Run Task…", Some(I::Play), RunTask),
        command("Agent", "Stop Following", None, StopFollowing),
        command("Git", "Switch Branch…", Some(I::GitBranch), SwitchBranch),
        command("Preferences", "Settings", Some(I::Settings), OpenSettings),
        command(
            "Preferences",
            "Keyboard Shortcuts",
            Some(I::Keyboard),
            OpenKeyboardShortcuts,
        ),
        command(
            "Preferences",
            "Open settings.json",
            Some(I::FileCode),
            OpenSettingsFile,
        ),
        command("Preferences", "Dark Theme", Some(I::Moon), UseDarkTheme),
        command("Preferences", "Light Theme", Some(I::Sun), UseLightTheme),
        command(
            "Preferences",
            "System Theme",
            Some(I::Monitor),
            UseSystemTheme,
        ),
        command("Ion", "Quit", Some(I::LogOut), Quit),
    ]
}

/// Commands matching a query: every word of the query must appear in
/// "Category: Label". Label prefix matches come first.
pub(crate) fn matching(commands: &[Command], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut found: Vec<(u8, usize)> = commands
        .iter()
        .enumerate()
        .filter_map(|(ix, command)| {
            let title = command.title().to_lowercase();
            if !words.iter().all(|word| title.contains(word)) {
                return None;
            }
            let label = command.label.to_lowercase();
            let rank = if query.is_empty() {
                1
            } else if label.starts_with(&query) {
                0
            } else {
                1
            };
            Some((rank, ix))
        })
        .collect();
    found.sort_by_key(|&(rank, ix)| (rank, ix));
    found.into_iter().map(|(_, ix)| ix).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_is_only_offered_in_remote_workspaces() {
        assert!(
            matching(
                &all_for_workspace(false, &[], &Default::default()),
                "disconnect ssh"
            )
            .is_empty()
        );
        assert_eq!(
            matching(
                &all_for_workspace(true, &[], &Default::default()),
                "disconnect ssh"
            )
            .len(),
            1
        );
        assert_eq!(
            matching(
                &all_for_workspace(false, &[], &Default::default()),
                "connect to ssh"
            )
            .len(),
            1
        );
    }

    #[test]
    fn matches_words_anywhere_and_ranks_prefixes() {
        let commands = all();
        let found: Vec<&str> = matching(&commands, "term")
            .into_iter()
            .map(|ix| commands[ix].label)
            .collect();
        assert!(found.contains(&"New Terminal") && found.contains(&"Toggle Terminal"));
        let found = matching(&commands, "sett");
        assert_eq!(commands[found[0]].label, "Settings");
        let found = matching(&commands, "split down");
        assert_eq!(commands[found[0]].label, "Split Down");
        assert!(matching(&commands, "zzz").is_empty());
        assert_eq!(matching(&commands, "").len(), commands.len());
    }
}
