//! Unified diffs of two texts in memory, shaped like parsed `git diff`
//! output so the same views can show them.

use crate::line_diff::{LineHunk, diff_lines};
use crate::patch::{DiffLine, FileDiff, Hunk, LineKind};

/// Diffs `old` to `new` for the file at `path`, with `context` unchanged
/// lines around each change. `old: None` is a new file.
pub fn diff_texts(path: &str, old: Option<&str>, new: &str, context: u32) -> FileDiff {
    let old_lines: Vec<&str> = old.map(|old| old.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();
    let changes = match old {
        Some(old) => diff_lines(old, new),
        None if new_lines.is_empty() => Vec::new(),
        None => vec![LineHunk {
            old: 0..0,
            new: 0..new_lines.len() as u32,
        }],
    };
    let mut hunks = Vec::new();
    let mut group: Vec<&LineHunk> = Vec::new();
    for change in &changes {
        // Changes whose context would touch or overlap share a hunk.
        let joins = group
            .last()
            .is_some_and(|last| change.old.start.saturating_sub(last.old.end) <= 2 * context);
        if !joins && !group.is_empty() {
            hunks.push(hunk(&group, &old_lines, &new_lines, context));
            group.clear();
        }
        group.push(change);
    }
    if !group.is_empty() {
        hunks.push(hunk(&group, &old_lines, &new_lines, context));
    }
    FileDiff {
        old_path: old.map(|_| path.to_owned()),
        new_path: Some(path.to_owned()),
        binary: false,
        hunks,
    }
}

/// The first line (zero-based) of `new` that differs from `old`, if any.
pub fn first_changed_line(old: &str, new: &str) -> Option<u32> {
    diff_lines(old, new).first().map(|change| change.new.start)
}

fn hunk(group: &[&LineHunk], old: &[&str], new: &[&str], context: u32) -> Hunk {
    let (first, last) = (group[0], group[group.len() - 1]);
    let lead = first.old.start.min(context);
    let old_start = first.old.start - lead;
    let new_start = first.new.start - lead;
    let old_end = (last.old.end + context).min(old.len() as u32);
    let mut lines = Vec::new();
    let (mut o, mut n) = (old_start, new_start);
    let push_context = |lines: &mut Vec<DiffLine>, o: &mut u32, n: &mut u32, until: u32| {
        while *o < until {
            lines.push(DiffLine {
                kind: LineKind::Context,
                text: old[*o as usize].to_owned(),
                old_line: Some(*o + 1),
                new_line: Some(*n + 1),
            });
            *o += 1;
            *n += 1;
        }
    };
    for change in group {
        push_context(&mut lines, &mut o, &mut n, change.old.start);
        for line in change.old.clone() {
            lines.push(DiffLine {
                kind: LineKind::Removed,
                text: old[line as usize].to_owned(),
                old_line: Some(line + 1),
                new_line: None,
            });
        }
        for line in change.new.clone() {
            lines.push(DiffLine {
                kind: LineKind::Added,
                text: new[line as usize].to_owned(),
                old_line: None,
                new_line: Some(line + 1),
            });
        }
        o = change.old.end;
        n = change.new.end;
    }
    push_context(&mut lines, &mut o, &mut n, old_end);
    let old_len = o - old_start;
    let new_len = n - new_start;
    // Git numbers an empty range by the line before it.
    let start = |start: u32, len: u32| if len == 0 { start } else { start + 1 };
    Hunk {
        header: format!(
            "@@ -{},{old_len} +{},{new_len} @@",
            start(old_start, old_len),
            start(new_start, new_len)
        ),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(diff: &FileDiff) -> Vec<Vec<(LineKind, &str)>> {
        diff.hunks
            .iter()
            .map(|hunk| {
                hunk.lines
                    .iter()
                    .map(|line| (line.kind, line.text.as_str()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn changes_get_context_and_separate_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nJ\n";
        let diff = diff_texts("x.rs", Some(old), new, 1);
        use LineKind::*;
        assert_eq!(
            kinds(&diff),
            [
                vec![(Context, "a"), (Removed, "b"), (Added, "B"), (Context, "c")],
                vec![(Context, "i"), (Removed, "j"), (Added, "J")],
            ]
        );
        assert_eq!(diff.hunks[0].header, "@@ -1,3 +1,3 @@");
        assert_eq!(diff.hunks[1].header, "@@ -9,2 +9,2 @@");
        assert_eq!(diff.hunks[1].lines[2].new_line, Some(10));
        assert_eq!(diff.stats(), (2, 2));
    }

    #[test]
    fn nearby_changes_share_a_hunk() {
        let diff = diff_texts("x", Some("a\nb\nc\nd\n"), "A\nb\nc\nD\n", 1);
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].header, "@@ -1,4 +1,4 @@");
    }

    #[test]
    fn new_and_unchanged_files() {
        let diff = diff_texts("new.rs", None, "one\ntwo\n", 3);
        assert_eq!(diff.old_path, None);
        assert_eq!(diff.hunks[0].header, "@@ -0,0 +1,2 @@");
        assert_eq!(diff.stats(), (2, 0));
        assert!(
            diff_texts("x", Some("same\n"), "same\r\n", 3)
                .hunks
                .is_empty()
        );
        assert!(diff_texts("x", None, "", 3).hunks.is_empty());
    }

    #[test]
    fn finds_the_first_changed_line() {
        assert_eq!(first_changed_line("a\nb\nc\n", "a\nb\nC\n"), Some(2));
        assert_eq!(first_changed_line("a\n", "a\n"), None);
    }
}
