//! Project-wide text search.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use regex::{Regex, RegexBuilder};

use crate::FileIndex;

/// Search stops collecting after this many matching lines.
pub const MAX_RESULTS: usize = 5_000;
/// Files larger than this are skipped.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Long lines are cut down to this many chars in the results.
const PREVIEW_CHARS: usize = 200;
/// After the first match, results are handed over at most this often.
const BATCH_INTERVAL: Duration = Duration::from_millis(50);
/// More threads than this read files no faster: the OS serializes much of
/// opening and reading them, so extra threads only add contention.
const MAX_THREADS: usize = 8;

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
    /// Where the file came in the search order; results show in this order.
    pub order: usize,
}

pub struct SearchResults {
    pub files: Vec<FileMatches>,
    /// More matches existed than were collected.
    pub truncated: bool,
}

/// What a search looks at before the rest of the project.
#[derive(Default)]
pub struct Sources {
    /// Absolute paths to search first, most relevant first (open, recent
    /// and changed files).
    pub first: Vec<PathBuf>,
    /// Unsaved contents of open files, searched instead of what's on disk.
    pub buffers: Vec<(PathBuf, String)>,
}

pub fn build_regex(query: &str, case_sensitive: bool) -> Option<Regex> {
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .build()
        .ok()
}

/// Searches every indexed file for `query` and returns all results at once,
/// in search order.
pub fn search(
    index: &FileIndex,
    query: &str,
    case_sensitive: bool,
    cancel: &AtomicBool,
) -> SearchResults {
    let found = Mutex::new(Vec::new());
    let truncated = search_streaming(
        index,
        query,
        case_sensitive,
        &Sources::default(),
        cancel,
        &|batch| found.lock().expect("search lock poisoned").extend(batch),
    );
    let mut files = found.into_inner().expect("search lock poisoned");
    files.sort_by_key(|file| file.order);
    SearchResults { files, truncated }
}

/// Searches every indexed file for `query` on several threads, handing
/// results to `on_batch` as they're found: the first match right away, then
/// at most every [`BATCH_INTERVAL`]. Files in `sources.first` go first, then
/// the index's [`search_order`](FileIndex::search_order). Stops early when
/// `cancel` is set (a newer search started). Returns whether results were
/// cut off at [`MAX_RESULTS`].
pub fn search_streaming(
    index: &FileIndex,
    query: &str,
    case_sensitive: bool,
    sources: &Sources,
    cancel: &AtomicBool,
    on_batch: &(dyn Fn(Vec<FileMatches>) + Sync),
) -> bool {
    let Some(regex) = build_regex(query, case_sensitive).filter(|_| !query.is_empty()) else {
        return false;
    };
    let mut first: Vec<u32> = Vec::new();
    let preferred = sources
        .first
        .iter()
        .chain(sources.buffers.iter().map(|(path, _)| path));
    for ix in preferred.filter_map(|path| index.position(path)) {
        if !first.contains(&(ix as u32)) {
            first.push(ix as u32);
        }
    }
    let mut buffers: Vec<(usize, &str)> = sources
        .buffers
        .iter()
        .filter_map(|(path, text)| Some((index.position(path)?, text.as_str())))
        .collect();
    buffers.sort_unstable_by_key(|(ix, _)| *ix);
    let skip: HashSet<u32> = first.iter().copied().collect();
    let order: Vec<u32> = first
        .iter()
        .copied()
        .chain(
            index
                .search_order
                .iter()
                .copied()
                .filter(|ix| !skip.contains(ix)),
        )
        .collect();

    let next = AtomicUsize::new(0);
    let total = AtomicUsize::new(0);
    let pending = Mutex::new(Vec::new());
    let has_pending = AtomicBool::new(false);
    let sent_any = AtomicBool::new(false);
    let start = Instant::now();
    // Microseconds from `start` to the last hand-over.
    let last_batch = AtomicU64::new(0);
    let flush = || {
        let batch = {
            let mut pending = pending.lock().expect("search lock poisoned");
            has_pending.store(false, Ordering::Relaxed);
            std::mem::take(&mut *pending)
        };
        sent_any.store(true, Ordering::Relaxed);
        last_batch.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        if !batch.is_empty() {
            on_batch(batch);
        }
    };
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(MAX_THREADS);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if cancel.load(Ordering::Relaxed)
                        || total.load(Ordering::Relaxed) >= MAX_RESULTS
                    {
                        return;
                    }
                    let position = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&ix) = order.get(position) else {
                        return;
                    };
                    let file = index.file(ix as usize);
                    let absolute = index.absolute(&file);
                    let lines = match buffers.binary_search_by_key(&(ix as usize), |(ix, _)| *ix) {
                        Ok(buffer) => search_text(buffers[buffer].1, &regex),
                        Err(_) => search_file(&absolute, &regex),
                    };
                    if !lines.is_empty() {
                        total.fetch_add(lines.len(), Ordering::Relaxed);
                        let mut pending = pending.lock().expect("search lock poisoned");
                        pending.push(FileMatches {
                            path: file.path.to_string(),
                            absolute,
                            lines,
                            order: position,
                        });
                        has_pending.store(true, Ordering::Relaxed);
                    }
                    if has_pending.load(Ordering::Relaxed) {
                        let since = (start.elapsed().as_micros() as u64)
                            .saturating_sub(last_batch.load(Ordering::Relaxed));
                        if !sent_any.load(Ordering::Relaxed)
                            || since >= BATCH_INTERVAL.as_micros() as u64
                        {
                            flush();
                        }
                    }
                }
            });
        }
    });
    flush();
    total.load(Ordering::Relaxed) >= MAX_RESULTS
}

/// Like [`search`], but runs on the server for remote projects.
#[cfg(feature = "remote")]
pub fn search_with_filesystem(
    filesystem: &crate::FileSystem,
    index: &FileIndex,
    query: &str,
    case_sensitive: bool,
    cancel: &AtomicBool,
) -> std::io::Result<SearchResults> {
    let found = Mutex::new(Vec::new());
    let truncated = stream_with_filesystem(
        filesystem,
        index,
        query,
        case_sensitive,
        &Sources::default(),
        cancel,
        &|batch| found.lock().expect("search lock poisoned").extend(batch),
    )?;
    let mut files = found.into_inner().expect("search lock poisoned");
    files.sort_by_key(|file| file.order);
    Ok(SearchResults { files, truncated })
}

/// Like [`search_streaming`], but runs on the server for remote projects.
/// The server searches in its own order and replies once; unsaved buffers
/// are then searched here, replacing the server's matches for those files.
#[cfg(feature = "remote")]
pub fn stream_with_filesystem(
    filesystem: &crate::FileSystem,
    index: &FileIndex,
    query: &str,
    case_sensitive: bool,
    sources: &Sources,
    cancel: &AtomicBool,
    on_batch: &(dyn Fn(Vec<FileMatches>) + Sync),
) -> std::io::Result<bool> {
    let Some(connection) = filesystem.remote() else {
        return Ok(search_streaming(
            index,
            query,
            case_sensitive,
            sources,
            cancel,
            on_batch,
        ));
    };
    if query.is_empty() || cancel.load(Ordering::Relaxed) {
        return Ok(false);
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
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(false),
        Err(error) => return Err(error),
    };
    if cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    let Some(regex) = build_regex(query, case_sensitive) else {
        return Ok(false);
    };
    // Unsaved buffers come first, then the server's files in its order (each
    // file's matches arrive together).
    let mut files: Vec<FileMatches> = sources
        .buffers
        .iter()
        .enumerate()
        .filter_map(|(order, (absolute, text))| {
            let relative = absolute.strip_prefix(&index.root).ok()?;
            let lines = search_text(text, &regex);
            (!lines.is_empty()).then(|| FileMatches {
                path: relative.to_string_lossy().replace('\\', "/"),
                absolute: absolute.clone(),
                lines,
                order,
            })
        })
        .collect();
    let edited: HashSet<String> = sources
        .buffers
        .iter()
        .filter_map(|(absolute, _)| {
            let relative = absolute.strip_prefix(&index.root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    for found in response.matches {
        if edited.contains(&found.path) {
            continue;
        }
        let line = LineMatch {
            line: found.line,
            columns: found.columns[0]..found.columns[1],
            preview: found.preview,
            preview_range: found.preview_range[0]..found.preview_range[1],
        };
        match files.last_mut() {
            Some(file) if file.path == found.path => file.lines.push(line),
            _ => files.push(FileMatches {
                absolute: crate::join_path(&index.root, &found.path),
                path: found.path,
                lines: vec![line],
                order: sources.buffers.len() + files.len(),
            }),
        }
    }
    on_batch(files);
    Ok(response.truncated)
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
    search_text(text, regex)
}

/// The first match on each line of `text`. Searches the whole text at once
/// and only looks at lines around matches, so a file without one costs a
/// single pass of the regex engine.
fn search_text(text: &str, regex: &Regex) -> Vec<LineMatch> {
    let mut found = Vec::new();
    // Line number of the line starting at `counted`.
    let (mut line, mut counted) = (0, 0);
    let mut from = 0;
    while found.len() < MAX_RESULTS
        && let Some(hit) = regex.find_at(text, from)
    {
        let start = text[..hit.start()].rfind('\n').map_or(0, |ix| ix + 1);
        let end = text[hit.start()..]
            .find('\n')
            .map_or(text.len(), |ix| hit.start() + ix);
        line += text[counted..start].bytes().filter(|&b| b == b'\n').count();
        counted = start;
        // As `str::lines` gives it: without the line ending.
        let content = &text[start..end];
        let content = if end < text.len() {
            content.strip_suffix('\r').unwrap_or(content)
        } else {
            content
        };
        // The hit could run past the line (a query ending in `\r`); match
        // the line alone, as a per-line search would.
        if let Some(on_line) = regex.find(content) {
            found.push(line_match(line, content, on_line.range()));
        }
        from = end + 1;
        if from > text.len() {
            break;
        }
    }
    found
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
    fn whole_text_search_matches_line_by_line() {
        let regex = build_regex("ab", false).unwrap();
        let text = "ab ab\r\nnone\n\nxAB\r\nab";
        let found = search_text(text, &regex);
        let lines: Vec<(usize, &str, Range<usize>)> = found
            .iter()
            .map(|m| (m.line, &*m.preview, m.columns.clone()))
            .collect();
        assert_eq!(
            lines,
            [(0, "ab ab", 0..2), (3, "xAB", 1..3), (4, "ab", 0..2)]
        );
        assert!(search_text("nothing\n", &regex).is_empty());
        assert!(search_text("", &regex).is_empty());
    }

    #[test]
    fn searches_files_in_index() {
        let dir = std::env::temp_dir().join(format!("ion-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn alpha() {}\nfn Beta() {}\n").unwrap();
        std::fs::write(dir.join("b.txt"), "nothing here").unwrap();
        let index = FileIndex::build(&dir);
        assert_eq!(index.len(), 2);
        let results = search(&index, "beta", false, &AtomicBool::new(false));
        assert_eq!(results.files.len(), 1);
        assert_eq!(results.files[0].path, "src/a.rs");
        assert_eq!(results.files[0].lines[0].line, 1);
        let strict = search(&index, "beta", true, &AtomicBool::new(false));
        assert!(strict.files.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn searches_preferred_files_then_source_then_text_and_unsaved_buffers() {
        let dir = std::env::temp_dir().join(format!("ion-search-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.md", "b.rs", "c.rs", "d.txt", "e.dat", "f.rs"] {
            std::fs::write(dir.join(name), "needle\n").unwrap();
        }
        let index = FileIndex::build(&dir);
        let sources = Sources {
            first: vec![dir.join("d.txt")],
            // Edited away in the editor, not yet saved.
            buffers: vec![(dir.join("f.rs"), "no match here\n".into())],
        };
        let batches = Mutex::new(Vec::new());
        let truncated = search_streaming(
            &index,
            "needle",
            false,
            &sources,
            &AtomicBool::new(false),
            &|batch| batches.lock().unwrap().push(batch),
        );
        assert!(!truncated);
        let mut files: Vec<FileMatches> = batches
            .into_inner()
            .unwrap()
            .into_iter()
            .flatten()
            .collect();
        files.sort_by_key(|file| file.order);
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["d.txt", "b.rs", "c.rs", "a.md", "e.dat"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
