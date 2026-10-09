use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, SyncSender},
};

use ignore::WalkBuilder;
use notify::{EventKind, RecursiveMode, Watcher};
use remote_protocol::WatchEvent;

pub enum Msg {
    Event(notify::Result<notify::Event>),
    Stop,
}
fn eligible(path: &Path) -> bool {
    if path.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some("node_modules" | "target" | ".venv" | "__pycache__")
        )
    }) {
        return false;
    }
    let value = path.to_string_lossy().replace('\\', "/");
    !value.contains("/.git/objects")
        && !value.contains("/.git/logs")
        && !value.contains("/.ion-save-")
}
/// inotify watches one directory at a time, so on Linux each eligible
/// directory is registered on its own and `node_modules`, `target` and the like
/// cost nothing. FSEvents (macOS) is one stream for the whole tree, and
/// re-registering it per directory would restart it thousands of times.
const PER_DIRECTORY: bool = cfg!(target_os = "linux");

fn add_tree(
    watcher: &mut notify::RecommendedWatcher,
    root: &Path,
    watched: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| eligible(entry.path()))
        .build()
    {
        let entry = entry.map_err(io::Error::other)?;
        if entry.file_type().is_some_and(|kind| kind.is_dir()) && !watched.contains(entry.path()) {
            watcher
                .watch(entry.path(), RecursiveMode::NonRecursive)
                .map_err(io::Error::other)?;
            watched.insert(entry.into_path());
        }
    }
    Ok(())
}
/// Watches `root` until `Msg::Stop` arrives; `tx` is the sender the OS callback feeds.
pub fn run(
    root: &str,
    emit: impl Fn(WatchEvent) -> io::Result<()>,
    tx: SyncSender<Msg>,
    rx: Receiver<Msg>,
) -> io::Result<()> {
    let root = std::fs::canonicalize(root)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let callback_overflow = overflow.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
        if tx.try_send(Msg::Event(event)).is_err() {
            callback_overflow.store(true, Ordering::Relaxed);
        }
    })
    .map_err(io::Error::other)?;
    let mut watched = HashSet::new();
    let added = if PER_DIRECTORY {
        add_tree(&mut watcher, &root, &mut watched)
    } else {
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(io::Error::other)
    };
    if let Err(error) = added {
        emit(WatchEvent {
            ready: false,
            paths: vec![],
            structural: false,
            error: Some(format!(
                "Cannot watch {}: {error}. Check directory permissions and the host's filesystem watch limits; Refresh Project remains available.",
                root.display()
            )),
        })?;
        return Ok(());
    }
    emit(WatchEvent {
        ready: true,
        paths: vec![],
        structural: false,
        error: None,
    })?;
    while let Ok(Msg::Event(event)) = rx.recv() {
        if overflow.swap(false, Ordering::Relaxed) {
            emit(WatchEvent {
                ready: false,
                paths: vec![root.to_string_lossy().replace('\\', "/")],
                structural: true,
                error: None,
            })?;
        }
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                emit(WatchEvent {
                    ready: false,
                    paths: vec![],
                    structural: false,
                    error: Some(error.to_string()),
                })?;
                continue;
            }
        };
        if matches!(event.kind, EventKind::Access(_) | EventKind::Other) {
            continue;
        }
        let structural = matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                | EventKind::Any
        );
        let mut paths = Vec::new();
        for path in event.paths.into_iter().filter(|path| eligible(path)) {
            if PER_DIRECTORY && structural && path.is_dir() {
                // Directory renames require replacing registrations keyed by the
                // old pathname, rather than retaining stale watch descriptors.
                let gone: Vec<_> = watched
                    .iter()
                    .filter(|old| !old.is_dir())
                    .cloned()
                    .collect();
                for old in gone {
                    let _ = watcher.unwatch(&old);
                    watched.remove(&old);
                }
                if let Err(error) = add_tree(&mut watcher, &path, &mut watched) {
                    emit(WatchEvent {
                        ready: false,
                        paths: vec![],
                        structural: false,
                        error: Some(error.to_string()),
                    })?;
                }
            }
            paths.push(path.to_string_lossy().replace('\\', "/"));
        }
        if !paths.is_empty() {
            emit(WatchEvent {
                ready: false,
                paths,
                structural,
                error: None,
            })?;
        }
    }
    Ok(())
}
