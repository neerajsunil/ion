//! The workspace's git integration: finding the repository, keeping status
//! fresh, gutter diff bases, blame for the cursor line and diff tabs.
//!
//! Nothing polls. Status refreshes when the file watcher or the `.git`
//! watcher reports a change, or after Ion runs a git command itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor::{BaseRow, Editor};
use file_tree::GitMark;
use futures::StreamExt;
use git::{Blame, FileState, GitError, Repository, Status};
use gpui::{AppContext, Context, Entity, EntityId, SharedString, Task, Window};
use project::FsWatcher;

use crate::branch_picker::{BranchPicker, BranchPickerEvent};
use crate::diff_view::{self, DiffContent, DiffTarget, HunkRevert};
use crate::git_panel::RepoState;
use crate::pane::{DiffTab, ItemKind, PaneId};
use crate::workspace::{Modal, Workspace};

/// Collect `.git` changes for this long (a commit touches several files).
const GIT_BATCH_DELAY: Duration = Duration::from_millis(80);
/// Blame the file once the cursor has rested this long.
const BLAME_DELAY: Duration = Duration::from_millis(300);
/// Shown when a hunk can't be reverted because its lines changed since.
const CHANGED_AGAIN: &str = "That change was edited again. Refresh the diff and try again.";

/// Converts between repository-relative paths and the absolute paths the
/// tree and editors use (which start with the project root exactly).
#[derive(Clone)]
pub(crate) struct PathMap {
    repo: Arc<Repository>,
    root: PathBuf,
    /// The project root relative to the repository root, with a trailing
    /// `/`, or empty when they're the same folder.
    prefix: String,
}

impl PathMap {
    fn new(repo: Arc<Repository>, root: PathBuf) -> Self {
        let prefix = match repo.relative(&root) {
            Some(prefix) if !prefix.is_empty() => format!("{prefix}/"),
            _ => String::new(),
        };
        Self { repo, root, prefix }
    }

    pub fn absolute(&self, relative: &str) -> PathBuf {
        match relative.strip_prefix(&self.prefix) {
            Some(inner) => project::join_path(&self.root, inner),
            None => self.repo.absolute(relative),
        }
    }

    pub fn relative(&self, path: &Path) -> Option<String> {
        match path.strip_prefix(&self.root) {
            Ok(inner) => Some(format!(
                "{}{}",
                self.prefix,
                inner.to_str()?.replace('\\', "/")
            )),
            Err(_) => self.repo.relative(path),
        }
    }
}

pub(crate) enum BlameState {
    Loading,
    Ready(Arc<Blame>),
    Unavailable,
}

pub(crate) struct GitState {
    pub repo: Arc<Repository>,
    pub paths: PathMap,
    pub status: Arc<Status>,
    /// `git status` has finished at least once.
    pub status_loaded: bool,
    status_task: Option<Task<()>>,
    /// Something changed while status was running; run it again after.
    status_stale: bool,
    _watcher: Option<FsWatcher>,
    _watch_task: Option<Task<()>>,
    /// The HEAD commit each open editor's diff base was read at.
    bases: HashMap<EntityId, Option<String>>,
    blame: HashMap<PathBuf, BlameState>,
    blame_timer: Option<Task<()>>,
}

/// What the status bar shows for the cursor line.
pub(crate) struct LineBlame {
    pub text: SharedString,
    pub tooltip: SharedString,
    pub oid: Option<String>,
}

impl Workspace {
    /// Looks for a repository at the project root, in the background.
    pub(crate) fn discover_git(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git = None;
        self.git_panel
            .update(cx, |panel, cx| panel.set_repo(RepoState::Loading, cx));
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.set_git_marks([], cx));
        }
        let discover = cx.background_spawn({
            let root = root.clone();
            let connection = self.filesystem.remote().cloned();
            async move { Repository::discover_with_connection(&root, connection) }
        });
        self.git_discovery = Some(cx.spawn_in(window, async move |this, cx| {
            let found = discover.await;
            this.update_in(cx, |this, window, cx| {
                this.git_discovery = None;
                if this.root.as_deref() != Some(root.as_path()) {
                    return;
                }
                match found {
                    Ok(Some(repo)) => this.install_repo(repo, root, window, cx),
                    Ok(None) => this.set_repo_state(RepoState::NotRepository, cx),
                    Err(_) => this.set_repo_state(RepoState::GitMissing, cx),
                }
            })
            .ok();
        }));
    }

    fn set_repo_state(&mut self, state: RepoState, cx: &mut Context<Self>) {
        self.git_panel
            .update(cx, |panel, cx| panel.set_repo(state, cx));
        cx.notify();
    }

    pub(crate) fn init_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let init = cx.background_spawn({
            let root = root.clone();
            let connection = self.filesystem.remote().cloned();
            async move { Repository::init_with_connection(&root, connection) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = init.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(repo) => this.install_repo(repo, root, window, cx),
                Err(err) => {
                    this.status = Some(format!("git init failed: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn install_repo(
        &mut self,
        repo: Repository,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repo = Arc::new(repo);
        // Commits, staging and branch switches from anywhere (the terminal,
        // an agent) show up through the `.git` folder.
        let (watcher, watch_task) = if self.filesystem.remote().is_some() {
            (None, None)
        } else {
            match project::watch_git_dir(repo.git_dir()) {
                Ok((watcher, mut events)) => {
                    let task = cx.spawn_in(window, async move |this, cx| {
                        while events.next().await.is_some() {
                            cx.background_executor().timer(GIT_BATCH_DELAY).await;
                            while events.try_recv().is_ok() {}
                            if this
                                .update(cx, |this, cx| this.refresh_git_status(cx))
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                    (Some(watcher), Some(task))
                }
                Err(_) => (None, None),
            }
        };
        self.git = Some(GitState {
            paths: PathMap::new(repo.clone(), root),
            repo: repo.clone(),
            status: Arc::default(),
            status_loaded: false,
            status_task: None,
            status_stale: false,
            _watcher: watcher,
            _watch_task: watch_task,
            bases: HashMap::new(),
            blame: HashMap::new(),
            blame_timer: None,
        });
        self.set_repo_state(RepoState::Ready(repo), cx);
        self.refresh_git_status(cx);
    }

    /// Re-reads `git status` in the background. Calls while it runs are
    /// coalesced into one more run.
    pub(crate) fn refresh_git_status(&mut self, cx: &mut Context<Self>) {
        let Some(git) = &mut self.git else {
            return;
        };
        if git.status_task.is_some() {
            git.status_stale = true;
            return;
        }
        let repo = git.repo.clone();
        git.status_task = Some(cx.spawn(async move |this, cx| {
            let status = cx.background_spawn(async move { repo.status() }).await;
            this.update(cx, |this, cx| {
                let Some(git) = &mut this.git else {
                    return;
                };
                git.status_task = None;
                let first = !std::mem::replace(&mut git.status_loaded, true);
                let stale = std::mem::take(&mut git.status_stale);
                match status {
                    Ok(status) => this.apply_git_status(status, first, cx),
                    Err(err) => {
                        this.status = Some(format!("git status failed: {err}").into());
                        cx.notify();
                    }
                }
                if stale {
                    this.refresh_git_status(cx);
                }
            })
            .ok();
        }));
    }

    fn apply_git_status(&mut self, status: Status, first: bool, cx: &mut Context<Self>) {
        let Some(git) = &mut self.git else {
            return;
        };
        if !first && *git.status == status {
            // The same files changed as before (most saves): the tree, the
            // panel and the diff bases stay. Open diffs show contents, which
            // may still have changed.
            self.refresh_diff_tabs(cx);
            return;
        }
        let old = &git.status.branch;
        let head_changed = old.oid != status.branch.oid || old.head != status.branch.head;
        if head_changed {
            git.blame.clear();
            git.bases.clear();
        }
        let status = Arc::new(status);
        git.status = status.clone();

        let marks: Vec<(PathBuf, GitMark)> = status
            .entries
            .iter()
            .filter_map(|entry| {
                let mark = if entry.conflicted {
                    GitMark::Conflict
                } else if entry.worktree == FileState::Untracked {
                    GitMark::Untracked
                } else if entry.index == FileState::Added && entry.worktree == FileState::Unmodified
                {
                    GitMark::Added
                } else if entry.worktree == FileState::Deleted || entry.index == FileState::Deleted
                {
                    return None;
                } else {
                    GitMark::Modified
                };
                Some((git.paths.absolute(&entry.path), mark))
            })
            .collect();
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.set_git_marks(marks, cx));
        }
        self.git_panel
            .update(cx, |panel, cx| panel.set_status(status, head_changed, cx));
        for editor in self.editors() {
            self.load_diff_base(&editor, cx);
        }
        self.refresh_diff_tabs(cx);
        cx.notify();
    }

    /// Forgets diff bases and blame of editors that were closed.
    pub(crate) fn prune_git_caches(&mut self, cx: &mut Context<Self>) {
        let editors = self.editors();
        let Some(git) = &mut self.git else {
            return;
        };
        git.bases
            .retain(|id, _| editors.iter().any(|editor| editor.entity_id() == *id));
        git.blame.retain(|path, _| {
            editors
                .iter()
                .any(|editor| editor.read(cx).path() == Some(path.as_path()))
        });
    }

    /// Reads the file's committed text for the editor's change markers, if
    /// it hasn't been read at the current HEAD yet.
    pub(crate) fn load_diff_base(&mut self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let Some(git) = &mut self.git else {
            return;
        };
        let Some(relative) = editor
            .read(cx)
            .path()
            .and_then(|path| git.paths.relative(path))
        else {
            return;
        };
        let head = git.status.branch.oid.clone();
        if git.bases.get(&editor.entity_id()) == Some(&head) {
            return;
        }
        git.bases.insert(editor.entity_id(), head.clone());
        let repo = git.repo.clone();
        let editor = editor.downgrade();
        cx.spawn(async move |_, cx| {
            let text = match head {
                Some(_) => cx
                    .background_spawn(async move { repo.head_text(&relative) })
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            editor
                .update(cx, |editor, cx| editor.set_diff_base(text.as_deref(), cx))
                .ok();
        })
        .detach();
    }

    // ---- blame ---------------------------------------------------------------

    /// Called when the active editor changes; blames its file once the
    /// cursor rests. Moving within an already blamed file costs nothing.
    pub(crate) fn schedule_blame(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .active_editor()
            .and_then(|editor| editor.read(cx).path().map(Path::to_path_buf))
        else {
            return;
        };
        let Some(git) = &mut self.git else {
            return;
        };
        if git.blame.contains_key(&path) {
            return;
        }
        // Replacing the timer cancels the previous one.
        git.blame_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLAME_DELAY).await;
            this.update(cx, |this, cx| this.blame_active_file(cx)).ok();
        }));
    }

    fn blame_active_file(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        let Some(git) = &mut self.git else {
            return;
        };
        let editor = editor.read(cx);
        // Files without a committed version have nothing to blame.
        let (Some(path), true) = (editor.path(), editor.has_diff_base()) else {
            return;
        };
        let Some(relative) = git.paths.relative(path) else {
            return;
        };
        let path = path.to_path_buf();
        if git.blame.contains_key(&path) {
            return;
        }
        git.blame.insert(path.clone(), BlameState::Loading);
        let repo = git.repo.clone();
        cx.spawn(async move |this, cx| {
            let blame = cx
                .background_spawn(async move { repo.blame(&relative) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(git) = &mut this.git
                    && let Some(state) = git.blame.get_mut(&path)
                {
                    *state = match blame {
                        Ok(blame) => BlameState::Ready(Arc::new(blame)),
                        Err(_) => BlameState::Unavailable,
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Blame for the active editor's cursor line.
    pub(crate) fn line_blame(&self, cx: &gpui::App) -> Option<LineBlame> {
        let git = self.git.as_ref()?;
        let editor = self.active_editor()?;
        let editor = editor.read(cx);
        let path = editor.path()?;
        let base_row = match editor.base_row(editor.cursor_row()) {
            BaseRow::Unknown => return None,
            BaseRow::Uncommitted => {
                return Some(LineBlame {
                    text: "Not committed yet".into(),
                    tooltip: "This line was added or changed since the last commit.".into(),
                    oid: None,
                });
            }
            BaseRow::Row(row) => row,
        };
        let BlameState::Ready(blame) = git.blame.get(path)? else {
            return None;
        };
        let commit = blame.commit_for_line(base_row)?;
        let age = git::relative_time(commit.author_time, git::now());
        Some(LineBlame {
            text: format!(
                "{}, {age} · {} · {}",
                commit.author,
                commit.short_oid(),
                commit.summary
            )
            .into(),
            tooltip: format!(
                "{}\n{} <{}>, {age}\n\n{}\n\nClick to open the commit.",
                commit.oid, commit.author, commit.author_mail, commit.summary
            )
            .into(),
            oid: Some(commit.oid.clone()),
        })
    }

    // ---- diff tabs -----------------------------------------------------------

    pub(crate) fn open_diff(
        &mut self,
        target: DiffTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(git) = &self.git else {
            return;
        };
        if let Some((pane, ix)) = self.tab_for_diff(&target) {
            self.activate_item(pane, ix, window, cx);
            return;
        }
        let load = cx.background_spawn(load_diff(
            git.repo.clone(),
            git.paths.clone(),
            target.clone(),
            self.filesystem.clone(),
        ));
        cx.spawn_in(window, async move |this, cx| {
            let content = load.await;
            this.update_in(cx, |this, window, cx| match content {
                Ok(content) => this.show_diff_tab(target, content, window, cx),
                Err(err) => {
                    this.status = Some(format!("Can't show the diff: {err}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn tab_for_diff(&self, target: &DiffTarget) -> Option<(PaneId, usize)> {
        let (pane, item) = self
            .items()
            .find(|(_, item)| item.diff().is_some_and(|diff| diff.target == *target))?;
        Some((pane, self.panes[&pane].position(item.id())?))
    }

    /// Opens a diff tab. Diff tabs work like previews: a new file diff
    /// replaces the open one (and a commit replaces the open commit), so
    /// browsing changes doesn't pile up tabs.
    fn show_diff_tab(
        &mut self,
        target: DiffTarget,
        content: DiffContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, ix)) = self.tab_for_diff(&target) {
            self.activate_item(pane, ix, window, cx);
            return;
        }
        let diff = DiffTab {
            target: target.clone(),
            jumps: content.jumps,
            reverts: content.reverts,
        };
        let editor = cx.new(|cx| {
            let mut editor = Editor::diff_view(
                content.title.clone(),
                &content.text,
                content.rows,
                content.language_hint,
                cx,
            );
            editor.set_revert_rows(diff.revert_rows(), cx);
            editor
        });
        let item = self.editor_item(editor, Some(diff), window, cx);
        let preview = self
            .items()
            .find(|(_, item)| {
                item.diff().is_some_and(|diff| {
                    diff.target.is_git() && diff.target.is_commit() == target.is_commit()
                })
            })
            .map(|(pane, item)| (pane, item.id()));
        match preview.and_then(|(pane, id)| Some((pane, self.panes[&pane].position(id)?))) {
            Some((pane, ix)) => {
                self.panes.get_mut(&pane).expect("found above").items[ix] = item;
                self.activate_item(pane, ix, window, cx);
            }
            None => {
                let pane = self.file_pane();
                self.insert_item(pane, item, window, cx);
            }
        }
    }

    /// Reloads open file diff tabs after the status changed.
    fn refresh_diff_tabs(&mut self, cx: &mut Context<Self>) {
        let Some(git) = &self.git else {
            return;
        };
        let tabs: Vec<(Entity<Editor>, DiffTarget)> = self
            .items()
            .filter_map(|(_, item)| {
                let diff = item
                    .diff()
                    .filter(|diff| diff.target.is_git() && !diff.target.is_commit())?;
                Some((item.editor()?.clone(), diff.target.clone()))
            })
            .collect();
        for (editor, target) in tabs {
            let load = cx.background_spawn(load_diff(
                git.repo.clone(),
                git.paths.clone(),
                target,
                self.filesystem.clone(),
            ));
            cx.spawn(async move |this, cx| {
                let Ok(content) = load.await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    let Some((pane, ix)) = this.find_item(editor.entity_id()) else {
                        return;
                    };
                    let mut revert_rows = Vec::new();
                    if let Some(pane) = this.panes.get_mut(&pane)
                        && let ItemKind::Editor {
                            diff: Some(diff), ..
                        } = &mut pane.items[ix].kind
                    {
                        diff.jumps = content.jumps;
                        diff.reverts = content.reverts;
                        revert_rows = diff.revert_rows();
                    }
                    editor.update(cx, |editor, cx| {
                        editor.set_diff_content(&content.text, content.rows, cx);
                        editor.set_revert_rows(revert_rows, cx);
                    });
                })
                .ok();
            })
            .detach();
        }
    }

    /// Double-click in a diff opens the file at that line.
    pub(crate) fn jump_from_diff(
        &mut self,
        editor: &Entity<Editor>,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, ix)) = self.find_item(editor.entity_id()) else {
            return;
        };
        let item = &self.panes[&pane].items[ix];
        let Some(Some((path, line))) = item.diff().and_then(|diff| diff.jumps.get(row)) else {
            return;
        };
        if self.filesystem.remote().is_none() && !path.is_file() {
            return;
        }
        let (path, line) = (path.clone(), *line as usize - 1);
        self.open_file_at(path, Some(((line, 0), (line, 0))), window, cx);
    }

    /// The Revert button on a diff hunk: puts the old lines back in the file.
    /// An open editor takes it as an undoable edit (saved unless it already
    /// had unsaved changes); otherwise the file is rewritten on disk.
    pub(crate) fn revert_diff_hunk(
        &mut self,
        editor: &Entity<Editor>,
        row: usize,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, ix)) = self.find_item(editor.entity_id()) else {
            return;
        };
        let Some(HunkRevert { path, revert, .. }) = self.panes[&pane].items[ix]
            .diff()
            .and_then(|diff| diff.reverts.iter().find(|revert| revert.row == row))
            .cloned()
        else {
            return;
        };
        if let Some((pane, ix)) = self.find_file(&path, cx)
            && let Some(file_editor) = self.panes[&pane].items[ix].editor().cloned()
        {
            let reverted = file_editor.update(cx, |editor, cx| {
                let text = revert.apply(&editor.text())?;
                let was_dirty = editor.is_dirty();
                editor.apply_text(&text, cx);
                if !was_dirty {
                    editor.save(cx).detach();
                }
                Some(())
            });
            if reverted.is_none() {
                self.status = Some(CHANGED_AGAIN.into());
                cx.notify();
            }
            return;
        }
        let filesystem = self.filesystem.clone();
        let write = cx.background_spawn(async move {
            let loaded = filesystem.load_text(&path).ok()?;
            let text = revert.apply(&loaded.text)?;
            filesystem
                .save_text(&path, loaded.has_bom, [text.as_str()])
                .ok()
        });
        cx.spawn(async move |this, cx| {
            let done = write.await.is_some();
            this.update(cx, |this, cx| {
                if !done {
                    this.status = Some(CHANGED_AGAIN.into());
                }
                this.refresh_git_status(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- branches ------------------------------------------------------------

    pub(crate) fn show_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(git) = &self.git else {
            return;
        };
        let repo = git.repo.clone();
        let current = git.status.branch.head.clone();
        let branches = cx.background_spawn(async move { repo.branches() });
        cx.spawn_in(window, async move |this, cx| {
            let branches = branches.await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                let picker = cx.new(|cx| BranchPicker::new(branches, current, window, cx));
                let subscription =
                    cx.subscribe_in(&picker, window, |this, _, event, window, cx| match event {
                        BranchPickerEvent::Switch { name, create } => {
                            let (name, create) = (name.clone(), *create);
                            this.dismiss_modal(window, cx);
                            this.switch_branch(name, create, cx);
                        }
                        BranchPickerEvent::Dismissed => this.dismiss_modal(window, cx),
                    });
                this.set_modal(Modal::Branches(picker), subscription, cx);
            })
            .ok();
        })
        .detach();
    }

    fn switch_branch(&mut self, name: String, create: bool, cx: &mut Context<Self>) {
        let Some(git) = &self.git else {
            return;
        };
        let repo = git.repo.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn({
                    let name = name.clone();
                    async move { repo.switch_branch(&name, create) }
                })
                .await;
            this.update(cx, |this, cx| {
                this.status = Some(match result {
                    Ok(()) => format!("Switched to {name}").into(),
                    Err(err) => format!("Can't switch to {name}: {}", first_line(&err)).into(),
                });
                this.refresh_git_status(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn first_line(err: &GitError) -> String {
    let message = err.to_string();
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches("error: ")
        .to_owned()
}

/// Builds a diff tab's contents (blocks on git; run in the background).
async fn load_diff(
    repo: Arc<Repository>,
    paths: PathMap,
    target: DiffTarget,
    filesystem: project::FileSystem,
) -> git::Result<DiffContent> {
    let absolute = |relative: &str| paths.absolute(relative);
    Ok(match &target {
        DiffTarget::Unstaged(path) => {
            diff_view::file_diff(&target, &repo.diff_unstaged(path)?, &absolute)
        }
        DiffTarget::Staged(path) => {
            diff_view::file_diff(&target, &repo.diff_staged(path)?, &absolute)
        }
        DiffTarget::Untracked(path) => {
            let file = match filesystem.load_text(&paths.absolute(path)) {
                Ok(loaded) => diff_view::untracked_file(path, &loaded.text),
                Err(_) => git::FileDiff {
                    new_path: Some(path.clone()),
                    binary: true,
                    ..Default::default()
                },
            };
            diff_view::file_diff(&target, &[file], &absolute)
        }
        DiffTarget::Commit(oid) => diff_view::commit(&repo.commit_details(oid)?, &absolute),
        // Built by follow mode and agents, never loaded from git.
        DiffTarget::Follow | DiffTarget::Proposal(_) => {
            diff_view::file_diff(&target, &[], &absolute)
        }
    })
}
