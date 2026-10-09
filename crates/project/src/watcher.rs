//! Watches the project folder for changes made outside Ion (agents, git, builds).
//!
//! Uses the OS notification API (ReadDirectoryChangesW on Windows), so the
//! watcher thread sleeps in the kernel until something actually changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use ignore::Match;
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

/// Starts watching `root` recursively. Changes inside `.git` and ignored
/// paths (build output, `node_modules`) are dropped on the watcher thread, so
/// a build doesn't flood the UI. Ignored means what the file index skips:
/// see [`IgnoreRules`].
pub fn watch(root: &Path) -> notify::Result<(FsWatcher, UnboundedReceiver<FsEvent>)> {
    let (tx, rx) = unbounded();
    let mut ignore = IgnoreRules::new(root);
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
        for path in &event.paths {
            ignore.forget_changed_rules(path);
        }
        let paths: Vec<PathBuf> = event
            .paths
            .into_iter()
            .filter(|path| !ignore.is_ignored(path))
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

/// The rules the file index walks with: `.gitignore` and `.ignore` in each
/// folder from the repository root down, `.git/info/exclude` and git's
/// global excludes file. A folder's rules are read the first time a change
/// under it needs them, and again after they change. Folders under an
/// ignored one are never read.
struct IgnoreRules {
    /// The repository root, or the project root outside a repository.
    top: PathBuf,
    folders: HashMap<PathBuf, Option<Gitignore>>,
    /// `.git/info/exclude`, then the global excludes file (repositories only).
    repo: Vec<Gitignore>,
}

impl IgnoreRules {
    fn new(root: &Path) -> Self {
        let repo_top = root
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .map(Path::to_path_buf);
        let mut repo = Vec::new();
        if let Some(top) = &repo_top {
            let mut exclude = GitignoreBuilder::new(top);
            exclude.add(top.join(".git/info/exclude"));
            repo.extend(exclude.build().ok());
            repo.push(Gitignore::global().0);
        }
        Self {
            top: repo_top.unwrap_or_else(|| root.to_path_buf()),
            folders: HashMap::new(),
            repo,
        }
    }

    /// Re-reads a folder's rules after its `.gitignore` or `.ignore` changed.
    fn forget_changed_rules(&mut self, path: &Path) {
        let name = path.file_name();
        if (name == Some(".gitignore".as_ref()) || name == Some(".ignore".as_ref()))
            && let Some(folder) = path.parent()
        {
            self.folders.remove(folder);
        }
    }

    /// Whether `path` is in `.git` or ignored, checking each folder on the
    /// way down as git does: nothing under an ignored folder comes back.
    fn is_ignored(&mut self, path: &Path) -> bool {
        if path.components().any(|part| part.as_os_str() == ".git") {
            return true;
        }
        let Ok(relative) = path.strip_prefix(&self.top) else {
            return false;
        };
        let mut level = self.top.clone();
        let mut parts = relative.components().peekable();
        while let Some(part) = parts.next() {
            let parent = level.clone();
            level.push(part);
            let is_dir = parts.peek().is_some() || level.is_dir();
            if self.decide(&parent, &level, is_dir).is_ignore() {
                return true;
            }
        }
        false
    }

    /// The rules for `path`, whose folder is `parent`: the nearest folder's
    /// rules win, then the repository-wide ones.
    fn decide(&mut self, parent: &Path, path: &Path, is_dir: bool) -> Match<()> {
        for folder in parent.ancestors() {
            if !self.folders.contains_key(folder) {
                self.folders
                    .insert(folder.to_path_buf(), folder_rules(folder));
            }
            if let Some(rules) = &self.folders[folder] {
                match rules.matched(path, is_dir) {
                    Match::None => {}
                    found => return found.map(|_| ()),
                }
            }
            if folder == self.top {
                break;
            }
        }
        let relative = path.strip_prefix(&self.top).unwrap_or(path);
        for rules in &self.repo {
            match rules.matched(relative, is_dir) {
                Match::None => {}
                found => return found.map(|_| ()),
            }
        }
        Match::None
    }
}

/// A folder's own `.gitignore` and `.ignore` (which wins), if it has either.
fn folder_rules(folder: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(folder);
    for name in [".gitignore", ".ignore"] {
        let file = folder.join(name);
        if file.is_file() {
            builder.add(file);
        }
    }
    let rules = builder.build().ok()?;
    (rules.num_ignores() + rules.num_whitelists() > 0).then_some(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_rules_and_ignored_folders() {
        let root = std::env::temp_dir().join(format!("ion-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::create_dir_all(root.join("app/dist")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(root.join("app/.gitignore"), "dist/\n!keep.log\n").unwrap();
        std::fs::write(root.join(".git/info/exclude"), "scratch.txt\n").unwrap();
        let mut rules = IgnoreRules::new(&root);
        let ignored = |rules: &mut IgnoreRules, path: &str| rules.is_ignored(&root.join(path));
        assert!(ignored(&mut rules, "target/debug/a.o"));
        assert!(ignored(&mut rules, "app/dist/bundle.js"));
        assert!(ignored(&mut rules, "app/x.log"));
        assert!(!ignored(&mut rules, "app/keep.log"));
        assert!(ignored(&mut rules, "app/scratch.txt"));
        assert!(ignored(&mut rules, ".git/index"));
        assert!(!ignored(&mut rules, "app/src/main.rs"));
        // Nothing under an ignored folder is read.
        assert!(!rules.folders.contains_key(&root.join("target")));

        std::fs::write(root.join("app/.gitignore"), "").unwrap();
        rules.forget_changed_rules(&root.join("app/.gitignore"));
        assert!(!ignored(&mut rules, "app/dist/bundle.js"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
