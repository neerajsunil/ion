use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use project::FileIndex;
use remote_protocol::{
    Entry, IndexUpdate, MAX_FRAME, Match, Operation, RemoteError, SearchResults, Text,
};
use serde_json::{Value, json};

/// A file index and the version the client knows it by.
struct Indexed {
    version: String,
    index: Arc<FileIndex>,
}

pub struct Service {
    indexes: Mutex<HashMap<PathBuf, Indexed>>,
    /// Distinguishes this process's index versions from an earlier server's.
    instance: u64,
    next_version: AtomicU64,
}
impl Default for Service {
    fn default() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as u64);
        Self {
            indexes: Mutex::default(),
            instance: nanos ^ u64::from(std::process::id()).rotate_left(32),
            next_version: AtomicU64::new(1),
        }
    }
}

fn path(value: &str) -> io::Result<PathBuf> {
    if value.contains('\0') {
        return Err(io::Error::other("Path contains NUL"));
    }
    Ok(PathBuf::from(value))
}
fn wire_path(value: &Path) -> String {
    value.to_string_lossy().replace('\\', "/")
}
/// Fails with a clear error if `value` couldn't be sent in one frame.
/// `estimate` (a cheap size guess) avoids serializing small results twice.
fn ensure_fits(value: &Value, estimate: usize) -> io::Result<()> {
    if estimate > MAX_FRAME / 8 && serde_json::to_vec(value)?.len() > MAX_FRAME - 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The result is too large to send over the connection",
        ));
    }
    Ok(())
}
fn remote_error(error: io::Error) -> RemoteError {
    RemoteError {
        kind: format!("{:?}", error.kind()),
        message: error.to_string(),
    }
}

impl Service {
    pub fn handle(&self, operation: Operation, cancel: &AtomicBool) -> Result<Value, RemoteError> {
        self.run(operation, cancel).map_err(remote_error)
    }
    fn run(&self, operation: Operation, cancel: &AtomicBool) -> io::Result<Value> {
        use Operation::*;
        match operation {
            Canonicalize { path: value } => Ok(json!(wire_path(&fs::canonicalize(path(&value)?)?))),
            ReadText { path: value } => {
                let loaded = project::load_text(&path(&value)?).map_err(|error| match error {
                    project::LoadError::Io(error) => error,
                    error => io::Error::new(io::ErrorKind::InvalidData, error),
                })?;
                let estimate = loaded.text.len();
                let value = serde_json::to_value(Text {
                    text: loaded.text,
                    has_bom: loaded.has_bom,
                })?;
                ensure_fits(&value, estimate)?;
                Ok(value)
            }
            SaveText {
                path: value,
                text,
                has_bom,
            } => {
                atomic_save(&path(&value)?, &text, has_bom)?;
                Ok(Value::Null)
            }
            ReadDir { path: value } => {
                let mut entries = Vec::new();
                for entry in fs::read_dir(path(&value)?)? {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name == ".git" {
                        continue;
                    }
                    let metadata = match entry.metadata() {
                        Ok(metadata) => metadata,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error),
                    };
                    let is_dir = if metadata.is_symlink() {
                        fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
                    } else {
                        metadata.is_dir()
                    };
                    entries.push(Entry { name, is_dir });
                }
                entries.sort_by_cached_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
                Ok(serde_json::to_value(entries)?)
            }
            IsDir { path: value } => Ok(json!(fs::metadata(path(&value)?)?.is_dir())),
            Exists { path: value } => Ok(json!(path(&value)?.try_exists()?)),
            Files { paths } => {
                let mut files = Vec::new();
                for value in paths {
                    if path(&value)?.is_file() {
                        files.push(value);
                    }
                }
                Ok(json!(files))
            }
            CreateDir { path: value } => {
                fs::create_dir_all(path(&value)?)?;
                Ok(Value::Null)
            }
            CreateFile { path: value } => {
                let value = path(&value)?;
                if let Some(parent) = value.parent() {
                    fs::create_dir_all(parent)?;
                }
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(value)?;
                Ok(Value::Null)
            }
            Rename { from, to } => {
                let from = path(&from)?;
                let to = path(&to)?;
                if fs::symlink_metadata(&to).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "Destination already exists",
                    ));
                }
                fs::rename(from, to)?;
                Ok(Value::Null)
            }
            CopyInto { from, dir, cut } => {
                let from = path(&from)?;
                let dir = path(&dir)?;
                let destination = project::unique_destination(
                    &dir,
                    &from.file_name().unwrap_or_default().to_string_lossy(),
                );
                if fs::canonicalize(&dir)?.starts_with(fs::canonicalize(&from)?) {
                    return Err(io::Error::other("Can't copy or move a folder into itself"));
                }
                if cut {
                    project::move_path(&from, &destination)?;
                } else {
                    project::copy_recursive(&from, &destination)?;
                }
                Ok(json!(wire_path(&destination)))
            }
            Delete { paths } => {
                for value in paths {
                    let value = path(&value)?;
                    if !value.is_absolute()
                        || value.parent().is_none()
                        || fs::canonicalize(&value)?.parent().is_none()
                    {
                        return Err(io::Error::other(
                            "Cannot delete a filesystem root or relative path",
                        ));
                    }
                    let metadata = fs::symlink_metadata(&value)?;
                    if metadata.is_dir() {
                        fs::remove_dir_all(value)?;
                    } else {
                        fs::remove_file(value)?;
                    }
                }
                Ok(Value::Null)
            }
            Index { root, base } => {
                let root = path(&root)?;
                let index = Arc::new(FileIndex::build(&root));
                let version = format!(
                    "{:x}-{}",
                    self.instance,
                    self.next_version.fetch_add(1, Ordering::Relaxed)
                );
                let previous = self
                    .lock_indexes()?
                    .insert(
                        root,
                        Indexed {
                            version: version.clone(),
                            index: index.clone(),
                        },
                    )
                    .filter(|previous| base.as_ref() == Some(&previous.version));
                let update = match previous {
                    Some(previous) => {
                        let (added, removed) = diff(&previous.index, &index);
                        // A delta bigger than the list (a different project) isn't worth it.
                        if added.len() + removed.len() < index.files.len() {
                            IndexUpdate {
                                version,
                                base: Some(previous.version),
                                added,
                                removed,
                            }
                        } else {
                            whole(version, &index)
                        }
                    }
                    None => whole(version, &index),
                };
                let estimate = update
                    .added
                    .iter()
                    .chain(&update.removed)
                    .map(|file| file.len() + 3)
                    .sum();
                let value = serde_json::to_value(update)?;
                ensure_fits(&value, estimate)?;
                Ok(value)
            }
            Search {
                root,
                query,
                case_sensitive,
            } => {
                let root = path(&root)?;
                let cached = self
                    .lock_indexes()?
                    .get(&root)
                    .map(|indexed| indexed.index.clone());
                // Not cached under a version: the client's next Index gets the whole list.
                let index = match cached {
                    Some(index) => index,
                    None => {
                        let index = Arc::new(FileIndex::build(&root));
                        self.lock_indexes()?.insert(
                            root,
                            Indexed {
                                version: String::new(),
                                index: index.clone(),
                            },
                        );
                        index
                    }
                };
                let results = project::search::search(&index, &query, case_sensitive, cancel);
                let mut matches = Vec::new();
                let mut truncated = results.truncated;
                for file in results.files {
                    for line in file.lines {
                        if matches.len() == project::search::MAX_RESULTS {
                            truncated = true;
                            break;
                        }
                        matches.push(Match {
                            path: file.path.clone(),
                            line: line.line,
                            columns: [line.columns.start, line.columns.end],
                            preview: line.preview,
                            preview_range: [line.preview_range.start, line.preview_range.end],
                        });
                    }
                }
                let estimate = matches
                    .iter()
                    .map(|m| m.path.len() + m.preview.len() + 64)
                    .sum();
                let value = serde_json::to_value(SearchResults { matches, truncated })?;
                ensure_fits(&value, estimate)?;
                Ok(value)
            }
        }
    }
}

impl Service {
    fn lock_indexes(&self) -> io::Result<std::sync::MutexGuard<'_, HashMap<PathBuf, Indexed>>> {
        self.indexes
            .lock()
            .map_err(|_| io::Error::other("Index lock poisoned"))
    }
}

fn whole(version: String, index: &FileIndex) -> IndexUpdate {
    IndexUpdate {
        version,
        base: None,
        added: index
            .files
            .iter()
            .map(|file| file.path.to_string())
            .collect(),
        removed: Vec::new(),
    }
}

/// Files in `new` but not `old`, and the reverse. Both lists are sorted.
fn diff(old: &FileIndex, new: &FileIndex) -> (Vec<String>, Vec<String>) {
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    let mut old = old.files.iter().map(|file| &*file.path).peekable();
    let mut new = new.files.iter().map(|file| &*file.path).peekable();
    loop {
        match (old.peek(), new.peek()) {
            (Some(a), Some(b)) if a == b => {
                old.next();
                new.next();
            }
            (Some(a), Some(b)) if a < b => removed.extend(old.next().map(str::to_owned)),
            (_, Some(_)) => added.extend(new.next().map(str::to_owned)),
            (Some(_), None) => removed.extend(old.next().map(str::to_owned)),
            (None, None) => return (added, removed),
        }
    }
}

fn atomic_save(destination: &Path, text: &str, has_bom: bool) -> io::Result<()> {
    let destination = if fs::symlink_metadata(destination).is_ok_and(|meta| meta.is_symlink()) {
        fs::canonicalize(destination)?
    } else {
        destination.to_path_buf()
    };
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("File has no parent directory"))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".ion-save-")
        .tempfile_in(parent)?;
    if let Ok(metadata) = fs::metadata(&destination) {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    if has_bom {
        temp.write_all(b"\xef\xbb\xbf")?;
    }
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replaces_existing_files_and_keeps_original_after_failure() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("space ' é.txt");
        fs::write(&file, "old").unwrap();
        atomic_save(&file, "héllo", true).unwrap();
        let loaded = project::load_text(&file).unwrap();
        assert_eq!(loaded.text, "héllo");
        assert!(loaded.has_bom);
        assert!(atomic_save(&root.path().join("missing/file"), "bad", false).is_err());
        assert_eq!(project::load_text(&file).unwrap().text, "héllo");
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ion-save-")
        }));
    }
    fn index(service: &Service, root: &Path, base: Option<String>) -> IndexUpdate {
        let value = service
            .handle(
                Operation::Index {
                    root: wire_path(root),
                    base,
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn index_sends_only_changes_since_the_clients_version() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        for name in ["a.rs", "b.rs", "src/c.rs"] {
            fs::write(root.path().join(name), "").unwrap();
        }
        let service = Service::default();
        let first = index(&service, root.path(), None);
        assert_eq!(first.base, None);
        assert_eq!(first.added, ["a.rs", "b.rs", "src/c.rs"]);

        fs::remove_file(root.path().join("b.rs")).unwrap();
        fs::write(root.path().join("src/d.rs"), "").unwrap();
        let second = index(&service, root.path(), Some(first.version.clone()));
        assert_eq!(second.base.as_ref(), Some(&first.version));
        assert_eq!(second.added, ["src/d.rs"]);
        assert_eq!(second.removed, ["b.rs"]);

        // An unknown or outdated version gets the whole list.
        let third = index(&service, root.path(), Some(first.version));
        assert_eq!(third.base, None);
        assert_eq!(third.added, ["a.rs", "src/c.rs", "src/d.rs"]);
        let restarted = index(&Service::default(), root.path(), Some(third.version));
        assert_eq!(restarted.base, None);
    }
    #[test]
    fn files_keeps_only_existing_regular_files() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.rs");
        fs::write(&file, "").unwrap();
        let paths = [
            &file,
            &root.path().join("gone.rs"),
            &root.path().to_path_buf(),
        ];
        let value = Service::default()
            .handle(
                Operation::Files {
                    paths: paths.iter().map(|path| wire_path(path)).collect(),
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(value, json!([wire_path(&file)]));
    }
    #[test]
    fn refuses_root_deletion_and_destination_overwrite() {
        let service = Service::default();
        let cancel = AtomicBool::new(false);
        assert!(
            service
                .handle(
                    Operation::Delete {
                        paths: vec!["/".into()]
                    },
                    &cancel
                )
                .is_err()
        );
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        assert!(
            service
                .handle(
                    Operation::Rename {
                        from: wire_path(&a),
                        to: wire_path(&b)
                    },
                    &cancel
                )
                .is_err()
        );
        assert_eq!(fs::read_to_string(b).unwrap(), "b");
    }
    #[cfg(unix)]
    #[test]
    fn save_follows_symlink_and_preserves_executable_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("script");
        let link = root.path().join("link");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&target, &link).unwrap();
        atomic_save(&link, "new", false).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(target.clone()).unwrap(), "new");
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
