//! Side-by-side diffs: a diff view can show its rows as two aligned
//! columns, the old text on the left and the new on the right. Each side is
//! its own read-only editor; the inline view stays the model (its rows are
//! what the workspace knows), and the sides map their rows back to it.

use gpui::{
    AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Styled, Subscription,
    Window, div,
};

use crate::{DiffRow, DiffRowKind, Editor, EditorEvent};

/// One column of a side-by-side diff.
#[derive(Debug, Default)]
pub(crate) struct Side {
    pub text: String,
    pub rows: Vec<DiffRow>,
    /// The inline row each of these rows shows.
    pub map: Vec<usize>,
}

impl Side {
    fn push(&mut self, text: &str, row: DiffRow, inline: usize) {
        if !self.rows.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(text);
        self.rows.push(row);
        self.map.push(inline);
    }

    /// An empty row opposite a line the other side has alone.
    fn filler(&mut self, inline: usize) {
        self.push(
            "",
            DiffRow {
                kind: DiffRowKind::Filler,
                old_line: None,
                new_line: None,
            },
            inline,
        );
    }
}

/// Splits an inline diff (`lines` with their `rows`) into aligned sides:
/// removed lines pair up with the added lines after them, headers and
/// context show on both.
pub(crate) fn split_sides(lines: &[&str], rows: &[DiffRow]) -> (Side, Side) {
    let (mut left, mut right) = (Side::default(), Side::default());
    // A run of changes waiting to be paired: inline row indices.
    let mut removed: Vec<usize> = Vec::new();
    let mut added: Vec<usize> = Vec::new();
    let flush =
        |left: &mut Side, right: &mut Side, removed: &mut Vec<usize>, added: &mut Vec<usize>| {
            for ix in 0..removed.len().max(added.len()) {
                let (old, new) = (removed.get(ix).copied(), added.get(ix).copied());
                match old {
                    Some(row) => left.push(lines[row], rows[row], row),
                    None => left.filler(new.unwrap_or_default()),
                }
                match new {
                    Some(row) => right.push(lines[row], rows[row], row),
                    None => right.filler(old.unwrap_or_default()),
                }
            }
            removed.clear();
            added.clear();
        };
    for (ix, row) in rows.iter().enumerate() {
        let text = lines.get(ix).copied().unwrap_or_default();
        match row.kind {
            DiffRowKind::Removed => {
                // Removed lines after added ones start a new run.
                if !added.is_empty() {
                    flush(&mut left, &mut right, &mut removed, &mut added);
                }
                removed.push(ix);
            }
            DiffRowKind::Added => added.push(ix),
            _ => {
                flush(&mut left, &mut right, &mut removed, &mut added);
                let (old_only, new_only) = if row.kind == DiffRowKind::Context {
                    (
                        DiffRow {
                            new_line: None,
                            ..*row
                        },
                        DiffRow {
                            old_line: None,
                            ..*row
                        },
                    )
                } else {
                    (*row, *row)
                };
                left.push(text, old_only, ix);
                right.push(text, new_only, ix);
            }
        }
    }
    flush(&mut left, &mut right, &mut removed, &mut added);
    (left, right)
}

pub(crate) struct SplitDiff {
    pub left: Entity<Editor>,
    pub right: Entity<Editor>,
    left_map: Vec<usize>,
    right_map: Vec<usize>,
    _subscriptions: Vec<Subscription>,
}

impl Editor {
    pub fn is_split_diff(&self) -> bool {
        self.split.is_some()
    }

    /// Shows this diff view side by side, or back inline.
    pub fn set_split_diff(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff_rows.is_none() || self.diff_side || on == self.split.is_some() {
            return;
        }
        if !on {
            let had_focus = self.split.take().is_some_and(|split| {
                split.left.focus_handle(cx).contains_focused(window, cx)
                    || split.right.focus_handle(cx).contains_focused(window, cx)
            });
            if had_focus {
                window.focus(&self.focus_handle);
            }
            cx.notify();
            return;
        }
        let hint = self.language_hint.clone();
        let side = |cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut editor = Editor::diff_view("", "", Vec::new(), hint.clone(), cx);
                editor.diff_side = true;
                editor
            })
        };
        let (left, right) = (side(cx), side(cx));
        left.update(cx, |editor, _| {
            editor.scroll_partner = Some(right.downgrade())
        });
        right.update(cx, |editor, _| {
            editor.scroll_partner = Some(left.downgrade())
        });
        let subscriptions = vec![
            cx.subscribe(&left, |this, _, event, cx| this.side_event(true, event, cx)),
            cx.subscribe(&right, |this, _, event, cx| {
                this.side_event(false, event, cx)
            }),
            // The tab's focus goes to the new side.
            cx.on_focus(&self.focus_handle, window, |this, window, cx| {
                if let Some(split) = &this.split {
                    window.focus(&split.right.focus_handle(cx));
                }
            }),
        ];
        let focused = self.focus_handle.is_focused(window);
        self.split = Some(SplitDiff {
            left,
            right: right.clone(),
            left_map: Vec::new(),
            right_map: Vec::new(),
            _subscriptions: subscriptions,
        });
        self.refresh_split(cx);
        self.sync_split_cursor(cx);
        if focused {
            window.focus(&right.focus_handle(cx));
        }
        cx.notify();
    }

    /// Rebuilds both sides from the inline rows.
    pub(crate) fn refresh_split(&mut self, cx: &mut Context<Self>) {
        let (Some(rows), Some(_)) = (self.diff_rows.clone(), self.split.as_ref()) else {
            return;
        };
        let text = self.buffer.text();
        let lines: Vec<&str> = text.split('\n').collect();
        let (left, right) = split_sides(&lines, &rows);
        let title = self.title();
        let hint = self.language_hint.clone();
        let revert_rows = self.revert_rows.clone();
        let Some(split) = self.split.as_mut() else {
            return;
        };
        for (editor, side) in [(&split.left, &left), (&split.right, &right)] {
            // Revert buttons on the hunk headers that have one inline.
            let reverts: Vec<usize> = side
                .rows
                .iter()
                .enumerate()
                .filter(|(ix, row)| {
                    row.kind == DiffRowKind::HunkHeader
                        && revert_rows.binary_search(&side.map[*ix]).is_ok()
                })
                .map(|(ix, _)| ix)
                .collect();
            editor.update(cx, |editor, cx| {
                editor.replace_diff(
                    title.clone(),
                    &side.text,
                    side.rows.clone(),
                    hint.clone(),
                    cx,
                );
                editor.set_revert_rows(reverts, cx);
            });
        }
        split.left_map = left.map;
        split.right_map = right.map;
    }

    /// Puts each side's cursor on the row showing the inline cursor's row.
    pub(crate) fn sync_split_cursor(&mut self, cx: &mut Context<Self>) {
        let row = self.cursor_row();
        let Some(split) = &self.split else {
            return;
        };
        for (editor, map) in [
            (&split.left, &split.left_map),
            (&split.right, &split.right_map),
        ] {
            let side_row = map.iter().position(|&inline| inline >= row).unwrap_or(0);
            editor.update(cx, |editor, cx| {
                editor.select_range((side_row, 0), (side_row, 0), cx)
            });
        }
    }

    /// Passes a side's double-click or Revert on, as the inline row's.
    fn side_event(&mut self, left: bool, event: &EditorEvent, cx: &mut Context<Self>) {
        let Some(split) = &self.split else {
            return;
        };
        let map = if left {
            &split.left_map
        } else {
            &split.right_map
        };
        match event {
            EditorEvent::DiffRowActivated(row) => {
                if let Some(&inline) = map.get(*row) {
                    cx.emit(EditorEvent::DiffRowActivated(inline));
                }
            }
            EditorEvent::RevertDiffHunk(row) => {
                if let Some(&inline) = map.get(*row) {
                    cx.emit(EditorEvent::RevertDiffHunk(inline));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn toggle_diff_layout(
        &mut self,
        _: &crate::ToggleDiffLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.diff_side || self.diff_rows.is_none() {
            // A side passes it to the view it belongs to.
            cx.propagate();
            return;
        }
        let on = self.split.is_none();
        self.set_split_diff(on, window, cx);
    }

    pub(crate) fn render_split(&self, split: &SplitDiff) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_r_1()
                    .border_color(theme::border())
                    .child(split.left.clone()),
            )
            .child(div().flex_1().min_w_0().h_full().child(split.right.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: DiffRowKind, old: Option<u32>, new: Option<u32>) -> DiffRow {
        DiffRow {
            kind,
            old_line: old,
            new_line: new,
        }
    }

    #[test]
    fn pairs_removed_with_added_lines() {
        use DiffRowKind::*;
        let lines = ["@@", "a", "b", "c", "B", "C", "D", "e"];
        let rows = [
            row(HunkHeader, None, None),
            row(Context, Some(1), Some(1)),
            row(Removed, Some(2), None),
            row(Removed, Some(3), None),
            row(Added, None, Some(2)),
            row(Added, None, Some(3)),
            row(Added, None, Some(4)),
            row(Context, Some(4), Some(5)),
        ];
        let (left, right) = split_sides(&lines, &rows);
        assert_eq!(left.text, "@@\na\nb\nc\n\ne");
        assert_eq!(right.text, "@@\na\nB\nC\nD\ne");
        assert_eq!(left.rows[4].kind, Filler);
        assert_eq!(left.map, [0, 1, 2, 3, 6, 7]);
        assert_eq!(right.map, [0, 1, 4, 5, 6, 7]);
        // Each side numbers only its own lines.
        assert_eq!(
            (left.rows[1].old_line, left.rows[1].new_line),
            (Some(1), None)
        );
        assert_eq!(
            (right.rows[5].old_line, right.rows[5].new_line),
            (None, Some(5))
        );
    }
}
