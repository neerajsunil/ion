//! Project-wide text search.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use regex::{Regex, RegexBuilder};

use crate::FileIndex;

/// Search stops collecting after this many matching lines.
pub const MAX_RESULTS: usize = 5_000;
/// Files larger than this are skipped.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Long lines are cut down to this many chars in the results.
const PREVIEW_CHARS: usize = 200;

pub struct LineMatch {
    /// Zero-based line number.
    pub line: usize,
    /// Char columns of the first match on the line.
    pub columns: Range<usize>,
    /// The line, trimmed and shortened for display.
    pub preview: String,
    /// Byte range of the match within `preview`.
    pub preview_range: Range<usize>,
}

pub struct FileMatches {
    /// Path relative to the project root, with `/` separators.
    pub path: String,
    pub absolute: PathBuf,
    pub lines: Vec<LineMatch>,
}

pub struct SearchResults {
    pub files: Vec<FileMatches>,
    /// More matches existed than were collected.
    pub truncated: bool,
}

pub fn build_regex(query: &str, case_sensitive: bool) -> Option<Regex> {
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .build()
        .ok()
}

/// Searches every indexed file for `query`, spread across all cores. Stops
/// early when `cancel` is set (a newer search started).
pub fn search(
    index: &FileIndex,
    query: &str,
    case_sensitive: bool,
    cancel: &AtomicBool,
) -> SearchResults {
    let Some(regex) = build_regex(query, case_sensitive).filter(|_| !query.is_empty()) else {
        return SearchResults {
            files: Vec::new(),
            truncated: false,
        };
    };
    let next = AtomicUsize::new(0);
    let total = AtomicUsize::new(0);
    let found = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if cancel.load(Ordering::Relaxed)
                        || total.load(Ordering::Relaxed) >= MAX_RESULTS
                    {
                        return;
                    }
                    let ix = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = index.files.get(ix) else {
                        return;
                    };
                    let absolute = index.absolute(file);
                    let lines = search_file(&absolute, &regex);
                    if !lines.is_empty() {
                        total.fetch_add(lines.len(), Ordering::Relaxed);
                        found
                            .lock()
                            .expect("search lock poisoned")
                            .push(FileMatches {
                                path: file.path.to_string(),
                                absolute,
                                lines,
                            });
                    }
                }
            });
        }
    });

    let mut files = found.into_inner().expect("search lock poisoned");
    files.sort_by(|a, b| a.path.cmp(&b.path));
    SearchResults {
        files,
        truncated: total.load(Ordering::Relaxed) >= MAX_RESULTS,
    }
}

#[cfg(feature = "remote")]
pub fn search_with_filesystem(
    filesystem: &crate::FileSystem,
    index: &FileIndex,
    query: &str,
    case_sensitive: bool,
    cancel: &AtomicBool,
) -> std::io::Result<SearchResults> {
    let Some(connection) = filesystem.remote() else {
        return Ok(search(index, query, case_sensitive, cancel));
    };
    let mut results = SearchResults {
        files: Vec::new(),
        truncated: false,
    };
    if query.is_empty() || cancel.load(Ordering::Relaxed) {
        return Ok(results);
    }
    let response: remote_protocol::SearchResults = match connection.request_job(
        remote_protocol::Operation::Search {
            root: remote::posix_path(&index.root)
                .to_string_lossy()
                .into_owned(),
            query: query.into(),
            case_sensitive,
        },
        cancel,
    ) {
        Ok(response) => response,
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(results),
        Err(error) => return Err(error),
    };
    if cancel.load(Ordering::Relaxed) {
        return Ok(results);
    }
    let mut files = std::collections::BTreeMap::<String, Vec<LineMatch>>::new();
    for found in response.matches {
        files.entry(found.path).or_default().push(LineMatch {
            line: found.line,
            columns: found.columns[0]..found.columns[1],
            preview: found.preview,
            preview_range: found.preview_range[0]..found.preview_range[1],
        });
    }
    results.truncated = response.truncated;
    results.files = files
        .into_iter()
        .map(|(path, lines)| FileMatches {
            absolute: crate::join_path(&index.root, &path),
            path,
            lines,
        })
        .collect();
    Ok(results)
}

fn search_file(path: &std::path::Path, regex: &Regex) -> Vec<LineMatch> {
    let too_big = std::fs::metadata(path).map_or(true, |meta| meta.len() > MAX_FILE_BYTES);
    if too_big {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(line, content)| {
            let found = regex.find(content)?;
            Some(line_match(line, content, found.range()))
        })
        .take(MAX_RESULTS)
        .collect()
}

fn line_match(line: usize, content: &str, bytes: Range<usize>) -> LineMatch {
    let columns = content[..bytes.start].chars().count()..content[..bytes.end].chars().count();
    // Keep some context before the match, then trim to the preview length.
    let trimmed_start = content.len() - content.trim_start().len();
    let context_start = content[..bytes.start]
        .char_indices()
        .rev()
        .nth(40)
        .map_or(0, |(ix, _)| ix)
        .max(trimmed_start)
        .min(bytes.start);
    let preview: String = content[context_start..]
        .chars()
        .take(PREVIEW_CHARS)
        .collect();
    let match_start = bytes.start - context_start;
    let match_end = (bytes.end - context_start).min(preview.len());
    LineMatch {
        line,
        columns,
        preview_range: match_start.min(match_end)..match_end,
        preview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_match_columns_and_preview() {
        // `é` is two bytes, so the match spans bytes 16..23 but chars 15..22.
        let m = line_match(3, "    let café = find_me();", 16..23);
        assert_eq!(m.columns, 15..22);
        assert_eq!(m.preview, "let café = find_me();");
        assert_eq!(&m.preview[m.preview_range.clone()], "find_me");
    }

    #[test]
    fn searches_files_in_index() {
        let dir = std::env::temp_dir().join(format!("ion-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn alpha() {}\nfn Beta() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "nothing here").unwrap();
        let index = FileIndex::build(&dir);
        assert_eq!(index.files.len(), 2);
        let results = search(&index, "beta", false, &AtomicBool::new(false));
        assert_eq!(results.files.len(), 1);
        assert_eq!(results.files[0].path, "src/a.rs");
        assert_eq!(results.files[0].lines[0].line, 1);
        let strict = search(&index, "beta", true, &AtomicBool::new(false));
        assert!(strict.files.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
