//! Turning git diffs into the text and row styles of a read-only diff tab.

use std::path::{Path, PathBuf};

use editor::{DiffRow, DiffRowKind};
use git::{CommitDetails, DiffLine, FileDiff, Hunk, LineKind, Revert};

/// What a diff tab shows. File paths are relative to the repository root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffTarget {
    /// Changes not staged yet (index to working tree).
    Unstaged(String),
    Staged(String),
    /// A new file git doesn't track yet.
    Untracked(String),
    Commit(String),
    /// The file an agent is editing, shown by follow mode.
    Follow,
    /// An edit an agent proposed, waiting for Accept or Reject.
    Proposal(u64),
}

impl DiffTarget {
    pub fn is_commit(&self) -> bool {
        matches!(self, Self::Commit(_))
    }

    /// Git diff tabs, which reload when the status changes and replace each
    /// other like previews.
    pub fn is_git(&self) -> bool {
        !matches!(self, Self::Follow | Self::Proposal(_))
    }
}

/// The finished contents of a diff tab.
pub(crate) struct DiffContent {
    pub title: String,
    pub text: String,
    pub rows: Vec<DiffRow>,
    /// For each row, the file and one-based line it corresponds to, for
    /// double-click to open.
    pub jumps: Vec<Option<(PathBuf, u32)>>,
    /// Picks the syntax language for single-file diffs.
    pub language_hint: Option<PathBuf>,
    /// Hunks that can be reverted in the working tree.
    pub reverts: Vec<HunkRevert>,
}

/// A diff hunk whose new side is the file on disk, so it can be undone there.
#[derive(Clone, Debug)]
pub(crate) struct HunkRevert {
    /// The hunk header's row in the diff view.
    pub row: usize,
    pub path: PathBuf,
    pub revert: Revert,
}

#[derive(Default)]
struct Builder {
    text: String,
    rows: Vec<DiffRow>,
    jumps: Vec<Option<(PathBuf, u32)>>,
    /// Whether the diffs' new side is the working tree.
    revertible: bool,
    reverts: Vec<HunkRevert>,
}

impl Builder {
    fn revertible() -> Self {
        Self {
            revertible: true,
            ..Self::default()
        }
    }

    fn push(&mut self, kind: DiffRowKind, text: &str, lines: (Option<u32>, Option<u32>)) {
        self.push_with_jump(kind, text, lines, None);
    }

    fn push_with_jump(
        &mut self,
        kind: DiffRowKind,
        text: &str,
        (old_line, new_line): (Option<u32>, Option<u32>),
        jump: Option<(PathBuf, u32)>,
    ) {
        if !self.rows.is_empty() {
            self.text.push('\n');
        }
        // Rows are lines, so a stray line break would misalign everything.
        self.text.push_str(&text.replace(['\r', '\n'], " "));
        self.rows.push(DiffRow {
            kind,
            old_line,
            new_line,
        });
        self.jumps.push(jump);
    }

    fn file(&mut self, file: &FileDiff, absolute: &dyn Fn(&str) -> PathBuf) {
        let (added, removed) = file.stats();
        let mut header = file.path().to_owned();
        if let Some(description) = file.describe() {
            header.push_str(&format!("  ({description})"));
        }
        if added + removed > 0 {
            header.push_str(&format!("  +{added} -{removed}"));
        }
        let path = file.new_path.as_deref().map(absolute);
        let first_line = file.hunks.first().and_then(first_new_line).unwrap_or(1);
        self.push_with_jump(
            DiffRowKind::FileHeader,
            &header,
            (None, None),
            path.clone().map(|path| (path, first_line)),
        );
        if file.binary {
            self.push(DiffRowKind::Meta, "Binary file not shown", (None, None));
        } else if file.hunks.is_empty() {
            self.push(DiffRowKind::Meta, "No content changes", (None, None));
        }
        for hunk in &file.hunks {
            self.hunk(hunk, path.as_deref());
        }
    }

    fn hunk(&mut self, hunk: &Hunk, path: Option<&Path>) {
        let jump = |line: u32| path.map(|path| (path.to_path_buf(), line.max(1)));
        // The next line in the new file, for removed lines to jump to.
        let mut next_new = first_new_line(hunk).unwrap_or(1);
        if self.revertible
            && let Some(path) = path
            && let Some(revert) = Revert::from_hunk(hunk)
        {
            self.reverts.push(HunkRevert {
                row: self.rows.len(),
                path: path.to_path_buf(),
                revert,
            });
        }
        self.push_with_jump(
            DiffRowKind::HunkHeader,
            &hunk.header,
            (None, None),
            jump(next_new),
        );
        for line in &hunk.lines {
            let DiffLine {
                kind,
                text,
                old_line,
                new_line,
            } = line;
            let row_kind = match kind {
                LineKind::Context => DiffRowKind::Context,
                LineKind::Added => DiffRowKind::Added,
                LineKind::Removed => DiffRowKind::Removed,
            };
            if let Some(new_line) = new_line {
                next_new = new_line + 1;
            }
            let target = new_line.unwrap_or(next_new);
            self.push_with_jump(row_kind, text, (*old_line, *new_line), jump(target));
        }
    }

    fn finish(mut self, title: String, language_hint: Option<PathBuf>) -> DiffContent {
        if self.rows.is_empty() {
            self.push(DiffRowKind::Meta, "No changes", (None, None));
        }
        DiffContent {
            title,
            text: self.text,
            rows: self.rows,
            jumps: self.jumps,
            language_hint,
            reverts: self.reverts,
        }
    }
}

fn first_new_line(hunk: &Hunk) -> Option<u32> {
    hunk.lines.iter().find_map(|line| line.new_line)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A tab showing one file's staged or unstaged changes.
pub(crate) fn file_diff(
    target: &DiffTarget,
    files: &[FileDiff],
    absolute: &dyn Fn(&str) -> PathBuf,
) -> DiffContent {
    let (path, label) = match target {
        DiffTarget::Unstaged(path) => (path, "Changes"),
        DiffTarget::Staged(path) => (path, "Staged"),
        DiffTarget::Untracked(path) => (path, "New File"),
        DiffTarget::Commit(oid) => (oid, "Commit"),
        DiffTarget::Follow | DiffTarget::Proposal(_) => {
            return follow(String::new(), "", files, absolute);
        }
    };
    let mut builder = match target {
        DiffTarget::Unstaged(_) => Builder::revertible(),
        _ => Builder::default(),
    };
    for file in files {
        builder.file(file, absolute);
    }
    builder.finish(
        format!("{} · {label}", file_name(path)),
        Some(PathBuf::from(file_name(path))),
    )
}

/// The follow tab: one file's changes, titled `agent ▸ file`.
pub(crate) fn follow(
    agent: String,
    path: &str,
    files: &[FileDiff],
    absolute: &dyn Fn(&str) -> PathBuf,
) -> DiffContent {
    let mut builder = Builder::revertible();
    for file in files {
        builder.file(file, absolute);
    }
    builder.finish(
        format!("{agent} ▸ {}", file_name(path)),
        Some(PathBuf::from(file_name(path))),
    )
}

/// A proposed edit's tab: the changes it would make to one file.
pub(crate) fn proposal(
    path: &str,
    files: &[FileDiff],
    absolute: &dyn Fn(&str) -> PathBuf,
) -> DiffContent {
    let mut builder = Builder::default();
    for file in files {
        builder.file(file, absolute);
    }
    builder.finish(
        format!("{} (proposed)", file_name(path)),
        Some(PathBuf::from(file_name(path))),
    )
}

/// The row to put the cursor on for an edit starting at zero-based line
/// `line` of the new file: the first change at or after it, else the last
/// change. With no line, the first change.
pub(crate) fn cursor_row(rows: &[DiffRow], line: Option<u32>) -> usize {
    let target = line.map_or(0, |line| line + 1);
    let mut next_new = 1;
    let mut last_change = None;
    for (ix, row) in rows.iter().enumerate() {
        // A removed line sits where the next new line would be.
        let at = row.new_line.unwrap_or(next_new);
        if let Some(new_line) = row.new_line {
            next_new = new_line + 1;
        }
        if matches!(row.kind, DiffRowKind::Added | DiffRowKind::Removed) {
            if at >= target {
                return ix;
            }
            last_change = Some(ix);
        }
    }
    last_change.unwrap_or(0)
}

/// An untracked file shown as entirely added.
pub(crate) fn untracked_file(path: &str, text: &str) -> FileDiff {
    let lines: Vec<DiffLine> = text
        .lines()
        .enumerate()
        .map(|(ix, line)| DiffLine {
            kind: LineKind::Added,
            text: line.to_owned(),
            old_line: None,
            new_line: Some(ix as u32 + 1),
        })
        .collect();
    FileDiff {
        old_path: None,
        new_path: Some(path.to_owned()),
        binary: false,
        hunks: vec![Hunk {
            header: format!("@@ -0,0 +1,{} @@", lines.len()),
            lines,
        }],
    }
}

/// A tab showing a commit: its message, then every file it changed.
pub(crate) fn commit(details: &CommitDetails, absolute: &dyn Fn(&str) -> PathBuf) -> DiffContent {
    let mut builder = Builder::default();
    let meta =
        |builder: &mut Builder, text: &str| builder.push(DiffRowKind::Meta, text, (None, None));
    meta(&mut builder, &format!("commit {}", details.oid));
    meta(
        &mut builder,
        &format!("Author: {} <{}>", details.author, details.email),
    );
    meta(&mut builder, &format!("Date:   {}", details.date));
    meta(&mut builder, "");
    for line in details.message.lines() {
        meta(&mut builder, &format!("    {line}"));
    }
    meta(&mut builder, "");
    let (added, removed) = details.files.iter().fold((0, 0), |(a, r), file| {
        let (fa, fr) = file.stats();
        (a + fa, r + fr)
    });
    let count = details.files.len();
    meta(
        &mut builder,
        &format!(
            "{count} file{} changed, +{added} -{removed}",
            if count == 1 { "" } else { "s" }
        ),
    );
    for file in &details.files {
        meta(&mut builder, "");
        builder.file(file, absolute);
    }
    let subject = details.message.lines().next().unwrap_or("");
    let short = &details.oid[..details.oid.len().min(7)];
    let mut title = format!("{short} {subject}");
    if title.chars().count() > 40 {
        title = title.chars().take(39).collect::<String>() + "…";
    }
    // One language for the whole view only makes sense for one file.
    let language_hint = match details.files.as_slice() {
        [file] => Some(PathBuf::from(file_name(file.path()))),
        _ => None,
    };
    builder.finish(title, language_hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_cursor_lands_on_the_latest_edit() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nJ\n";
        let diff = git::diff_texts("x.rs", Some(old), new, 1);
        let content = follow("Codex".into(), "src/x.rs", &[diff], &|path| {
            PathBuf::from(path)
        });
        assert_eq!(content.title, "Codex ▸ x.rs");
        let line = |ix: usize| content.text.lines().nth(ix).unwrap().to_owned();
        // An edit on line 10 (zero-based 9): its removed line comes first.
        assert_eq!(line(cursor_row(&content.rows, Some(9))), "j");
        assert_eq!(line(cursor_row(&content.rows, Some(1))), "b");
        assert_eq!(line(cursor_row(&content.rows, None)), "b");
        // Past every change: the last one.
        assert_eq!(line(cursor_row(&content.rows, Some(50))), "J");
    }

    #[test]
    fn working_tree_hunks_can_be_reverted() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let diff = git::diff_texts("x.rs", Some(old), new, 3);
        let absolute = |path: &str| PathBuf::from(path);
        let target = DiffTarget::Unstaged("x.rs".into());
        let content = file_diff(&target, std::slice::from_ref(&diff), &absolute);
        let [revert] = content.reverts.as_slice() else {
            panic!("one revertible hunk");
        };
        assert_eq!(content.rows[revert.row].kind, DiffRowKind::HunkHeader);
        assert_eq!(revert.revert.apply(new).as_deref(), Some(old));
        // Staged changes aren't in the working tree.
        let staged = file_diff(&DiffTarget::Staged("x.rs".into()), &[diff], &absolute);
        assert!(staged.reverts.is_empty());
    }

    #[test]
    fn untracked_files_jump_to_their_lines() {
        let file = untracked_file("src/new.rs", "fn a() {}\nfn b() {}\n");
        let target = DiffTarget::Untracked("src/new.rs".into());
        let content = file_diff(&target, &[file], &|path| PathBuf::from(path));
        // File header, hunk header, two added lines.
        assert_eq!(content.rows.len(), 4);
        assert_eq!(content.text.lines().count(), 4);
        assert_eq!(content.rows[3].new_line, Some(2));
        assert_eq!(content.jumps[3], Some((PathBuf::from("src/new.rs"), 2)));
        assert_eq!(content.title, "new.rs · New File");
    }
}
