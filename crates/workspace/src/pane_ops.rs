//! Managing panes and tabs: opening, moving, splitting, closing, focus.

use std::path::{Path, PathBuf};

use editor::{Editor, EditorEvent};
use gpui::{
    AppContext, Bounds, Context, Entity, EntityId, Pixels, PromptLevel, SharedString, Window,
};
use terminal::{Harness, ShellProfile, TerminalEvent, TerminalView};

use crate::pane::{DiffTab, Item, ItemKind, Pane, PaneId, PaneNode, Region, Side};
use crate::workspace::Workspace;

/// What a new terminal tab runs.
pub(crate) enum Launch {
    /// The shell from settings, or the system default.
    Default,
    /// A shell picked from the menu.
    Shell(ShellProfile),
    /// An agent harness instead of a shell.
    Agent(Harness),
}

impl Workspace {
    pub(crate) fn new_pane(&mut self) -> PaneId {
        self.next_pane_id += 1;
        self.panes.insert(self.next_pane_id, Pane::default());
        self.next_pane_id
    }

    pub(crate) fn region_of(&self, pane: PaneId) -> Region {
        if self.dock.contains(pane) {
            Region::Dock
        } else {
            Region::Center
        }
    }

    fn tree_mut(&mut self, region: Region) -> &mut PaneNode {
        match region {
            Region::Center => &mut self.center,
            Region::Dock => &mut self.dock,
        }
    }

    /// Every tab, in layout order.
    pub(crate) fn items(&self) -> impl Iterator<Item = (PaneId, &Item)> {
        self.center
            .panes()
            .into_iter()
            .chain(self.dock.panes())
            .filter_map(|id| Some((id, self.panes.get(&id)?)))
            .flat_map(|(id, pane)| pane.items.iter().map(move |item| (id, item)))
    }

    pub(crate) fn editors(&self) -> Vec<Entity<Editor>> {
        self.items()
            .filter_map(|(_, item)| item.editor().cloned())
            .collect()
    }

    pub(crate) fn find_item(&self, id: EntityId) -> Option<(PaneId, usize)> {
        self.panes
            .iter()
            .find_map(|(pane_id, pane)| Some((*pane_id, pane.position(id)?)))
    }

    /// The tab editing this file, if one is open.
    pub(crate) fn find_file(&self, path: &Path, cx: &gpui::App) -> Option<(PaneId, usize)> {
        let (pane, item) = self.items().find(|(_, item)| match &item.kind {
            ItemKind::Editor { editor, diff: None } => editor.read(cx).path() == Some(path),
            ItemKind::Image(view) => view.read(cx).path() == path,
            _ => false,
        })?;
        Some((pane, self.panes.get(&pane)?.position(item.id())?))
    }

    /// The editor that find, save and blame act on: the active tab of the
    /// last focused pane showing an editor.
    pub(crate) fn active_editor(&self) -> Option<Entity<Editor>> {
        self.panes
            .get(&self.editor_pane)?
            .active_item()?
            .editor()
            .cloned()
    }

    pub(crate) fn active_item(&self) -> Option<&Item> {
        self.panes.get(&self.active_pane)?.active_item()
    }

    /// Where files open: the last focused editor area pane.
    pub(crate) fn file_pane(&self) -> PaneId {
        if self.center.contains(self.editor_pane) {
            self.editor_pane
        } else if self.center.contains(self.active_pane) {
            self.active_pane
        } else {
            self.center.first_pane()
        }
    }

    /// Where new terminals open: the focused pane if it shows a terminal,
    /// otherwise the dock.
    pub(crate) fn terminal_pane(&self) -> PaneId {
        let active_is_terminal = self
            .active_item()
            .is_some_and(|item| item.terminal().is_some());
        if active_is_terminal {
            self.active_pane
        } else if self.dock.contains(self.last_dock_pane) {
            self.last_dock_pane
        } else {
            self.dock.first_pane()
        }
    }

    // ---- creating tabs -------------------------------------------------------

    pub(crate) fn editor_item(
        &mut self,
        editor: Entity<Editor>,
        diff: Option<DiffTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Item {
        let subscriptions = vec![
            // Redraw tabs and the status bar when the editor changes, and
            // blame the file once the cursor rests.
            cx.observe(&editor, |this, editor, cx| {
                if this.active_editor() == Some(editor) {
                    this.schedule_blame(cx);
                    this.schedule_ide_selection(cx);
                }
                cx.notify()
            }),
            cx.subscribe_in(
                &editor,
                window,
                |this, editor, event, window, cx| match event {
                    EditorEvent::SaveFailed(err) => {
                        this.status = Some(format!("Save failed: {err}").into());
                        cx.notify();
                    }
                    EditorEvent::FormatFailed(err) => {
                        this.status = Some(format!("Saved without formatting: {err}").into());
                        cx.notify();
                    }
                    EditorEvent::DiffRowActivated(row) => {
                        this.jump_from_diff(editor, *row, window, cx)
                    }
                    EditorEvent::RevertDiffHunk(row) => this.revert_diff_hunk(editor, *row, cx),
                    EditorEvent::Edited => this.schedule_auto_save(editor, cx),
                },
            ),
        ];
        self.load_diff_base(&editor, cx);
        if diff.is_none() {
            self.apply_problems_to(&editor, cx);
        } else if self.diff_split {
            editor.update(cx, |editor, cx| editor.set_split_diff(true, window, cx));
        }
        Item {
            kind: ItemKind::Editor { editor, diff },
            _subscriptions: subscriptions,
        }
    }

    /// The shells new terminals can start: this machine's, or the server's
    /// for a remote project (none until they're known).
    pub(crate) fn shells(&self) -> Vec<ShellProfile> {
        if self.filesystem.remote().is_none() {
            return terminal::available_shells();
        }
        let paths = self.remote_tools.iter().flat_map(|tools| &tools.shells);
        paths
            .map(|path| ShellProfile {
                name: path.rsplit('/').next().unwrap_or(path).to_owned(),
                program: path.clone(),
                args: vec!["-l".into()],
            })
            .collect()
    }

    /// Names of the shells and agents the + menu can offer, for Settings.
    pub(crate) fn terminal_names(&self) -> (Vec<String>, Vec<&'static str>) {
        let shells = self.shells().into_iter().map(|shell| shell.name).collect();
        let agents = self.harnesses().iter().map(|h| h.kind.name()).collect();
        (shells, agents)
    }

    /// The agents new terminals can start: installed on this machine, or on
    /// the server for a remote project.
    pub(crate) fn harnesses(&self) -> Vec<Harness> {
        if self.filesystem.remote().is_none() {
            return terminal::available_harnesses();
        }
        let names = self.remote_tools.iter().flat_map(|tools| &tools.agents);
        names
            .filter_map(|name| terminal::HarnessKind::from_command(name))
            .map(|kind| Harness {
                kind,
                path: PathBuf::from(kind.command()),
            })
            .collect()
    }

    /// A terminal tab. Remote projects run it on the server.
    pub(crate) fn terminal_item(
        &mut self,
        cwd: Option<PathBuf>,
        name: Option<SharedString>,
        launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Item {
        let (shell, harness) = match launch {
            Launch::Default => (None, None),
            Launch::Shell(shell) => (Some(shell), None),
            Launch::Agent(harness) => (None, Some(harness)),
        };
        let picked = shell.as_ref().map(|shell| shell.name.clone());
        let view = match (self.filesystem.remote(), &harness) {
            (Some(connection), _) => {
                let connection = connection.clone();
                let folder = cwd.clone().unwrap_or_else(|| PathBuf::from("."));
                let program = match (&harness, &shell) {
                    (Some(harness), _) => remote::Program::Command(harness.kind.command().into()),
                    (None, Some(shell)) => remote::Program::Shell(shell.program.clone()),
                    (None, None) => remote::Program::LoginShell,
                };
                let kind = harness.as_ref().map(|harness| harness.kind);
                cx.new(|cx| TerminalView::new_remote(connection, folder, program, kind, window, cx))
            }
            (None, Some(harness)) => {
                cx.new(|cx| TerminalView::new_harness(cwd.clone(), harness, window, cx))
            }
            (None, None) => {
                let profile =
                    shell.or_else(|| ShellProfile::from_setting(&settings::get(cx).terminal_shell));
                cx.new(|cx| TerminalView::new(cwd.clone(), profile, window, cx))
            }
        };
        let harness = view.read(cx).harness();
        let subscriptions =
            vec![
                cx.subscribe_in(&view, window, |this, view, event, window, cx| match event {
                    TerminalEvent::Changed => cx.notify(),
                    TerminalEvent::Exited => {
                        if let Some((pane, ix)) = this.find_item(view.entity_id()) {
                            this.remove_item(pane, ix, window, cx);
                        }
                    }
                    TerminalEvent::Attention(message) => {
                        this.notify_attention(view, message.clone(), window, cx)
                    }
                    TerminalEvent::OutputSettled => this.scan_problems(view, cx),
                    TerminalEvent::OpenLink(link) => {
                        let link = link.clone();
                        this.open_terminal_link(view.entity_id(), link, window, cx)
                    }
                }),
            ];
        Item {
            kind: ItemKind::Terminal {
                view,
                name,
                cwd,
                shell: picked,
                harness,
            },
            _subscriptions: subscriptions,
        }
    }

    /// Adds a tab next to the pane's active one, then shows and focuses it.
    /// Opens a file Ctrl+clicked in a terminal. Relative paths are tried
    /// against the terminal's folder, then the project.
    fn open_terminal_link(
        &mut self,
        terminal: EntityId,
        link: terminal::FileLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.find_item(terminal).and_then(|(pane, ix)| {
            match &self.panes[&pane].items[ix].kind {
                ItemKind::Terminal { cwd, .. } => cwd.clone(),
                _ => None,
            }
        });
        let remote = self.filesystem.remote().is_some();
        let text = link.path.clone();
        let absolute = if remote {
            text.starts_with('/')
        } else {
            Path::new(&text).is_absolute()
        };
        let candidates: Vec<PathBuf> = if absolute {
            vec![PathBuf::from(&text)]
        } else {
            cwd.iter()
                .chain(self.root.iter())
                .map(|base| project::join_path(base, &text.replace('\\', "/")))
                .collect()
        };
        let filesystem = self.filesystem.clone();
        let found = cx.background_spawn(async move {
            candidates.into_iter().find(|path| {
                filesystem.exists(path).unwrap_or(false) && !filesystem.is_dir(path).unwrap_or(true)
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let found = found.await;
            this.update_in(cx, |this, window, cx| match found {
                Some(path) => {
                    let selection = link.line.map(|line| {
                        let point = (line.max(1) - 1, link.column.unwrap_or(1).max(1) - 1);
                        (point, point)
                    });
                    this.open_file_at(path, selection, window, cx);
                }
                None => {
                    this.status = Some(format!("Can't find {}", link.path).into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn insert_item(
        &mut self,
        pane: PaneId,
        item: Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_item_with_focus(pane, item, true, window, cx);
    }

    /// Like [`Self::insert_item`]; with `focus` false the keyboard stays
    /// where it is (opening a file from the tree keeps the tree focused).
    pub(crate) fn insert_item_with_focus(
        &mut self,
        pane: PaneId,
        item: Item,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.panes.get_mut(&pane) else {
            return;
        };
        let ix = if target.items.is_empty() {
            0
        } else {
            (target.active + 1).min(target.items.len())
        };
        target.items.insert(ix, item);
        if self.dock.contains(pane) {
            self.dock_visible = true;
        }
        self.show_item(pane, ix, focus, window, cx);
        self.layout_changed(cx);
    }

    /// Shows the Settings tab, opening it next to the current file.
    pub(crate) fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_settings_page(None, window, cx);
    }

    /// Opens Settings, or brings it forward, optionally at `page`.
    pub(crate) fn show_settings_page(
        &mut self,
        page: Option<crate::settings_view::Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.items().find_map(|(pane, item)| match &item.kind {
            ItemKind::Settings(view) => Some((pane, item.id(), view.clone())),
            _ => None,
        });
        if let Some((pane, id, view)) = existing
            && let Some(ix) = self.panes[&pane].position(id)
        {
            if let Some(page) = page {
                view.update(cx, |view, cx| view.show_page(page, cx));
            }
            self.activate_item(pane, ix, window, cx);
            return;
        }
        let (shells, agents) = self.terminal_names();
        let view = cx.new(|cx| {
            let mut view = crate::settings_view::SettingsView::new(shells, agents, cx);
            if let Some(page) = page {
                view.show_page(page, cx);
            }
            view
        });
        let item = Item {
            kind: ItemKind::Settings(view),
            _subscriptions: Vec::new(),
        };
        let pane = self.file_pane();
        self.insert_item(pane, item, window, cx);
    }

    /// Opens a new terminal tab in `pane`.
    pub(crate) fn new_terminal_in(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = self.terminal_item(self.root.clone(), None, Launch::Default, window, cx);
        self.insert_item(pane, item, window, cx);
    }

    /// Opens an agent harness in a new terminal tab in `pane`.
    pub(crate) fn new_agent_in(
        &mut self,
        pane: PaneId,
        harness: Harness,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let launch = Launch::Agent(harness);
        let item = self.terminal_item(self.root.clone(), None, launch, window, cx);
        self.insert_item(pane, item, window, cx);
    }

    // ---- focus ---------------------------------------------------------------

    /// Shows a tab and focuses it.
    pub(crate) fn activate_item(
        &mut self,
        pane: PaneId,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_item(pane, ix, true, window, cx);
    }

    /// Shows a tab, focusing it if `focus` is set.
    pub(crate) fn show_item(
        &mut self,
        pane: PaneId,
        ix: usize,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.panes.get_mut(&pane) else {
            return;
        };
        if ix >= target.items.len() {
            return;
        }
        target.active = ix;
        let handle = target.items[ix].focus_handle(cx);
        if self.dock.contains(pane) {
            self.dock_visible = true;
        }
        if focus {
            window.focus(&handle);
        }
        self.pane_focused(pane, cx);
    }

    /// Bookkeeping when a pane gets focus (by click or keyboard).
    pub(crate) fn pane_focused(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(item) = self.panes.get(&pane).and_then(Pane::active_item) else {
            self.active_pane = pane;
            cx.notify();
            return;
        };
        let editor = item.editor().cloned();
        self.active_pane = pane;
        if self.dock.contains(pane) {
            self.last_dock_pane = pane;
        }
        self.status = None;
        if let Some(editor) = editor {
            let changed = self.shown_editor != Some(editor.entity_id());
            if changed && let Some(previous) = self.shown_editor {
                self.auto_save_on_focus_change(previous, cx);
            }
            self.editor_pane = pane;
            self.shown_editor = Some(editor.entity_id());
            if changed {
                let path = editor.read(cx).path().map(Path::to_path_buf);
                if let Some(path) = &path {
                    self.note_recent_file(path.clone());
                    self.record_file_use(path, cx);
                }
                if let Some(tree) = &self.file_tree {
                    tree.update(cx, |tree, cx| tree.set_active(path, cx));
                }
                if self.find_visible {
                    self.find_bar.update(cx, |bar, cx| bar.retarget(editor, cx));
                }
                self.schedule_ide_selection(cx);
                self.schedule_blame(cx);
            }
        }
        cx.notify();
    }

    /// Focuses the active tab of the active pane, or the window.
    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = if self
            .panes
            .get(&self.active_pane)
            .is_some_and(|p| !p.items.is_empty())
            && (self.dock_visible || !self.dock.contains(self.active_pane))
        {
            self.active_pane
        } else {
            self.file_pane()
        };
        match self.panes.get(&pane).and_then(Pane::active_item) {
            Some(item) => {
                window.focus(&item.focus_handle(cx));
                self.pane_focused(pane, cx);
            }
            None => {
                self.active_pane = pane;
                window.focus(&self.focus_handle);
            }
        }
    }

    /// Focuses the nearest pane in a direction (Ctrl+Alt+Arrow).
    pub(crate) fn focus_pane_toward(
        &mut self,
        side: Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = self.layout_bounds.borrow();
        let visible = |id: &PaneId| self.dock_visible || !self.dock.contains(*id);
        let Some(from) = bounds.panes.get(&self.active_pane).copied() else {
            return;
        };
        let center = |b: &Bounds<Pixels>| (f32::from(b.center().x), f32::from(b.center().y));
        let (fx, fy) = center(&from);
        let best = bounds
            .panes
            .iter()
            .filter(|(id, _)| **id != self.active_pane && visible(id))
            .filter_map(|(id, b)| {
                let (x, y) = center(b);
                let (along, across) = match side {
                    Side::Left => (fx - x, (y - fy).abs()),
                    Side::Right => (x - fx, (y - fy).abs()),
                    Side::Up => (fy - y, (x - fx).abs()),
                    Side::Down => (y - fy, (x - fx).abs()),
                };
                (along > 1.).then_some((*id, along + across * 2.))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id);
        drop(bounds);
        if let Some(pane) = best {
            let ix = self.panes[&pane].active;
            if self.panes[&pane].items.is_empty() {
                self.active_pane = pane;
                window.focus(&self.focus_handle);
                cx.notify();
            } else {
                self.activate_item(pane, ix, window, cx);
            }
        }
    }

    // ---- closing -------------------------------------------------------------

    /// Closes a tab, asking first if it has unsaved changes.
    pub(crate) fn close_item(
        &mut self,
        pane: PaneId,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.panes.get(&pane).and_then(|p| p.items.get(ix)) else {
            return;
        };
        let id = item.id();
        let Some(editor) = item.editor().filter(|e| e.read(cx).is_dirty()).cloned() else {
            self.remove_item(pane, ix, window, cx);
            return;
        };
        let message = format!("Save changes to {}?", editor.read(cx).title());
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let close = match answer.await {
                Ok(0) => {
                    let Ok(save) = this.update_in(cx, |this, window, cx| {
                        this.save_editor(editor.clone(), window, cx)
                    }) else {
                        return;
                    };
                    save.await
                }
                Ok(1) => true,
                _ => false,
            };
            if close {
                this.update_in(cx, |this, window, cx| {
                    if let Some((pane, ix)) = this.find_item(id) {
                        this.remove_item(pane, ix, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Takes a tab out of its pane (dropping it closes an editor or shell).
    /// An emptied pane is removed from the layout.
    pub(crate) fn remove_item(
        &mut self,
        pane: PaneId,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Item> {
        let item = self.take_item(pane, ix)?;
        self.forget_item(item.id(), cx);
        self.prune_git_caches(cx);
        // Ctrl+Shift+T brings it back.
        if let ItemKind::Editor { editor, diff: None } = &item.kind
            && let Some(path) = editor.read(cx).path()
        {
            let path = path.to_path_buf();
            self.closed_files.retain(|closed| *closed != path);
            self.closed_files.push(path);
            if self.closed_files.len() > 20 {
                self.closed_files.remove(0);
            }
        }
        let refocus = self.active_pane == pane;
        let remaining = self.panes.get(&pane).map_or(0, |p| p.items.len());
        if remaining == 0 {
            self.remove_pane_if_empty(pane, cx);
        }
        if refocus {
            if self.panes.contains_key(&pane) && remaining > 0 {
                let active = self.panes[&pane].active;
                self.activate_item(pane, active, window, cx);
            } else {
                self.focus_active(window, cx);
            }
        }
        if self.active_editor().is_none() {
            if self.find_visible {
                self.close_find_bar(window, cx);
            }
            if self.shown_editor.take().is_some()
                && let Some(tree) = &self.file_tree
            {
                tree.update(cx, |tree, cx| tree.set_active(None, cx));
            }
        }
        self.layout_changed(cx);
        Some(item)
    }

    fn take_item(&mut self, pane: PaneId, ix: usize) -> Option<Item> {
        let target = self.panes.get_mut(&pane)?;
        if ix >= target.items.len() {
            return None;
        }
        let item = target.items.remove(ix);
        if ix < target.active || target.active >= target.items.len() {
            target.active = target.active.saturating_sub(1);
        }
        Some(item)
    }

    /// Drops an empty pane from the layout. A region's last pane stays;
    /// the dock hides instead.
    fn remove_pane_if_empty(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.panes.get(&pane).is_none_or(|p| !p.items.is_empty()) {
            return;
        }
        let region = self.region_of(pane);
        if self.tree_mut(region).remove(pane) {
            self.panes.remove(&pane);
            if self.zoomed == Some(pane) {
                self.zoomed = None;
            }
            for slot in [&mut self.active_pane, &mut self.editor_pane] {
                if *slot == pane {
                    *slot = self.center.first_pane();
                }
            }
        } else if region == Region::Dock {
            self.dock_visible = false;
            self.zoomed = None;
        }
        cx.notify();
    }

    /// Closes every tab in a pane, then the pane itself.
    pub(crate) fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<EntityId> = self
            .panes
            .get(&pane)
            .map(|p| p.items.iter().map(Item::id).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            let region = self.region_of(pane);
            if self.tree_mut(region).remove(pane) {
                self.panes.remove(&pane);
                if self.active_pane == pane || self.editor_pane == pane {
                    self.editor_pane = self.center.first_pane();
                    self.active_pane = self.editor_pane;
                    self.focus_active(window, cx);
                }
            } else if region == Region::Dock {
                self.dock_visible = false;
            }
            self.layout_changed(cx);
            return;
        }
        for id in ids.into_iter().rev() {
            if let Some((pane, ix)) = self.find_item(id) {
                self.close_item(pane, ix, window, cx);
            }
        }
    }

    // ---- moving and splitting ------------------------------------------------

    /// Moves a tab into another pane, at `index` or after its active tab.
    pub(crate) fn move_item(
        &mut self,
        item: EntityId,
        to: PaneId,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((from, ix)) = self.find_item(item) else {
            return;
        };
        if !self.panes.contains_key(&to) {
            return;
        }
        if from == to {
            // Reorder within the pane.
            let pane = self.panes.get_mut(&to).expect("checked above");
            let moved = pane.items.remove(ix);
            let index = index.unwrap_or(pane.items.len()).min(pane.items.len());
            let index = if index > ix { index - 1 } else { index };
            let index = index.min(pane.items.len());
            pane.items.insert(index, moved);
            self.activate_item(to, index, window, cx);
            self.layout_changed(cx);
            return;
        }
        let Some(moved) = self.take_item(from, ix) else {
            return;
        };
        let pane = self.panes.get_mut(&to).expect("checked above");
        let index = index
            .unwrap_or(if pane.items.is_empty() {
                0
            } else {
                pane.active + 1
            })
            .min(pane.items.len());
        pane.items.insert(index, moved);
        self.remove_pane_if_empty(from, cx);
        self.zoomed = None;
        self.activate_item(to, index, window, cx);
        self.layout_changed(cx);
    }

    /// Moves a tab into a new pane beside `target`.
    pub(crate) fn move_item_to_split(
        &mut self,
        item: EntityId,
        target: PaneId,
        side: Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((from, _)) = self.find_item(item) else {
            return;
        };
        // Splitting a pane with its only tab would just move it in place.
        if from == target && self.panes[&from].items.len() == 1 {
            return;
        }
        let region = self.region_of(target);
        let new = self.new_pane();
        self.tree_mut(region).split(target, new, side);
        self.move_item(item, new, None, window, cx);
    }

    /// Splits the active pane (Ctrl+\). Terminal panes get a new shell;
    /// editor panes get an empty pane for the next file.
    pub(crate) fn split_active_pane(
        &mut self,
        side: Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.active_pane;
        if !self.panes.contains_key(&pane) {
            return;
        }
        let is_terminal = self
            .active_item()
            .is_some_and(|item| item.terminal().is_some());
        let region = self.region_of(pane);
        let new = self.new_pane();
        self.tree_mut(region).split(pane, new, side);
        self.zoomed = None;
        if is_terminal {
            self.new_terminal_in(new, window, cx);
        } else {
            self.active_pane = new;
            self.editor_pane = new;
            window.focus(&self.focus_handle);
            self.layout_changed(cx);
        }
    }

    pub(crate) fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None => Some(self.active_pane),
        };
        cx.notify();
    }

    /// Sets a split's sizes while dragging a divider.
    pub(crate) fn resize_split(
        &mut self,
        region: Region,
        path: &[usize],
        divider: usize,
        position: Pixels,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self
            .layout_bounds
            .borrow()
            .splits
            .get(&(region, path.to_vec()))
            .copied()
        else {
            return;
        };
        let Some(PaneNode::Split { axis, flexes, .. }) = self.tree_mut(region).at_mut(path) else {
            return;
        };
        let (start, size) = match axis {
            crate::pane::Axis::Row => (bounds.left(), bounds.size.width),
            crate::pane::Axis::Column => (bounds.top(), bounds.size.height),
        };
        let size = f32::from(size).max(1.);
        let total: f32 = flexes.iter().sum();
        let before: f32 = flexes[..divider].iter().sum();
        let pair = flexes[divider] + flexes[divider + 1];
        // Keep each pane at least ~120 px.
        let min = (total * 120. / size).min(pair / 2.);
        let at = f32::from(position - start) / size * total - before;
        let first = at.clamp(min, pair - min);
        flexes[divider] = first;
        flexes[divider + 1] = pair - first;
        cx.notify();
    }

    /// Makes a split's panes equal (double-click a divider).
    pub(crate) fn equalize_split(
        &mut self,
        region: Region,
        path: &[usize],
        cx: &mut Context<Self>,
    ) {
        if let Some(PaneNode::Split { flexes, .. }) = self.tree_mut(region).at_mut(path) {
            flexes.iter_mut().for_each(|flex| *flex = 1.);
            self.layout_changed(cx);
        }
    }

    /// Saves the layout and redraws after it changed.
    pub(crate) fn layout_changed(&mut self, cx: &mut Context<Self>) {
        self.save_session(cx);
        cx.notify();
    }
}
