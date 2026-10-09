//! The list of files in a project, for quick open and project search.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ignore::{WalkBuilder, WalkState};

/// Indexing stops after this many files.
const MAX_FILES: usize = 500_000;
/// How often indexing reports how many files it has found.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct IndexedFile {
    /// Path relative to the root, with `/` separators.
    pub path: Box<str>,
    /// Lowercased `path`, for case-insensitive matching.
    pub path_lower: Box<str>,
    /// Byte offset where the file name starts in `path`.
    pub name_start: usize,
}

impl IndexedFile {
    fn new(path: String) -> Self {
        Self {
            name_start: path.rfind('/').map_or(0, |slash| slash + 1),
            path_lower: path.to_lowercase().into(),
            path: path.into(),
        }
    }

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
    /// Positions in `files`, in the order project search scans them: source
    /// code, then other text, then everything else (see [`Tier`]).
    pub search_order: Box<[u32]>,
    /// The server's version of this list (remote projects), so the next
    /// rebuild fetches only what changed.
    pub version: Option<String>,
}

/// How likely a file is to hold what a project search looks for, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Source,
    Text,
    Other,
}

/// Programming language extensions, lowercase.
const SOURCE_EXTENSIONS: &[&str] = &[
    "asm", "bash", "bat", "c", "cc", "cjs", "clj", "cljs", "cmake", "cmd", "cpp", "cs", "cts",
    "cu", "cxx", "dart", "erl", "ex", "exs", "fish", "fs", "fsx", "glsl", "go", "gradle",
    "graphql", "h", "hh", "hlsl", "hpp", "hrl", "hs", "hxx", "java", "jl", "js", "jsx", "kt",
    "kts", "lua", "m", "metal", "mjs", "ml", "mli", "mm", "mts", "nim", "php", "pl", "pm", "proto",
    "ps1", "psm1", "py", "pyi", "r", "rb", "rs", "s", "scala", "sh", "sql", "sv", "svelte",
    "swift", "ts", "tsx", "v", "vhd", "vue", "wgsl", "zig", "zsh",
];

/// Docs, config and data formats, lowercase.
const TEXT_EXTENSIONS: &[&str] = &[
    "adoc",
    "cfg",
    "conf",
    "css",
    "csv",
    "env",
    "htm",
    "html",
    "ini",
    "json",
    "json5",
    "jsonc",
    "less",
    "markdown",
    "md",
    "properties",
    "rst",
    "sass",
    "scss",
    "toml",
    "tsv",
    "txt",
    "xml",
    "yaml",
    "yml",
];

impl Tier {
    /// The tier of a file name. Names without an extension (`Makefile`,
    /// `README`, scripts) are usually text.
    pub fn of(name: &str) -> Self {
        // A leading dot (`.gitignore`) starts a name, not an extension.
        let Some((_, extension)) = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty())
        else {
            return Self::Text;
        };
        let known = |list: &[&str]| {
            list.binary_search_by(|probe| {
                probe
                    .bytes()
                    .cmp(extension.bytes().map(|b| b.to_ascii_lowercase()))
            })
            .is_ok()
        };
        if known(SOURCE_EXTENSIONS) {
            Self::Source
        } else if known(TEXT_EXTENSIONS) {
            Self::Text
        } else {
            Self::Other
        }
    }
}

impl FileIndex {
    /// Walks the project on all cores (call from a background thread).
    pub fn build(root: &Path) -> Self {
        Self::build_with_progress(root, &|_| {})
    }

    /// Like [`build`](Self::build), calling `progress` with the number of
    /// files found so far at most every [`PROGRESS_INTERVAL`].
    pub fn build_with_progress(root: &Path, progress: &(dyn Fn(usize) + Sync)) -> Self {
        // The files found, and when their count was last reported.
        let found = Mutex::new((Vec::new(), Instant::now()));
        walker(root).build_parallel().run(|| {
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
                let (paths, last_report) = &mut *found;
                if paths.len() >= MAX_FILES {
                    return WalkState::Quit;
                }
                paths.push(path);
                if last_report.elapsed() >= PROGRESS_INTERVAL {
                    *last_report = Instant::now();
                    progress(paths.len());
                }
                WalkState::Continue
            })
        });

        Self::from_paths(
            root.to_path_buf(),
            found.into_inner().expect("index lock poisoned").0,
        )
    }

    pub fn from_paths(root: PathBuf, paths: impl IntoIterator<Item = String>) -> Self {
        Self::from_files(root, paths.into_iter().map(IndexedFile::new).collect())
    }

    fn from_files(root: PathBuf, mut files: Vec<IndexedFile>) -> Self {
        // Stable: mostly sorted input (an update) sorts in about one pass.
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        files.truncate(MAX_FILES);
        let mut search_order: Box<[u32]> = (0..files.len() as u32).collect();
        // Stable, so each tier stays in path order.
        search_order.sort_by_cached_key(|&ix| Tier::of(files[ix as usize].name()));
        Self {
            root,
            files,
            search_order,
            version: None,
        }
    }

    /// Where `path` (absolute) is in `files`.
    pub fn position(&self, path: &Path) -> Option<usize> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        self.files
            .binary_search_by(|file| (*file.path).cmp(relative.as_str()))
            .ok()
    }

    /// This list with `removed` taken out and `added` put in.
    pub fn with_changes(&self, added: Vec<String>, removed: &[String]) -> Self {
        let removed: HashSet<&str> = removed.iter().map(String::as_str).collect();
        let kept = self
            .files
            .iter()
            .filter(|file| !removed.contains(&*file.path))
            .cloned();
        Self::from_files(
            self.root.clone(),
            kept.chain(added.into_iter().map(IndexedFile::new))
                .collect(),
        )
    }

    /// This list after files or folders at `changed` (absolute paths the
    /// file watcher reported, already filtered for ignored ones) were
    /// created, removed or renamed. Each is looked up on disk: what's gone
    /// is dropped with everything under it, and a folder that appeared is
    /// walked. `None` when the root itself changed (walk it all again).
    pub fn updated(&self, changed: &[PathBuf]) -> Option<Self> {
        let mut gone: HashSet<String> = HashSet::new();
        let mut added = Vec::new();
        for path in changed {
            let relative = path.strip_prefix(&self.root).ok()?;
            if relative.as_os_str().is_empty() {
                return None;
            }
            gone.insert(relative.to_string_lossy().replace('\\', "/"));
            if path.is_file() {
                added.push(path.clone());
            } else if path.is_dir() {
                for entry in walker(path).build().flatten() {
                    if entry.file_type().is_some_and(|kind| kind.is_file()) {
                        added.push(entry.into_path());
                    }
                }
            }
        }
        // A file is gone if it or any folder above it is.
        let is_gone = |path: &str| {
            gone.contains(path)
                || path
                    .match_indices('/')
                    .any(|(slash, _)| gone.contains(&path[..slash]))
        };
        let kept = self
            .files
            .iter()
            .filter(|file| !is_gone(&file.path))
            .cloned();
        let added = added.into_iter().filter_map(|path| {
            let relative = path.strip_prefix(&self.root).ok()?;
            Some(IndexedFile::new(
                relative.to_string_lossy().replace('\\', "/"),
            ))
        });
        Some(Self::from_files(
            self.root.clone(),
            kept.chain(added).collect(),
        ))
    }

    pub fn absolute(&self, file: &IndexedFile) -> PathBuf {
        // Index paths use `/`; give back native separators.
        crate::join_path(&self.root, &file.path)
    }
}

/// Walks `path` the way the index does: hidden files included, `.git`
/// skipped, and ignore files honored even outside a repository (as the file
/// watcher does).
fn walker(path: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(path);
    builder
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_lists_are_sorted() {
        for list in [SOURCE_EXTENSIONS, TEXT_EXTENSIONS] {
            assert!(list.windows(2).all(|pair| pair[0] < pair[1]), "{list:?}");
        }
    }

    #[test]
    fn tiers_by_name() {
        assert_eq!(Tier::of("main.rs"), Tier::Source);
        assert_eq!(Tier::of("entry.S"), Tier::Source);
        assert_eq!(Tier::of("README.md"), Tier::Text);
        assert_eq!(Tier::of("Makefile"), Tier::Text);
        assert_eq!(Tier::of(".gitignore"), Tier::Text);
        assert_eq!(Tier::of("logo.png"), Tier::Other);
    }

    #[test]
    fn search_order_puts_source_first_then_text() {
        let index = FileIndex::from_paths(
            PathBuf::from("/p"),
            ["b.png", "a.md", "z.rs", "c/d.py", "Makefile"].map(String::from),
        );
        let order: Vec<&str> = index
            .search_order
            .iter()
            .map(|&ix| &*index.files[ix as usize].path)
            .collect();
        assert_eq!(order, ["c/d.py", "z.rs", "Makefile", "a.md", "b.png"]);
        assert_eq!(index.position(Path::new("/p/z.rs")), Some(4));
        assert_eq!(index.position(Path::new("/p/missing.rs")), None);
    }

    #[test]
    fn updates_from_watched_changes() {
        let root = std::env::temp_dir().join(format!("ion-index-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for file in ["a.rs", "old/x.rs", "old/y.rs", "src/b.rs"] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let index = FileIndex::build(&root);
        assert_eq!(index.files.len(), 4);

        // A folder renamed, a file deleted, a file created.
        std::fs::rename(root.join("old"), root.join("new")).unwrap();
        std::fs::remove_file(root.join("a.rs")).unwrap();
        std::fs::write(root.join("src/c.rs"), "").unwrap();
        let changed = ["old", "new", "a.rs", "src/c.rs"].map(|path| root.join(path));
        let next = index.updated(&changed).unwrap();
        let paths: Vec<&str> = next.files.iter().map(|file| &*file.path).collect();
        assert_eq!(paths, ["new/x.rs", "new/y.rs", "src/b.rs", "src/c.rs"]);
        assert_eq!(next.files[3].name(), "c.rs");
        assert_eq!(next.search_order.len(), 4);
        assert!(index.updated(std::slice::from_ref(&root)).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
