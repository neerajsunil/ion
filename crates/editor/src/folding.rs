//! Folding commands and the gutter chevrons. The folds themselves live in
//! the buffer ([`text::Buffer::fold_at`]); [`crate::wrap::WrapMap`] hides
//! their lines.

use gpui::{Context, Pixels, Point, Window};

use crate::Editor;
use crate::actions::{Fold, FoldAll, Unfold, UnfoldAll};
use crate::element::GUTTER_PADDING_RIGHT;

impl Editor {
    /// Code editors fold; inputs and diff views don't.
    pub(crate) fn can_fold(&self) -> bool {
        !self.single_line && !self.input && self.diff_rows.is_none()
    }

    /// After folding: a cursor in a closed fold moves to its first line.
    fn folds_changed(&mut self, cx: &mut Context<Self>) {
        self.buffer.clear_extra_cursors();
        let row = self.cursor_row();
        let hiding = self
            .buffer
            .hidden_ranges()
            .into_iter()
            .find(|&(first, last)| (first..=last).contains(&row));
        if let Some((first, _)) = hiding {
            let header = first - 1;
            let end = self.buffer.line_start(header) + self.buffer.line_len(header);
            self.buffer.place_cursor(end, false);
        }
        self.autoscroll = true;
        cx.notify();
    }

    pub(crate) fn toggle_fold_at(&mut self, row: usize, cx: &mut Context<Self>) {
        self.buffer.toggle_fold(row);
        self.folds_changed(cx);
    }

    pub(crate) fn fold(&mut self, _: &Fold, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_fold() && self.buffer.fold_at(self.cursor_row()) {
            self.folds_changed(cx);
        }
    }

    pub(crate) fn unfold(&mut self, _: &Unfold, _: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.unfold_at(self.cursor_row()) {
            cx.notify();
        }
    }

    pub(crate) fn fold_all(&mut self, _: &FoldAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.can_fold() {
            self.buffer.fold_all();
            self.folds_changed(cx);
        }
    }

    pub(crate) fn unfold_all(&mut self, _: &UnfoldAll, _: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unfold_all();
        cx.notify();
    }

    /// The row whose fold chevron is at `position`, if it's over one.
    pub(crate) fn fold_toggle_at(&self, position: Point<Pixels>) -> Option<usize> {
        if !self.can_fold() {
            return None;
        }
        let layout = self.layout.as_ref()?;
        let zone = layout.text_left - gpui::px(GUTTER_PADDING_RIGHT)..layout.text_left;
        if !zone.contains(&position.x) || !layout.bounds.contains(&position) {
            return None;
        }
        let y = position.y - layout.bounds.top() + self.scroll.y;
        let display_row = (y / layout.line_height).floor().max(0.) as usize;
        let line = layout.display_line(display_row)?;
        let row = line.row;
        (line.start_col == 0 && (self.buffer.is_folded(row) || self.buffer.is_foldable(row)))
            .then_some(row)
    }

    /// Tracks whether the mouse is over the gutter, which shows chevrons.
    pub(crate) fn update_gutter_hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.can_fold() {
            return;
        }
        let hovered = self.layout.as_ref().is_some_and(|layout| {
            layout.bounds.contains(&position) && position.x < layout.text_left
        });
        if hovered != self.gutter_hovered {
            self.gutter_hovered = hovered;
            cx.notify();
        }
    }
}
