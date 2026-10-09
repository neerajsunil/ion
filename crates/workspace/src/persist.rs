//! Saving and restoring the session: folder, pane layout, files and
//! terminals.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor::Editor;
use gpui::{AppContext, Context, Window, px};
use project::LoadedText;

use crate::pane::{Axis, Item, ItemKind, Pane, PaneNode};
use crate::pane_ops::Launch;
use crate::session::{self, SavedItem, SavedNode, WorkspaceState};
use crate::workspace::{DOCK_HEIGHT, SIDEBAR_WIDTH, Workspace};

impl Workspace {
    /// Writes recent folders and the layout to disk (in the background).
    pub(crate) fn save_session(&mut self, cx: &mut Context<Self>) {
        // Remote windows must never overwrite the local window's session or
        // serialize credentials/SSH terminal launch commands.
        if self.filesystem.remote().is_some() {
            return;
        }
        let Some(root) = self.root.clone().filter(|_| !self.restoring) else {
            return;
        };
        let center = self.saved_node(&self.center, cx);
        let dock = self.saved_node(&self.dock, cx);
        let last = Some(WorkspaceState {
            root,
            open_files: Vec::new(),
            active_file: 0,
            terminal_visible: self.dock_visible,
            sidebar_visible: self.sidebar_visible,
            sidebar_width: Some(f32::from(self.sidebar_width)),
            center: Some(center),
            dock: Some(dock),
            dock_height: Some(f32::from(self.dock_height)),
        });
        session::update(cx, |session| session.last = last);
    }

    fn saved_node(&self, node: &PaneNode, cx: &gpui::App) -> SavedNode {
        match node {
            PaneNode::Pane(id) => {
                let pane = self.panes.get(id);
                let mut items = Vec::new();
                let mut active = 0;
                for (ix, item) in pane.iter().flat_map(|pane| pane.items.iter().enumerate()) {
                    let saved = match &item.kind {
                        // Diff tabs and untitled buffers aren't restored.
                        ItemKind::Editor { editor, diff: None } => {
                            editor.read(cx).path().map(|path| SavedItem::File {
                                path: path.to_path_buf(),
                            })
                        }
                        ItemKind::Editor { .. } => None,
                        ItemKind::Terminal {
                            name,
                            cwd,
                            shell,
                            harness,
                            ..
                        } => Some(SavedItem::Terminal {
                            name: name.as_ref().map(ToString::to_string),
                            cwd: cwd.clone(),
                            shell: shell.clone(),
                            harness: harness.map(|kind| kind.command().to_owned()),
                        }),
                        ItemKind::Problems(_) => Some(SavedItem::Problems),
                        ItemKind::Settings(_) | ItemKind::Preview(_) | ItemKind::Image(_) => None,
                    };
                    if let Some(saved) = saved {
                        if pane.is_some_and(|pane| pane.active == ix) {
                            active = items.len();
                        }
                        items.push(saved);
                    }
                }
                SavedNode::Pane { items, active }
            }
            PaneNode::Split {
                axis,
                children,
                flexes,
            } => SavedNode::Split {
                row: *axis == Axis::Row,
                children: children
                    .iter()
                    .map(|child| self.saved_node(child, cx))
                    .collect(),
                flexes: flexes.clone(),
            },
        }
    }

    /// Reopens the folder, layout, files and terminals from the previous
    /// run. Returns false if there was nothing to restore.
    pub fn restore_session(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(last) = session::get(cx)
            .last
            .clone()
            .filter(|last| last.root.is_dir())
        else {
            return false;
        };
        self.restoring = true;
        self.set_root(last.root.clone(), window, cx);
        self.sidebar_visible = last.sidebar_visible;
        self.sidebar_width = px(last.sidebar_width.unwrap_or(SIDEBAR_WIDTH));
        self.dock_height = px(last.dock_height.unwrap_or(DOCK_HEIGHT));

        // Sessions from before panes existed list files and a terminal flag.
        let center = last.center.clone().unwrap_or_else(|| SavedNode::Pane {
            items: last
                .open_files
                .iter()
                .map(|path| SavedItem::File { path: path.clone() })
                .collect(),
            active: last.active_file,
        });
        let dock = last.dock.clone().unwrap_or_else(|| SavedNode::Pane {
            items: if last.terminal_visible {
                vec![SavedItem::Terminal {
                    name: None,
                    cwd: None,
                    shell: None,
                    harness: None,
                }]
            } else {
                Vec::new()
            },
            active: 0,
        });
        let dock_visible = last.terminal_visible;

        let mut files = Vec::new();
        center.files(&mut files);
        dock.files(&mut files);
        let loads: Vec<_> = files
            .into_iter()
            .filter(|path| path.is_file())
            .map(|path| {
                cx.background_spawn(async move {
                    let loaded = project::load_text(&path);
                    (path, loaded)
                })
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut loaded = HashMap::new();
            for load in loads {
                let (path, file) = load.await;
                if let Ok(file) = file {
                    loaded.insert(path, file);
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.rebuild_layout(&center, &dock, dock_visible, &loaded, window, cx);
            })
            .ok();
        })
        .detach();
        true
    }

    fn rebuild_layout(
        &mut self,
        center: &SavedNode,
        dock: &SavedNode,
        dock_visible: bool,
        files: &HashMap<PathBuf, LoadedText>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Replace the empty startup panes.
        self.panes.retain(|_, pane| !pane.items.is_empty());
        let center = self
            .build_node(center, files, window, cx)
            .unwrap_or_else(|| PaneNode::Pane(self.new_pane()));
        let dock = self
            .build_node(dock, files, window, cx)
            .unwrap_or_else(|| PaneNode::Pane(self.new_pane()));
        let dock_has_items = dock
            .panes()
            .iter()
            .any(|pane| !self.panes[pane].items.is_empty());
        self.center = center;
        self.dock = dock;
        self.dock_visible = dock_visible && dock_has_items;
        self.editor_pane = self.center.first_pane();
        self.last_dock_pane = self.dock.first_pane();
        self.active_pane = self.editor_pane;
        self.focus_active(window, cx);
        self.restoring = false;
        self.save_session(cx);
        cx.notify();
    }

    /// Builds a saved layout node. Panes whose files are all gone are dropped.
    fn build_node(
        &mut self,
        node: &SavedNode,
        files: &HashMap<PathBuf, LoadedText>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<PaneNode> {
        match node {
            SavedNode::Pane { items, active } => {
                let mut pane = Pane::default();
                for (ix, saved) in items.iter().enumerate() {
                    let item = match saved {
                        SavedItem::File { path } => {
                            let Some(file) = files.get(path) else {
                                continue;
                            };
                            self.restored_editor(path, file, window, cx)
                        }
                        SavedItem::Terminal {
                            name,
                            cwd,
                            shell,
                            harness,
                        } => {
                            let cwd = cwd
                                .clone()
                                .filter(|dir| dir.is_dir())
                                .or_else(|| self.root.clone());
                            let launch = self.saved_launch(shell.as_deref(), harness.as_deref());
                            self.terminal_item(
                                cwd,
                                name.clone().map(Into::into),
                                launch,
                                window,
                                cx,
                            )
                        }
                        SavedItem::Problems => self.problems_item(window, cx),
                    };
                    if ix == *active {
                        pane.active = pane.items.len();
                    }
                    pane.items.push(item);
                }
                if pane.items.is_empty() {
                    return None;
                }
                let id = self.new_pane();
                self.panes.insert(id, pane);
                Some(PaneNode::Pane(id))
            }
            SavedNode::Split {
                row,
                children,
                flexes,
            } => {
                let mut built = Vec::new();
                let mut kept_flexes = Vec::new();
                for (child, flex) in children.iter().zip(flexes) {
                    if let Some(child) = self.build_node(child, files, window, cx) {
                        built.push(child);
                        kept_flexes.push(flex.max(0.05));
                    }
                }
                match built.len() {
                    0 => None,
                    1 => built.pop(),
                    _ => Some(PaneNode::Split {
                        axis: if *row { Axis::Row } else { Axis::Column },
                        children: built,
                        flexes: kept_flexes,
                    }),
                }
            }
        }
    }

    fn restored_editor(
        &mut self,
        path: &Path,
        file: &LoadedText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Item {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(&file.text, Some(path.to_path_buf()), file.has_bom, cx);
            editor.set_disk_hash(file.hash);
            editor
        });
        self.editor_item(editor, None, window, cx)
    }
}

impl Workspace {
    /// What a saved terminal ran. A harness that is no longer installed gets
    /// a shell instead.
    fn saved_launch(&self, shell: Option<&str>, harness: Option<&str>) -> Launch {
        let installed = harness
            .and_then(terminal::HarnessKind::from_command)
            .and_then(|kind| self.harnesses().into_iter().find(|h| h.kind == kind));
        if let Some(harness) = installed {
            return Launch::Agent(harness);
        }
        shell
            .and_then(|name| self.shells().into_iter().find(|shell| shell.name == name))
            .map_or(Launch::Default, Launch::Shell)
    }
}
