//! Live file updates for remote projects: `ion-server` streams inotify
//! events over the project's SSH connection.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use remote::{Connection, StreamCloser, join_posix, posix_path};

use crate::FsEvent;

/// Stops the remote watcher when dropped.
pub struct RemoteWatcher {
    _closer: StreamCloser,
}

pub fn watch_remote(
    connection: Arc<Connection>,
    root: &Path,
) -> io::Result<(RemoteWatcher, UnboundedReceiver<Result<FsEvent, String>>)> {
    let (closer, events) = connection.watch(root)?;
    let first = events.recv();
    if !first.as_ref().is_ok_and(|event| event.ready) {
        return Err(io::Error::other(format!(
            "Remote file watcher could not start: {}",
            first
                .ok()
                .and_then(|event| event.error)
                .unwrap_or_else(|| "it ended unexpectedly".to_owned())
        )));
    }
    let (tx, rx) = unbounded();
    let root = root.to_path_buf();
    let posix_root = posix_path(&root).to_string_lossy().into_owned();
    std::thread::Builder::new()
        .name("ion-ssh-watch".into())
        .spawn(move || {
            for event in events {
                if let Some(error) = event.error {
                    let _ = tx.unbounded_send(Err(error));
                    return;
                }
                let paths = event
                    .paths
                    .iter()
                    .filter_map(|path| relative_to(path, &posix_root))
                    .map(|relative| match relative {
                        "" => root.clone(),
                        relative => join_posix(&root, relative),
                    })
                    .collect::<Vec<PathBuf>>();
                let event = FsEvent {
                    paths,
                    structural: event.structural,
                };
                if tx.unbounded_send(Ok(event)).is_err() {
                    return;
                }
            }
            // The stream ends when the connection drops; the workspace
            // starts a new watcher once it's back.
            let _ = tx.unbounded_send(Err(
                "Live file updates paused: the SSH connection was lost.".into(),
            ));
        })?;
    Ok((RemoteWatcher { _closer: closer }, rx))
}

fn relative_to<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    let rest = path.strip_prefix(root)?;
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('/')
    }
}

#[cfg(test)]
mod tests {
    use super::relative_to;

    #[test]
    fn paths_relative_to_the_project() {
        assert_eq!(
            relative_to("/srv/app/src/main.rs", "/srv/app"),
            Some("src/main.rs")
        );
        assert_eq!(relative_to("/srv/app", "/srv/app/"), Some(""));
        assert_eq!(relative_to("/srv/application/x", "/srv/app"), None);
    }
}
