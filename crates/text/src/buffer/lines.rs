//! Whole-line editing: comments, duplicating, moving and deleting lines, and
//! typing with bracket pairs. Each command is one undo step.

use super::{Buffer, EditKind, Selection};

/// Characters typed in pairs.
const PAIRS: [(char, char); 6] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('"', '"'),
    ('\'', '\''),
    ('`', '`'),
];

impl Buffer {
    fn char_at(&self, offset: usize) -> Option<char> {
        (offset < self.rope.len_chars()).then(|| self.rope.char(offset))
    }

    fn line_end(&self, row: usize) -> usize {
        self.line_start(row) + self.line_len(row)
    }

    /// Replaces rows `first..=last` with `lines` and puts the selection at
    /// the given (row, col) points.
    fn replace_rows(
        &mut self,
        first: usize,
        last: usize,
        lines: &[String],
        anchor: (usize, usize),
        head: (usize, usize),
    ) {
        let text = lines.join(self.line_ending);
        self.edit(
            self.line_start(first)..self.line_end(last),
            &text,
            EditKind::Other,
        );
        let clamp = |this: &Self, (row, col): (usize, usize)| {
            let row = row.min(this.line_count().saturating_sub(1));
            this.offset(row, col.min(this.line_len(row)))
        };
        self.selection = Selection {
            anchor: clamp(self, anchor),
            head: clamp(self, head),
        };
    }

    /// Comments out the selected lines, or uncomments them if they all are.
    /// `close` is empty for line comments (`// `), or ends a block comment
    /// wrapped around each line (`<!-- ` … ` -->`).
    pub fn toggle_comment(&mut self, open: &str, close: &str) {
        let rows = self.selected_rows();
        let (first, last) = (*rows.start(), *rows.end());
        let lines: Vec<String> = rows.clone().map(|row| self.line_text(row)).collect();
        let (open_mark, close_mark) = (open.trim_end(), close.trim_start());
        let indent = |line: &str| line.chars().take_while(|c| c.is_whitespace()).count();
        let code: Vec<&String> = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if code.is_empty() {
            return;
        }
        let commented = code.iter().all(|line| {
            let body = line.trim();
            body.starts_with(open_mark) && body.ends_with(close_mark)
        });
        let column = code.iter().map(|line| indent(line)).min().unwrap_or(0);
        // (column where the change starts, chars added or removed) per row.
        let mut shifts = Vec::new();
        let new_lines: Vec<String> = lines
            .iter()
            .map(|line| {
                if line.trim().is_empty() {
                    shifts.push((0, 0isize));
                    return line.clone();
                }
                let chars: Vec<char> = line.chars().collect();
                if commented {
                    let at = indent(line);
                    let rest: String = chars[at..].iter().collect();
                    let mut body = rest
                        .strip_prefix(open)
                        .or_else(|| rest.strip_prefix(open_mark))
                        .unwrap_or(&rest);
                    if !close.is_empty() {
                        body = body
                            .strip_suffix(close)
                            .or_else(|| body.strip_suffix(close_mark))
                            .unwrap_or(body);
                    }
                    let removed = rest.chars().count() as isize - body.chars().count() as isize;
                    shifts.push((
                        at,
                        -(rest
                            .find(body)
                            .map_or(removed, |i| rest[..i].chars().count() as isize)),
                    ));
                    format!("{}{body}", chars[..at].iter().collect::<String>())
                } else {
                    shifts.push((column, open.chars().count() as isize));
                    let head: String = chars[..column].iter().collect();
                    let body: String = chars[column..].iter().collect();
                    format!("{head}{open}{body}{close}")
                }
            })
            .collect();
        let moved = |offset: usize| {
            let (row, col) = self.row_col(offset);
            let (at, shift) = shifts[row - first];
            let col = if col < at {
                col
            } else {
                (col as isize + shift).max(at as isize) as usize
            };
            (row, col)
        };
        let (anchor, head) = (moved(self.selection.anchor), moved(self.selection.head));
        self.replace_rows(first, last, &new_lines, anchor, head);
    }

    /// Copies the selected lines below them and selects the copy.
    pub fn duplicate_lines(&mut self) {
        let rows = self.selected_rows();
        let (first, last) = (*rows.start(), *rows.end());
        let lines: Vec<String> = rows.map(|row| self.line_text(row)).collect();
        let count = lines.len();
        let mut doubled = lines.clone();
        doubled.extend(lines);
        let (anchor, head) = (
            self.row_col(self.selection.anchor),
            self.row_col(self.selection.head),
        );
        self.replace_rows(
            first,
            last,
            &doubled,
            (anchor.0 + count, anchor.1),
            (head.0 + count, head.1),
        );
    }

    /// Moves the selected lines up or down past their neighbor.
    pub fn move_lines(&mut self, up: bool) {
        let rows = self.selected_rows();
        let (first, last) = (*rows.start(), *rows.end());
        let last_row = self.line_count().saturating_sub(1);
        // The empty line after a final newline stays put.
        let last_movable = if self.line_len(last_row) == 0 && last_row > 0 {
            last_row - 1
        } else {
            last_row
        };
        if (up && first == 0) || (!up && last >= last_movable) {
            return;
        }
        let (from, to) = if up {
            (first - 1, last)
        } else {
            (first, last + 1)
        };
        let mut lines: Vec<String> = (from..=to).map(|row| self.line_text(row)).collect();
        if up {
            lines.rotate_left(1);
        } else {
            lines.rotate_right(1);
        }
        let step = |(row, col): (usize, usize)| if up { (row - 1, col) } else { (row + 1, col) };
        let (anchor, head) = (
            step(self.row_col(self.selection.anchor)),
            step(self.row_col(self.selection.head)),
        );
        self.replace_rows(from, to, &lines, anchor, head);
    }

    /// Deletes the selected lines.
    pub fn delete_lines(&mut self) {
        let rows = self.selected_rows();
        let (first, last) = (*rows.start(), *rows.end());
        let col = self.row_col(self.selection.head).1;
        let range = if last + 1 < self.line_count() {
            self.line_start(first)..self.line_start(last + 1)
        } else if first > 0 {
            self.line_end(first - 1)..self.len()
        } else {
            0..self.len()
        };
        self.edit(range, "", EditKind::Other);
        let row = first.min(self.line_count().saturating_sub(1));
        self.selection = Selection::cursor(self.offset(row, col.min(self.line_len(row))));
    }

    /// Types `c`, pairing brackets and quotes: an opener gets its closer
    /// (or wraps the selection), and typing a closer that's already next
    /// steps over it. Returns false if `c` is ordinary text.
    pub fn type_pair(&mut self, c: char, step_over: bool) -> bool {
        let range = self.selection.range();
        let next = self.char_at(range.end);
        if step_over
            && range.is_empty()
            && next == Some(c)
            && PAIRS.iter().any(|(_, close)| *close == c)
        {
            self.move_to(range.end + 1, false);
            return true;
        }
        let Some(&(open, close)) = PAIRS.iter().find(|(open, _)| *open == c) else {
            return false;
        };
        if !range.is_empty() {
            let selected = self.text_in(range.clone());
            if selected.contains('\n') && open == close {
                return false;
            }
            self.edit(
                range.clone(),
                &format!("{open}{selected}{close}"),
                EditKind::Other,
            );
            let len = selected.chars().count();
            self.selection = Selection {
                anchor: range.start + 1,
                head: range.start + 1 + len,
            };
            return true;
        }
        // Only pair before whitespace, a closer or the end of the line.
        let before_ok = next
            .is_none_or(|n| n.is_whitespace() || matches!(n, ')' | ']' | '}' | ',' | ';' | ':'));
        let prev = range
            .start
            .checked_sub(1)
            .and_then(|offset| self.char_at(offset));
        // Quotes after a word are apostrophes (don't, it's), not strings.
        let quote_ok = open != close || prev.is_none_or(|p| !p.is_alphanumeric() && p != open);
        if !before_ok || !quote_ok {
            return false;
        }
        self.edit(range.clone(), &format!("{open}{close}"), EditKind::Insert);
        self.selection = Selection::cursor(range.start + 1);
        true
    }

    /// Backspace between a pair (`(|)`) removes both. Returns false if the
    /// cursor isn't between one.
    pub fn backspace_pair(&mut self) -> bool {
        let range = self.selection.range();
        if !range.is_empty() || range.start == 0 {
            return false;
        }
        let (prev, next) = (self.char_at(range.start - 1), self.char_at(range.start));
        let paired = PAIRS
            .iter()
            .any(|(open, close)| prev == Some(*open) && next == Some(*close));
        if paired {
            self.edit(range.start - 1..range.start + 1, "", EditKind::Delete);
        }
        paired
    }
}

impl Buffer {
    /// Before a save: strips trailing spaces and tabs (except on lines with
    /// a cursor, so typing isn't disturbed) and ends the text with a line
    /// break. One undo step; cursors keep their places. Returns whether the
    /// text changed.
    pub fn tidy_for_save(&mut self, trim_whitespace: bool, final_newline: bool) -> bool {
        // Rows with a cursor or selection on them, sorted by start.
        let busy: Vec<(usize, usize)> = self
            .selections()
            .iter()
            .map(|selection| {
                let range = selection.range();
                (self.row_col(range.start).0, self.row_col(range.end).0)
            })
            .collect();
        let mut edits: Vec<(std::ops::Range<usize>, &str)> = Vec::new();
        if trim_whitespace {
            let mut busy = busy.iter().peekable();
            for row in 0..self.line_count() {
                while busy.next_if(|(_, last)| *last < row).is_some() {}
                if busy.peek().is_some_and(|(first, _)| *first <= row) {
                    continue;
                }
                let len = self.line_len(row);
                let line = self.rope.line(row);
                let trailing = line
                    .chars_at(len)
                    .reversed()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .count();
                if trailing > 0 {
                    let start = self.line_start(row);
                    edits.push((start + len - trailing..start + len, ""));
                }
            }
        }
        let len = self.len();
        if final_newline && len > 0 && self.rope.char(len - 1) != '\n' {
            edits.push((len..len, self.line_ending));
        }
        if edits.is_empty() {
            return false;
        }
        // Rows don't change, nor do columns on cursor lines.
        let place = |buffer: &Self, selection: Selection| {
            let (anchor, head) = (
                buffer.row_col(selection.anchor),
                buffer.row_col(selection.head),
            );
            (anchor, head)
        };
        let primary = place(self, self.selection);
        let extra: Vec<_> = self.extra.iter().map(|s| place(self, *s)).collect();
        self.batch = Some((self.selection, self.extra.clone(), false));
        for (range, text) in edits.into_iter().rev() {
            self.edit(range, text, EditKind::Other);
        }
        self.batch = None;
        let restore = |buffer: &Self, (anchor, head): ((usize, usize), (usize, usize))| Selection {
            anchor: buffer.offset(anchor.0, anchor.1),
            head: buffer.offset(head.0, head.1),
        };
        self.selection = restore(self, primary);
        self.extra = extra.into_iter().map(|s| restore(self, s)).collect();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_for_save_trims_other_lines_and_adds_a_final_newline() {
        let mut buffer = Buffer::new("a  \nb\t\nc  ");
        // The cursor sits at the end of "b\t".
        buffer.place_cursor(6, false);
        assert!(buffer.tidy_for_save(true, true));
        assert_eq!(buffer.text(), "a\nb\t\nc\n");
        assert_eq!(buffer.row_col(buffer.cursor()), (1, 2));
        assert!(!buffer.tidy_for_save(true, true));
        // One undo step brings it all back.
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "a  \nb\t\nc  ");
        let mut crlf = Buffer::new("x\r\ny");
        crlf.place_cursor(0, false);
        assert!(crlf.tidy_for_save(false, true));
        assert_eq!(crlf.text(), "x\r\ny\r\n");
    }

    fn buffer(text: &str, anchor: (usize, usize), head: (usize, usize)) -> Buffer {
        let mut b = Buffer::new(text);
        let (a, h) = (b.offset(anchor.0, anchor.1), b.offset(head.0, head.1));
        b.set_selection(Selection { anchor: a, head: h });
        b
    }

    #[test]
    fn toggles_line_comments_at_the_shared_indent() {
        let mut b = buffer("fn a() {\n    x();\n\n    y();\n}\n", (1, 4), (3, 6));
        b.toggle_comment("// ", "");
        assert_eq!(b.text(), "fn a() {\n    // x();\n\n    // y();\n}\n");
        assert_eq!(b.row_col(b.selection.head), (3, 9));
        b.toggle_comment("// ", "");
        assert_eq!(b.text(), "fn a() {\n    x();\n\n    y();\n}\n");
        assert_eq!(b.row_col(b.selection.head), (3, 6));
        // Mixed lines get commented, not uncommented.
        let mut b = buffer("// a\nb\n", (0, 0), (1, 1));
        b.toggle_comment("# ", "");
        assert_eq!(b.text(), "# // a\n# b\n");
        // Comments typed without the space still toggle off.
        let mut b = buffer("    //x\n", (0, 0), (0, 0));
        b.toggle_comment("// ", "");
        assert_eq!(b.text(), "    x\n");
    }

    #[test]
    fn toggles_block_comments() {
        let mut b = buffer("<p>hi</p>\n", (0, 2), (0, 2));
        b.toggle_comment("<!-- ", " -->");
        assert_eq!(b.text(), "<!-- <p>hi</p> -->\n");
        b.toggle_comment("<!-- ", " -->");
        assert_eq!(b.text(), "<p>hi</p>\n");
    }

    #[test]
    fn duplicates_moves_and_deletes_lines() {
        let mut b = buffer("a\nb\nc\n", (1, 1), (1, 1));
        b.duplicate_lines();
        assert_eq!(b.text(), "a\nb\nb\nc\n");
        assert_eq!(b.row_col(b.selection.head), (2, 1));
        b.move_lines(true);
        b.move_lines(true);
        assert_eq!(b.text(), "b\na\nb\nc\n");
        assert_eq!(b.row_col(b.selection.head), (0, 1));
        b.move_lines(true);
        assert_eq!(b.text(), "b\na\nb\nc\n");
        let mut b = buffer("a\nb\nc\n", (0, 0), (1, 1));
        b.move_lines(false);
        assert_eq!(b.text(), "c\na\nb\n");
        b.move_lines(false);
        assert_eq!(b.text(), "c\na\nb\n", "the trailing empty line stays last");
        b.delete_lines();
        assert_eq!(b.text(), "c\n");
        let mut b = buffer("x\ny", (1, 0), (1, 0));
        b.delete_lines();
        assert_eq!(b.text(), "x");
        b.undo();
        assert_eq!(b.text(), "x\ny");
    }

    #[test]
    fn pairs_brackets_and_quotes() {
        let mut b = Buffer::new("");
        assert!(b.type_pair('(', true));
        assert_eq!((b.text().as_str(), b.cursor()), ("()", 1));
        assert!(b.type_pair(')', true));
        assert_eq!((b.text().as_str(), b.cursor()), ("()", 2));
        // Apostrophes in words aren't paired.
        let mut b = Buffer::new("don");
        b.move_to(3, false);
        assert!(!b.type_pair('\'', true));
        // Before a word, no pair.
        let mut b = Buffer::new("x");
        assert!(!b.type_pair('(', true));
        // Wrap a selection.
        let mut b = Buffer::new("name");
        b.select_all();
        assert!(b.type_pair('"', true));
        assert_eq!(b.text(), "\"name\"");
        assert_eq!(b.selected_text(), "name");
        // Backspace removes both halves.
        let mut b = Buffer::new("[]");
        b.move_to(1, false);
        assert!(b.backspace_pair());
        assert_eq!(b.text(), "");
    }
}
