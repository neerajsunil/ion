//! Undoing one change: putting a region's old lines back into a text.

use std::ops::Range;

use crate::patch::{Hunk, LineKind, parse_hunk_header};

/// A change that can be put back: the current text's lines `rows`
/// (zero-based, end-exclusive) were `old` before and are `new` now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revert {
    pub rows: Range<usize>,
    pub old: Vec<String>,
    pub new: Vec<String>,
}

impl Revert {
    /// The revert for a hunk of a diff whose new side is the current text.
    pub fn from_hunk(hunk: &Hunk) -> Option<Self> {
        let (_, (start, len)) = parse_hunk_header(&hunk.header)?;
        // An empty range is numbered by the line before it.
        let start = if len == 0 {
            start
        } else {
            start.checked_sub(1)?
        } as usize;
        let side = |skip: LineKind| -> Vec<String> {
            hunk.lines
                .iter()
                .filter(|line| line.kind != skip)
                .map(|line| line.text.clone())
                .collect()
        };
        let new = side(LineKind::Removed);
        (new.len() == len as usize).then(|| Self {
            rows: start..start + new.len(),
            old: side(LineKind::Added),
            new,
        })
    }

    /// `current` with this change undone, or `None` if the text no longer
    /// has the new lines where the change was (it changed again since).
    pub fn apply(&self, current: &str) -> Option<String> {
        let lines: Vec<&str> = current.lines().collect();
        let found = lines.get(self.rows.clone())?;
        let same = found.len() == self.new.len()
            && found.iter().zip(&self.new).all(|(a, b)| *a == b.as_str());
        if !same {
            return None;
        }
        let old: Vec<&str> = self.old.iter().map(String::as_str).collect();
        Some(replace_lines(current, self.rows.clone(), &old))
    }
}

/// `current` with its lines `rows` (zero-based, end-exclusive) replaced by
/// `lines`. Keeps the text's line ending and whether it ends with a newline.
pub fn replace_lines(current: &str, rows: Range<usize>, lines: &[&str]) -> String {
    let ending = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing = current.is_empty() || current.ends_with('\n');
    let mut all: Vec<&str> = current.lines().collect();
    let end = rows.end.min(all.len());
    let start = rows.start.min(end);
    all.splice(start..end, lines.iter().copied());
    let mut text = all.join(ending);
    if trailing && !all.is_empty() {
        text.push_str(ending);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_texts;

    fn revert_all(old: &str, new: &str) -> String {
        let diff = diff_texts("x", Some(old), new, 3);
        // Undo from the bottom so earlier rows stay valid.
        diff.hunks.iter().rev().fold(new.to_owned(), |text, hunk| {
            Revert::from_hunk(hunk).unwrap().apply(&text).unwrap()
        })
    }

    #[test]
    fn reverts_modified_added_and_deleted_lines() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        assert_eq!(revert_all(old, "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n"), old);
        assert_eq!(
            revert_all(old, "a\nb\nnew\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n"),
            old
        );
        assert_eq!(revert_all(old, "b\nc\nd\ne\nf\ng\nh\ni\nj\nk\n"), old);
    }

    #[test]
    fn keeps_line_endings_and_the_final_newline() {
        assert_eq!(revert_all("a\r\nb\r\n", "a\r\nB\r\n"), "a\r\nb\r\n");
        assert_eq!(replace_lines("a\nb", 1..2, &["c"]), "a\nc");
        assert_eq!(replace_lines("a", 1..1, &["b"]), "a\nb");
        assert_eq!(replace_lines("", 0..0, &["a"]), "a\n");
        assert_eq!(replace_lines("a\n", 0..1, &[]), "");
    }

    #[test]
    fn refuses_a_text_that_changed_again() {
        let diff = diff_texts("x", Some("a\nb\n"), "a\nB\n", 3);
        let revert = Revert::from_hunk(&diff.hunks[0]).unwrap();
        assert_eq!(revert.rows, 0..2);
        assert_eq!(revert.apply("a\nC\n"), None);
        assert_eq!(revert.apply("a\nB\n").as_deref(), Some("a\nb\n"));
    }

    #[test]
    fn reads_git_hunks() {
        let hunk = Hunk {
            header: "@@ -3,0 +4,1 @@".into(),
            lines: vec![crate::DiffLine {
                kind: LineKind::Added,
                text: "x".into(),
                old_line: None,
                new_line: Some(4),
            }],
        };
        let revert = Revert::from_hunk(&hunk).unwrap();
        assert_eq!(revert.rows, 3..4);
        assert!(revert.old.is_empty());
    }
}
