//! Code folding by indentation: a line followed by more-indented lines can
//! hide them. Works for any language, braces or not; a closing `}` at the
//! header's indent stays visible. Folds are kept as header rows, shifted as
//! lines are added or removed above them.

use super::Buffer;

const TAB_WIDTH: usize = 4;

impl Buffer {
    /// Leading whitespace width, or `None` for a blank line.
    fn indent_of(&self, row: usize) -> Option<usize> {
        let line = self.rope.line(row);
        let mut width = 0;
        for ch in line.chars() {
            match ch {
                ' ' => width += 1,
                '\t' => width += TAB_WIDTH - width % TAB_WIDTH,
                '\n' | '\r' => return None,
                _ => return Some(width),
            }
        }
        None
    }

    /// The rows a fold at `row` hides, `first..=last`: the more-indented
    /// lines after it, without trailing blank lines.
    pub fn fold_range(&self, row: usize) -> Option<(usize, usize)> {
        let base = self.indent_of(row)?;
        let line_count = self.line_count();
        let mut last = None;
        for next in row + 1..line_count {
            match self.indent_of(next) {
                Some(indent) if indent > base => last = Some(next),
                Some(_) => break,
                None => {}
            }
        }
        last.map(|last| (row + 1, last))
    }

    /// Whether `row` starts a block that can fold (cheaper than
    /// [`Buffer::fold_range`]: looks at the next non-blank line only).
    pub fn is_foldable(&self, row: usize) -> bool {
        let Some(base) = self.indent_of(row) else {
            return false;
        };
        (row + 1..self.line_count())
            .find_map(|next| self.indent_of(next))
            .is_some_and(|indent| indent > base)
    }

    /// Folded header rows, sorted.
    pub fn folds(&self) -> &[usize] {
        &self.folds
    }

    pub fn is_folded(&self, row: usize) -> bool {
        self.folds.binary_search(&row).is_ok()
    }

    /// Changes whenever folds do, for caches.
    pub fn folds_version(&self) -> u64 {
        self.folds_version
    }

    /// Hidden row ranges (`first..=last`), sorted, nested folds merged.
    pub fn hidden_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for &row in &self.folds {
            if ranges.last().is_some_and(|&(_, last)| row <= last) {
                continue;
            }
            if let Some(range) = self.fold_range(row) {
                ranges.push(range);
            }
        }
        ranges
    }

    /// The block `row` is in: its own if it starts one, else the nearest
    /// enclosing one above.
    fn fold_header_for(&self, row: usize) -> Option<usize> {
        if self.is_foldable(row) {
            return Some(row);
        }
        (0..row)
            .rev()
            .find(|&header| self.fold_range(header).is_some_and(|(_, last)| last >= row))
    }

    /// Folds the block at (or around) `row`. Returns whether anything folded.
    pub fn fold_at(&mut self, row: usize) -> bool {
        let Some(header) = self.fold_header_for(row) else {
            return false;
        };
        match self.folds.binary_search(&header) {
            Ok(_) => false,
            Err(ix) => {
                self.folds.insert(ix, header);
                self.folds_changed();
                true
            }
        }
    }

    /// Unfolds the fold at `row` or hiding it. Returns whether any opened.
    pub fn unfold_at(&mut self, row: usize) -> bool {
        let before = self.folds.len();
        let hiding: Vec<usize> = self
            .folds
            .iter()
            .copied()
            .filter(|&header| {
                header == row
                    || self
                        .fold_range(header)
                        .is_some_and(|(first, last)| (first..=last).contains(&row))
            })
            .collect();
        self.folds.retain(|header| !hiding.contains(header));
        let changed = self.folds.len() != before;
        if changed {
            self.folds_changed();
        }
        changed
    }

    pub fn toggle_fold(&mut self, row: usize) {
        if !self.unfold_at(row) {
            self.fold_at(row);
        }
    }

    /// Folds every block (nested ones too, so unfolding one level at a time
    /// works).
    pub fn fold_all(&mut self) {
        self.folds = (0..self.line_count())
            .filter(|&row| self.is_foldable(row))
            .collect();
        self.folds_changed();
    }

    pub fn unfold_all(&mut self) {
        if !self.folds.is_empty() {
            self.folds.clear();
            self.folds_changed();
        }
    }

    /// Opens folds that hide a cursor, so it's never invisible.
    pub fn reveal_cursors(&mut self) {
        if self.folds.is_empty() {
            return;
        }
        let hidden = self.hidden_ranges();
        let rows: Vec<usize> = self
            .selections()
            .iter()
            .map(|selection| self.row_col(selection.head).0)
            .filter(|row| {
                hidden
                    .iter()
                    .any(|&(first, last)| (first..=last).contains(row))
            })
            .collect();
        for row in rows {
            self.unfold_at(row);
        }
    }

    fn folds_changed(&mut self) {
        self.folds_version += 1;
    }

    /// Before an edit of `range`: each fold's row and whether the edit moves
    /// it (it's after the edit), or `None` if its line is deleted.
    pub(super) fn folds_after_edit(
        &self,
        range: &std::ops::Range<usize>,
    ) -> Vec<Option<(usize, bool)>> {
        let deletes_break = || self.rope.slice(range.clone()).chars().any(|ch| ch == '\n');
        self.folds
            .iter()
            .map(|&row| {
                let start = self.rope.line_to_char(row);
                if start < range.start {
                    Some((row, false))
                } else if start == range.start {
                    if range.is_empty() {
                        // Text inserted before the line pushes it down.
                        Some((row, true))
                    } else {
                        (!deletes_break()).then_some((row, false))
                    }
                } else if start < range.end {
                    None
                } else {
                    Some((row, true))
                }
            })
            .collect()
    }

    /// Applies [`Buffer::folds_after_edit`] once the edit changed the line
    /// count by `delta`.
    pub(super) fn shift_folds(&mut self, folds: Vec<Option<(usize, bool)>>, delta: isize) {
        let before = std::mem::take(&mut self.folds);
        self.folds = folds
            .into_iter()
            .flatten()
            .map(|(row, moves)| {
                if moves {
                    (row as isize + delta).max(0) as usize
                } else {
                    row
                }
            })
            .collect();
        self.folds.dedup();
        if self.folds != before {
            self.folds_changed();
        }
    }

    /// After undo or redo replaced the text: drop folds past the end.
    pub(super) fn clamp_folds(&mut self) {
        let line_count = self.line_count();
        let before = self.folds.len();
        self.folds.retain(|&row| row < line_count);
        if self.folds.len() != before {
            self.folds_changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "fn a() {\n    if x {\n        y();\n    }\n\n    z();\n}\nfn b() {}\n";

    #[test]
    fn finds_blocks_by_indentation() {
        let b = Buffer::new(CODE);
        assert_eq!(b.fold_range(0), Some((1, 5)));
        assert_eq!(b.fold_range(1), Some((2, 2)));
        assert_eq!(b.fold_range(2), None);
        assert!(b.is_foldable(0));
        assert!(!b.is_foldable(6));
    }

    #[test]
    fn folds_shift_with_edits() {
        let mut b = Buffer::new(CODE);
        assert!(b.fold_at(2));
        assert_eq!(b.folds(), [1]);
        assert_eq!(b.hidden_ranges(), [(2, 2)]);
        b.fold_at(0);
        assert_eq!(b.hidden_ranges(), [(1, 5)]);
        // A line added above moves both folds down.
        b.set_selection(crate::Selection::cursor(0));
        b.insert("// doc\n");
        assert_eq!(b.folds(), [1, 2]);
        // Deleting a header's line drops its fold.
        let start = b.line_start(2);
        let end = b.line_start(3);
        b.replace(Some(start..end), "");
        assert_eq!(b.folds(), [1]);
        b.unfold_at(3);
        assert!(b.folds().is_empty());
    }

    #[test]
    fn cursors_inside_a_fold_open_it() {
        let mut b = Buffer::new(CODE);
        b.fold_at(0);
        b.set_selection(crate::Selection::cursor(b.line_start(2)));
        b.reveal_cursors();
        assert!(b.folds().is_empty());
    }
}
