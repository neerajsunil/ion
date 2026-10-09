//! The project file tree shown in the sidebar.
//!
//! Folders are read only when expanded, and rows are drawn with a virtualized
//! list, so large projects cost nothing until you browse into them.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::AppContext;
use gpui::{
    Context, Div, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, ScrollStrategy,
    SharedString, Stateful, StatefulInteractiveElement, Styled, UniformListScrollHandle, Window,
    actions, div, prelude::FluentBuilder, px, uniform_list,
};
use project::DirEntry;

const ROW_HEIGHT: f32 = 24.;
const INDENT: f32 = 14.;
const CONTEXT: &str = "FileTree";

actions!(
    file_tree,
    [
        SelectPrevious,
        SelectNext,
        ExpandOrEnter,
        CollapseOrParent,
        Confirm,
        DeleteSelected,
        RenameSelected
    ]
);

pub fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("right", ExpandOrEnter, context),
        KeyBinding::new("left", CollapseOrParent, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("delete", DeleteSelected, context),
        KeyBinding::new("f2", RenameSelected, context),
    ];
    if cfg!(target_os = "macos") {
        bindings.push(KeyBinding::new("cmd-backspace", DeleteSelected, context));
    }
    bindings
}

pub enum FileTreeEvent {
    /// Open a file. With `focus` the editor takes the keyboard (double-click,
    /// Enter); a single click keeps it in the tree.
    OpenFile {
        path: PathBuf,
        focus: bool,
    },
    /// Right click. `entries` are the targets with whether each is a folder:
    /// the selection if the clicked row is part of it, else the clicked row
    /// (or the root, for empty space).
    ContextMenu {
        entries: Vec<(PathBuf, bool)>,
        position: Point<Pixels>,
    },
    Delete(Vec<PathBuf>),
    Rename(PathBuf),
    Error(String),
}

/// A file's git state, for coloring the tree. Later variants win when a
/// folder holds several.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitMark {
    Untracked,
    Added,
    Modified,
    Conflict,
}

impl GitMark {
    fn letter(self) -> &'static str {
        match self {
            Self::Untracked => "U",
            Self::Added => "A",
            Self::Modified => "M",
            Self::Conflict => "!",
        }
    }

    fn color(self) -> gpui::Hsla {
        match self {
            Self::Untracked | Self::Added => theme::git_added(),
            Self::Modified => theme::git_modified(),
            Self::Conflict => theme::git_deleted(),
        }
    }
}

/// Files dragged out of the tree: into a terminal (their paths are typed
/// there) or onto an editor pane (they open).
#[derive(Clone, Debug)]
pub struct DraggedFiles {
    pub paths: Vec<PathBuf>,
}

/// What follows the mouse while files are dragged.
struct DragLabel(SharedString);

impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(5.))
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::border())
            .font_family(theme::ui_font())
            .text_size(theme::ui_font_size_small())
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

struct Row {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
    depth: usize,
}

pub struct FileTree {
    root: PathBuf,
    filesystem: project::FileSystem,
    loads: HashMap<PathBuf, gpui::Task<()>>,
    focus_handle: FocusHandle,
    /// Cached directory listings, filled when a folder is first expanded.
    children: HashMap<PathBuf, Vec<DirEntry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    /// The file shown in the editor.
    active: Option<PathBuf>,
    /// Selected rows (Ctrl+click adds, Shift+click selects a range).
    selected: HashSet<PathBuf>,
    /// Where Shift+click ranges start.
    anchor: Option<PathBuf>,
    /// The row the keyboard acts on.
    cursor: Option<PathBuf>,
    /// Git marks for changed files and the folders containing them.
    git_marks: HashMap<PathBuf, GitMark>,
    scroll_handle: UniformListScrollHandle,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        Self::with_filesystem(root, project::FileSystem::Local, cx)
    }

    pub fn with_filesystem(
        root: PathBuf,
        filesystem: project::FileSystem,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut tree = Self {
            root: root.clone(),
            filesystem,
            loads: HashMap::new(),
            focus_handle: cx.focus_handle(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            active: None,
            selected: HashSet::new(),
            anchor: None,
            cursor: None,
            git_marks: HashMap::new(),
            scroll_handle: UniformListScrollHandle::new(),
        };
        tree.expand(root, cx);
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn refresh_loaded(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<_> = self
            .children
            .keys()
            .cloned()
            .chain(std::iter::once(self.root.clone()))
            .collect();
        for dir in dirs {
            self.load(&dir, cx);
        }
    }

    pub fn is_directory(&self, path: &Path) -> bool {
        path == self.root || self.rows.iter().any(|row| row.path == path && row.is_dir)
    }

    /// Highlights the file shown in the editor, expanding its folders and
    /// scrolling it into view. It becomes the selection.
    pub fn set_active(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.active == path {
            return;
        }
        if let Some(path) = &path
            && let Ok(relative) = path.strip_prefix(&self.root)
        {
            let mut dir = self.root.clone();
            let parents: Vec<_> = relative
                .parent()
                .into_iter()
                .flat_map(Path::components)
                .collect();
            let mut changed = false;
            for part in parents {
                dir.push(part);
                if !self.expanded.contains(&dir) {
                    self.load(&dir, cx);
                    self.expanded.insert(dir.clone());
                    changed = true;
                }
            }
            if changed {
                self.rebuild_rows();
            }
            self.select_only(path.clone());
            self.scroll_to(path, ScrollStrategy::Center);
        }
        self.active = path;
        cx.notify();
    }

    /// Colors changed files, and the folders above them up to the root.
    pub fn set_git_marks(
        &mut self,
        files: impl IntoIterator<Item = (PathBuf, GitMark)>,
        cx: &mut Context<Self>,
    ) {
        let mut marks = HashMap::new();
        for (path, mark) in files {
            let mut dir = path.parent();
            while let Some(parent) = dir.filter(|dir| dir.starts_with(&self.root)) {
                if parent == self.root {
                    break;
                }
                let entry = marks.entry(parent.to_path_buf()).or_insert(mark);
                *entry = (*entry).max(mark);
                dir = parent.parent();
            }
            let entry = marks.entry(path).or_insert(mark);
            *entry = (*entry).max(mark);
        }
        if marks != self.git_marks {
            self.git_marks = marks;
            cx.notify();
        }
    }

    /// Re-reads folders after changes on disk. Folders that were never
    /// expanded are skipped; they'll be read fresh when opened.
    pub fn refresh(&mut self, dirs: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        let mut changed = false;
        for dir in dirs {
            if self.children.contains_key(&dir) {
                self.load(&dir, cx);
                changed = true;
            }
        }
        if changed {
            self.rebuild_rows();
            cx.notify();
        }
    }

    /// Collapses every folder.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.retain(|dir| *dir == self.root);
        self.children.retain(|dir, _| *dir == self.root);
        self.rebuild_rows();
        cx.notify();
    }

    fn load(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let dir = dir.to_path_buf();
        let filesystem = self.filesystem.clone();
        let work = cx.background_spawn({
            let dir = dir.clone();
            async move { filesystem.read_dir(&dir) }
        });
        let task = cx.spawn({
            let dir = dir.clone();
            async move |this, cx| {
                let entries = work.await;
                this.update(cx, |this, cx| {
                    this.loads.remove(&dir);
                    match entries {
                        // Collapsed while loading: not needed any more.
                        Ok(_) if !this.is_shown(&dir) => {}
                        Ok(entries) => {
                            this.children.insert(dir.clone(), entries);
                        }
                        Err(error) => {
                            cx.emit(FileTreeEvent::Error(format!(
                                "Can't list {}: {error}",
                                dir.display()
                            )));
                        }
                    }
                    this.rebuild_rows();
                    cx.notify();
                })
                .ok();
            }
        });
        self.loads.insert(dir, task);
    }

    /// Expands `dir`, and reads it and the folders under it that were open
    /// when it was collapsed.
    fn expand(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.expanded.insert(dir.clone());
        let reopened: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|open| open.starts_with(&dir) && !self.children.contains_key(*open))
            .filter(|open| self.is_shown(open))
            .cloned()
            .collect();
        for open in reopened {
            self.load(&open, cx);
        }
        self.rebuild_rows();
    }

    /// Collapses `dir` and forgets the listings under it (they're read again
    /// when it's expanded). Which folders inside were open is remembered.
    fn collapse(&mut self, dir: &Path) {
        self.expanded.remove(dir);
        self.children.retain(|listed, _| !listed.starts_with(dir));
        self.loads.retain(|loading, _| !loading.starts_with(dir));
        self.rebuild_rows();
    }

    /// Whether `dir` and every folder above it up to the root are expanded,
    /// so its entries are drawn.
    fn is_shown(&self, dir: &Path) -> bool {
        dir.ancestors()
            .take_while(|ancestor| ancestor.starts_with(&self.root))
            .all(|ancestor| self.expanded.contains(ancestor))
    }

    /// Flattens the expanded part of the tree into the rows that are drawn.
    fn rebuild_rows(&mut self) {
        fn visit(tree: &FileTree, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
            let Some(entries) = tree.children.get(dir) else {
                return;
            };
            for entry in entries {
                rows.push(Row {
                    path: entry.path.clone(),
                    name: entry.name.clone().into(),
                    is_dir: entry.is_dir,
                    depth,
                });
                if entry.is_dir && tree.expanded.contains(&entry.path) {
                    visit(tree, &entry.path, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::with_capacity(self.rows.len());
        visit(self, &self.root, 0, &mut rows);
        self.rows = rows;
        // Hidden or deleted rows can't stay selected.
        let visible: HashSet<&PathBuf> = self.rows.iter().map(|row| &row.path).collect();
        self.selected.retain(|path| visible.contains(path));
    }

    fn row_index(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    fn scroll_to(&self, path: &Path, strategy: ScrollStrategy) {
        if let Some(ix) = self.row_index(path) {
            self.scroll_handle.scroll_to_item(ix, strategy);
        }
    }

    fn select_only(&mut self, path: PathBuf) {
        self.selected.clear();
        self.selected.insert(path.clone());
        self.anchor = Some(path.clone());
        self.cursor = Some(path);
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.expanded.contains(&path) {
            self.collapse(&path);
        } else {
            self.expand(path, cx);
        }
    }

    // ---- mouse ---------------------------------------------------------------

    fn mouse_down_row(
        &mut self,
        ix: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus_handle);
        let Some(row) = self.rows.get(ix) else {
            return;
        };
        let (path, is_dir) = (row.path.clone(), row.is_dir);
        if event.modifiers.secondary() {
            // Ctrl+click: add or remove one row.
            if !self.selected.remove(&path) {
                self.selected.insert(path.clone());
            }
            self.anchor = Some(path.clone());
            self.cursor = Some(path);
        } else if event.modifiers.shift {
            // Shift+click: everything between the anchor and this row.
            let from = self
                .anchor
                .as_deref()
                .and_then(|anchor| self.row_index(anchor))
                .unwrap_or(ix);
            let (start, end) = (from.min(ix), from.max(ix));
            self.selected = self.rows[start..=end]
                .iter()
                .map(|row| row.path.clone())
                .collect();
            self.cursor = Some(path);
        } else {
            self.select_only(path.clone());
            match (is_dir, event.click_count) {
                (true, 1) => self.toggle_dir(path, cx),
                (true, _) => {}
                (false, count) => cx.emit(FileTreeEvent::OpenFile {
                    path,
                    focus: count >= 2,
                }),
            }
        }
        cx.notify();
    }

    fn right_click_row(&mut self, ix: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else {
            return;
        };
        if !self.selected.contains(&row.path) {
            self.select_only(row.path.clone());
        }
        // Targets in tree order.
        let entries = self
            .rows
            .iter()
            .filter(|row| self.selected.contains(&row.path))
            .map(|row| (row.path.clone(), row.is_dir))
            .collect();
        cx.emit(FileTreeEvent::ContextMenu { entries, position });
        cx.notify();
    }

    /// Selected paths in tree order.
    pub fn selection(&self) -> Vec<PathBuf> {
        self.rows
            .iter()
            .filter(|row| self.selected.contains(&row.path))
            .map(|row| row.path.clone())
            .collect()
    }

    // ---- keyboard ------------------------------------------------------------

    fn cursor_index(&self) -> Option<usize> {
        self.cursor.as_deref().and_then(|path| self.row_index(path))
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = match self.cursor_index() {
            Some(ix) => (ix as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
            None => 0,
        };
        let path = self.rows[ix].path.clone();
        self.select_only(path);
        self.scroll_handle.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn expand_or_enter(&mut self, _: &ExpandOrEnter, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_index() else {
            return;
        };
        let row = &self.rows[ix];
        if !row.is_dir {
            return;
        }
        if self.expanded.contains(&row.path) {
            self.move_cursor(1, cx);
        } else {
            self.expand(row.path.clone(), cx);
            cx.notify();
        }
    }

    fn collapse_or_parent(&mut self, _: &CollapseOrParent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_index() else {
            return;
        };
        let row = &self.rows[ix];
        if row.is_dir && self.expanded.contains(&row.path) {
            let path = row.path.clone();
            self.collapse(&path);
        } else if let Some(parent) = row.path.parent().filter(|parent| *parent != self.root) {
            let parent = parent.to_path_buf();
            self.scroll_to(&parent, ScrollStrategy::Top);
            self.select_only(parent);
        }
        cx.notify();
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_index() else {
            return;
        };
        let row = &self.rows[ix];
        let path = row.path.clone();
        if row.is_dir {
            self.toggle_dir(path, cx);
            cx.notify();
        } else {
            cx.emit(FileTreeEvent::OpenFile { path, focus: true });
        }
    }

    fn delete_selected(&mut self, _: &DeleteSelected, _: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection();
        if !selection.is_empty() {
            cx.emit(FileTreeEvent::Delete(selection));
        }
    }

    fn rename_selected(&mut self, _: &RenameSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self
            .cursor
            .clone()
            .filter(|path| self.selected.contains(path))
        {
            cx.emit(FileTreeEvent::Rename(path));
        }
    }

    // ---- rendering -----------------------------------------------------------

    fn render_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let focused = self.focus_handle.is_focused(window);
        // Dragging a selected row drags the whole selection, in tree order.
        let selection: Vec<PathBuf> = if self.selected.len() > 1 {
            self.rows
                .iter()
                .filter(|row| self.selected.contains(&row.path))
                .map(|row| row.path.clone())
                .collect()
        } else {
            Vec::new()
        };
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let is_selected = self.selected.contains(&row.path);
                let is_active = self.active.as_deref() == Some(row.path.as_path());
                let is_cursor = focused && self.cursor.as_deref() == Some(row.path.as_path());
                let mark = self.git_marks.get(&row.path).copied();
                let chevron = match (row.is_dir, self.expanded.contains(&row.path)) {
                    (true, true) => "▾",
                    (true, false) => "▸",
                    (false, _) => "",
                };
                Some(
                    div()
                        .id(ix)
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl(px(8. + row.depth as f32 * INDENT))
                        .pr_2()
                        .border_1()
                        .border_color(gpui::transparent_black())
                        .cursor_pointer()
                        .text_color(if row.is_dir {
                            theme::text()
                        } else {
                            theme::text_muted()
                        })
                        .hover(|style| style.bg(theme::hover_bg()))
                        .when(is_active, |style| style.text_color(theme::text()))
                        // Brighter while the tree has focus.
                        .when(is_selected, |style| {
                            style
                                .bg(if focused {
                                    theme::selection()
                                } else {
                                    theme::active_bg()
                                })
                                .text_color(theme::text())
                        })
                        .when(is_cursor, |style| style.border_color(theme::accent()))
                        .child(
                            div()
                                .w(px(12.))
                                .flex_none()
                                .text_color(theme::text_faint())
                                .child(chevron),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when_some(mark, |name, mark| name.text_color(mark.color()))
                                .child(row.name.clone()),
                        )
                        .when_some(mark.filter(|_| !row.is_dir), |row, mark| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(mark.color())
                                    .child(mark.letter()),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.mouse_down_row(ix, event, window, cx)
                            }),
                        )
                        .on_drag(
                            DraggedFiles {
                                paths: if is_selected && !selection.is_empty() {
                                    selection.clone()
                                } else {
                                    vec![row.path.clone()]
                                },
                            },
                            |files, _, _, cx| {
                                let label = match files.paths.as_slice() {
                                    [path] => path
                                        .file_name()
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                    paths => format!("{} items", paths.len()),
                                };
                                cx.new(|_| DragLabel(label.into()))
                            },
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                window.focus(&this.focus_handle);
                                this.right_click_row(ix, event.position, cx);
                            }),
                        ),
                )
            })
            .collect()
    }
}

impl Render for FileTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(Self::expand_or_enter))
            .on_action(cx.listener(Self::collapse_or_parent))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::delete_selected))
            .on_action(cx.listener(Self::rename_selected))
            // Clicking empty space clears the selection; right click there
            // targets the project root.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle);
                    this.selected.clear();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.emit(FileTreeEvent::ContextMenu {
                        entries: vec![(this.root.clone(), true)],
                        position: event.position,
                    });
                }),
            )
            .child(
                uniform_list(
                    "file-tree",
                    self.rows.len(),
                    cx.processor(|this, range, window, cx| this.render_rows(range, window, cx)),
                )
                .track_scroll(self.scroll_handle.clone())
                .size_full()
                .py_1(),
            )
    }
}
