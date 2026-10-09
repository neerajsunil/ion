//! The project model: the open folder, its file index and file system watching.
//!
//! Pure logic with no UI dependencies. Heavy work (directory walks, indexing,
//! search) runs on background threads and hands results back to the UI.

mod filesystem;
mod fs;
mod index;
#[cfg(feature = "remote")]
mod remote_watcher;
pub mod search;
mod tasks;
mod watcher;

pub use filesystem::FileSystem;
pub use fs::{
    DirEntry, LoadError, LoadedText, content_hash, copy_recursive, load_text, move_path,
    read_dir_sorted, save_text, unique_destination,
};
pub use index::{FileIndex, IndexedFile};
#[cfg(feature = "remote")]
pub use remote_watcher::{RemoteWatcher, watch_remote};
pub use tasks::{RunTask, detect_tasks};
pub use watcher::{FsEvent, FsWatcher, watch, watch_git_dir};

use std::path::{Path, PathBuf};

/// Joins a `/`-separated relative path onto `dir`. Remote (Linux) paths
/// start with `/` and keep `/`; local paths get native separators.
pub fn join_path(dir: &Path, relative: &str) -> PathBuf {
    let dir_text = dir.to_string_lossy();
    if dir_text.starts_with('/') {
        let relative = relative.replace('\\', "/");
        let relative = relative.trim_start_matches('/');
        if relative.is_empty() {
            return dir.to_path_buf();
        }
        return PathBuf::from(format!(
            "{}/{relative}",
            dir_text.replace('\\', "/").trim_end_matches('/')
        ));
    }
    dir.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod join_tests {
    use super::*;

    #[test]
    fn remote_paths_keep_forward_slashes() {
        assert_eq!(
            join_path(Path::new("/home/dev/app"), "src/main.rs"),
            PathBuf::from("/home/dev/app/src/main.rs")
        );
        assert_eq!(join_path(Path::new("/"), "etc"), PathBuf::from("/etc"));
        assert_eq!(join_path(Path::new("/srv"), ""), PathBuf::from("/srv"));
        #[cfg(windows)]
        assert_eq!(
            join_path(Path::new(r"C:\code"), "src/main.rs"),
            PathBuf::from(r"C:\code\src\main.rs")
        );
    }
}
