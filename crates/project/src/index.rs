//! The list of files in a project, for quick open and project search.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};

/// Indexing stops after this many files.
const MAX_FILES: usize = 500_000;

pub struct IndexedFile {
    /// Path relative to the root, with `/` separators.
    pub path: Box<str>,
    /// Lowercased `path`, for case-insensitive matching.
    pub path_lower: Box<str>,
    /// Byte offset where the file name starts in `path`.
    pub name_start: usize,
}

impl IndexedFile {
    pub fn name(&self) -> &str {
        &self.path[self.name_start..]
    }

    pub fn name_lower(&self) -> &str {
        &self.path_lower[self.name_start..]
    }
}

/// Every file in the project, honoring `.gitignore`.
pub struct FileIndex {
    pub root: PathBuf,
    pub files: Vec<IndexedFile>,
}

impl FileIndex {
    /// Walks the project on all cores (call from a background thread).
    pub fn build(root: &Path) -> Self {
        let found = Mutex::new(Vec::new());
        WalkBuilder::new(root)
            .hidden(false)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build_parallel()
            .run(|| {
                Box::new(|entry| {
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                        return WalkState::Continue;
                    }
                    let Ok(relative) = entry.path().strip_prefix(root) else {
                        return WalkState::Continue;
                    };
                    let path = relative.to_string_lossy().replace('\\', "/");
                    let mut found = found.lock().expect("index lock poisoned");
                    if found.len() >= MAX_FILES {
                        return WalkState::Quit;
                    }
                    found.push(path);
                    WalkState::Continue
                })
            });

        Self::from_paths(
            root.to_path_buf(),
            found.into_inner().expect("index lock poisoned"),
        )
    }

    pub fn from_paths(root: PathBuf, paths: impl IntoIterator<Item = String>) -> Self {
        let mut paths: Vec<_> = paths.into_iter().take(MAX_FILES).collect();
        paths.sort_unstable();
        paths.dedup();
        let files = paths
            .into_iter()
            .map(|path| IndexedFile {
                name_start: path.rfind('/').map_or(0, |slash| slash + 1),
                path_lower: path.to_lowercase().into(),
                path: path.into(),
            })
            .collect();
        Self { root, files }
    }

    pub fn absolute(&self, file: &IndexedFile) -> PathBuf {
        // Index paths use `/`; give back native separators.
        crate::join_path(&self.root, &file.path)
    }
}
