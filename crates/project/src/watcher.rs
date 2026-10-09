//! Watches the project folder for changes made outside Ion (agents, git, builds).
//!
//! Uses the OS notification API (ReadDirectoryChangesW on Windows), so the
//! watcher thread sleeps in the kernel until something actually changes.

use std::path::{Path, PathBuf};

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// A batch of changed paths reported by the OS.
#[derive(Debug)]
pub struct FsEvent {
    pub paths: Vec<PathBuf>,
    /// Files or folders were created, removed or renamed (the tree and file
    /// index need updating), as opposed to only contents changing.
    pub structural: bool,
}

/// Keeps the OS watch alive; dropping it stops watching.
pub struct FsWatcher {
    _watcher: RecommendedWatcher,
}

/// Starts watching `root` recursively. Changes inside `.git` and paths
/// ignored by the root `.gitignore` (build output, `node_modules`) are dropped
/// on the watcher thread, so a build doesn't flood the UI.
pub fn watch(root: &Path) -> notify::Result<(FsWatcher, UnboundedReceiver<FsEvent>)> {
    let (tx, rx) = unbounded();
    let ignore = root_gitignore(root);
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let structural = match event.kind {
            EventKind::Create(_) | EventKind::Remove(_) => true,
            EventKind::Modify(notify::event::ModifyKind::Name(_)) => true,
            EventKind::Modify(_) | EventKind::Any => false,
            EventKind::Access(_) | EventKind::Other => return,
        };
        let paths: Vec<PathBuf> = event
            .paths
            .into_iter()
            .filter(|path| !is_ignored(&ignore, path))
            .collect();
        if !paths.is_empty() {
            let _ = tx.unbounded_send(FsEvent { paths, structural });
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok((FsWatcher { _watcher: watcher }, rx))
}

/// Watches a repository's `.git` folder for changes to HEAD, the index and
/// refs (commits, staging, branch switches, fetches), from Ion or anywhere
/// else. Object writes and lock files are dropped on the watcher thread.
pub fn watch_git_dir(git_dir: &Path) -> notify::Result<(FsWatcher, UnboundedReceiver<()>)> {
    let (tx, rx) = unbounded();
    let root = git_dir.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.paths.iter().any(|path| is_git_state(&root, path)) {
            let _ = tx.unbounded_send(());
        }
    })?;
    watcher.watch(git_dir, RecursiveMode::NonRecursive)?;
    let refs = git_dir.join("refs");
    if refs.is_dir() {
        watcher.watch(&refs, RecursiveMode::Recursive)?;
    }
    Ok((FsWatcher { _watcher: watcher }, rx))
}

fn is_git_state(git_dir: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(git_dir) else {
        return false;
    };
    if path.extension().is_some_and(|ext| ext == "lock") {
        return false;
    }
    let first = relative.components().next();
    first.is_some_and(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some("HEAD" | "index" | "refs" | "packed-refs" | "MERGE_HEAD" | "FETCH_HEAD")
        )
    })
}

fn root_gitignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    builder.add(root.join(".gitignore"));
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn is_ignored(ignore: &Gitignore, path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == ".git")
        || (path.starts_with(ignore.path())
            && ignore.matched_path_or_any_parents(path, false).is_ignore())
}
