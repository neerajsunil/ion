//! File and folder operations from the file tree: its context menu,
//! Delete and F2, and cut/copy/paste of files.

use std::path::{Path, PathBuf};

use gpui::{AppContext, ClickEvent, ClipboardItem, Context, Pixels, Point, PromptLevel, Window};
use ui::{IconName, Menu, MenuEntry};

use crate::workspace::{MenuKind, PromptPurpose, Workspace};

/// Files cut or copied in the tree, waiting to be pasted.
pub(crate) struct FileClipboard {
    pub paths: Vec<PathBuf>,
    /// Paste moves instead of copying.
    pub cut: bool,
}

#[derive(Clone, Copy)]
enum MenuItem {
    SendToAgent,
    OpenToSide,
    NewFile,
    NewFolder,
    OpenInTerminal,
    Cut,
    Copy,
    Paste,
    Duplicate,
    CopyPath,
    CopyRelativePath,
    Rename,
    Delete,
    Reveal,
    CollapseAll,
}

impl MenuItem {
    fn label(self, count: usize) -> String {
        let plural = if count > 1 { "s" } else { "" };
        match self {
            MenuItem::SendToAgent => "Send to Agent".into(),
            MenuItem::OpenToSide => "Open to the Side".into(),
            MenuItem::NewFile => "New File…".into(),
            MenuItem::NewFolder => "New Folder…".into(),
            MenuItem::OpenInTerminal => "Open in Terminal".into(),
            MenuItem::Cut => "Cut".into(),
            MenuItem::Copy => "Copy".into(),
            MenuItem::Paste => "Paste".into(),
            MenuItem::Duplicate => "Duplicate".into(),
            MenuItem::CopyPath => format!("Copy Path{plural}"),
            MenuItem::CopyRelativePath => format!("Copy Relative Path{plural}"),
            MenuItem::Rename => "Rename…".into(),
            MenuItem::Delete if count > 1 => format!("Delete {count} Items"),
            MenuItem::Delete => "Delete".into(),
            MenuItem::Reveal => {
                if cfg!(target_os = "macos") {
                    "Reveal in Finder".into()
                } else {
                    "Reveal in File Explorer".into()
                }
            }
            MenuItem::CollapseAll => "Collapse All Folders".into(),
        }
    }

    fn shortcut(self) -> Option<&'static str> {
        match self {
            MenuItem::Rename => Some("F2"),
            MenuItem::Delete => Some("Del"),
            _ => None,
        }
    }

    fn icon(self) -> IconName {
        match self {
            MenuItem::SendToAgent => IconName::Bot,
            MenuItem::OpenToSide => IconName::Columns2,
            MenuItem::NewFile => IconName::FilePlus,
            MenuItem::NewFolder => IconName::FolderPlus,
            MenuItem::OpenInTerminal => IconName::SquareTerminal,
            MenuItem::Cut => IconName::Scissors,
            MenuItem::Copy | MenuItem::Duplicate => IconName::Copy,
            MenuItem::Paste => IconName::ClipboardPaste,
            MenuItem::CopyPath | MenuItem::CopyRelativePath => IconName::FileText,
            MenuItem::Rename => IconName::Pencil,
            MenuItem::Delete => IconName::Trash2,
            MenuItem::Reveal => IconName::ExternalLink,
            MenuItem::CollapseAll => IconName::ChevronsDownUp,
        }
    }
}

impl Workspace {
    /// Opens the file tree's right-click menu for the clicked items.
    pub(crate) fn show_tree_menu(
        &mut self,
        entries: Vec<(PathBuf, bool)>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let count = entries.len();
        let items = self
            .menu_items(&entries)
            .into_iter()
            .map(|item| match item {
                None => MenuEntry::Separator,
                Some(item) => {
                    let entries = entries.clone();
                    MenuEntry::item(
                        item.label(count),
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.run_menu_item(item, entries.clone(), window, cx)
                        }),
                    )
                    .shortcut(item.shortcut().map(Into::into))
                    .icon(item.icon())
                }
            })
            .collect();
        self.open_menu(MenuKind::Tree, Menu::new(position, items), cx);
    }

    /// Menu entries for the right-clicked items; `None` is a separator.
    fn menu_items(&self, entries: &[(PathBuf, bool)]) -> Vec<Option<MenuItem>> {
        use MenuItem::*;
        let can_paste = self.file_clipboard.is_some();
        let [(path, is_dir)] = entries else {
            // Several items: only what makes sense for all of them.
            return vec![
                Some(SendToAgent),
                None,
                Some(Cut),
                Some(Copy),
                None,
                Some(CopyPath),
                Some(CopyRelativePath),
                None,
                Some(Delete),
            ];
        };
        let is_root = self.root.as_deref() == Some(path.as_path());
        let mut items = vec![Some(SendToAgent), None];
        if *is_dir {
            items.extend([Some(NewFile), Some(NewFolder), Some(OpenInTerminal), None]);
        } else {
            items.extend([Some(OpenToSide), None]);
        }
        if !is_root {
            items.extend([Some(Cut), Some(Copy)]);
        }
        if can_paste {
            items.push(Some(Paste));
        }
        if !is_root {
            items.push(Some(Duplicate));
        }
        if items.last().is_some_and(Option::is_some) {
            items.push(None);
        }
        items.extend([Some(CopyPath), Some(CopyRelativePath), None]);
        if !is_root {
            items.extend([Some(Rename), Some(Delete), None]);
        }
        if self.filesystem.remote().is_none() {
            items.push(Some(Reveal));
        }
        if is_root {
            items.push(Some(CollapseAll));
        }
        items
    }

    fn run_menu_item(
        &mut self,
        item: MenuItem,
        entries: Vec<(PathBuf, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = entries.iter().map(|(path, _)| path.clone()).collect();
        let (path, is_dir) = entries[0].clone();
        // New items and pastes go into the folder, or next to the file.
        let folder = if is_dir {
            path.clone()
        } else {
            path.parent()
                .map_or_else(|| path.clone(), Path::to_path_buf)
        };
        match item {
            MenuItem::SendToAgent => self.send_paths_to_agent(paths, window, cx),
            MenuItem::OpenToSide => self.open_file_to_side(path, window, cx),
            MenuItem::NewFile => self.show_prompt(
                format!("New file in {}", self.display_relative(&path)),
                "",
                0..0,
                PromptPurpose::NewFile { dir: path },
                window,
                cx,
            ),
            MenuItem::NewFolder => self.show_prompt(
                format!("New folder in {}", self.display_relative(&path)),
                "",
                0..0,
                PromptPurpose::NewFolder { dir: path },
                window,
                cx,
            ),
            MenuItem::OpenInTerminal => {
                let item = self.terminal_item(
                    Some(path),
                    None,
                    crate::pane_ops::Launch::Default,
                    window,
                    cx,
                );
                let pane = self.terminal_pane();
                self.insert_item(pane, item, window, cx);
            }
            MenuItem::Cut | MenuItem::Copy => {
                self.file_clipboard = Some(FileClipboard {
                    paths,
                    cut: matches!(item, MenuItem::Cut),
                });
            }
            MenuItem::Paste => self.paste_files(folder, cx),
            MenuItem::Duplicate => self.copy_files(vec![path], folder, false, cx),
            MenuItem::Rename => self.rename_path(path, window, cx),
            MenuItem::Delete => self.delete_paths(paths, window, cx),
            MenuItem::CopyPath => {
                let text: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
            }
            MenuItem::CopyRelativePath => {
                let text: Vec<String> = paths.iter().map(|p| self.display_relative(p)).collect();
                cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
            }
            MenuItem::Reveal => reveal(&path),
            MenuItem::CollapseAll => {
                if let Some(tree) = &self.file_tree {
                    tree.update(cx, |tree, cx| tree.collapse_all(cx));
                }
            }
        }
    }

    /// Asks for a new name (F2 or the menu).
    pub(crate) fn rename_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.root.as_deref() == Some(path.as_path()) {
            return;
        }
        let name = file_name(&path);
        // Select the name without its extension, like most editors.
        let stem = match name.rfind('.') {
            Some(dot)
                if dot > 0
                    && !self
                        .file_tree
                        .as_ref()
                        .is_some_and(|tree| tree.read(cx).is_directory(&path)) =>
            {
                name[..dot].chars().count()
            }
            _ => name.chars().count(),
        };
        self.show_prompt(
            format!("Rename {name}"),
            &name,
            0..stem,
            PromptPurpose::Rename { path },
            window,
            cx,
        );
    }

    fn paste_files(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let Some(clipboard) = &self.file_clipboard else {
            return;
        };
        let (paths, cut) = (clipboard.paths.clone(), clipboard.cut);
        if cut {
            // A cut is used up by pasting it.
            self.file_clipboard = None;
        }
        self.copy_files(paths, folder, cut, cx);
    }

    /// Copies (or moves) files into `folder` in the background, picking
    /// free names ("main copy.rs") so nothing is overwritten.
    fn copy_files(
        &mut self,
        paths: Vec<PathBuf>,
        folder: PathBuf,
        cut: bool,
        cx: &mut Context<Self>,
    ) {
        let filesystem = self.filesystem.clone();
        let work = cx.background_spawn(async move {
            let mut moved = Vec::new();
            for path in paths {
                if cut && path.parent() == Some(folder.as_path()) {
                    continue;
                }
                let name = file_name(&path);
                let target = match filesystem.copy_into(&path, &folder, cut) {
                    Ok(target) => target,
                    Err(err) => return (moved, Some(format!("Couldn't paste {name}: {err}"))),
                };
                moved.push((path, target));
            }
            (moved, None)
        });
        cx.spawn(async move |this, cx| {
            let (moved, error) = work.await;
            this.update(cx, |this, cx| {
                for (from, to) in &moved {
                    if cut {
                        this.retarget_tabs(from, to, cx);
                        this.refresh_tree_for(from, cx);
                    }
                    this.refresh_tree_for(to, cx);
                }
                if cut && !moved.is_empty() {
                    this.save_session(cx);
                }
                if let Some(error) = error {
                    this.status = Some(error.into());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn finish_prompt(
        &mut self,
        purpose: PromptPurpose,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let PromptPurpose::RenameTerminal { item } = purpose {
            if let Some((pane, ix)) = self.find_item(item)
                && let Some(pane) = self.panes.get_mut(&pane)
                && let crate::pane::ItemKind::Terminal { name, .. } = &mut pane.items[ix].kind
            {
                let text = text.trim();
                *name = (!text.is_empty()).then(|| text.to_owned().into());
            }
            self.layout_changed(cx);
            return;
        }
        if let PromptPurpose::GoToLine { editor } = purpose {
            let mut parts = text.trim().splitn(2, [':', ',']);
            let line = parts.next().and_then(|n| n.trim().parse::<usize>().ok());
            let column = parts.next().and_then(|n| n.trim().parse::<usize>().ok());
            if let Some(line) = line {
                let row = line.max(1) - 1;
                let col = column.unwrap_or(1).max(1) - 1;
                editor.update(cx, |editor, cx| {
                    editor.select_range((row, col), (row, col), cx)
                });
                window.focus(&gpui::Focusable::focus_handle(editor.read(cx), cx));
            }
            return;
        }
        if let PromptPurpose::SaveRemote { editor, answer } = purpose {
            let path = PathBuf::from(text);
            if !path.has_root() {
                self.status = Some("Use an absolute remote file path, starting with /".into());
                let _ = answer.send(false);
                cx.notify();
                return;
            }
            let saved_path = path.clone();
            let filesystem = editor.read(cx).filesystem().clone();
            let previous = editor.read(cx).path().map(Path::to_path_buf);
            let exists = cx.background_spawn({
                let path = path.clone();
                async move { filesystem.exists(&path) }
            });
            cx.spawn_in(window, async move |this, cx| {
                match exists.await {
                    Ok(true) if previous.as_ref() != Some(&path) => {
                        let prompt = format!("Replace {} on the remote server?", path.display());
                        let Ok(confirmation) = this.update_in(cx, |_, window, cx| {
                            window.prompt(
                                PromptLevel::Warning,
                                &prompt,
                                Some("The existing file will be overwritten."),
                                &["Replace", "Cancel"],
                                cx,
                            )
                        }) else {
                            return;
                        };
                        if !matches!(confirmation.await, Ok(0)) {
                            let _ = answer.send(false);
                            return;
                        }
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            this.status = Some(error.to_string().into());
                            cx.notify();
                        })
                        .ok();
                        let _ = answer.send(false);
                        return;
                    }
                    _ => {}
                }
                let Ok(save) = editor.update(cx, |editor, cx| {
                    editor.set_path(path, cx);
                    editor.save(cx)
                }) else {
                    return;
                };
                let saved = save.await;
                let _ = answer.send(saved);
                if saved {
                    this.update(cx, |this, cx| this.refresh_tree_for(&saved_path, cx))
                        .ok();
                }
            })
            .detach();
            return;
        }
        if text.contains("..") || Path::new(text).has_root() || text.contains('\0') {
            self.status = Some("Names can't contain \"..\"".into());
            cx.notify();
            return;
        }
        let filesystem = self.filesystem.clone();
        let text = text.to_owned();
        let work = cx.background_spawn(async move {
            match purpose {
                PromptPurpose::NewFile { dir } => {
                    let path = project::join_path(&dir, &text);
                    filesystem.create_file(&path).map(|()| (path, None, true))
                }
                PromptPurpose::NewFolder { dir } => {
                    let path = project::join_path(&dir, &text);
                    filesystem
                        .create_dir_all(&path)
                        .map(|()| (path, None, false))
                }
                PromptPurpose::Rename { path } => {
                    let target = match path.parent() {
                        Some(parent) => project::join_path(parent, &text),
                        None => path.with_file_name(&text),
                    };
                    filesystem
                        .rename(&path, &target)
                        .map(|()| (target, Some(path), false))
                }
                PromptPurpose::RenameTerminal { .. }
                | PromptPurpose::SaveRemote { .. }
                | PromptPurpose::GoToLine { .. } => {
                    unreachable!("handled above")
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((path, previous, open)) => {
                        if let Some(previous) = previous {
                            this.retarget_tabs(&previous, &path, cx);
                            this.refresh_tree_for(&previous, cx);
                        }
                        this.refresh_tree_for(&path, cx);
                        if open {
                            this.open_file(path, window, cx);
                        }
                        this.save_session(cx);
                    }
                    Err(error) => this.status = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Asks, then moves files and folders to the Recycle Bin.
    pub(crate) fn delete_paths(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| self.root.as_deref() != Some(path.as_path()))
            .collect();
        if paths.is_empty() {
            return;
        }
        let filesystem = self.filesystem.clone();
        let remote = filesystem.remote().is_some();
        let bin = if cfg!(target_os = "macos") {
            "Trash"
        } else {
            "Recycle Bin"
        };
        let message = if remote {
            format!(
                "Permanently delete {} item(s) from the remote server?",
                paths.len()
            )
        } else {
            match paths.as_slice() {
                [path] => format!("Move {} to the {bin}?", file_name(path)),
                _ => format!("Move {} items to the {bin}?", paths.len()),
            }
        };
        let detail = (paths.len() > 1).then(|| {
            let mut names: Vec<String> = paths.iter().take(8).map(|p| file_name(p)).collect();
            if paths.len() > 8 {
                names.push(format!("and {} more", paths.len() - 8));
            }
            names.join("\n")
        });
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            detail.as_deref(),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            let deleted = cx
                .background_spawn({
                    let paths = paths.clone();
                    async move {
                        if remote {
                            filesystem.delete(&paths).map_err(|error| error.to_string())
                        } else {
                            trash::delete_all(&paths).map_err(|error| error.to_string())
                        }
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                match deleted {
                    Ok(()) => {
                        // Close unmodified tabs for deleted files.
                        let doomed: Vec<gpui::EntityId> = this
                            .editors()
                            .into_iter()
                            .filter(|editor| {
                                let editor = editor.read(cx);
                                let gone = editor
                                    .path()
                                    .is_some_and(|p| paths.iter().any(|d| p.starts_with(d)));
                                gone && !editor.is_dirty()
                            })
                            .map(|editor| editor.entity_id())
                            .collect();
                        for id in doomed {
                            if let Some((pane, ix)) = this.find_item(id) {
                                this.remove_item(pane, ix, window, cx);
                            }
                        }
                        for path in &paths {
                            this.refresh_tree_for(path, cx);
                        }
                    }
                    Err(err) => this.status = Some(format!("Couldn't delete: {err}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Points open tabs at a renamed or moved file or folder.
    fn retarget_tabs(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for editor in self.editors() {
            let renamed = editor
                .read(cx)
                .path()
                .and_then(|path| path.strip_prefix(from).ok())
                .map(|rest| {
                    if rest.as_os_str().is_empty() {
                        to.to_path_buf()
                    } else {
                        project::join_path(to, &rest.to_string_lossy().replace('\\', "/"))
                    }
                });
            if let Some(new_path) = renamed {
                editor.update(cx, |editor, cx| editor.set_path(new_path, cx));
            }
        }
    }

    /// Re-reads the folder containing `path` right away (the watcher would
    /// catch it too, a moment later).
    pub(crate) fn refresh_tree_for(&mut self, path: &Path, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = path
            .ancestors()
            .skip(1)
            .take(1)
            .map(Path::to_path_buf)
            .collect();
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.refresh(dirs, cx));
        }
        self.rebuild_index(true, cx);
    }

    fn display_relative(&self, path: &Path) -> String {
        let relative = self
            .root
            .as_deref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path);
        let text = relative.display().to_string().replace('\\', "/");
        if text.is_empty() { ".".into() } else { text }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn reveal(path: &Path) {
    let result = if cfg!(windows) {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
    } else {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
    };
    // Explorer's own window reports errors; nothing useful to show here.
    drop(result);
}
