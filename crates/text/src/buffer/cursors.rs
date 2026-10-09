//! More than one cursor: commands run at each selection in turn, from the
//! end of the text back, as one undo step.

use super::{Buffer, Selection};

impl Buffer {
    /// Whether there's more than one cursor.
    pub fn has_extra_cursors(&self) -> bool {
        !self.extra.is_empty()
    }

    /// Every selection, the primary one included, in text order.
    pub fn selections(&self) -> Vec<Selection> {
        let mut all: Vec<Selection> = std::iter::once(self.selection)
            .chain(self.extra.iter().copied())
            .collect();
        all.sort_by_key(|selection| selection.range().start);
        all
    }

    /// Back to one cursor. Returns whether there were others.
    pub fn clear_extra_cursors(&mut self) -> bool {
        let had = !self.extra.is_empty();
        self.extra.clear();
        had
    }

    /// Runs `op` at every selection (with its index in text order), as one
    /// undo step. With a single cursor it just runs once.
    pub fn edit_each(&mut self, mut op: impl FnMut(&mut Self, usize)) {
        if self.extra.is_empty() {
            op(self, 0);
            return;
        }
        let extra = std::mem::take(&mut self.extra);
        let primary = self.selection;
        let mut all: Vec<(Selection, bool)> = std::iter::once((primary, true))
            .chain(extra.iter().map(|selection| (*selection, false)))
            .collect();
        all.sort_by_key(|(selection, _)| selection.range().start);
        self.batch = Some((primary, extra, false));
        // From the end back, so edits don't move the selections still to
        // come; the ones done are shifted by each edit's length change.
        let mut done: Vec<(Selection, bool)> = Vec::with_capacity(all.len());
        for (ix, (selection, is_primary)) in all.into_iter().enumerate().rev() {
            self.selection = selection;
            self.preferred_col = None;
            let before = self.len() as isize;
            op(self, ix);
            let delta = self.len() as isize - before;
            if delta != 0 {
                let shift = |offset: usize| (offset as isize + delta).max(0) as usize;
                for (selection, _) in &mut done {
                    selection.anchor = shift(selection.anchor);
                    selection.head = shift(selection.head);
                }
            }
            done.push((self.selection, is_primary));
        }
        self.batch = None;
        self.preferred_col = None;
        self.set_cursors(done);
    }

    /// [`Buffer::edit_each`] without the index.
    pub fn for_each_selection(&mut self, mut op: impl FnMut(&mut Self)) {
        self.edit_each(|buffer, _| op(buffer));
    }

    /// Sets the selections, merging overlapping ones. The one flagged
    /// primary (or the last) becomes the primary.
    fn set_cursors(&mut self, mut all: Vec<(Selection, bool)>) {
        all.sort_by_key(|(selection, _)| (selection.range().start, selection.range().end));
        let mut merged: Vec<(Selection, bool)> = Vec::with_capacity(all.len());
        for (selection, primary) in all {
            if let Some((last, last_primary)) = merged.last_mut() {
                let (a, b) = (last.range(), selection.range());
                if b.start < a.end || a == b {
                    if b.end > a.end {
                        *last = Selection {
                            anchor: a.start,
                            head: b.end,
                        };
                    }
                    *last_primary |= primary;
                    continue;
                }
            }
            merged.push((selection, primary));
        }
        let primary = merged
            .iter()
            .position(|(_, primary)| *primary)
            .unwrap_or(merged.len() - 1);
        self.selection = merged.remove(primary).0;
        self.extra = merged.into_iter().map(|(selection, _)| selection).collect();
    }

    /// Alt+click: adds a cursor at `offset`, or removes the one there.
    pub fn toggle_cursor(&mut self, offset: usize) {
        let offset = offset.min(self.len());
        if let Some(ix) = self
            .extra
            .iter()
            .position(|s| s.range().contains(&offset) || s.head == offset)
        {
            self.extra.remove(ix);
            return;
        }
        if self.selection.head == offset && !self.extra.is_empty() {
            self.selection = self.extra.pop().unwrap_or(self.selection);
            return;
        }
        let mut all: Vec<(Selection, bool)> = std::iter::once((self.selection, false))
            .chain(self.extra.drain(..).map(|selection| (selection, false)))
            .collect();
        all.push((Selection::cursor(offset), true));
        self.preferred_col = None;
        self.set_cursors(all);
    }

    /// The selected text to look for, selecting the word at the cursor first
    /// if nothing is selected (then there's nothing more to add yet).
    fn occurrence_needle(&mut self) -> Option<String> {
        if self.selection.is_empty() {
            let extra = std::mem::take(&mut self.extra);
            self.select_word_at(self.cursor());
            self.extra = extra;
            return None;
        }
        Some(self.selected_text())
    }

    /// Char ranges where `needle` occurs, in order.
    fn occurrences(&self, needle: &str) -> Vec<std::ops::Range<usize>> {
        let text = self.rope.to_string();
        let len = needle.chars().count();
        text.match_indices(needle)
            .map(|(byte, _)| {
                let start = self.rope.byte_to_char(byte);
                start..start + len
            })
            .collect()
    }

    /// Ctrl+D: selects the word at the cursor, then each next occurrence of
    /// the selected text, wrapping around. Returns false when every one is
    /// already selected.
    pub fn add_next_occurrence(&mut self) -> bool {
        let Some(needle) = self.occurrence_needle() else {
            return true;
        };
        let selected = self.selections();
        let taken = |range: &std::ops::Range<usize>| selected.iter().any(|s| s.range() == *range);
        let after = self.selection.range().end;
        let occurrences = self.occurrences(&needle);
        let next = occurrences
            .iter()
            .filter(|range| range.start >= after)
            .chain(occurrences.iter())
            .find(|range| !taken(range))
            .cloned();
        let Some(next) = next else {
            return false;
        };
        let previous = std::mem::replace(
            &mut self.selection,
            Selection {
                anchor: next.start,
                head: next.end,
            },
        );
        self.extra.push(previous);
        self.preferred_col = None;
        self.last_edit = None;
        true
    }

    /// Ctrl+Shift+L: selects every occurrence of the selection (or of the
    /// word at the cursor).
    pub fn select_all_occurrences(&mut self) {
        if self.selection.is_empty() {
            self.occurrence_needle();
        }
        if self.selection.is_empty() {
            return;
        }
        let needle = self.selected_text();
        let current = self.selection.range();
        let all: Vec<(Selection, bool)> = self
            .occurrences(&needle)
            .into_iter()
            .map(|range| {
                let primary = range == current;
                (
                    Selection {
                        anchor: range.start,
                        head: range.end,
                    },
                    primary,
                )
            })
            .collect();
        if !all.is_empty() {
            self.set_cursors(all);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_at_every_cursor_as_one_undo_step() {
        let mut b = Buffer::new("ab\nab\nab");
        b.set_selection(Selection::cursor(1));
        b.toggle_cursor(4);
        b.toggle_cursor(7);
        b.for_each_selection(|b| b.insert("X"));
        assert_eq!(b.text(), "aXb\naXb\naXb");
        assert_eq!(
            b.selections().iter().map(|s| s.head).collect::<Vec<_>>(),
            [2, 6, 10]
        );
        b.for_each_selection(|b| b.backspace());
        assert_eq!(b.text(), "ab\nab\nab");
        b.undo();
        assert_eq!(b.text(), "aXb\naXb\naXb");
        b.undo();
        assert_eq!(b.text(), "ab\nab\nab");
        assert_eq!(b.selections().len(), 3);
    }

    #[test]
    fn ctrl_d_adds_occurrences_and_wraps() {
        let mut b = Buffer::new("foo bar foo baz foo");
        b.set_selection(Selection::cursor(9));
        assert!(b.add_next_occurrence());
        assert_eq!(b.selected_text(), "foo");
        assert_eq!(b.selection.range(), 8..11);
        assert!(b.add_next_occurrence());
        assert_eq!(b.selection.range(), 16..19);
        assert!(b.add_next_occurrence());
        assert_eq!(b.selection.range(), 0..3);
        assert!(!b.add_next_occurrence());
        b.for_each_selection(|b| b.insert("x"));
        assert_eq!(b.text(), "x bar x baz x");
    }

    #[test]
    fn merges_cursors_that_meet() {
        let mut b = Buffer::new("abc");
        b.set_selection(Selection::cursor(1));
        b.toggle_cursor(2);
        b.for_each_selection(|b| b.move_doc_start(false));
        assert!(!b.has_extra_cursors());
        b.select_all_occurrences();
        assert_eq!(b.selection.range(), 0..3);
    }
}
