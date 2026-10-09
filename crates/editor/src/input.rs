//! Platform text input (typing, IME composition, accessibility queries).
//!
//! The platform speaks UTF-16 offsets; the buffer uses char offsets.

use std::ops::Range;

use gpui::{
    Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, point, px, size,
};

use crate::Editor;

impl Editor {
    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.buffer
            .rope()
            .char_to_utf16_cu(offset.min(self.buffer.len()))
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let rope = self.buffer.rope();
        rope.utf16_cu_to_char(offset.min(rope.len_utf16_cu()))
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// The range an input event replaces: the given one, else the IME
    /// composition, else the selection.
    fn input_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.buffer.selection.range())
    }
}

/// Converts a UTF-16 offset within `text` to a char offset.
fn utf16_to_chars(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    text.chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= utf16
        })
        .count()
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.secret {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.buffer.text_in(range))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.buffer.selection;
        Some(UTF16Selection {
            range: self.range_to_utf16(&selection.range()),
            reversed: selection.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let text = if self.single_line {
            text.replace(['\r', '\n'], "")
        } else {
            text.to_owned()
        };
        // Brackets and quotes typed in code come in pairs.
        let mut chars = text.chars();
        let pair = match (chars.next(), chars.next()) {
            (Some(c), None)
                if !self.single_line
                    && !self.input
                    && self.marked_range.is_none()
                    && settings::get(cx).auto_close_brackets =>
            {
                Some(c)
            }
            _ => None,
        };
        // A closing bracket typed on a blank line moves back a level.
        let outdent = (!self.single_line && !self.input && self.marked_range.is_none())
            .then(|| self.indent_rules())
            .flatten()
            .map(|_| settings::get(cx).indent_unit());
        let mut chars = text.chars();
        let typed = match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        };
        let type_char = |buffer: &mut text::Buffer| {
            let Some(c) = typed else {
                return false;
            };
            pair.is_some_and(|c| buffer.type_pair(c, true))
                || outdent
                    .as_deref()
                    .is_some_and(|unit| buffer.type_closer_outdented(c, unit))
        };
        if range_utf16.is_none() && self.marked_range.is_none() && self.buffer.has_extra_cursors() {
            // Typing at every cursor.
            self.buffer.for_each_selection(|buffer| {
                if !type_char(buffer) {
                    buffer.replace(None, &text);
                }
            });
        } else {
            let range = self.input_range(range_utf16);
            self.buffer.clear_extra_cursors();
            let paired = typed.is_some() && {
                self.buffer.set_selection(text::Selection {
                    anchor: range.start,
                    head: range.end,
                });
                type_char(&mut self.buffer)
            };
            if !paired {
                self.buffer.replace(Some(range), &text);
            }
        }
        self.text_changed(cx);
        self.marked_range = None;
        self.autoscroll = true;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = self.input_range(range_utf16);
        self.buffer.clear_extra_cursors();
        let start = range.start;
        let len = text.chars().count();
        self.buffer.replace(Some(range), text);
        self.text_changed(cx);
        self.marked_range = (len > 0).then(|| start..start + len);
        if let Some(selected) = new_selected_range_utf16 {
            let mut selection = self.buffer.selection;
            selection.anchor = start + utf16_to_chars(text, selected.start);
            selection.head = start + utf16_to_chars(text, selected.end);
            self.buffer.set_selection(selection);
        }
        self.autoscroll = true;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let start = self.offset_from_utf16(range_utf16.start);
        let (row, col) = self.buffer.row_col(start);
        let line = layout.line(row, col)?;
        let x = layout.text_left - self.scroll.x + line.x_for_col(col);
        let y = layout.bounds.top() + layout.line_height * line.display_row as f32 - self.scroll.y;
        Some(Bounds::new(point(x, y), size(px(1.), layout.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_for_position(point)?;
        Some(self.offset_to_utf16(offset))
    }
}

#[cfg(test)]
mod tests {
    use super::utf16_to_chars;

    #[test]
    fn utf16_offsets_to_chars() {
        // '😀' is two UTF-16 units but one char.
        assert_eq!(utf16_to_chars("a😀b", 0), 0);
        assert_eq!(utf16_to_chars("a😀b", 3), 2);
        assert_eq!(utf16_to_chars("a😀b", 4), 3);
    }
}
