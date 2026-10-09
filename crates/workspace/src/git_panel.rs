//! The Git view in the sidebar: commit box, staged and unstaged changes,
//! and the commit history.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use editor::Editor;
use git::{
    CommitSummary, FileState, Graph, GraphRow, RefName, Repository, Span, Status, StatusEntry,
};
use gpui::{
    AppContext, ClickEvent, ClipboardItem, Context, Div, Entity, EventEmitter, Focusable, Hsla,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement,
    PathBuilder, Pixels, Point, PromptLevel, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Task, UniformListScrollHandle, Window, actions, canvas,
    div, fill, prelude::FluentBuilder, px, uniform_list,
};
use ui::{IconName, MenuEntry};

use crate::diff_view::DiffTarget;

actions!(git_panel, [Commit]);

/// Exchanging commits with the remote.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sync {
    Fetch,
    Pull,
    Push,
    /// Pull, then push.
    Both,
}

const CONTEXT: &str = "GitPanel";
const ROW_HEIGHT: f32 = 22.;
/// Commits loaded per page of history.
const HISTORY_PAGE: usize = 200;
/// Width of one lane in the history graph.
const LANE_WIDTH: f32 = 11.;
/// Lanes drawn before the graph is cut off, so it can't crowd out subjects.
const MAX_LANES: u16 = 6;

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("secondary-enter", Commit, Some(CONTEXT))]
}

pub enum GitPanelEvent {
    OpenDiff(DiffTarget),
    /// Open a file (repository-relative path) in an editor tab.
    OpenFile(String),
    OpenCommit(String),
    /// Ion changed the repository; refresh the status now.
    Changed,
    InitRepository,
}

/// Whether the project is in a repository.
pub(crate) enum RepoState {
    Loading,
    /// `git` isn't installed.
    GitMissing,
    NotRepository,
    Ready(Arc<Repository>),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Conflicts,
    Staged,
    Changes,
    History,
}

enum Row {
    Header(Section, usize),
    Entry(Section, usize),
    Commit(usize),
    Note(&'static str),
}

pub struct GitPanel {
    pub(crate) repo: RepoState,
    status: Arc<Status>,
    /// A fetch, pull or push in progress.
    syncing: Option<Sync>,
    message: Entity<Editor>,
    rows: Vec<Row>,
    collapsed: HashSet<Section>,
    history: Vec<CommitSummary>,
    /// Graph row for each commit in `history`.
    graph: Vec<GraphRow>,
    /// Lane state after the last loaded commit.
    graph_state: Graph,
    /// Lanes the loaded graph uses, at most `MAX_LANES`.
    graph_lanes: u16,
    history_complete: bool,
    history_task: Option<Task<()>>,
    /// A commit or other operation is running.
    busy: bool,
    error: Option<SharedString>,
    scroll_handle: UniformListScrollHandle,
    /// Selected changed files (Ctrl+click adds, Shift+click selects a range).
    selected: HashSet<(Section, String)>,
    /// Row where Shift+click ranges start.
    anchor: Option<usize>,
    menu: Option<ui::Menu>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Stage,
    Unstage,
    Discard,
    OpenFile,
    OpenDiff,
    CopyPath,
}

impl EventEmitter<GitPanelEvent> for GitPanel {}

impl GitPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            repo: RepoState::Loading,
            status: Arc::default(),
            syncing: None,
            message: cx.new(|cx| Editor::multi_line_input("Commit message", cx)),
            rows: Vec::new(),
            collapsed: HashSet::new(),
            history: Vec::new(),
            graph: Vec::new(),
            graph_state: Graph::default(),
            graph_lanes: 1,
            history_complete: false,
            history_task: None,
            busy: false,
            error: None,
            scroll_handle: UniformListScrollHandle::new(),
            selected: HashSet::new(),
            anchor: None,
            menu: None,
        }
    }

    pub(crate) fn set_repo(&mut self, repo: RepoState, cx: &mut Context<Self>) {
        self.repo = repo;
        self.status = Arc::default();
        self.error = None;
        self.reload_history(cx);
        self.rebuild_rows();
        cx.notify();
    }

    pub fn set_status(&mut self, status: Arc<Status>, head_changed: bool, cx: &mut Context<Self>) {
        let (old, new) = (&self.status.branch, &status.branch);
        // A fetch moves the upstream, which the history shows too.
        let upstream_changed =
            old.upstream != new.upstream || old.ahead != new.ahead || old.behind != new.behind;
        self.status = status;
        // Keep selected files that are still in the same section.
        let still_there: HashSet<(Section, String)> =
            [Section::Conflicts, Section::Staged, Section::Changes]
                .into_iter()
                .flat_map(|section| {
                    self.entries(section)
                        .map(move |(_, entry)| (section, entry.path.clone()))
                        .collect::<Vec<_>>()
                })
                .collect();
        self.selected.retain(|key| still_there.contains(key));
        if head_changed || upstream_changed {
            self.reload_history(cx);
        }
        self.rebuild_rows();
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.message.focus_handle(cx));
    }

    fn repository(&self) -> Option<Arc<Repository>> {
        match &self.repo {
            RepoState::Ready(repo) => Some(repo.clone()),
            _ => None,
        }
    }

    fn entries(&self, section: Section) -> impl Iterator<Item = (usize, &StatusEntry)> {
        self.status
            .entries
            .iter()
            .enumerate()
            .filter(move |(_, entry)| match section {
                Section::Conflicts => entry.conflicted,
                Section::Staged => entry.is_staged(),
                Section::Changes => entry.is_unstaged(),
                Section::History => false,
            })
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for section in [Section::Conflicts, Section::Staged, Section::Changes] {
            let entries: Vec<usize> = self.entries(section).map(|(ix, _)| ix).collect();
            // Always show "Changes", even when empty, so the view isn't blank.
            if entries.is_empty() && section != Section::Changes {
                continue;
            }
            rows.push(Row::Header(section, entries.len()));
            if self.collapsed.contains(&section) {
                continue;
            }
            if entries.is_empty() {
                rows.push(Row::Note("No changes"));
            }
            rows.extend(entries.into_iter().map(|ix| Row::Entry(section, ix)));
        }
        rows.push(Row::Header(Section::History, self.history.len()));
        if !self.collapsed.contains(&Section::History) {
            if self.history.is_empty() && self.history_complete {
                rows.push(Row::Note("No commits yet"));
            }
            rows.extend((0..self.history.len()).map(Row::Commit));
            if !self.history_complete {
                rows.push(Row::Note("Loading…"));
            }
        }
        self.rows = rows;
    }

    // ---- history -------------------------------------------------------------

    fn reload_history(&mut self, cx: &mut Context<Self>) {
        self.history.clear();
        self.graph.clear();
        self.graph_state = Graph::default();
        self.graph_lanes = 1;
        self.history_complete = self.repository().is_none();
        self.history_task = None;
        self.load_more_history(cx);
    }

    fn load_more_history(&mut self, cx: &mut Context<Self>) {
        if self.history_task.is_some() || self.history_complete {
            return;
        }
        let Some(repo) = self.repository() else {
            return;
        };
        let skip = self.history.len();
        let upstream = self.status.branch.upstream.clone();
        let mut graph = std::mem::take(&mut self.graph_state);
        self.history_task = Some(cx.spawn(async move |this, cx| {
            let page = cx
                .background_spawn(async move {
                    let page = repo.log(upstream.as_deref(), skip, HISTORY_PAGE)?;
                    let rows: Vec<GraphRow> = page
                        .iter()
                        .map(|commit| graph.push(&commit.oid, &commit.parents))
                        .collect();
                    git::Result::Ok((page, rows, graph))
                })
                .await;
            this.update(cx, |this, cx| {
                this.history_task = None;
                match page {
                    Ok((page, rows, graph)) => {
                        this.history_complete = page.len() < HISTORY_PAGE;
                        this.history.extend(page);
                        let lanes = rows.iter().map(GraphRow::width).fold(1, u16::max);
                        this.graph_lanes = this.graph_lanes.max(lanes).min(MAX_LANES);
                        this.graph.extend(rows);
                        this.graph_state = graph;
                    }
                    Err(err) => {
                        this.history_complete = true;
                        this.error = Some(err.to_string().into());
                    }
                }
                this.rebuild_rows();
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- operations ----------------------------------------------------------

    /// Runs a git operation in the background, then asks for a refresh.
    fn run(
        &mut self,
        cx: &mut Context<Self>,
        op: impl FnOnce(&Repository) -> git::Result<()> + Send + 'static,
    ) {
        let Some(repo) = self.repository() else {
            return;
        };
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { op(&repo) }).await;
            this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.error = Some(err.to_string().into());
                }
                cx.emit(GitPanelEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn stage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.run(cx, move |repo| {
            repo.stage(&paths.iter().map(String::as_str).collect::<Vec<_>>())
        });
    }

    fn unstage(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        self.run(cx, move |repo| {
            repo.unstage(&paths.iter().map(String::as_str).collect::<Vec<_>>())
        });
    }

    /// Asks, then throws away unstaged changes (untracked files go to the
    /// Recycle Bin).
    fn discard(&mut self, entries: Vec<StatusEntry>, window: &mut Window, cx: &mut Context<Self>) {
        if entries.is_empty() {
            return;
        }
        let name = |entry: &StatusEntry| {
            entry
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&entry.path)
                .to_owned()
        };
        let untracked = entries
            .iter()
            .filter(|entry| entry.worktree == FileState::Untracked)
            .count();
        let message = match entries.as_slice() {
            [entry] if untracked == 1 => format!("Delete {}?", name(entry)),
            [entry] => format!("Discard changes to {}?", name(entry)),
            _ => format!("Discard changes to {} files?", entries.len()),
        };
        let detail = if untracked > 0 {
            "Unstaged changes will be lost. Files git doesn't track yet go to the Recycle Bin."
        } else {
            "Your unstaged changes will be lost."
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(detail),
            &["Discard", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            this.update(cx, |this, cx| {
                this.run(cx, move |repo| {
                    let (new, tracked): (Vec<_>, Vec<_>) = entries
                        .iter()
                        .partition(|entry| entry.worktree == FileState::Untracked);
                    if !tracked.is_empty() {
                        let paths: Vec<&str> = tracked.iter().map(|e| e.path.as_str()).collect();
                        repo.discard(&paths)?;
                    }
                    if !new.is_empty() {
                        trash::delete_all(new.iter().map(|e| repo.absolute(&e.path)))
                            .map_err(|err| git::GitError::Failed(err.to_string()))?;
                    }
                    Ok(())
                })
            })
            .ok();
        })
        .detach();
    }

    /// Asks, then throws away every unstaged change (staged ones are kept).
    pub(crate) fn discard_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self
            .entries(Section::Changes)
            .map(|(_, entry)| entry.clone())
            .collect();
        self.discard(entries, window, cx);
    }

    /// What a click on a row acts on: the selection if the row is in it,
    /// otherwise just the row. In row order.
    fn targets_for(&self, section: Section, path: &str) -> Vec<(Section, String)> {
        let key = (section, path.to_owned());
        if !self.selected.contains(&key) {
            return vec![key];
        }
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Entry(section, ix) => {
                    let key = (*section, self.status.entries[*ix].path.clone());
                    self.selected.contains(&key).then_some(key)
                }
                _ => None,
            })
            .collect()
    }

    fn entry(&self, path: &str) -> Option<&StatusEntry> {
        self.status.entries.iter().find(|entry| entry.path == path)
    }

    fn apply(
        &mut self,
        action: MenuAction,
        targets: Vec<(Section, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths_in = |pick: &dyn Fn(Section) -> bool| -> Vec<String> {
            let mut paths: Vec<String> = targets
                .iter()
                .filter(|(section, _)| pick(*section))
                .map(|(_, path)| path.clone())
                .collect();
            paths.dedup();
            paths
        };
        match action {
            MenuAction::Stage => {
                let paths = paths_in(&|section| section != Section::Staged);
                if !paths.is_empty() {
                    self.stage(paths, cx);
                }
            }
            MenuAction::Unstage => {
                let paths = paths_in(&|section| section == Section::Staged);
                if !paths.is_empty() {
                    self.unstage(paths, cx);
                }
            }
            MenuAction::Discard => {
                let entries = paths_in(&|section| section == Section::Changes)
                    .iter()
                    .filter_map(|path| self.entry(path).cloned())
                    .collect();
                self.discard(entries, window, cx);
            }
            MenuAction::OpenFile => {
                for path in paths_in(&|_| true) {
                    let deleted = self.entry(&path).is_some_and(|entry| {
                        entry.worktree == FileState::Deleted || entry.index == FileState::Deleted
                    });
                    if !deleted {
                        cx.emit(GitPanelEvent::OpenFile(path));
                    }
                }
            }
            MenuAction::OpenDiff => {
                if let Some((section, path)) = targets.first()
                    && let Some(target) = self.diff_target(*section, path)
                {
                    cx.emit(GitPanelEvent::OpenDiff(target));
                }
            }
            MenuAction::CopyPath => {
                let text = paths_in(&|_| true).join("\n");
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        cx.notify();
    }

    fn diff_target(&self, section: Section, path: &str) -> Option<DiffTarget> {
        let entry = self.entry(path)?;
        let path = path.to_owned();
        match section {
            Section::Staged => Some(DiffTarget::Staged(path)),
            Section::Changes if entry.worktree == FileState::Untracked => {
                Some(DiffTarget::Untracked(path))
            }
            Section::Changes => Some(DiffTarget::Unstaged(path)),
            Section::Conflicts | Section::History => None,
        }
    }

    fn mouse_down_entry(
        &mut self,
        row: usize,
        section: Section,
        path: String,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        let key = (section, path.clone());
        if event.modifiers.secondary() {
            if !self.selected.remove(&key) {
                self.selected.insert(key);
            }
            self.anchor = Some(row);
        } else if event.modifiers.shift {
            let from = self.anchor.unwrap_or(row);
            let (start, end) = (from.min(row), from.max(row));
            self.selected = (start..=end)
                .filter_map(|ix| match self.rows.get(ix) {
                    Some(Row::Entry(section, entry)) => {
                        Some((*section, self.status.entries[*entry].path.clone()))
                    }
                    _ => None,
                })
                .collect();
        } else {
            self.selected = HashSet::from([key]);
            self.anchor = Some(row);
            match self.diff_target(section, &path) {
                Some(target) => cx.emit(GitPanelEvent::OpenDiff(target)),
                None => cx.emit(GitPanelEvent::OpenFile(path)),
            }
        }
        cx.notify();
    }

    fn show_menu(
        &mut self,
        position: Point<Pixels>,
        targets: Vec<(Section, String)>,
        cx: &mut Context<Self>,
    ) {
        let sections: HashSet<Section> = targets.iter().map(|(s, _)| *s).collect();
        let count = targets.len();
        let files = if count == 1 {
            String::new()
        } else {
            format!(" {count} Files")
        };
        let item = |label: String, icon: IconName, action: MenuAction, cx: &mut Context<Self>| {
            let targets = targets.clone();
            MenuEntry::item(
                label,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.apply(action, targets.clone(), window, cx)
                }),
            )
            .icon(icon)
        };
        let mut entries = Vec::new();
        if sections.contains(&Section::Changes) || sections.contains(&Section::Conflicts) {
            entries.push(item(
                format!("Stage{files}"),
                IconName::Plus,
                MenuAction::Stage,
                cx,
            ));
        }
        if sections.contains(&Section::Staged) {
            entries.push(item(
                format!("Unstage{files}"),
                IconName::Minus,
                MenuAction::Unstage,
                cx,
            ));
        }
        if sections.contains(&Section::Changes) {
            entries.push(item(
                "Discard Changes…".into(),
                IconName::Undo2,
                MenuAction::Discard,
                cx,
            ));
        }
        entries.push(MenuEntry::Separator);
        if count == 1 {
            entries.push(item(
                "Open Changes".into(),
                IconName::GitCompare,
                MenuAction::OpenDiff,
                cx,
            ));
        }
        let open = if count == 1 {
            "Open File"
        } else {
            "Open Files"
        };
        entries.push(item(
            open.into(),
            IconName::FileText,
            MenuAction::OpenFile,
            cx,
        ));
        let copy = if count == 1 {
            "Copy Path"
        } else {
            "Copy Paths"
        };
        entries.push(item(copy.into(), IconName::Copy, MenuAction::CopyPath, cx));
        self.menu = Some(ui::Menu::new(position, entries));
        cx.notify();
    }

    fn commit(&mut self, _: &Commit, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).text().trim().to_owned();
        if self.busy || self.repository().is_none() {
            return;
        }
        if message.is_empty() {
            self.error = Some("Write a commit message first.".into());
            cx.notify();
            return;
        }
        let staged = self.entries(Section::Staged).count();
        let changes = self.entries(Section::Changes).count();
        if staged == 0 && changes == 0 {
            self.error = Some("There's nothing to commit.".into());
            cx.notify();
            return;
        }
        if staged > 0 {
            self.start_commit(message, false, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Info,
            "Nothing is staged",
            Some("Stage all changes and commit them?"),
            &["Stage All and Commit", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if matches!(answer.await, Ok(0)) {
                this.update(cx, |this, cx| this.start_commit(message, true, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Fetch, pull and/or push in the background.
    pub(crate) fn sync(&mut self, sync: Sync, cx: &mut Context<Self>) {
        let Some(repo) = self.repository() else {
            return;
        };
        if self.busy || self.syncing.is_some() {
            return;
        }
        let has_upstream = self.status.branch.upstream.is_some();
        self.syncing = Some(sync);
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match sync {
                        Sync::Fetch => repo.fetch(),
                        Sync::Pull => repo.pull(),
                        Sync::Push => repo.push(has_upstream),
                        Sync::Both => {
                            if has_upstream {
                                repo.pull()?;
                            }
                            repo.push(has_upstream)
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.syncing = None;
                if let Err(err) = result {
                    this.error = Some(err.to_string().into());
                }
                cx.emit(GitPanelEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_sync(&self, cx: &mut Context<Self>) -> Option<Div> {
        let branch = &self.status.branch;
        branch.head.as_ref()?;
        let (label, sync, tip) = if let Some(syncing) = self.syncing {
            let label = match syncing {
                Sync::Fetch => "Fetching…",
                Sync::Pull => "Pulling…",
                Sync::Push | Sync::Both if branch.upstream.is_none() => "Publishing…",
                Sync::Push => "Pushing…",
                Sync::Both => "Syncing…",
            };
            (label.to_owned(), syncing, "Working…")
        } else if branch.upstream.is_none() {
            (
                "Publish Branch".to_owned(),
                Sync::Push,
                "Push this branch and track it",
            )
        } else if branch.ahead > 0 || branch.behind > 0 {
            let mut label = "Sync".to_owned();
            if branch.behind > 0 {
                label.push_str(&format!("  ↓{}", branch.behind));
            }
            if branch.ahead > 0 {
                label.push_str(&format!("  ↑{}", branch.ahead));
            }
            (label, Sync::Both, "Pull, then push")
        } else {
            return None;
        };
        Some(
            div().child(
                ui::secondary_button("git-sync", label)
                    .w_full()
                    .when(self.syncing.is_some(), |button| button.opacity(0.6))
                    .tooltip(ui::text_tooltip(tip))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.sync(sync, cx))),
            ),
        )
    }

    fn start_commit(&mut self, message: String, stage_all: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repository() else {
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if stage_all {
                        repo.stage_all()?;
                    }
                    repo.commit(&message)
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.message.update(cx, |input, cx| input.set_text("", cx)),
                    Err(err) => this.error = Some(err.to_string().into()),
                }
                cx.emit(GitPanelEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- rendering -----------------------------------------------------------

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        // Scrolled to the end of the loaded history: fetch the next page.
        if range.end + 20 >= self.rows.len() && !self.collapsed.contains(&Section::History) {
            self.load_more_history(cx);
        }
        let now = git::now();
        range
            .filter_map(|ix| {
                let row = div()
                    .id(ix)
                    .group("git-row")
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pr_2();
                Some(match self.rows.get(ix)? {
                    Row::Header(section, count) => self.render_header(row, *section, *count, cx),
                    Row::Entry(section, entry) => self.render_entry(row, *section, *entry, cx),
                    Row::Commit(commit) => self.render_commit(row, *commit, now, cx),
                    Row::Note(text) => row.pl(px(26.)).text_color(theme::text_faint()).child(*text),
                })
            })
            .collect()
    }

    fn render_header(
        &self,
        row: Stateful<Div>,
        section: Section,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let title = match section {
            Section::Conflicts => "MERGE CONFLICTS",
            Section::Staged => "STAGED",
            Section::Changes => "CHANGES",
            Section::History => "HISTORY",
        };
        let chevron = if self.collapsed.contains(&section) {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };
        let actions: &[(&'static str, IconName, &'static str)] = match section {
            Section::Staged => &[("unstage-all", IconName::Minus, "Unstage All")],
            Section::Changes if count > 0 => &[
                ("discard-all", IconName::Undo2, "Discard All Changes"),
                ("stage-all", IconName::Plus, "Stage All"),
            ],
            _ => &[],
        };
        row.pl_2()
            .mt_1()
            .cursor_pointer()
            .text_size(theme::ui_font_size_small())
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::text_faint())
            .hover(|row| row.text_color(theme::text_muted()))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if !this.collapsed.remove(&section) {
                    this.collapsed.insert(section);
                }
                this.rebuild_rows();
                cx.notify();
            }))
            .child(ui::icon_sized(chevron, px(14.), theme::text_faint()))
            .child(div().flex_1().min_w_0().truncate().child(title))
            .children(actions.iter().map(|&(id, name, label)| {
                ui::small_icon_button(id, name)
                    .invisible()
                    .group_hover("git-row", |button| button.visible())
                    .tooltip(ui::tooltip(label, None))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        match id {
                            "unstage-all" => this.run(cx, |repo| repo.unstage_all()),
                            "discard-all" => this.discard_all(window, cx),
                            _ => this.run(cx, |repo| repo.stage_all()),
                        }
                    }))
            }))
            .when(section != Section::History || count > 0, |row| {
                row.child(
                    div()
                        .flex_none()
                        .min_w(px(20.))
                        .flex()
                        .justify_center()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(theme::text_faint())
                        .child(if section == Section::History && !self.history_complete {
                            format!("{count}+")
                        } else {
                            count.to_string()
                        }),
                )
            })
    }

    fn render_entry(
        &self,
        row: Stateful<Div>,
        section: Section,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let entry = &self.status.entries[ix];
        let state = match section {
            Section::Staged => entry.index,
            _ => entry.worktree,
        };
        let (letter, color) = if entry.conflicted {
            ("!", theme::git_deleted())
        } else {
            (state.letter(), state_color(state))
        };
        let (dir, name) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
        let path = entry.path.clone();
        let deleted = state == FileState::Deleted;

        let selected = self.selected.contains(&(section, path.clone()));
        let row_ix = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Entry(s, e) if *s == section && *e == ix))
            .unwrap_or(0);
        // Inline buttons act on the whole selection when the row is in it.
        let targets = self.targets_for(section, &path);
        let button = |id: &'static str, name: IconName, label: &'static str, action: MenuAction| {
            let targets = targets.clone();
            ui::small_icon_button((id, ix), name)
                .tooltip(ui::tooltip(label, None))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.apply(action, targets.clone(), window, cx);
                }))
        };
        let mut buttons: Vec<Stateful<Div>> = Vec::new();
        if !deleted {
            buttons.push(button(
                "open",
                IconName::FileText,
                "Open File",
                MenuAction::OpenFile,
            ));
        }
        match section {
            Section::Changes => {
                buttons.push(button(
                    "discard",
                    IconName::Undo2,
                    "Discard Changes",
                    MenuAction::Discard,
                ));
                buttons.push(button("stage", IconName::Plus, "Stage", MenuAction::Stage));
            }
            Section::Staged => buttons.push(button(
                "unstage",
                IconName::Minus,
                "Unstage",
                MenuAction::Unstage,
            )),
            // Staging a conflicted file marks it resolved.
            Section::Conflicts => buttons.push(button(
                "resolve",
                IconName::Check,
                "Mark Resolved",
                MenuAction::Stage,
            )),
            Section::History => {}
        }

        let menu_path = path.clone();
        row.pl(px(24.))
            .cursor_pointer()
            .hover(|row| row.bg(theme::hover_bg()))
            .when(selected, |row| row.bg(theme::selection()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.mouse_down_entry(row_ix, section, path.clone(), event, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let key = (section, menu_path.clone());
                    if !this.selected.contains(&key) {
                        this.selected = HashSet::from([key]);
                        this.anchor = Some(row_ix);
                    }
                    let targets = this.targets_for(section, &menu_path);
                    this.show_menu(event.position, targets, cx);
                }),
            )
            .child(ui::file_icon(name, px(14.)))
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text())
                    .when(deleted, |name| name.line_through())
                    .child(name.to_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_faint())
                    .child(dir.to_owned()),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(1.))
                    .invisible()
                    .group_hover("git-row", |buttons| buttons.visible())
                    .children(buttons),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(14.))
                    .flex()
                    .justify_center()
                    .text_size(theme::ui_font_size_small())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(color)
                    .group_hover("git-row", |letter| letter.invisible())
                    .child(letter),
            )
    }

    fn render_commit(
        &self,
        row: Stateful<Div>,
        ix: usize,
        now: i64,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let commit = &self.history[ix];
        let oid = commit.oid.clone();
        let branch = &self.status.branch;
        let mut is_head = false;
        let mut local = None;
        let mut remote = None;
        for name in commit.ref_names() {
            match name {
                RefName::Head => is_head = true,
                RefName::Branch(name) if branch.head.as_deref() == Some(name) => local = Some(name),
                RefName::Branch(name) if branch.upstream.as_deref() == Some(name) => {
                    remote = Some(name)
                }
                _ => {}
            }
        }
        let tooltip = if commit.refs.is_empty() {
            format!(
                "{}\n{} · {}",
                commit.subject, commit.author, commit.short_oid
            )
        } else {
            format!(
                "{}\n{} · {} · {}",
                commit.subject, commit.author, commit.short_oid, commit.refs
            )
        };
        row.pl(px(10.))
            .cursor_pointer()
            .hover(|row| row.bg(theme::hover_bg()))
            .tooltip(ui::text_tooltip(tooltip))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(GitPanelEvent::OpenCommit(oid.clone()))
            }))
            .child(self.render_graph(ix, is_head))
            .when_some(local, |row, name| {
                let tip = if remote.is_some() {
                    format!(
                        "{name} (local) is up to date with {}",
                        branch.upstream.as_deref().unwrap_or("")
                    )
                } else {
                    format!("{name} (local)")
                };
                row.child(
                    ref_badge(IconName::GitBranch, name)
                        .when(remote.is_some(), |badge| {
                            badge.child(ui::icon_sized(
                                IconName::Cloud,
                                px(11.),
                                theme::text_muted(),
                            ))
                        })
                        .id("local-ref")
                        .tooltip(ui::text_tooltip(tip)),
                )
            })
            .when_some(remote.filter(|_| local.is_none()), |row, name| {
                row.child(
                    ref_badge(IconName::Cloud, name)
                        .id("remote-ref")
                        .tooltip(ui::text_tooltip(format!("{name} (remote)"))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text())
                    .child(commit.subject.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_faint())
                    .child(short_age(commit.time, now)),
            )
    }

    /// The history graph's slice for one commit row.
    fn render_graph(&self, ix: usize, is_head: bool) -> impl IntoElement {
        let row = self.graph.get(ix).cloned().unwrap_or_default();
        let lanes = self.graph_lanes;
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let x = |lane: u16| bounds.left() + px(LANE_WIDTH * (lane as f32 + 0.5));
                let top = bounds.top();
                let mid = bounds.center().y;
                let bottom = bounds.bottom();
                for edge in &row.edges {
                    if edge.from >= lanes || edge.to >= lanes {
                        continue;
                    }
                    let mut path = PathBuilder::stroke(px(1.5));
                    let (from, to) = (x(edge.from), x(edge.to));
                    match edge.span {
                        Span::Through => {
                            path.move_to(gpui::point(from, top));
                            path.line_to(gpui::point(to, bottom));
                        }
                        Span::Upper => {
                            path.move_to(gpui::point(from, top));
                            path.line_to(gpui::point(to, mid));
                        }
                        // Leaves the dot sideways and bends down into the
                        // parent's lane.
                        Span::Lower if from != to => {
                            path.move_to(gpui::point(from, mid));
                            let bend = gpui::point(to, mid);
                            path.cubic_bezier_to(gpui::point(to, bottom), bend, bend);
                        }
                        Span::Lower => {
                            path.move_to(gpui::point(from, mid));
                            path.line_to(gpui::point(to, bottom));
                        }
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, lane_color(edge.color));
                    }
                }
                let center = gpui::point(x(row.lane.min(lanes - 1)), mid);
                let dot = |radius: f32| {
                    gpui::Bounds::centered_at(center, gpui::size(px(radius * 2.), px(radius * 2.)))
                };
                let color = lane_color(row.color);
                if is_head {
                    // A ring marks the checked-out commit.
                    window.paint_quad(fill(dot(4.5), color).corner_radii(px(4.5)));
                    window.paint_quad(fill(dot(2.5), theme::panel_bg()).corner_radii(px(2.5)));
                } else {
                    window.paint_quad(fill(dot(3.5), color).corner_radii(px(3.5)));
                }
            },
        )
        .flex_none()
        .w(px(LANE_WIDTH * lanes as f32))
        .h_full()
    }

    fn render_message_box(&self, cx: &mut Context<Self>) -> Div {
        let staged = self.entries(Section::Staged).count();
        let label = if self.busy {
            "Committing…".to_owned()
        } else if staged > 0 {
            format!("Commit {staged} File{}", if staged == 1 { "" } else { "s" })
        } else {
            "Commit".to_owned()
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .pb_2()
            .child(
                div()
                    .h(px(68.))
                    .px_1()
                    .py(px(3.))
                    .rounded(px(7.))
                    .bg(theme::bg())
                    .border_1()
                    .border_color(theme::border())
                    .child(self.message.clone()),
            )
            .child(
                ui::primary_button("commit", label)
                    .w_full()
                    .when(self.busy, |button| button.opacity(0.6))
                    .tooltip(ui::tooltip("Commit", Some(Box::new(Commit))))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.commit(&Commit, window, cx)
                    })),
            )
            .children(self.render_sync(cx))
            .children(self.error.clone().map(|error| {
                div()
                    .px_1()
                    .text_size(theme::ui_font_size_small())
                    .text_color(theme::git_deleted())
                    .child(error)
            }))
    }

    fn render_placeholder(&self, cx: &mut Context<Self>) -> Div {
        let message = match &self.repo {
            RepoState::Loading => "Looking for a git repository…",
            RepoState::GitMissing => "Git isn't installed. Install it to see changes and history.",
            RepoState::NotRepository => "This folder isn't a git repository.",
            RepoState::Ready(_) => "",
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .px_3()
            .pt_2()
            .text_color(theme::text_muted())
            .child(message)
            .when(matches!(self.repo, RepoState::NotRepository), |content| {
                content.child(
                    ui::primary_button("git-init", "Initialize Repository").on_click(cx.listener(
                        |_, _: &ClickEvent, _, cx| cx.emit(GitPanelEvent::InitRepository),
                    )),
                )
            })
    }
}

pub(crate) fn state_color(state: FileState) -> Hsla {
    match state {
        FileState::Added | FileState::Untracked => theme::git_added(),
        FileState::Deleted => theme::git_deleted(),
        FileState::Renamed | FileState::Copied => theme::git_changed_marker(),
        FileState::Modified | FileState::TypeChanged | FileState::Unmodified => {
            theme::git_modified()
        }
    }
}

/// "5m", "3h", "2d", "4w", "7mo", "2y".
fn short_age(then: i64, now: i64) -> String {
    let minutes = (now - then).max(0) / 60;
    let (hours, days) = (minutes / 60, minutes / 1440);
    match () {
        _ if minutes < 60 => format!("{}m", minutes.max(1)),
        _ if hours < 24 => format!("{hours}h"),
        _ if days < 14 => format!("{days}d"),
        _ if days < 60 => format!("{}w", days / 7),
        _ if days < 365 => format!("{}mo", days / 30),
        _ => format!("{}y", days / 365),
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = matches!(self.repo, RepoState::Ready(_));
        let this = cx.entity().downgrade();
        let menu = self.menu.as_ref().map(|menu| {
            ui::render_menu(
                menu,
                move |_, cx| {
                    this.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    })
                    .ok();
                },
                window,
            )
        });
        div()
            .key_context(CONTEXT)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::commit))
            .when(!ready, |panel| panel.child(self.render_placeholder(cx)))
            .when(ready, |panel| {
                panel.child(self.render_message_box(cx)).child(
                    uniform_list(
                        "git-rows",
                        self.rows.len(),
                        cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
                    )
                    .track_scroll(self.scroll_handle.clone())
                    .flex_1(),
                )
            })
            .children(menu)
    }
}

/// Color of a history graph lane.
fn lane_color(ix: u16) -> Hsla {
    let syntax = theme::syntax();
    let colors = [
        syntax.function,
        syntax.string,
        syntax.keyword,
        syntax.number,
        syntax.ty,
        syntax.tag,
    ];
    gpui::rgba(colors[ix as usize % colors.len()]).into()
}

/// A branch name next to a commit subject.
fn ref_badge(icon: IconName, name: &str) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.))
        .px_1()
        .max_w(px(110.))
        .rounded_sm()
        .border_1()
        .border_color(theme::border())
        .bg(theme::elevated_bg())
        .text_size(theme::ui_font_size_small())
        .text_color(theme::text_muted())
        .child(ui::icon_sized(icon, px(11.), theme::text_muted()))
        .child(div().min_w_0().truncate().child(name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::short_age;

    #[test]
    fn short_ages() {
        assert_eq!(short_age(0, 30), "1m");
        assert_eq!(short_age(0, 7200), "2h");
        assert_eq!(short_age(0, 3 * 86_400), "3d");
        assert_eq!(short_age(0, 400 * 86_400), "1y");
    }
}
