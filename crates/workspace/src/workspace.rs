use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use editor::Editor;
use file_tree::{FileTree, FileTreeEvent};
use gpui::{
    AppContext, Context, CursorStyle, Entity, EntityId, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, PathPromptOptions, Pixels, PromptLevel, Render, SharedString, Styled,
    Subscription, Task, Window, actions, div, prelude::FluentBuilder, px,
};
use project::{FileIndex, FileSystem, FsWatcher};

use crate::branch_picker::BranchPicker;
use crate::diff_view::DiffTarget;
use crate::find_bar::{FindBar, FindBarEvent};
use crate::git_panel::{GitPanel, GitPanelEvent};
use crate::git_state::GitState;
use crate::palette::{Palette, PaletteEvent};
use crate::pane::{Axis, Pane, PaneId, PaneNode, Region, Side};
use crate::pane_view::{LayoutBounds, Resize};
use crate::project_search::{ProjectSearch, ProjectSearchEvent};
use crate::prompt::{InputPrompt, InputPromptEvent};
use crate::session;

actions!(
    workspace,
    [
        OpenFolder,
        ConnectSsh,
        DisconnectSsh,
        RefreshProject,
        NewFile,
        SaveAs,
        CloseTab,
        NextTab,
        PreviousTab,
        ToggleSidebar,
        ShowFiles,
        ToggleTerminal,
        NewTerminal,
        ToggleFileFinder,
        Find,
        FindReplace,
        SearchProject,
        ShowGit,
        ShowAgents,
        ShowProblems,
        RunTask,
        StopFollowing,
        SendToAgent,
        OpenMarkdownPreview,
        SplitRight,
        SplitDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        ToggleMaximizePane,
        CommandPalette,
        SwitchBranch,
        OpenSettings,
        OpenSettingsFile,
        UseDarkTheme,
        UseLightTheme,
        UseSystemTheme,
        GoToLine,
        GitFetch,
        DiscardAllChanges,
        GitPull,
        GitPush,
        ReopenClosedTab,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        Quit
    ]
);

/// Opens an installed agent harness in a new terminal tab.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct NewAgent(pub terminal::HarnessKind);

const CONTEXT: Option<&str> = Some("Workspace");
pub(crate) const SIDEBAR_WIDTH: f32 = 260.;
const SIDEBAR_MIN_WIDTH: f32 = 180.;
const SIDEBAR_MAX_WIDTH: f32 = 600.;
pub(crate) const BAR_HEIGHT: f32 = 34.;
const DOCK_MIN_HEIGHT: f32 = 120.;
pub(crate) const DOCK_HEIGHT: f32 = 300.;
const EDITOR_MIN_HEIGHT: f32 = 160.;
const RESIZE_HANDLE: f32 = 5.;

pub fn key_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("secondary-o", OpenFolder, CONTEXT),
        KeyBinding::new("secondary-n", NewFile, CONTEXT),
        KeyBinding::new("secondary-shift-s", SaveAs, CONTEXT),
        KeyBinding::new("secondary-w", CloseTab, CONTEXT),
        KeyBinding::new("ctrl-tab", NextTab, CONTEXT),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, CONTEXT),
        KeyBinding::new("secondary-b", ToggleSidebar, CONTEXT),
        KeyBinding::new("secondary-shift-e", ShowFiles, CONTEXT),
        KeyBinding::new("ctrl-`", ToggleTerminal, CONTEXT),
        KeyBinding::new("ctrl-shift-`", NewTerminal, CONTEXT),
        // Some layouts report Shift+` as ~.
        KeyBinding::new("ctrl-~", NewTerminal, CONTEXT),
        KeyBinding::new("ctrl-shift-~", NewTerminal, CONTEXT),
        KeyBinding::new("secondary-p", ToggleFileFinder, CONTEXT),
        KeyBinding::new("secondary-f", Find, CONTEXT),
        KeyBinding::new("secondary-shift-f", SearchProject, CONTEXT),
        KeyBinding::new("secondary-shift-g", ShowGit, CONTEXT),
        KeyBinding::new("secondary-shift-a", ShowAgents, CONTEXT),
        KeyBinding::new("secondary-shift-m", ShowProblems, CONTEXT),
        KeyBinding::new("secondary-alt-k", SendToAgent, CONTEXT),
        KeyBinding::new("secondary-shift-v", OpenMarkdownPreview, CONTEXT),
        KeyBinding::new("secondary-\\", SplitRight, CONTEXT),
        KeyBinding::new("secondary-shift-\\", SplitDown, CONTEXT),
        // Shift+\ arrives as | on most layouts.
        KeyBinding::new("secondary-|", SplitDown, CONTEXT),
        KeyBinding::new("secondary-shift-|", SplitDown, CONTEXT),
        KeyBinding::new("ctrl-alt-left", FocusPaneLeft, CONTEXT),
        KeyBinding::new("ctrl-alt-right", FocusPaneRight, CONTEXT),
        KeyBinding::new("ctrl-alt-up", FocusPaneUp, CONTEXT),
        KeyBinding::new("ctrl-alt-down", FocusPaneDown, CONTEXT),
        KeyBinding::new("secondary-shift-enter", ToggleMaximizePane, CONTEXT),
        KeyBinding::new("secondary-shift-p", CommandPalette, CONTEXT),
        KeyBinding::new("f1", CommandPalette, CONTEXT),
        KeyBinding::new("secondary-,", OpenSettings, CONTEXT),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("ctrl-g", GoToLine, CONTEXT),
        KeyBinding::new("secondary-shift-t", ReopenClosedTab, CONTEXT),
        KeyBinding::new("secondary-=", ZoomIn, CONTEXT),
        KeyBinding::new("secondary-+", ZoomIn, CONTEXT),
        KeyBinding::new("secondary--", ZoomOut, CONTEXT),
        KeyBinding::new("secondary-0", ResetZoom, CONTEXT),
    ];
    bindings.push(if cfg!(target_os = "macos") {
        KeyBinding::new("cmd-alt-f", FindReplace, CONTEXT)
    } else {
        KeyBinding::new("ctrl-h", FindReplace, CONTEXT)
    });
    bindings
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarMode {
    Files,
    Search,
    Git,
    Agents,
    Problems,
}

/// What a text prompt is for.
pub(crate) enum PromptPurpose {
    NewFile {
        dir: PathBuf,
    },
    NewFolder {
        dir: PathBuf,
    },
    Rename {
        path: PathBuf,
    },
    RenameTerminal {
        item: EntityId,
    },
    SaveRemote {
        editor: Entity<Editor>,
        answer: futures::channel::oneshot::Sender<bool>,
    },
    GoToLine {
        editor: Entity<Editor>,
    },
}

pub(crate) enum Modal {
    Palette(Entity<Palette>),
    Prompt(Entity<InputPrompt>),
    Branches(Entity<BranchPicker>),
    Ssh(Entity<crate::ssh_view::SshView>),
}

/// Which popup menu is open (only one is, at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKind {
    App,
    Project,
    Pane,
    Tab,
    Tree,
    Shell,
    Agent,
    Run,
}

/// Root view of an Ion window.
pub struct Workspace {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) root: Option<PathBuf>,
    pub(crate) filesystem: FileSystem,
    pub(crate) remote_watch: Option<project::RemoteWatcher>,
    /// The SSH connection's state, in remote windows.
    pub(crate) remote_state: Option<remote::ConnectionState>,
    /// Login questions and state changes from the connection.
    pub(crate) remote_tasks: Vec<Task<()>>,
    pub(crate) file_tree: Option<Entity<FileTree>>,
    /// Every pane, by id. The trees below say where each one is.
    pub(crate) panes: HashMap<PaneId, Pane>,
    pub(crate) next_pane_id: PaneId,
    /// The editor area.
    pub(crate) center: PaneNode,
    /// The terminal dock under the editor area.
    pub(crate) dock: PaneNode,
    pub(crate) dock_visible: bool,
    pub(crate) dock_height: Pixels,
    /// The pane with keyboard focus.
    pub(crate) active_pane: PaneId,
    /// The last focused pane showing an editor (find, save, blame use it).
    pub(crate) editor_pane: PaneId,
    /// The editor the tree, find bar and blame were last pointed at.
    pub(crate) shown_editor: Option<EntityId>,
    /// The last focused dock pane, where new terminals go.
    pub(crate) last_dock_pane: PaneId,
    /// A pane temporarily filling the whole window.
    pub(crate) zoomed: Option<PaneId>,
    pub(crate) layout_bounds: Rc<RefCell<LayoutBounds>>,
    pub(crate) resize: Option<Resize>,
    /// The open popup menu.
    pub(crate) menu: Option<ui::Menu>,
    pub(crate) menu_kind: Option<MenuKind>,
    pub(crate) sidebar_visible: bool,
    pub(crate) sidebar_width: Pixels,
    pub(crate) sidebar_mode: SidebarMode,
    /// The agent the editor follows.
    pub(crate) following: Option<crate::agents::Following>,
    /// Files each agent terminal changed, oldest first.
    pub(crate) touched: HashMap<EntityId, Vec<PathBuf>>,
    /// Problems read from each terminal's output.
    pub(crate) problems: HashMap<EntityId, crate::problems::TerminalProblems>,
    pub(crate) problem_scans: HashMap<EntityId, Task<()>>,
    /// New diff tabs open side by side (the last choice).
    pub(crate) diff_split: bool,
    /// The terminal each Run task's last run is in, by command.
    pub(crate) task_terminals: HashMap<String, EntityId>,
    /// Last error to show in the status bar.
    pub(crate) status: Option<SharedString>,
    pub(crate) find_bar: Entity<FindBar>,
    pub(crate) find_visible: bool,
    pub(crate) project_search: Entity<ProjectSearch>,
    pub(crate) git_panel: Entity<GitPanel>,
    /// The project's repository, once found.
    pub(crate) git: Option<GitState>,
    pub(crate) git_discovery: Option<Task<()>>,
    pub(crate) modal: Option<Modal>,
    /// Events from the open modal; dropped with it.
    modal_subscription: Option<Subscription>,
    pub(crate) file_clipboard: Option<crate::file_ops::FileClipboard>,
    pub(crate) file_index: Option<Arc<FileIndex>>,
    pub(crate) index_task: Option<Task<()>>,
    /// Files changed while the index was being built; build again after.
    pub(crate) index_stale: bool,
    pub(crate) watcher: Option<FsWatcher>,
    pub(crate) watch_task: Option<Task<()>>,
    /// Files with a "changed on disk" prompt open, so it isn't asked twice.
    pub(crate) conflicts: std::collections::HashSet<PathBuf>,
    /// Pending "save after a delay" timers, per editor.
    pub(crate) auto_save_tasks: HashMap<EntityId, Task<()>>,
    /// Recently closed files, newest last (Ctrl+Shift+T reopens).
    pub(crate) closed_files: Vec<PathBuf>,
    /// The saved layout is still loading; don't overwrite it yet.
    pub(crate) restoring: bool,
    /// Edits agents proposed, waiting for Accept or Reject.
    pub(crate) proposals: Vec<crate::ide::Proposal>,
    pub(crate) next_proposal: u64,
    /// Waits for the selection to rest before telling agents.
    pub(crate) ide_selection_task: Option<Task<()>>,
    _file_tree_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Ask before closing the window with unsaved changes.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |workspace, cx| workspace.confirm_quit(window, cx))
                .unwrap_or(true)
        });

        // Follow the system light/dark mode when the theme is "System".
        let appearance = window.observe_window_appearance(|_, cx| settings::reapply(cx));
        // Auto save on focus change also covers switching to another app.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.auto_save_all(cx);
            }
        });

        // Load the shared session once, before anything renders.
        session::get(cx);
        let find_bar = cx.new(FindBar::new);
        let project_search = cx.new(ProjectSearch::new);
        let git_panel = cx.new(GitPanel::new);
        let subscriptions = vec![
            cx.subscribe_in(
                &git_panel,
                window,
                |this, _, event, window, cx| match event {
                    GitPanelEvent::OpenDiff(target) => this.open_diff(target.clone(), window, cx),
                    GitPanelEvent::OpenCommit(oid) => {
                        this.open_diff(DiffTarget::Commit(oid.clone()), window, cx)
                    }
                    GitPanelEvent::OpenFile(relative) => {
                        if let Some(git) = &this.git {
                            let path = git.paths.absolute(relative);
                            this.open_file(path, window, cx);
                        }
                    }
                    GitPanelEvent::Changed => this.refresh_git_status(cx),
                    GitPanelEvent::InitRepository => this.init_repository(window, cx),
                },
            ),
            cx.subscribe_in(
                &find_bar,
                window,
                |this, _, event, window, cx| match event {
                    FindBarEvent::Dismissed => this.close_find_bar(window, cx),
                },
            ),
            cx.subscribe_in(
                &project_search,
                window,
                |this, _, event, window, cx| match event {
                    ProjectSearchEvent::Open {
                        path,
                        line,
                        columns,
                    } => {
                        let selection = ((*line, columns.start), (*line, columns.end));
                        this.open_file_at(path.clone(), Some(selection), window, cx);
                    }
                },
            ),
        ];

        let mut panes = HashMap::new();
        panes.insert(1, Pane::default());
        panes.insert(2, Pane::default());
        Self {
            focus_handle: cx.focus_handle(),
            root: None,
            filesystem: FileSystem::Local,
            remote_watch: None,
            remote_state: None,
            remote_tasks: Vec::new(),
            file_tree: None,
            panes,
            next_pane_id: 2,
            center: PaneNode::Pane(1),
            dock: PaneNode::Pane(2),
            dock_visible: false,
            dock_height: px(DOCK_HEIGHT),
            active_pane: 1,
            editor_pane: 1,
            shown_editor: None,
            last_dock_pane: 2,
            zoomed: None,
            layout_bounds: Rc::default(),
            resize: None,
            menu: None,
            menu_kind: None,
            sidebar_visible: true,
            sidebar_width: px(SIDEBAR_WIDTH),
            sidebar_mode: SidebarMode::Files,
            following: None,
            touched: HashMap::new(),
            problems: HashMap::new(),
            problem_scans: HashMap::new(),
            task_terminals: HashMap::new(),
            diff_split: false,
            status: None,
            find_bar,
            find_visible: false,
            project_search,
            git_panel,
            git: None,
            git_discovery: None,
            modal: None,
            modal_subscription: None,
            file_clipboard: None,
            file_index: None,
            index_task: None,
            index_stale: false,
            watcher: None,
            watch_task: None,
            conflicts: Default::default(),
            restoring: false,
            proposals: Vec::new(),
            next_proposal: 0,
            ide_selection_task: None,
            closed_files: Vec::new(),
            _file_tree_subscription: None,
            auto_save_tasks: HashMap::new(),
            _subscriptions: subscriptions
                .into_iter()
                .chain([appearance, activation])
                .collect(),
        }
    }

    /// Opens a folder as the project, or a file inside its parent folder.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.filesystem.remote().is_some() {
            Self::open_local_window(path, cx);
            return;
        }
        let path = path.canonicalize().map(simplify_path).unwrap_or(path);
        if path.is_dir() {
            self.set_root(path, window, cx);
        } else {
            if let Some(parent) = path.parent() {
                self.set_root(parent.to_path_buf(), window, cx);
            }
            self.open_file(path, window, cx);
        }
    }

    pub(crate) fn set_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let suffix = self
            .filesystem
            .remote()
            .map(|connection| format!(" [SSH: {}]", connection.label()))
            .unwrap_or_default();
        window.set_window_title(&format!("{name}{suffix} — Ion"));
        let file_tree =
            cx.new(|cx| FileTree::with_filesystem(root.clone(), self.filesystem.clone(), cx));
        self._file_tree_subscription = Some(cx.subscribe_in(
            &file_tree,
            window,
            |this, _, event, window, cx| match event {
                FileTreeEvent::OpenFile { path, focus } => {
                    this.open_file_with(path.clone(), None, *focus, window, cx)
                }
                FileTreeEvent::ContextMenu { entries, position } => {
                    this.show_tree_menu(entries.clone(), *position, cx)
                }
                FileTreeEvent::Delete(paths) => this.delete_paths(paths.clone(), window, cx),
                FileTreeEvent::Rename(path) => this.rename_path(path.clone(), window, cx),
                FileTreeEvent::Error(error) => {
                    this.status = Some(error.clone().into());
                    cx.notify();
                }
            },
        ));
        self.file_tree = Some(file_tree);
        self.root = Some(root.clone());
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Files;
        self.file_index = None;
        self.project_search.update(cx, |search, cx| {
            search.set_filesystem(self.filesystem.clone());
            search.set_index(None, cx);
        });
        self.rebuild_index(false, cx);
        self.start_watching(&root, window, cx);
        self.discover_git(root.clone(), window, cx);
        match self.filesystem.remote() {
            Some(connection) => {
                let project = session::RemoteProject {
                    connection: connection.options().clone(),
                    folder: root.clone(),
                };
                session::update(cx, |session| session.add_recent_remote(project));
            }
            None => session::update(cx, |session| session.add_recent(&root)),
        }
        self.save_session(cx);
        crate::ide::refresh_folders(cx);
        cx.notify();
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        // In a remote window, folders come from the server.
        if self.filesystem.remote().is_some() {
            self.pick_remote_folder(window, cx);
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await
                && let Some(path) = paths.pop()
            {
                this.update_in(cx, |this, window, cx| this.open_path(path, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    pub(crate) fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_at(path, None, window, cx);
    }

    /// Opens (or switches to) a file, optionally selecting a range given as
    /// zero-based (line, column) positions.
    pub(crate) fn open_file_at(
        &mut self,
        path: PathBuf,
        selection: Option<((usize, usize), (usize, usize))>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file_with(path, selection, true, window, cx);
    }

    /// Opens a file in a new pane to the right of the current one.
    pub(crate) fn open_file_to_side(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((pane, ix)) = self.find_file(&path, cx) {
            self.activate_item(pane, ix, window, cx);
            return;
        }
        let target = self.file_pane();
        let new = self.new_pane();
        self.center.split(target, new, Side::Right);
        self.editor_pane = new;
        self.zoomed = None;
        self.open_file_with(path, None, true, window, cx);
    }

    /// Opens a file; with `focus` false the keyboard stays where it is.
    pub(crate) fn open_file_with(
        &mut self,
        path: PathBuf,
        selection: Option<((usize, usize), (usize, usize))>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let filesystem = self.filesystem.clone();
        self.open_file_on(path, filesystem, selection, focus, window, cx);
    }

    /// Opens a file from `filesystem` (local files can open in a remote
    /// window, like settings.json).
    pub(crate) fn open_file_on(
        &mut self,
        path: PathBuf,
        filesystem: FileSystem,
        selection: Option<((usize, usize), (usize, usize))>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let select = move |editor: &Entity<Editor>, cx: &mut gpui::App| {
            if let Some((start, end)) = selection {
                editor.update(cx, |editor, cx| editor.select_range(start, end, cx));
            }
        };
        if let Some((pane, ix)) = self.find_file(&path, cx) {
            self.show_item(pane, ix, focus, window, cx);
            if let Some(editor) = self.panes[&pane].items[ix].editor().cloned() {
                select(&editor, cx);
            }
            return;
        }
        if matches!(filesystem, FileSystem::Local) && crate::image_view::is_image(&path) {
            let view = cx.new(|cx| crate::image_view::ImageView::new(path, cx));
            let item = crate::pane::Item {
                kind: crate::pane::ItemKind::Image(view),
                _subscriptions: Vec::new(),
            };
            let pane = self.file_pane();
            self.insert_item_with_focus(pane, item, focus, window, cx);
            return;
        }
        let load = cx.background_spawn({
            let path = path.clone();
            let filesystem = filesystem.clone();
            async move { filesystem.load_text(&path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let loaded = load.await;
            this.update_in(cx, |this, window, cx| match loaded {
                Ok(loaded) => {
                    // Opened twice while loading: just show the first one.
                    if let Some((pane, ix)) = this.find_file(&path, cx) {
                        this.show_item(pane, ix, focus, window, cx);
                        return;
                    }
                    let editor = cx.new(|cx| {
                        let mut editor = Editor::new(&loaded.text, Some(path), loaded.has_bom, cx);
                        editor.set_filesystem(filesystem);
                        editor.set_disk_hash(loaded.hash);
                        editor
                    });
                    let item = this.editor_item(editor.clone(), None, window, cx);
                    let pane = this.file_pane();
                    this.insert_item_with_focus(pane, item, focus, window, cx);
                    select(&editor, cx);
                }
                Err(err) => {
                    this.status = Some(format!("Can't open {}: {err}", path.display()).into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Adds an editor tab to the pane where files open.
    pub(crate) fn open_editor(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = self.editor_item(editor, None, window, cx);
        let pane = self.file_pane();
        self.insert_item(pane, item, window, cx);
    }

    pub(crate) fn project_search_set_index(
        &mut self,
        index: Option<Arc<FileIndex>>,
        cx: &mut Context<Self>,
    ) {
        self.project_search
            .update(cx, |search, cx| search.set_index(index, cx));
    }

    pub(crate) fn save_editor(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        if editor.read(cx).path().is_some() {
            editor.update(cx, |editor, cx| editor.save(cx))
        } else {
            self.save_editor_as(editor, window, cx)
        }
    }

    /// Asks where to save, then saves there.
    fn save_editor_as(
        &mut self,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let current = editor.read(cx).path().map(Path::to_path_buf);
        if editor.read(cx).filesystem().remote().is_some() {
            let initial = current
                .or_else(|| self.root.clone().map(|root| root.join("untitled")))
                .map(|path| remote::posix_path(&path).to_string_lossy().into_owned())
                .unwrap_or_default();
            let len = initial.chars().count();
            let (answer, receive) = futures::channel::oneshot::channel();
            self.show_prompt(
                "Save file on remote server".into(),
                &initial,
                0..len,
                PromptPurpose::SaveRemote { editor, answer },
                window,
                cx,
            );
            return cx.spawn(async move |_, _| receive.await.unwrap_or(false));
        }
        let dir = current
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.root.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let name = editor.read(cx).title().to_string();
        let chosen = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return false;
            };
            let Ok(save) = editor.update(cx, |editor, cx| {
                editor.set_path(path, cx);
                editor.save(cx)
            }) else {
                return false;
            };
            let saved = save.await;
            this.update_in(cx, |this, window, cx| {
                if let Some((pane, ix)) = this.find_item(editor.entity_id()) {
                    this.activate_item(pane, ix, window, cx);
                }
                this.save_session(cx);
            })
            .ok();
            saved
        })
    }

    /// Called when the window is about to close. Returns whether to close now.
    pub(crate) fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.save_session(cx);
        let dirty = self.items().filter(|(_, item)| item.is_dirty(cx)).count();
        if dirty == 0 {
            return true;
        }
        let message = format!(
            "You have unsaved changes in {dirty} file{}.",
            if dirty == 1 { "" } else { "s" }
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Quit anyway and lose them?"),
            &["Quit Without Saving", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(0) = answer.await {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
        false
    }

    // ---- actions -------------------------------------------------------------

    pub(crate) fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new("", None, false, cx);
            editor.set_filesystem(self.filesystem.clone());
            editor
        });
        self.open_editor(editor, window, cx);
    }

    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor().filter(|e| !e.read(cx).is_read_only()) {
            self.save_editor_as(editor, window, cx).detach();
        }
    }

    /// The editor's Save bubbles up here for untitled files.
    fn save_untitled(&mut self, _: &editor::Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor().filter(|e| !e.read(cx).is_read_only()) {
            self.save_editor(editor, window, cx).detach();
        }
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        match self.panes.get(&pane) {
            // Ctrl+W in an empty split closes the split.
            Some(p) if p.items.is_empty() => self.close_pane(pane, window, cx),
            Some(p) => {
                let ix = p.active;
                self.close_item(pane, ix, window, cx);
            }
            None => {}
        }
    }

    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane;
        let Some(target) = self.panes.get(&pane) else {
            return;
        };
        let len = target.items.len() as isize;
        if len > 0 {
            let ix = (target.active as isize + delta).rem_euclid(len) as usize;
            self.activate_item(pane, ix, window, cx);
        }
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(1, window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(-1, window, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        self.save_session(cx);
        cx.notify();
    }

    fn show_files(&mut self, _: &ShowFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Files;
        cx.notify();
    }

    fn show_git(&mut self, _: &ShowGit, window: &mut Window, cx: &mut Context<Self>) {
        if self.root.is_none() {
            return;
        }
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Git;
        self.git_panel
            .update(cx, |panel, cx| panel.focus(window, cx));
        cx.notify();
    }

    fn show_agents(&mut self, _: &ShowAgents, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Agents;
        cx.notify();
    }

    fn show_problems_action(&mut self, _: &ShowProblems, _: &mut Window, cx: &mut Context<Self>) {
        self.show_problems(cx);
    }

    fn run_task_action(&mut self, _: &RunTask, window: &mut Window, cx: &mut Context<Self>) {
        self.show_run_menu_at_title(window, cx);
    }

    fn stop_following_action(
        &mut self,
        _: &StopFollowing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_following(window, cx);
    }

    fn search_project(&mut self, _: &SearchProject, window: &mut Window, cx: &mut Context<Self>) {
        if self.root.is_none() {
            return;
        }
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Search;
        let selected = self
            .active_editor()
            .map(|editor| editor.read(cx).selected_text())
            .filter(|text| !text.is_empty() && !text.contains('\n'));
        self.project_search
            .update(cx, |search, cx| search.focus(selected, window, cx));
        cx.notify();
    }

    fn toggle_file_finder(
        &mut self,
        _: &ToggleFileFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_palette("", window, cx);
    }

    fn command_palette(&mut self, _: &CommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_palette(">", window, cx);
    }

    /// Opens the palette for files ("") or commands (">"), or closes it.
    fn toggle_palette(&mut self, prefix: &str, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Palette(_))) {
            self.dismiss_modal(window, cx);
            return;
        }
        let index = self.file_index.clone();
        let remote = self.filesystem.remote().is_some();
        let palette = cx.new(|cx| Palette::new(index, prefix, remote, window, cx));
        let subscription =
            cx.subscribe_in(&palette, window, |this, _, event, window, cx| match event {
                PaletteEvent::OpenFile(path) => {
                    let path = path.clone();
                    this.dismiss_modal(window, cx);
                    this.open_file(path, window, cx);
                }
                PaletteEvent::Run(action) => {
                    let action = action.boxed_clone();
                    // Give focus back first, so the command acts on the
                    // editor or terminal it was opened from.
                    this.dismiss_modal(window, cx);
                    window.dispatch_action(action, cx);
                }
                PaletteEvent::Dismissed => this.dismiss_modal(window, cx),
            });
        self.set_modal(Modal::Palette(palette), subscription, cx);
    }

    /// Shows a popup menu, replacing any open one.
    pub(crate) fn open_menu(&mut self, kind: MenuKind, menu: ui::Menu, cx: &mut Context<Self>) {
        self.menu = Some(menu);
        self.menu_kind = Some(kind);
        cx.notify();
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.menu_kind = None;
            cx.notify();
        }
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings(window, cx);
    }

    fn open_settings_file(
        &mut self,
        _: &OpenSettingsFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = settings::settings_file() else {
            return;
        };
        if !path.exists() {
            settings::save(settings::get(cx)).ok();
        }
        // settings.json is always local, even in a remote window.
        self.open_file_on(path, FileSystem::Local, None, true, window, cx);
    }

    pub(crate) fn set_modal(
        &mut self,
        modal: Modal,
        subscription: Subscription,
        cx: &mut Context<Self>,
    ) {
        self.modal = Some(modal);
        self.modal_subscription = Some(subscription);
        cx.notify();
    }

    /// Shows a one-line text prompt for a file operation.
    pub(crate) fn show_prompt(
        &mut self,
        title: String,
        initial: &str,
        selection: std::ops::Range<usize>,
        purpose: PromptPurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.new(|cx| InputPrompt::new(title, initial, selection, window, cx));
        let mut purpose = Some(purpose);
        let subscription =
            cx.subscribe_in(
                &prompt,
                window,
                move |this, _, event, window, cx| match event {
                    InputPromptEvent::Confirmed(text) => {
                        this.dismiss_modal(window, cx);
                        if let Some(purpose) = purpose.take() {
                            this.finish_prompt(purpose, text, window, cx);
                        }
                    }
                    InputPromptEvent::Dismissed => this.dismiss_modal(window, cx),
                },
            );
        self.set_modal(Modal::Prompt(prompt), subscription, cx);
    }

    pub(crate) fn dismiss_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal_subscription = None;
        if self.modal.take().is_some() {
            self.focus_editor(window, cx);
            cx.notify();
        }
    }

    /// Ctrl+G: asks for a line (or line:column) and jumps there.
    fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        let (row, _) = editor.read(cx).cursor_position();
        let lines = editor.read(cx).line_count();
        let current = (row + 1).to_string();
        let len = current.len();
        self.show_prompt(
            format!("Go to line (1–{lines}), or line:column"),
            &current,
            0..len,
            PromptPurpose::GoToLine { editor },
            window,
            cx,
        );
    }

    fn find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        self.show_find_bar(false, window, cx);
    }

    fn find_replace(&mut self, _: &FindReplace, window: &mut Window, cx: &mut Context<Self>) {
        self.show_find_bar(true, window, cx);
    }

    fn show_find_bar(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        self.find_visible = true;
        self.find_bar
            .update(cx, |bar, cx| bar.show(editor, replace, window, cx));
        cx.notify();
    }

    pub(crate) fn close_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.find_visible {
            return;
        }
        self.find_visible = false;
        self.find_bar.update(cx, |bar, cx| bar.clear(cx));
        self.focus_editor(window, cx);
        cx.notify();
    }

    /// Shows the terminal dock and focuses it, or hides it if it's focused.
    pub(crate) fn toggle_terminal(
        &mut self,
        _: &ToggleTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dock_empty = self
            .dock
            .panes()
            .iter()
            .all(|pane| self.panes.get(pane).is_none_or(|p| p.items.is_empty()));
        if self.dock_visible && self.dock.contains(self.active_pane) {
            self.dock_visible = false;
            if self.zoomed.is_some_and(|pane| self.dock.contains(pane)) {
                self.zoomed = None;
            }
            self.active_pane = self.file_pane();
            self.focus_active(window, cx);
        } else if dock_empty {
            let pane = self.dock.first_pane();
            self.new_terminal_in(pane, window, cx);
        } else {
            self.dock_visible = true;
            let pane = if self.dock.contains(self.last_dock_pane) {
                self.last_dock_pane
            } else {
                self.dock.first_pane()
            };
            let ix = self.panes[&pane].active;
            self.activate_item(pane, ix, window, cx);
        }
        self.layout_changed(cx);
    }

    pub(crate) fn new_terminal(
        &mut self,
        _: &NewTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.terminal_pane();
        self.new_terminal_in(pane, window, cx);
    }

    fn new_agent(&mut self, action: &NewAgent, window: &mut Window, cx: &mut Context<Self>) {
        if self.filesystem.remote().is_some() {
            return;
        }
        let Some(harness) = terminal::available_harnesses()
            .into_iter()
            .find(|harness| harness.kind == action.0)
        else {
            return;
        };
        let pane = self.terminal_pane();
        self.new_agent_in(pane, harness, window, cx);
    }

    fn split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split_active_pane(Side::Right, window, cx);
    }

    fn split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split_active_pane(Side::Down, window, cx);
    }

    fn toggle_maximize(&mut self, _: &ToggleMaximizePane, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_zoom(cx);
    }

    pub(crate) fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active(window, cx);
    }

    /// Focuses the first terminal that rang its bell.
    pub(crate) fn show_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let found = self
            .items()
            .find(|(_, item)| item.needs_attention(cx))
            .map(|(pane, item)| (pane, item.id()));
        if let Some((pane, id)) = found
            && let Some(ix) = self.panes[&pane].position(id)
        {
            self.activate_item(pane, ix, window, cx);
        }
    }

    /// What to show for a terminal that needs attention and isn't on
    /// screen: its notification, or a generic label after a bell.
    pub(crate) fn hidden_attention(&self, cx: &gpui::App) -> Option<SharedString> {
        self.panes.iter().find_map(|(id, pane)| {
            let shown = |ix: usize| {
                ix == pane.active
                    && (self.dock_visible || !self.dock.contains(*id))
                    && self.zoomed.is_none_or(|zoomed| zoomed == *id)
            };
            let (_, item) = pane
                .items
                .iter()
                .enumerate()
                .find(|(ix, item)| item.needs_attention(cx) && !shown(*ix))?;
            Some(
                item.notification(cx)
                    .unwrap_or_else(|| "Terminal needs attention".into()),
            )
        })
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = &self.resize else {
            return;
        };
        match resize {
            Resize::Sidebar => {
                self.sidebar_width = event
                    .position
                    .x
                    .clamp(px(SIDEBAR_MIN_WIDTH), px(SIDEBAR_MAX_WIDTH));
                cx.notify();
            }
            Resize::Dock => {
                let window_height = window.viewport_size().height;
                let chrome = crate::chrome::TITLE_HEIGHT + crate::chrome::STATUS_HEIGHT;
                let max = window_height - px(chrome + BAR_HEIGHT + EDITOR_MIN_HEIGHT);
                let height = window_height - px(crate::chrome::STATUS_HEIGHT) - event.position.y;
                self.dock_height = height.min(max).max(px(DOCK_MIN_HEIGHT));
                cx.notify();
            }
            Resize::Split {
                region,
                path,
                divider,
                axis,
            } => {
                let (region, path, divider) = (*region, path.clone(), *divider);
                let position = match axis {
                    Axis::Row => event.position.x,
                    Axis::Column => event.position.y,
                };
                self.resize_split(region, &path, divider, position, cx);
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.resize.take().is_some() {
            self.layout_changed(cx);
        }
    }

    // ---- rendering -----------------------------------------------------------

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let content: gpui::AnyView = match self.modal.as_ref()? {
            Modal::Palette(palette) => palette.clone().into(),
            Modal::Prompt(prompt) => prompt.clone().into(),
            Modal::Branches(picker) => picker.clone().into(),
            Modal::Ssh(view) => view.clone().into(),
        };
        // Clicking outside the modal dismisses it.
        Some(
            div()
                .id("modal-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(72.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.dismiss_modal(window, cx)),
                )
                .child(
                    div()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(content),
                )
                .into_any_element(),
        )
    }

    /// The editor area and terminal dock, or the maximized pane.
    fn render_main(&self, cx: &mut Context<Self>) -> gpui::Div {
        if let Some(pane) = self.zoomed.filter(|pane| self.panes.contains_key(pane)) {
            let region = self.region_of(pane);
            return div()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.render_pane(pane, region, cx));
        }
        // With no files open, the terminal dock takes the whole area.
        let center_empty = matches!(self.center, PaneNode::Pane(id)
            if self.panes.get(&id).is_none_or(|pane| pane.items.is_empty()));
        let dock_fills = self.dock_visible && center_empty;
        let dock = self.dock_visible.then(|| {
            div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(theme::border())
                .map(|dock| {
                    if dock_fills {
                        dock.flex_1().min_h_0()
                    } else {
                        dock.flex_none().h(self.dock_height)
                    }
                })
                .when(!dock_fills, |dock| {
                    dock.child(
                        div()
                            .id("dock-resize")
                            .h(px(RESIZE_HANDLE))
                            .flex_none()
                            .cursor(CursorStyle::ResizeUpDown)
                            .bg(theme::panel_bg())
                            .hover(|handle| handle.bg(theme::active_bg()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.resize = Some(Resize::Dock);
                                    cx.stop_propagation();
                                }),
                            ),
                    )
                })
                .child(div().flex_1().min_h_0().flex().child(self.render_tree(
                    Region::Dock,
                    &self.dock,
                    Vec::new(),
                    cx,
                )))
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .when(!dock_fills, |column| {
                column.child(div().flex_1().min_h_0().flex().child(self.render_tree(
                    Region::Center,
                    &self.center,
                    Vec::new(),
                    cx,
                )))
            })
            .children(dock)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self
            .file_tree
            .clone()
            .filter(|_| self.sidebar_visible)
            .map(|tree| {
                // Drag the right edge to resize; double-click to reset.
                let handle = div()
                    .id("sidebar-resize")
                    .absolute()
                    .top_0()
                    .right_0()
                    .w(px(RESIZE_HANDLE))
                    .h_full()
                    .cursor(CursorStyle::ResizeLeftRight)
                    .hover(|handle| handle.bg(theme::active_bg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                this.sidebar_width = px(SIDEBAR_WIDTH);
                                this.layout_changed(cx);
                            } else {
                                this.resize = Some(Resize::Sidebar);
                            }
                        }),
                    );
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .child(self.render_sidebar(&tree, cx))
                    .child(handle)
            });
        let main = self.render_main(cx);
        let modal = self.render_modal(cx);
        let this = cx.entity().downgrade();
        let menu = self.menu.as_ref().map(|menu| {
            ui::render_menu(
                menu,
                move |_, cx| {
                    this.update(cx, |this, cx| this.close_menu(cx)).ok();
                },
                window,
            )
        });
        let title_bar = self.render_title_bar(window, cx);
        let resize_cursor = match &self.resize {
            Some(Resize::Sidebar) => Some(CursorStyle::ResizeLeftRight),
            Some(Resize::Dock) => Some(CursorStyle::ResizeUpDown),
            Some(Resize::Split {
                axis: Axis::Row, ..
            }) => Some(CursorStyle::ResizeLeftRight),
            Some(Resize::Split {
                axis: Axis::Column, ..
            }) => Some(CursorStyle::ResizeUpDown),
            None => None,
        };

        div()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .font_family(theme::ui_font())
            .text_size(theme::ui_font_size())
            .text_color(theme::text())
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::connect_ssh))
            .on_action(cx.listener(Self::disconnect_ssh))
            .on_action(cx.listener(Self::refresh_project))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::save_untitled))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::show_files))
            .on_action(cx.listener(Self::search_project))
            .on_action(cx.listener(Self::show_git))
            .on_action(cx.listener(Self::show_agents))
            .on_action(cx.listener(Self::show_problems_action))
            .on_action(cx.listener(Self::run_task_action))
            .on_action(cx.listener(Self::stop_following_action))
            .on_action(cx.listener(|this, _: &OpenMarkdownPreview, window, cx| {
                this.open_markdown_preview(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SendToAgent, window, cx| {
                this.send_selection_to_agent(window, cx)
            }))
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_agent))
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::toggle_maximize))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| {
                this.focus_pane_toward(Side::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| {
                this.focus_pane_toward(Side::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| {
                this.focus_pane_toward(Side::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| {
                this.focus_pane_toward(Side::Down, window, cx)
            }))
            .on_action(cx.listener(Self::toggle_file_finder))
            .on_action(cx.listener(Self::command_palette))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::open_settings_file))
            .on_action(
                cx.listener(|this, _: &SwitchBranch, window, cx| {
                    this.show_branch_picker(window, cx)
                }),
            )
            .on_action(cx.listener(|_, _: &UseDarkTheme, _, cx| {
                settings::update(cx, |s| s.theme = settings::ThemeChoice::Dark)
            }))
            .on_action(cx.listener(|_, _: &UseLightTheme, _, cx| {
                settings::update(cx, |s| s.theme = settings::ThemeChoice::Light)
            }))
            .on_action(cx.listener(|_, _: &UseSystemTheme, _, cx| {
                settings::update(cx, |s| s.theme = settings::ThemeChoice::System)
            }))
            .on_action(cx.listener(|this, _: &Quit, window, cx| this.quit_all(window, cx)))
            .on_action(cx.listener(Self::find))
            .on_action(cx.listener(Self::find_replace))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.go_to_line(window, cx)))
            .on_action(cx.listener(|this, _: &GitFetch, _, cx| {
                this.git_panel.update(cx, |panel, cx| {
                    panel.sync(crate::git_panel::Sync::Fetch, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &DiscardAllChanges, window, cx| {
                this.git_panel
                    .update(cx, |panel, cx| panel.discard_all(window, cx))
            }))
            .on_action(cx.listener(|this, _: &GitPull, _, cx| {
                this.git_panel
                    .update(cx, |panel, cx| panel.sync(crate::git_panel::Sync::Pull, cx))
            }))
            .on_action(cx.listener(|this, _: &GitPush, _, cx| {
                this.git_panel
                    .update(cx, |panel, cx| panel.sync(crate::git_panel::Sync::Push, cx))
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                if let Some(path) = this.closed_files.pop() {
                    this.open_file(path, window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &ZoomIn, _, cx| zoom(1, cx)))
            .on_action(cx.listener(|_, _: &ZoomOut, _, cx| zoom(-1, cx)))
            .on_action(cx.listener(|_, _: &ResetZoom, _, cx| zoom(0, cx)))
            // Text fields pass Tab up: move to the next field.
            .on_action(cx.listener(|_, _: &editor::Tab, window, _| window.focus_next()))
            .on_action(cx.listener(|_, _: &editor::Backtab, window, _| window.focus_prev()))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .when_some(resize_cursor, |root, cursor| root.cursor(cursor))
            .child(title_bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .children(sidebar)
                    .child(main),
            )
            .child(self.render_status_bar(cx))
            .children(modal)
            .children(menu)
    }
}

/// Ctrl+= / Ctrl+-: every text size up or down one step; 0 resets them.
fn zoom(step: i32, cx: &mut gpui::App) {
    let defaults = settings::Settings::default();
    settings::update(cx, |s| {
        let size = |value: u32, default: u32, min: u32, max: u32| {
            if step == 0 {
                default
            } else {
                (value as i32 + step).clamp(min as i32, max as i32) as u32
            }
        };
        s.ui_font_size = size(s.ui_font_size, defaults.ui_font_size, 10, 20);
        s.editor_font_size = size(s.editor_font_size, defaults.editor_font_size, 8, 32);
        s.terminal_font_size = size(s.terminal_font_size, defaults.terminal_font_size, 8, 32);
    });
}

/// Strips the `\\?\` prefix Windows adds to canonical paths, for display.
fn simplify_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(stripped) if !stripped.starts_with("UNC\\") => PathBuf::from(stripped),
        _ => path,
    }
}
