//! File system helpers: directory listing and text file loading and saving.

use std::fmt;
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hasher};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Directory names never shown in the file tree.
const HIDDEN_DIRS: &[&str] = &[".git"];

/// How many leading bytes are checked for NUL when detecting binary files.
const BINARY_SNIFF_LEN: usize = 8 * 1024;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Lists a directory with folders first, then files, each sorted case-insensitively.
pub fn read_dir_sorted(dir: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries: Vec<DirEntry> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().ok()?.is_dir();
            if is_dir && HIDDEN_DIRS.contains(&name.as_str()) {
                return None;
            }
            Some(DirEntry {
                path: entry.path(),
                name,
                is_dir,
            })
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
    Ok(entries)
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Binary,
    NotUtf8,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(err) => write!(f, "{err}"),
            LoadError::Binary => f.write_str("binary file"),
            LoadError::NotUtf8 => f.write_str("file is not valid UTF-8"),
        }
    }
}

impl std::error::Error for LoadError {}

#[derive(Debug, PartialEq, Eq)]
pub struct LoadedText {
    pub text: String,
    /// Whether the file started with a UTF-8 byte order mark (stripped from `text`).
    pub has_bom: bool,
    /// [`content_hash`] of `text`.
    pub hash: u64,
}

/// Hash of text content, fed in chunks (e.g. rope chunks). Used to tell our
/// own saves apart from outside changes when the watcher reports a file.
pub fn content_hash<'a>(chunks: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for chunk in chunks {
        hasher.write(chunk.as_bytes());
    }
    hasher.finish()
}

pub fn load_text(path: &Path) -> Result<LoadedText, LoadError> {
    let bytes = fs::read(path).map_err(LoadError::Io)?;
    decode_text(&bytes)
}

pub(crate) fn decode_text(bytes: &[u8]) -> Result<LoadedText, LoadError> {
    let (has_bom, body) = match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    if body[..body.len().min(BINARY_SNIFF_LEN)].contains(&0) {
        return Err(LoadError::Binary);
    }
    let text = String::from_utf8(body.to_vec()).map_err(|_| LoadError::NotUtf8)?;
    let hash = content_hash([text.as_str()]);
    Ok(LoadedText {
        text,
        has_bom,
        hash,
    })
}

/// Writes text chunks to `path`, replacing its contents.
pub fn save_text<'a>(
    path: &Path,
    has_bom: bool,
    chunks: impl IntoIterator<Item = &'a str>,
) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    if has_bom {
        out.write_all(UTF8_BOM)?;
    }
    for chunk in chunks {
        out.write_all(chunk.as_bytes())?;
    }
    out.flush()
}

/// A path in `dir` for an entry named `name` that doesn't exist yet:
/// `name`, then "stem copy.ext", "stem copy 2.ext", ...
pub fn unique_destination(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    // Keep the extension last ("main copy.rs"); dotfiles have no stem split.
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                " copy".to_owned()
            } else {
                format!(" copy {n}")
            };
            dir.join(format!("{stem}{suffix}{ext}"))
        })
        .find(|path| !path.exists())
        .expect("some name is free")
}

/// Copies a file, or a folder and everything in it.
pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() {
        if to.starts_with(from) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "can't copy a folder into itself",
            ));
        }
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(drop)
    }
}

/// Moves a file or folder, copying across drives when a rename can't.
pub fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    if from.is_dir() && to.starts_with(from) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "can't move a folder into itself",
        ));
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_recursive(from, to)?;
    if from.is_dir() {
        fs::remove_dir_all(from)
    } else {
        fs::remove_file(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ion-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_folders_first_and_hides_git() {
        let dir = temp_dir("list");
        fs::create_dir(dir.join(".git")).unwrap();
        fs::create_dir(dir.join("src")).unwrap();
        fs::write(dir.join("b.txt"), "").unwrap();
        fs::write(dir.join("A.txt"), "").unwrap();
        let names: Vec<_> = read_dir_sorted(&dir)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["src", "A.txt", "b.txt"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_and_save_round_trip_keeps_bom() {
        let dir = temp_dir("bom");
        let path = dir.join("bom.txt");
        fs::write(&path, b"\xEF\xBB\xBFhello").unwrap();
        let loaded = load_text(&path).unwrap();
        assert_eq!(loaded.text, "hello");
        assert!(loaded.has_bom);
        save_text(&path, loaded.has_bom, ["hel", "lo!"]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\xEF\xBB\xBFhello!");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hash_ignores_chunk_boundaries() {
        assert_eq!(
            content_hash(["hello world"]),
            content_hash(["hel", "lo wo", "rld"])
        );
        assert_ne!(content_hash(["a"]), content_hash(["b"]));
    }

    #[test]
    fn copies_and_moves_with_free_names() {
        let dir = temp_dir("copy");
        fs::create_dir_all(dir.join("src/inner")).unwrap();
        fs::write(dir.join("src/inner/a.rs"), "a").unwrap();
        fs::write(dir.join("main.rs"), "m").unwrap();

        let copy = unique_destination(&dir, "main.rs");
        assert_eq!(copy, dir.join("main copy.rs"));
        copy_recursive(&dir.join("main.rs"), &copy).unwrap();
        assert_eq!(
            unique_destination(&dir, "main.rs"),
            dir.join("main copy 2.rs")
        );
        assert_eq!(unique_destination(&dir, ".env"), dir.join(".env"));

        let folder = unique_destination(&dir, "src");
        copy_recursive(&dir.join("src"), &folder).unwrap();
        assert_eq!(fs::read_to_string(folder.join("inner/a.rs")).unwrap(), "a");
        assert!(copy_recursive(&dir.join("src"), &dir.join("src/inner/x")).is_err());

        move_path(&folder, &dir.join("moved")).unwrap();
        assert!(!folder.exists() && dir.join("moved/inner/a.rs").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_binary() {
        let dir = temp_dir("bin");
        let path = dir.join("x.bin");
        fs::write(&path, [1u8, 0, 2]).unwrap();
        assert!(matches!(load_text(&path), Err(LoadError::Binary)));
        fs::remove_dir_all(dir).unwrap();
    }
}
