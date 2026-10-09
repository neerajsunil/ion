//! File operations use the backend attached to the project.
use crate::{DirEntry, FileIndex, LoadError, LoadedText};
#[cfg(feature = "remote")]
use remote::{Connection, posix_path};
#[cfg(feature = "remote")]
use remote_protocol::{IndexUpdate, Operation};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(feature = "remote")]
use std::sync::Arc;

#[derive(Clone, Default, Debug)]
pub enum FileSystem {
    #[default]
    Local,
    #[cfg(feature = "remote")]
    Ssh(Arc<Connection>),
}
#[cfg(feature = "remote")]
fn wire(path: &Path) -> String {
    posix_path(path).to_string_lossy().into_owned()
}
impl FileSystem {
    #[cfg(feature = "remote")]
    pub fn remote(&self) -> Option<&Arc<Connection>> {
        match self {
            Self::Local => None,
            Self::Ssh(connection) => Some(connection),
        }
    }
    pub fn load_text(&self, path: &Path) -> Result<LoadedText, LoadError> {
        match self {
            Self::Local => crate::load_text(path),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => {
                let response: remote_protocol::Text = connection
                    .request(Operation::ReadText { path: wire(path) })
                    .map_err(|error| match error.to_string().as_str() {
                        "binary file" => LoadError::Binary,
                        "file is not valid UTF-8" => LoadError::NotUtf8,
                        _ => LoadError::Io(error),
                    })?;
                Ok(LoadedText {
                    hash: crate::content_hash([response.text.as_str()]),
                    text: response.text,
                    has_bom: response.has_bom,
                })
            }
        }
    }
    pub fn save_text<'a>(
        &self,
        path: &Path,
        has_bom: bool,
        chunks: impl IntoIterator<Item = &'a str>,
    ) -> io::Result<()> {
        match self {
            Self::Local => crate::save_text(path, has_bom, chunks),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::SaveText {
                path: wire(path),
                text: chunks.into_iter().collect(),
                has_bom,
            }),
        }
    }
    pub fn read_dir(&self, dir: &Path) -> io::Result<Vec<DirEntry>> {
        match self {
            Self::Local => crate::read_dir_sorted(dir),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => {
                let entries: Vec<remote_protocol::Entry> =
                    connection.request(Operation::ReadDir { path: wire(dir) })?;
                Ok(entries
                    .into_iter()
                    .map(|entry| DirEntry {
                        path: crate::join_path(dir, &entry.name),
                        name: entry.name,
                        is_dir: entry.is_dir,
                    })
                    .collect())
            }
        }
    }
    pub fn is_dir(&self, path: &Path) -> io::Result<bool> {
        match self {
            Self::Local => Ok(path.is_dir()),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::IsDir { path: wire(path) }),
        }
    }
    pub fn exists(&self, path: &Path) -> io::Result<bool> {
        match self {
            Self::Local => Ok(path.exists()),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::Exists { path: wire(path) }),
        }
    }
    /// Those of `paths` that are regular files now, in one round trip for
    /// remote projects.
    pub fn files_among(&self, paths: Vec<PathBuf>) -> io::Result<Vec<PathBuf>> {
        match self {
            Self::Local => Ok(paths.into_iter().filter(|path| path.is_file()).collect()),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => {
                let files: Vec<String> = connection.request(Operation::Files {
                    paths: paths.iter().map(|path| wire(path)).collect(),
                })?;
                let files: std::collections::HashSet<String> = files.into_iter().collect();
                Ok(paths
                    .into_iter()
                    .filter(|path| files.contains(&wire(path)))
                    .collect())
            }
        }
    }
    pub fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        match self {
            Self::Local => std::fs::create_dir_all(path),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::CreateDir { path: wire(path) }),
        }
    }
    pub fn create_file(&self, path: &Path) -> io::Result<()> {
        match self {
            Self::Local => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::File::create_new(path).map(drop)
            }
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::CreateFile { path: wire(path) }),
        }
    }
    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match self {
            Self::Local => {
                if to.exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "Destination already exists",
                    ));
                }
                std::fs::rename(from, to)
            }
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::Rename {
                from: wire(from),
                to: wire(to),
            }),
        }
    }
    pub fn copy_into(&self, from: &Path, dir: &Path, cut: bool) -> io::Result<PathBuf> {
        match self {
            Self::Local => {
                let destination = crate::unique_destination(
                    dir,
                    &from.file_name().unwrap_or_default().to_string_lossy(),
                );
                if cut {
                    crate::move_path(from, &destination)?;
                } else {
                    crate::copy_recursive(from, &destination)?;
                }
                Ok(destination)
            }
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection
                .request::<String>(Operation::CopyInto {
                    from: wire(from),
                    dir: wire(dir),
                    cut,
                })
                .map(PathBuf::from),
        }
    }
    pub fn delete(&self, paths: &[PathBuf]) -> io::Result<()> {
        #[cfg(not(feature = "remote"))]
        let _ = paths;
        match self {
            Self::Local => Err(io::Error::other("Use the local Recycle Bin")),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => connection.request(Operation::Delete {
                paths: paths.iter().map(|path| wire(path)).collect(),
            }),
        }
    }
    /// The project's files. A remote rebuild given the `previous` index
    /// fetches only the changes since it.
    #[cfg_attr(not(feature = "remote"), allow(unused_variables))]
    /// Lists the project's files. Local projects call `progress` with the
    /// number found so far as they go; remote ones get the list in one reply.
    pub fn build_index(
        &self,
        root: &Path,
        previous: Option<&FileIndex>,
        progress: &(dyn Fn(usize) + Sync),
    ) -> io::Result<FileIndex> {
        match self {
            Self::Local => Ok(FileIndex::build_with_progress(root, progress)),
            #[cfg(feature = "remote")]
            Self::Ssh(connection) => {
                let previous = previous.filter(|previous| previous.root == root);
                let update: IndexUpdate = connection.request_job(
                    Operation::Index {
                        root: wire(root),
                        base: previous.and_then(|previous| previous.version.clone()),
                    },
                    &std::sync::atomic::AtomicBool::new(false),
                )?;
                let mut index = match (update.base, previous) {
                    (Some(_), Some(previous)) => {
                        previous.with_changes(update.added, &update.removed)
                    }
                    (Some(_), None) => {
                        return Err(io::Error::other(
                            "Ion server sent changes to an unknown list",
                        ));
                    }
                    (None, _) => FileIndex::from_paths(root.to_path_buf(), update.added),
                };
                index.version = Some(update.version);
                Ok(index)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_backend_preserves_bom_and_refuses_overwrites() {
        let dir = std::env::temp_dir().join(format!("ion-backend-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("first.txt");
        FileSystem::Local.save_text(&path, true, ["héllo"]).unwrap();
        let loaded = FileSystem::Local.load_text(&path).unwrap();
        assert!(loaded.has_bom);
        assert_eq!(loaded.text, "héllo");
        assert!(FileSystem::Local.create_file(&path).is_err());
        let copy = FileSystem::Local.copy_into(&path, &dir, false).unwrap();
        assert_eq!(copy.file_name().unwrap(), "first copy.txt");
        assert!(FileSystem::Local.rename(&path, &copy).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn index_changes_apply_to_the_previous_list() {
        let root = PathBuf::from("/project");
        let index = FileIndex::from_paths(root, ["b.rs", "a.rs", "src/c.rs"].map(String::from));
        let next = index.with_changes(vec!["src/d.rs".into(), "0.rs".into()], &["b.rs".into()]);
        let paths: Vec<&str> = next.files.iter().map(|file| &*file.path).collect();
        assert_eq!(paths, ["0.rs", "a.rs", "src/c.rs", "src/d.rs"]);
        assert_eq!(next.files[3].name(), "d.rs");
    }
}
