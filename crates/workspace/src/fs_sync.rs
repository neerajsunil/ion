//! Keeping Ion in sync with the disk: live reload of open files, tree
//! refresh and the file index, driven by the file system watcher.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor::Editor;
use futures::StreamExt;
use gpui::{AppContext, Context, Entity, PromptLevel, Window};
use project::{FsEvent, LoadError};

use crate::workspace::{Modal, Workspace};

/// Collect watcher events for this long before acting, so a burst of writes
/// (an agent saving several files, a git checkout) is handled once.
const BATCH_DELAY: Duration = Duration::from_millis(80);
/// Wait this long before re-indexing after files are added or removed.
const REINDEX_DELAY: Duration = Duration::from_millis(400);

/// More changes than this pending at once (a checkout, an unpacked archive)
/// re-walk the project instead of updating the index path by path.
const MAX_INDEX_CHANGES: usize = 1_000;

/// More files than this changed together isn't an agent's edit.
const MAX_AGENT_BATCH: usize = 10;
impl Workspace {
    pub(crate) fn start_watching(
        &mut self,
        root: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.watch_task = None;
        self.watcher = None;
        self.remote_watch = None;
        if let Some(connection) = self.filesystem.remote().cloned() {
            let root = root.to_path_buf();
            let watched_root = root.clone();
            let watch =
                cx.background_spawn(async move { project::watch_remote(connection, &root) });
            self.watch_task = Some(cx.spawn_in(window, async move |this, cx| {
                let (watcher, mut events) = match watch.await {
                    Ok(watch) => watch,
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            this.status = Some(error.to_string().into());
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                };
                if this
                    .update(cx, |this, _| {
                        if this.root.as_ref() == Some(&watched_root) {
                            this.remote_watch = Some(watcher);
                        }
                    })
                    .is_err()
                {
                    return;
                }
                while let Some(event) = events.next().await {
                    match event {
                        Ok(first) => {
                            cx.background_executor().timer(BATCH_DELAY).await;
                            let mut batch = vec![first];
                            let mut error = None;
                            while let Ok(event) = events.try_recv() {
                                match event {
                                    Ok(event) => batch.push(event),
                                    Err(message) => error = Some(message),
                                }
                            }
                            if this
                                .update_in(cx, |this, window, cx| {
                                    this.handle_fs_events(batch, window, cx);
                                    if let Some(error) = error {
                                        this.status = Some(error.into());
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(error) => {
                            this.update(cx, |this, cx| {
                                this.status = Some(error.into());
                                cx.notify();
                            })
                            .ok();
                            break;
                        }
                    }
                }
            }));
            return;
        }
        let (watcher, mut events) = match project::watch(root) {
            Ok(watch) => watch,
            Err(err) => {
                self.status = Some(format!("Not watching for file changes: {err}").into());
                return;
            }
        };
        self.watcher = Some(watcher);
        self.watch_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = events.next().await {
                cx.background_executor().timer(BATCH_DELAY).await;
                let mut batch = vec![first];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let handled = this.update_in(cx, |this, window, cx| {
                    this.handle_fs_events(batch, window, cx)
                });
                if handled.is_err() {
                    break;
                }
            }
        }));
    }

    fn handle_fs_events(
        &mut self,
        events: Vec<FsEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Any change in the working tree can change `git status`.
        self.refresh_git_status(cx);
        let structural = events.iter().any(|event| event.structural);
        let changed: HashSet<PathBuf> = events.into_iter().flat_map(|event| event.paths).collect();
        let full_refresh = self
            .root
            .as_ref()
            .is_some_and(|root| changed.contains(root));

        let open: Vec<(Entity<Editor>, PathBuf)> = self
            .editors()
            .into_iter()
            .filter_map(|editor| {
                let path = editor.read(cx).path()?.to_path_buf();
                (full_refresh || changed.contains(&path)).then_some((editor, path))
            })
            .collect();
        // Files an agent may have written. Open ones are checked against
        // their buffers first (that skips Ion's own saves); many files at once
        // is a checkout or similar, not an agent's edit.
        if !full_refresh && changed.len() <= MAX_AGENT_BATCH {
            let candidates: Vec<PathBuf> = changed
                .iter()
                .filter(|path| !open.iter().any(|(_, open)| open == *path))
                .filter(|path| !path.components().any(|part| part.as_os_str() == ".git"))
                .cloned()
                .collect();
            self.note_written_files(candidates, window, cx);
        }
        for (editor, path) in open {
            self.sync_editor_with_disk(editor, path, window, cx);
        }

        if structural {
            // Refresh each changed entry's folder, and the entry itself if it
            // is a folder the tree has loaded.
            let dirs: HashSet<PathBuf> = changed
                .iter()
                .flat_map(|path| [path.parent().map(Path::to_path_buf), Some(path.clone())])
                .flatten()
                .collect();
            if let Some(tree) = &self.file_tree {
                tree.update(cx, |tree, cx| {
                    if full_refresh {
                        tree.refresh_loaded(cx);
                    } else {
                        tree.refresh(dirs, cx);
                    }
                });
            }
            if full_refresh {
                self.rebuild_index(true, cx);
            } else {
                self.update_index(changed, cx);
            }
        }
    }

    /// Notes which of `candidates` are files (not folders or deleted paths)
    /// as an agent's edits. Checked in the background: on a remote project
    /// that's a request to the server.
    fn note_written_files(
        &mut self,
        candidates: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if candidates.is_empty() {
            return;
        }
        let filesystem = self.filesystem.clone();
        let files = cx.background_spawn(async move { filesystem.files_among(candidates) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(mut files) = files.await else {
                return;
            };
            files.sort();
            this.update_in(cx, |this, window, cx| {
                for path in files {
                    this.note_external_change(path, None, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reloads an open file that changed on disk. Clean buffers update
    /// silently (undoable with Ctrl+Z); buffers with unsaved edits ask first.
    pub(crate) fn sync_editor_with_disk(
        &mut self,
        editor: Entity<Editor>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let load = cx.background_spawn({
            let path = path.clone();
            let filesystem = editor.read(cx).filesystem().clone();
            async move { filesystem.load_text(&path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let loaded = match load.await {
                Ok(loaded) => loaded,
                // Deleted or replaced by something unreadable: keep the buffer.
                Err(LoadError::Io(_) | LoadError::Binary | LoadError::NotUtf8) => return,
            };
            this.update_in(cx, |this, window, cx| {
                let (dirty, ours) = {
                    let editor = editor.read(cx);
                    (editor.is_dirty(), editor.matches_disk(loaded.hash))
                };
                if ours {
                    // Our own save, or content we already have.
                    return;
                }
                if !dirty {
                    let row = editor.update(cx, |editor, cx| {
                        editor.reload_from_disk(&loaded.text, loaded.hash, cx)
                    });
                    if row.is_some() {
                        this.note_external_change(path, row, window, cx);
                    }
                    return;
                }
                this.note_external_change(path.clone(), None, window, cx);
                if !this.conflicts.insert(path.clone()) {
                    return;
                }
                let name = editor.read(cx).title();
                let answer = window.prompt(
                    PromptLevel::Warning,
                    &format!("{name} changed on disk"),
                    Some(
                        "It was modified outside Ion while you have unsaved changes. \
                         Reload it (your version stays in undo history) or keep yours?",
                    ),
                    &["Reload", "Keep Mine"],
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    let reload = matches!(answer.await, Ok(0));
                    this.update(cx, |this, cx| {
                        this.conflicts.remove(&path);
                        if reload {
                            editor.update(cx, |editor, cx| {
                                editor.reload_from_disk(&loaded.text, loaded.hash, cx);
                            });
                        } else {
                            // Remember the disk version so we don't ask again
                            // until it changes once more.
                            editor.update(cx, |editor, _| editor.set_disk_hash(loaded.hash));
                        }
                    })
                    .ok();
                })
                .detach();
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the file index in the background. Calls during a rebuild are
    /// coalesced into one more rebuild afterwards.
    pub(crate) fn rebuild_index(&mut self, delayed: bool, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.index_task.is_some() {
            self.index_stale = true;
            return;
        }
        // The walk sees these; changes from now on are applied after it.
        self.index_changes.clear();
        let filesystem = self.filesystem.clone();
        let previous = self.file_index.clone();
        let indexed_root = root.clone();
        self.index_progress = 0;
        self.index_task = Some(cx.spawn(async move |this, cx| {
            if delayed {
                cx.background_executor().timer(REINDEX_DELAY).await;
            }
            let (sender, mut counts) = futures::channel::mpsc::unbounded();
            let build = cx.background_spawn(async move {
                filesystem.build_index(&root, previous.as_deref(), &|found| {
                    sender.unbounded_send(found).ok();
                })
            });
            while let Some(found) = counts.next().await {
                let shown = this.update(cx, |this, cx| {
                    this.index_progress = found;
                    // Only the first index of a project shows in the status bar.
                    if this.file_index.is_none() {
                        cx.notify();
                    }
                });
                if shown.is_err() {
                    return;
                }
            }
            let index = build.await;
            this.update(cx, |this, cx| {
                this.index_task = None;
                match index {
                    Ok(index) => this.install_index(index, cx),
                    Err(error) => {
                        if this.root.as_ref() == Some(&indexed_root) {
                            this.status = Some(format!("Can't index project: {error}").into());
                            cx.notify();
                        }
                    }
                }
            })
            .ok();
        }));
    }

    /// Applies created, removed or renamed paths to the index in the
    /// background, without walking the whole project. Remote projects (and
    /// a project still being indexed) build it again instead: the server
    /// already sends only what changed.
    fn update_index(&mut self, changed: HashSet<PathBuf>, cx: &mut Context<Self>) {
        if self.filesystem.remote().is_some() || self.file_index.is_none() {
            self.rebuild_index(true, cx);
            return;
        }
        self.index_changes.extend(changed);
        if self.index_changes.len() > MAX_INDEX_CHANGES {
            self.rebuild_index(true, cx);
            return;
        }
        if self.index_task.is_none() {
            self.start_index_update(cx);
        }
    }

    fn start_index_update(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.file_index.clone() else {
            return;
        };
        self.index_task = Some(cx.spawn(async move |this, cx| {
            // Let a burst of changes collect into one update.
            cx.background_executor().timer(REINDEX_DELAY).await;
            let Ok(changed) = this.update(cx, |this, _| std::mem::take(&mut this.index_changes))
            else {
                return;
            };
            let updated = cx
                .background_spawn(async move { index.updated(&changed) })
                .await;
            this.update(cx, |this, cx| {
                this.index_task = None;
                match updated {
                    Some(index) => this.install_index(index, cx),
                    None => this.rebuild_index(false, cx),
                }
            })
            .ok();
        }));
    }

    /// Makes a freshly built or updated index current, then catches up with
    /// changes made meanwhile.
    fn install_index(&mut self, index: project::FileIndex, cx: &mut Context<Self>) {
        // The project changed while indexing; this result is for the old one.
        if self.root.as_deref() != Some(index.root.as_path()) {
            self.rebuild_index(false, cx);
            return;
        }
        let index = Arc::new(index);
        self.file_index = Some(index.clone());
        if let Some(Modal::Palette(palette)) = &self.modal {
            palette.update(cx, |palette, cx| palette.set_index(index.clone(), cx));
        }
        self.project_search_set_index(Some(index), cx);
        if std::mem::take(&mut self.index_stale) {
            self.rebuild_index(true, cx);
        } else if !self.index_changes.is_empty() {
            self.start_index_update(cx);
        }
        cx.notify();
    }
}
