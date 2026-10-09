//! Converting buffer lines into the text that is actually drawn.

use gpui::{Bounds, Pixels, ShapedLine};

/// Lines longer than this are cut off when drawn, so one huge minified line
/// can't stall a frame.
pub(crate) const MAX_DRAWN_CHARS: usize = 10_000;

/// A buffer line prepared for drawing: tabs expanded to spaces, plus a map from
/// each char column to its byte offset in the drawn text.
pub(crate) struct DisplayLine {
    pub text: String,
    /// `col_to_byte[col]` is the byte offset of char `col`; one extra entry
    /// marks the end of the line.
    pub col_to_byte: Vec<usize>,
}

impl DisplayLine {
    pub fn masked(line: &str) -> Self {
        Self::new(&"•".repeat(line.chars().count().min(MAX_DRAWN_CHARS)), 1)
    }
    /// `tab_width` is how many columns a tab character spans.
    pub fn new(line: &str, tab_width: usize) -> Self {
        let tab_width = tab_width.max(1);
        let mut text = String::with_capacity(line.len());
        let mut col_to_byte = Vec::with_capacity(line.len() + 1);
        let mut visual_col = 0;
        for ch in line.chars().take(MAX_DRAWN_CHARS) {
            col_to_byte.push(text.len());
            if ch == '\t' {
                let spaces = tab_width - visual_col % tab_width;
                text.extend(std::iter::repeat_n(' ', spaces));
                visual_col += spaces;
            } else {
                text.push(ch);
                visual_col += 1;
            }
        }
        col_to_byte.push(text.len());
        Self { text, col_to_byte }
    }

    pub fn byte_for_col(&self, col: usize) -> usize {
        self.col_to_byte[col.min(self.col_to_byte.len() - 1)]
    }

    pub fn col_for_byte(&self, byte: usize) -> usize {
        self.col_to_byte.partition_point(|&b| b < byte)
    }
}

/// A display row that was drawn in the last frame: a buffer line, or one
/// segment of a wrapped line.
pub(crate) struct VisibleLine {
    /// Buffer row.
    pub row: usize,
    /// Screen row, counting wrapped segments.
    pub display_row: usize,
    /// Buffer column of the segment's first char.
    pub start_col: usize,
    /// Where the next segment starts, if the line continues below.
    pub end_col: Option<usize>,
    pub display: DisplayLine,
    pub shaped: ShapedLine,
}

impl VisibleLine {
    /// X of a buffer column, clamped to this segment.
    pub fn x_for_col(&self, col: usize) -> Pixels {
        let col = col.saturating_sub(self.start_col);
        self.shaped.x_for_index(self.display.byte_for_col(col))
    }

    /// The buffer column nearest `x`. A continued segment stops before its
    /// last column, which belongs to the row below.
    pub fn col_for_x(&self, x: Pixels) -> usize {
        let col = self.start_col
            + self
                .display
                .col_for_byte(self.shaped.closest_index_for_x(x));
        match self.end_col {
            Some(end) => col.min(end.saturating_sub(1).max(self.start_col)),
            None => col,
        }
    }

    /// Whether the cursor at buffer `col` of this row shows on this segment.
    pub fn contains_col(&self, col: usize) -> bool {
        col >= self.start_col && self.end_col.is_none_or(|end| col < end)
    }
}

/// Geometry of the last frame, used for mouse hit testing and IME placement.
pub(crate) struct EditorLayout {
    pub bounds: Bounds<Pixels>,
    /// Left edge of the text area (right of the gutter).
    pub text_left: Pixels,
    pub line_height: Pixels,
    pub lines: Vec<VisibleLine>,
}

impl EditorLayout {
    /// The drawn segment showing buffer (row, col).
    pub fn line(&self, row: usize, col: usize) -> Option<&VisibleLine> {
        let start = self.lines.partition_point(|line| line.row < row);
        self.lines[start..]
            .iter()
            .take_while(|line| line.row == row)
            .find(|line| line.contains_col(col))
    }

    /// The drawn line on screen row `display_row`.
    pub fn display_line(&self, display_row: usize) -> Option<&VisibleLine> {
        let first = self.lines.first()?.display_row;
        self.lines.get(display_row.checked_sub(first)?)
    }

    pub fn visible_rows(&self) -> usize {
        (self.bounds.size.height / self.line_height).floor().max(1.) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tabs_to_tab_stops() {
        let line = DisplayLine::new("a\tb", 4);
        assert_eq!(line.text, "a   b");
        assert_eq!(line.col_to_byte, [0, 1, 4, 5]);
        assert_eq!(line.col_for_byte(4), 2);
    }

    #[test]
    fn maps_multibyte_chars() {
        let line = DisplayLine::new("é!", 4);
        assert_eq!(line.col_to_byte, [0, 2, 3]);
        assert_eq!(line.byte_for_col(99), 3);
    }

    #[test]
    fn passwords_mask_unicode_and_keep_cursor_columns() {
        let line = DisplayLine::masked("séc\tret");
        assert_eq!(line.text, "•••••••");
        assert_eq!(line.col_to_byte.len(), 8);
        assert_eq!(line.col_for_byte(line.byte_for_col(3)), 3);
    }
}
