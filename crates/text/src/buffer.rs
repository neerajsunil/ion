//! A rope plus a selection and undo history. All offsets are char indices.

use ropey::Rope;
use std::ops::Range;
use std::time::{Duration, Instant};

mod cursors;
mod folds;
mod lines;

const UNDO_GROUP_TIMEOUT: Duration = Duration::from_millis(800);
pub const TAB: &str = "    ";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn cursor(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

/// One change to the text, in the form incremental parsers (tree-sitter) need:
/// byte offsets plus `(row, byte column)` points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
}

/// Changes since the last [`Buffer::take_edits`].
#[derive(Debug, Default)]
pub struct EditBatch {
    pub edits: Vec<TextEdit>,
    /// The text was replaced wholesale (undo/redo); `edits` don't describe it.
    pub reset: bool,
}

struct Snapshot {
    rope: Rope,
    selection: Selection,
    extra: Vec<Selection>,
    version: u64,
}

pub struct Buffer {
    rope: Rope,
    /// The primary selection (the newest cursor).
    pub selection: Selection,
    /// More cursors (Ctrl+D, Alt+click), not overlapping, in no order.
    pub extra: Vec<Selection>,
    /// Set while an edit runs at every cursor: the selections from before
    /// it, and whether its undo step was taken.
    batch: Option<(Selection, Vec<Selection>, bool)>,
    /// Folded header rows, sorted.
    folds: Vec<usize>,
    folds_version: u64,
    line_ending: &'static str,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    last_edit: Option<(EditKind, Instant)>,
    version: u64,
    next_version: u64,
    saved_version: u64,
    /// Column to aim for when moving vertically through shorter lines.
    preferred_col: Option<usize>,
    /// Edits recorded for incremental parsing; `None` when nobody listens.
    edit_log: Option<EditBatch>,
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Self {
            rope: Rope::from_str(text),
            selection: Selection::default(),
            extra: Vec::new(),
            batch: None,
            folds: Vec::new(),
            folds_version: 0,
            line_ending,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            version: 0,
            next_version: 1,
            saved_version: 0,
            preferred_col: None,
            edit_log: None,
        }
    }

    /// Starts recording edits for [`Buffer::take_edits`].
    pub fn track_edits(&mut self) {
        self.edit_log.get_or_insert_with(EditBatch::default);
    }

    /// Returns and clears the edits recorded since the last call.
    pub fn take_edits(&mut self) -> EditBatch {
        self.edit_log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// `(row, byte column)` of a byte offset.
    fn byte_point(&self, byte: usize) -> (usize, usize) {
        let row = self.rope.byte_to_line(byte);
        (row, byte - self.rope.line_to_byte(row))
    }

    fn record_reset(&mut self) {
        if let Some(log) = &mut self.edit_log {
            log.edits.clear();
            log.reset = true;
        }
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    pub fn line_ending(&self) -> &'static str {
        self.line_ending
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    /// Identifies the current contents; changes on every edit, undo and redo.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn mark_saved(&mut self) {
        self.mark_saved_at(self.version);
    }

    /// Records that the contents at `version` were saved. Used when a save runs
    /// in the background while the user keeps typing.
    pub fn mark_saved_at(&mut self, version: u64) {
        self.saved_version = version;
        self.last_edit = None;
    }

    pub fn cursor(&self) -> usize {
        self.selection.head
    }

    pub fn line_start(&self, row: usize) -> usize {
        self.rope.line_to_char(row)
    }

    /// Length of a line in chars, excluding its line break.
    pub fn line_len(&self, row: usize) -> usize {
        let line = self.rope.line(row);
        let mut len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && line.char(len - 1) == '\r' {
                len -= 1;
            }
        } else if len > 0 && line.char(len - 1) == '\r' {
            len -= 1;
        }
        len
    }

    /// Line contents without the line break.
    pub fn line_text(&self, row: usize) -> String {
        let start = self.line_start(row);
        self.rope
            .slice(start..start + self.line_len(row))
            .to_string()
    }

    pub fn row_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.len());
        let row = self.rope.char_to_line(offset);
        let col = (offset - self.line_start(row)).min(self.line_len(row));
        (row, col)
    }

    pub fn offset(&self, row: usize, col: usize) -> usize {
        let row = row.min(self.line_count().saturating_sub(1));
        self.line_start(row) + col.min(self.line_len(row))
    }

    pub fn text_in(&self, range: Range<usize>) -> String {
        self.rope.slice(range).to_string()
    }

    pub fn selected_text(&self) -> String {
        self.text_in(self.selection.range())
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    // ---- editing ---------------------------------------------------------

    fn edit(&mut self, range: Range<usize>, text: &str, kind: EditKind) {
        let now = Instant::now();
        let coalesce = kind != EditKind::Other
            && !text.contains('\n')
            && matches!(self.last_edit, Some((k, t)) if k == kind && now - t < UNDO_GROUP_TIMEOUT);
        // An edit at several cursors is one undo step.
        let in_step = matches!(self.batch, Some((_, _, true)));
        if !coalesce && !in_step {
            let (selection, extra) = match &self.batch {
                Some((selection, extra, _)) => (*selection, extra.clone()),
                None => (self.selection, self.extra.clone()),
            };
            self.undo_stack.push(Snapshot {
                rope: self.rope.clone(),
                selection,
                extra,
                version: self.version,
            });
        }
        if let Some((_, _, taken)) = &mut self.batch {
            *taken = true;
        }
        self.redo_stack.clear();
        self.last_edit = Some((kind, now));

        let start_byte = self.rope.char_to_byte(range.start);
        let old_end_byte = self.rope.char_to_byte(range.end);
        let old_points = self
            .edit_log
            .is_some()
            .then(|| (self.byte_point(start_byte), self.byte_point(old_end_byte)));

        let folds_before = (!self.folds.is_empty()).then(|| self.folds_after_edit(&range));
        let lines_before = self.rope.len_lines();
        if !range.is_empty() {
            self.rope.remove(range.clone());
        }
        if !text.is_empty() {
            self.rope.insert(range.start, text);
        }
        if let Some(folds) = folds_before {
            let delta = self.rope.len_lines() as isize - lines_before as isize;
            self.shift_folds(folds, delta);
        }
        self.selection = Selection::cursor(range.start + text.chars().count());
        if let Some((start_point, old_end_point)) = old_points {
            let new_end_byte = start_byte + text.len();
            let new_end_point = self.byte_point(new_end_byte);
            if let Some(log) = &mut self.edit_log {
                log.edits.push(TextEdit {
                    start_byte,
                    old_end_byte,
                    new_end_byte,
                    start_point,
                    old_end_point,
                    new_end_point,
                });
            }
        }
        self.version = self.next_version;
        self.next_version += 1;
        self.preferred_col = None;
    }

    /// Replaces `range` (or the selection) with `text`. Used for typing and IME.
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = range.unwrap_or_else(|| self.selection.range());
        let kind = if text.chars().count() == 1 && range.is_empty() {
            EditKind::Insert
        } else {
            EditKind::Other
        };
        self.edit(range, text, kind);
    }

    pub fn insert(&mut self, text: &str) {
        self.replace(None, text);
    }

    /// The rows the selection touches. A selection ending at the start of a
    /// line doesn't include that line.
    fn selected_rows(&self) -> std::ops::RangeInclusive<usize> {
        let range = self.selection.range();
        let first = self.row_col(range.start).0;
        let (mut last, col) = self.row_col(range.end);
        if col == 0 && last > first {
            last -= 1;
        }
        first..=last
    }

    /// Whether the selection spans more than one line.
    pub fn selection_is_multiline(&self) -> bool {
        let rows = self.selected_rows();
        rows.start() != rows.end()
    }

    /// Indents every selected line by `unit`, or outdents by one level (a
    /// tab, or up to `unit`'s width of spaces). One undo step; the selection
    /// stays on the same text.
    pub fn indent_lines(&mut self, unit: &str, outdent: bool) {
        let rows = self.selected_rows();
        let width = unit.chars().count().max(1);
        let mut changes = Vec::new();
        for row in rows.clone() {
            let line = self.line_text(row);
            let change: isize = if outdent {
                let removed = if line.starts_with('\t') {
                    1
                } else {
                    line.chars().take(width).take_while(|c| *c == ' ').count()
                };
                -(removed as isize)
            } else if line.is_empty() && rows.start() != rows.end() {
                // Leave blank lines blank.
                0
            } else {
                width as isize
            };
            changes.push(change);
        }
        if changes.iter().all(|change| *change == 0) {
            return;
        }
        let (anchor, head) = (
            self.row_col(self.selection.anchor),
            self.row_col(self.selection.head),
        );
        let start = self.line_start(*rows.start());
        let end = self.line_start(*rows.end()) + self.line_len(*rows.end());
        let mut text = String::new();
        for (ix, row) in rows.clone().enumerate() {
            if ix > 0 {
                text.push_str(self.line_ending);
            }
            let line = self.line_text(row);
            match changes[ix] {
                change if change < 0 => text.extend(line.chars().skip((-change) as usize)),
                0 => text.push_str(&line),
                _ => {
                    text.push_str(unit);
                    text.push_str(&line);
                }
            }
        }
        self.edit(start..end, &text, EditKind::Other);
        let moved = |(row, col): (usize, usize)| -> usize {
            let change = rows
                .clone()
                .position(|r| r == row)
                .map_or(0, |ix| changes[ix]);
            let col = if change < 0 {
                col.saturating_sub((-change) as usize)
            } else if col == 0 && change > 0 && row != *rows.start() {
                0
            } else {
                col + change as usize
            };
            self.offset(row, col)
        };
        self.selection = Selection {
            anchor: moved(anchor),
            head: moved(head),
        };
    }

    /// Pastes text, normalizing line breaks to the buffer's line ending.
    pub fn paste(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n");
        let normalized = if self.line_ending == "\n" {
            normalized
        } else {
            normalized.replace('\n', self.line_ending)
        };
        self.edit(self.selection.range(), &normalized, EditKind::Other);
    }

    pub fn newline(&mut self) {
        let (row, col) = self.row_col(self.selection.range().start);
        let indent: String = self
            .line_text(row)
            .chars()
            .take(col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let text = format!("{}{}", self.line_ending, indent);
        self.edit(self.selection.range(), &text, EditKind::Other);
    }

    pub fn backspace(&mut self) {
        let range = if self.selection.is_empty() {
            let end = self.cursor();
            self.prev_char_boundary(end)..end
        } else {
            self.selection.range()
        };
        if !range.is_empty() {
            self.edit(range, "", EditKind::Delete);
        }
    }

    pub fn delete(&mut self) {
        let range = if self.selection.is_empty() {
            let start = self.cursor();
            start..self.next_char_boundary(start)
        } else {
            self.selection.range()
        };
        if !range.is_empty() {
            self.edit(range, "", EditKind::Delete);
        }
    }

    pub fn delete_word_left(&mut self) {
        if self.selection.is_empty() {
            let start = self.word_left(self.cursor());
            self.selection.anchor = start;
        }
        self.backspace();
    }

    pub fn delete_word_right(&mut self) {
        if self.selection.is_empty() {
            let end = self.word_right(self.cursor());
            self.selection.anchor = end;
        }
        self.delete();
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(Snapshot {
            rope: std::mem::replace(&mut self.rope, snapshot.rope),
            selection: self.selection,
            extra: std::mem::replace(&mut self.extra, snapshot.extra),
            version: self.version,
        });
        self.selection = snapshot.selection;
        self.version = snapshot.version;
        self.last_edit = None;
        self.preferred_col = None;
        self.record_reset();
        self.clamp_folds();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(Snapshot {
            rope: std::mem::replace(&mut self.rope, snapshot.rope),
            selection: self.selection,
            extra: std::mem::replace(&mut self.extra, snapshot.extra),
            version: self.version,
        });
        self.selection = snapshot.selection;
        self.version = snapshot.version;
        self.last_edit = None;
        self.preferred_col = None;
        self.record_reset();
        self.clamp_folds();
        true
    }

    // ---- movement --------------------------------------------------------

    /// Moves the cursor to `offset`, extending the selection if `select`.
    pub fn move_to(&mut self, offset: usize, select: bool) {
        let offset = offset.min(self.len());
        if select {
            self.selection.head = offset;
        } else {
            self.selection = Selection::cursor(offset);
        }
        self.last_edit = None;
    }

    /// Moves the cursor as a direct placement (e.g. a mouse click), forgetting
    /// the column remembered for vertical movement.
    pub fn place_cursor(&mut self, offset: usize, select: bool) {
        self.preferred_col = None;
        self.extra.clear();
        self.move_to(offset, select);
    }

    /// Selects one range, dropping any other cursors.
    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
        self.extra.clear();
        self.preferred_col = None;
        self.last_edit = None;
    }

    pub fn select_all(&mut self) {
        self.set_selection(Selection {
            anchor: 0,
            head: self.len(),
        });
    }

    pub fn move_left(&mut self, select: bool) {
        self.preferred_col = None;
        if !select && !self.selection.is_empty() {
            return self.move_to(self.selection.range().start, false);
        }
        let target = self.prev_char_boundary(self.cursor());
        self.move_to(target, select);
    }

    pub fn move_right(&mut self, select: bool) {
        self.preferred_col = None;
        if !select && !self.selection.is_empty() {
            return self.move_to(self.selection.range().end, false);
        }
        let target = self.next_char_boundary(self.cursor());
        self.move_to(target, select);
    }

    pub fn move_vertical(&mut self, delta: isize, select: bool) {
        let (row, col) = self.row_col(self.cursor());
        let goal = *self.preferred_col.get_or_insert(col);
        let last_row = self.line_count() - 1;
        let target_row = row as isize + delta;
        let target = if target_row < 0 {
            0
        } else if target_row as usize > last_row {
            self.len()
        } else {
            self.offset(target_row as usize, goal)
        };
        self.move_to(target, select);
    }

    pub fn move_word_left(&mut self, select: bool) {
        self.preferred_col = None;
        let target = self.word_left(self.cursor());
        self.move_to(target, select);
    }

    pub fn move_word_right(&mut self, select: bool) {
        self.preferred_col = None;
        let target = self.word_right(self.cursor());
        self.move_to(target, select);
    }

    /// Smart home: toggles between the first non-blank char and column zero.
    pub fn move_line_start(&mut self, select: bool) {
        self.preferred_col = None;
        let (row, col) = self.row_col(self.cursor());
        let indent = self
            .line_text(row)
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        let target_col = if col == indent { 0 } else { indent };
        self.move_to(self.line_start(row) + target_col, select);
    }

    pub fn move_line_end(&mut self, select: bool) {
        self.preferred_col = None;
        let (row, _) = self.row_col(self.cursor());
        self.move_to(self.line_start(row) + self.line_len(row), select);
    }

    pub fn move_doc_start(&mut self, select: bool) {
        self.preferred_col = None;
        self.move_to(0, select);
    }

    pub fn move_doc_end(&mut self, select: bool) {
        self.preferred_col = None;
        self.move_to(self.len(), select);
    }

    pub fn select_word_at(&mut self, offset: usize) {
        let offset = offset.min(self.len());
        let class_at = |i: usize| char_class(self.rope.char(i));
        let (start, end) = if offset < self.len() && class_at(offset) != CharClass::Newline {
            let class = class_at(offset);
            let mut start = offset;
            while start > 0 && class_at(start - 1) == class {
                start -= 1;
            }
            let mut end = offset;
            while end < self.len() && class_at(end) == class {
                end += 1;
            }
            (start, end)
        } else {
            (offset, offset)
        };
        self.set_selection(Selection {
            anchor: start,
            head: end,
        });
    }

    pub fn select_line_at(&mut self, offset: usize) {
        let (row, _) = self.row_col(offset);
        let start = self.line_start(row);
        let end = if row + 1 < self.line_count() {
            self.line_start(row + 1)
        } else {
            self.len()
        };
        self.set_selection(Selection {
            anchor: start,
            head: end,
        });
    }

    // ---- boundaries ------------------------------------------------------

    fn prev_char_boundary(&self, offset: usize) -> usize {
        if offset == 0 {
            return 0;
        }
        if offset >= 2 && self.rope.char(offset - 1) == '\n' && self.rope.char(offset - 2) == '\r' {
            offset - 2
        } else {
            offset - 1
        }
    }

    fn next_char_boundary(&self, offset: usize) -> usize {
        let len = self.len();
        if offset >= len {
            return len;
        }
        if offset + 1 < len && self.rope.char(offset) == '\r' && self.rope.char(offset + 1) == '\n'
        {
            offset + 2
        } else {
            offset + 1
        }
    }

    fn word_left(&self, offset: usize) -> usize {
        let mut i = offset;
        while i > 0 && char_class(self.rope.char(i - 1)) == CharClass::Space {
            i -= 1;
        }
        if i > 0 && char_class(self.rope.char(i - 1)) == CharClass::Newline {
            return self.prev_char_boundary(i);
        }
        if i > 0 {
            let class = char_class(self.rope.char(i - 1));
            while i > 0 && char_class(self.rope.char(i - 1)) == class {
                i -= 1;
            }
        }
        i
    }

    fn word_right(&self, offset: usize) -> usize {
        let len = self.len();
        let mut i = offset;
        while i < len && char_class(self.rope.char(i)) == CharClass::Space {
            i += 1;
        }
        if i < len && char_class(self.rope.char(i)) == CharClass::Newline {
            return self.next_char_boundary(i);
        }
        if i < len {
            let class = char_class(self.rope.char(i));
            while i < len && char_class(self.rope.char(i)) == class {
                i += 1;
            }
        }
        i
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Punct,
    Space,
    Newline,
}

fn char_class(c: char) -> CharClass {
    if c == '\n' || c == '\r' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_undo() {
        let mut b = Buffer::new("hello");
        b.move_doc_end(false);
        b.insert(" ");
        b.insert("w");
        assert_eq!(b.text(), "hello w");
        assert!(b.is_dirty());
        b.undo();
        assert_eq!(b.text(), "hello");
        assert!(!b.is_dirty());
        b.redo();
        assert_eq!(b.text(), "hello w");
    }

    #[test]
    fn newline_keeps_indent() {
        let mut b = Buffer::new("    foo");
        b.move_doc_end(false);
        b.newline();
        b.insert("b");
        assert_eq!(b.text(), "    foo\n    b");
    }

    #[test]
    fn crlf_is_one_step() {
        let mut b = Buffer::new("a\r\nb");
        b.move_to(1, false);
        b.move_right(false);
        assert_eq!(b.cursor(), 3);
        b.backspace();
        assert_eq!(b.text(), "ab");
        assert_eq!(b.line_ending(), "\r\n");
    }

    #[test]
    fn vertical_movement_remembers_column() {
        let mut b = Buffer::new("abcdef\nab\nabcdef");
        b.move_to(5, false);
        b.move_vertical(1, false);
        assert_eq!(b.row_col(b.cursor()), (1, 2));
        b.move_vertical(1, false);
        assert_eq!(b.row_col(b.cursor()), (2, 5));
    }

    #[test]
    fn word_movement() {
        let mut b = Buffer::new("foo.bar  baz");
        b.move_word_right(false);
        assert_eq!(b.cursor(), 3);
        b.move_word_right(false);
        assert_eq!(b.cursor(), 4);
        b.move_word_right(false);
        assert_eq!(b.cursor(), 7);
        b.move_word_right(false);
        assert_eq!(b.cursor(), 12);
        b.move_word_left(false);
        assert_eq!(b.cursor(), 9);
    }

    #[test]
    fn records_edits_for_parsers() {
        let mut b = Buffer::new("ab\ncd");
        b.track_edits();
        b.move_to(4, false);
        b.insert("é");
        let batch = b.take_edits();
        assert!(!batch.reset);
        assert_eq!(
            batch.edits,
            [TextEdit {
                start_byte: 4,
                old_end_byte: 4,
                new_end_byte: 6,
                start_point: (1, 1),
                old_end_point: (1, 1),
                new_end_point: (1, 3),
            }]
        );
        b.undo();
        assert!(b.take_edits().reset);
        assert!(b.take_edits().edits.is_empty());
    }

    #[test]
    fn indents_and_outdents_selected_lines() {
        let mut b = Buffer::new("a\n  b\n\tc\n");
        b.set_selection(Selection {
            anchor: 0,
            head: b.offset(2, 1),
        });
        b.indent_lines("    ", false);
        assert_eq!(b.text(), "    a\n      b\n    \tc\n");
        b.indent_lines("    ", true);
        assert_eq!(b.text(), "a\n  b\n\tc\n");
        b.indent_lines("    ", true);
        assert_eq!(b.text(), "a\nb\nc\n");
        // One undo step back.
        b.undo();
        assert_eq!(b.text(), "a\n  b\n\tc\n");
        // A selection ending at a line start leaves that line alone.
        let mut b = Buffer::new("x\ny\n");
        b.set_selection(Selection {
            anchor: 0,
            head: b.offset(1, 0),
        });
        assert!(!b.selection_is_multiline());
        b.indent_lines("  ", false);
        assert_eq!(b.text(), "  x\ny\n");
    }

    #[test]
    fn line_len_excludes_break() {
        let b = Buffer::new("ab\r\ncd\n");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_len(0), 2);
        assert_eq!(b.line_len(1), 2);
        assert_eq!(b.line_len(2), 0);
    }
}
