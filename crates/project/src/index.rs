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

/// A file in the index, borrowed from it.
#[derive(Clone, Copy)]
pub struct IndexedFile<'a> {
    /// Path relative to the root, with `/` separators.
    pub path: &'a str,
    /// Lowercased `path`, for case-insensitive matching.
    pub path_lower: &'a str,
    /// Byte offset where the file name starts in `path`.
    pub name_start: usize,
}

impl<'a> IndexedFile<'a> {
    pub fn name(&self) -> &'a str {
        &self.path[self.name_start..]
    }

    pub fn name_lower(&self) -> &'a str {
        let start = self.path_lower.rfind('/').map_or(0, |slash| slash + 1);
        &self.path_lower[start..]
    }
}

/// Where one file's strings are in the arenas.
#[derive(Clone, Copy)]
struct Entry {
    path: u32,
    /// In `lower`, or [`SAME_LOWER`] when the path has no uppercase.
    lower: u32,
    path_len: u16,
    lower_len: u16,
    name_start: u16,
}

const SAME_LOWER: u32 = u32::MAX;

/// Every file in the project, honoring `.gitignore`. Paths live in one
/// string (lowercase copies only for paths that need one), so a large
/// repository costs little more than its path bytes.
pub struct FileIndex {
    pub root: PathBuf,
    paths: String,
    lower: String,
    entries: Box<[Entry]>,
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
        let paths: Vec<String> = paths.into_iter().collect();
        Self::from_sorted(root, paths.iter().map(String::as_str).collect())
    }

    /// Builds the arenas from `paths`, sorting and deduplicating them first.
    fn from_sorted(root: PathBuf, mut paths: Vec<&str>) -> Self {
        // Stable: mostly sorted input (an update) sorts in about one pass.
        paths.sort();
        paths.dedup();
        // Longer paths than an entry can describe aren't real files.
        paths.retain(|path| path.len() <= u16::MAX as usize);
        paths.truncate(MAX_FILES);
        let mut arena = String::with_capacity(paths.iter().map(|path| path.len()).sum());
        let mut lower = String::new();
        let mut entries = Vec::with_capacity(paths.len());
        for path in &paths {
            let lowered = path.to_lowercase();
            let lower_start = if lowered == *path || lowered.len() > u16::MAX as usize {
                SAME_LOWER
            } else {
                let start = lower.len() as u32;
                lower.push_str(&lowered);
                start
            };
            entries.push(Entry {
                path: arena.len() as u32,
                lower: lower_start,
                path_len: path.len() as u16,
                lower_len: if lower_start == SAME_LOWER {
                    0
                } else {
                    lowered.len() as u16
                },
                name_start: path.rfind('/').map_or(0, |slash| slash + 1) as u16,
            });
            arena.push_str(path);
        }
        lower.shrink_to_fit();
        let mut index = Self {
            root,
            paths: arena,
            lower,
            entries: entries.into(),
            search_order: Box::new([]),
            version: None,
        };
        let mut search_order: Box<[u32]> = (0..index.len() as u32).collect();
        // Stable, so each tier stays in path order.
        search_order.sort_by_cached_key(|&ix| Tier::of(index.file(ix as usize).name()));
        index.search_order = search_order;
        index
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The file at `ix`, in path order.
    pub fn file(&self, ix: usize) -> IndexedFile<'_> {
        let entry = self.entries[ix];
        let path = &self.paths[entry.path as usize..][..entry.path_len as usize];
        let path_lower = if entry.lower == SAME_LOWER {
            path
        } else {
            &self.lower[entry.lower as usize..][..entry.lower_len as usize]
        };
        IndexedFile {
            path,
            path_lower,
            name_start: entry.name_start as usize,
        }
    }

    /// Every file, in path order.
    pub fn files(&self) -> impl ExactSizeIterator<Item = IndexedFile<'_>> + '_ {
        (0..self.len()).map(|ix| self.file(ix))
    }

    /// Where `path` (absolute) is in `files`.
    pub fn position(&self, path: &Path) -> Option<usize> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        let mut lo = 0;
        let mut hi = self.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.file(mid).path.cmp(relative.as_str()) {
                std::cmp::Ordering::Equal => return Some(mid),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// This list with `removed` taken out and `added` put in.
    pub fn with_changes(&self, added: Vec<String>, removed: &[String]) -> Self {
        let removed: HashSet<&str> = removed.iter().map(String::as_str).collect();
        let paths = self
            .files()
            .map(|file| file.path)
            .filter(|path| !removed.contains(path))
            .chain(added.iter().map(String::as_str))
            .collect();
        Self::from_sorted(self.root.clone(), paths)
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
        let added: Vec<String> = added
            .into_iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&self.root).ok()?;
                Some(relative.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        let paths = self
            .files()
            .map(|file| file.path)
            .filter(|path| !is_gone(path))
            .chain(added.iter().map(String::as_str))
            .collect();
        Some(Self::from_sorted(self.root.clone(), paths))
    }

    pub fn absolute(&self, file: &IndexedFile) -> PathBuf {
        // Index paths use `/`; give back native separators.
        crate::join_path(&self.root, file.path)
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
            .map(|&ix| index.file(ix as usize).path)
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
        assert_eq!(index.len(), 4);

        // A folder renamed, a file deleted, a file created.
        std::fs::rename(root.join("old"), root.join("new")).unwrap();
        std::fs::remove_file(root.join("a.rs")).unwrap();
        std::fs::write(root.join("src/c.rs"), "").unwrap();
        let changed = ["old", "new", "a.rs", "src/c.rs"].map(|path| root.join(path));
        let next = index.updated(&changed).unwrap();
        let paths: Vec<&str> = next.files().map(|file| file.path).collect();
        assert_eq!(paths, ["new/x.rs", "new/y.rs", "src/b.rs", "src/c.rs"]);
        assert_eq!(next.file(3).name(), "c.rs");
        assert_eq!(next.search_order.len(), 4);
        assert!(index.updated(std::slice::from_ref(&root)).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
