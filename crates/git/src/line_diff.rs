//! In-process line diff, for change markers in the editor gutter.

use std::ops::Range;

use imara_diff::intern::InternedInput;
use imara_diff::{Algorithm, diff};

/// A changed region: lines `old` of the base were replaced by lines `new` of
/// the current text (zero-based, end-exclusive). One side may be empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineHunk {
    pub old: Range<u32>,
    pub new: Range<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChange {
    Added,
    Modified,
    /// Lines were removed just above this row.
    DeletedAbove,
}

impl LineHunk {
    pub fn change(&self) -> LineChange {
        if self.old.is_empty() {
            LineChange::Added
        } else if self.new.is_empty() {
            LineChange::DeletedAbove
        } else {
            LineChange::Modified
        }
    }
}

/// Diffs two texts by line. Line endings are ignored (`\r\n` equals `\n`).
pub fn diff_lines(base: &str, current: &str) -> Vec<LineHunk> {
    let base = normalize(base);
    let current = normalize(current);
    let input = InternedInput::new(base.as_str(), current.as_str());
    let mut hunks = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        |old: Range<u32>, new: Range<u32>| hunks.push(LineHunk { old, new }),
    );
    hunks
}

/// Strips `\r` and ends the text with a newline, so a missing final newline
/// doesn't mark the last line as changed.
fn normalize(text: &str) -> String {
    let mut text = text.replace("\r\n", "\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Where a row of the current text was in the base, if it's unchanged.
pub fn base_row(hunks: &[LineHunk], row: u32) -> Option<u32> {
    let mut delta: i64 = 0;
    for hunk in hunks {
        if row < hunk.new.start {
            break;
        }
        if row < hunk.new.end {
            return None;
        }
        delta = hunk.old.end as i64 - hunk.new.end as i64;
    }
    u32::try_from(row as i64 + delta).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_added_modified_and_deleted_lines() {
        let base = "a\nb\nc\nd\ne\n";
        let current = "a\nB\nc\nnew\nd\n";
        let hunks = diff_lines(base, current);
        let changes: Vec<_> = hunks.iter().map(LineHunk::change).collect();
        assert_eq!(
            changes,
            [
                LineChange::Modified,
                LineChange::Added,
                LineChange::DeletedAbove
            ]
        );
        assert_eq!(hunks[0].new, 1..2);
        assert_eq!(hunks[1].new, 3..4);
        assert_eq!(hunks[2].old, 4..5);
    }

    #[test]
    fn ignores_line_endings_and_final_newline() {
        assert!(diff_lines("a\r\nb\r\n", "a\nb").is_empty());
    }

    #[test]
    fn maps_rows_back_to_the_base() {
        let hunks = diff_lines("a\nb\nc\nd\n", "a\nx\ny\nb\nd\n");
        // "x", "y" were added; "c" was removed.
        assert_eq!(base_row(&hunks, 0), Some(0));
        assert_eq!(base_row(&hunks, 1), None);
        assert_eq!(base_row(&hunks, 3), Some(1));
        assert_eq!(base_row(&hunks, 4), Some(3));
    }
}
