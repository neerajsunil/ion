//! The custom element that draws an [`Editor`]: gutter, text, selection and cursor.
//!
//! Only the lines inside the viewport are shaped and painted, so cost per frame
//! depends on window size, not file size.

use std::ops::Range;

use git::LineChange;
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels,
    ShapedLine, Style, TextRun, Window, fill, font, point, px, relative, size,
};

use crate::editor::DiffRowKind;
use crate::highlighting::line_runs;
use crate::layout::{DisplayLine, EditorLayout, VisibleLine};
use crate::{DiffRow, Editor};

const GUTTER_PADDING_LEFT: f32 = 16.;
pub(crate) const GUTTER_PADDING_RIGHT: f32 = 20.;
const FOLD_ICON_SIZE: f32 = 13.;
const MIN_GUTTER_DIGITS: usize = 3;
const CURSOR_WIDTH: f32 = 2.;
/// Space between the old and new line number columns of a diff.
const DIFF_COLUMN_GAP: f32 = 12.;
/// Change markers at the left edge of the gutter.
const MARKER_LEFT: f32 = 4.;
const MARKER_WIDTH: f32 = 3.;
/// Left padding of single-line inputs (they have no gutter).
const INPUT_PADDING_LEFT: f32 = 8.;

pub(crate) struct EditorElement {
    editor: Entity<Editor>,
}

impl EditorElement {
    pub fn new(editor: Entity<Editor>) -> Self {
        Self { editor }
    }
}

pub(crate) struct PrepaintState {
    layout: Option<EditorLayout>,
    gutter_numbers: Vec<(Pixels, Pixels, ShapedLine)>,
    /// Diff line backgrounds and git change markers, drawn under the text.
    row_backgrounds: Vec<PaintQuad>,
    git_markers: Vec<PaintQuad>,
    text_area: Bounds<Pixels>,
    line_origin_x: Pixels,
    top_offset: Pixels,
    current_line: Option<PaintQuad>,
    search_matches: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    /// Wavy underlines under problems: origin, width, color.
    underlines: Vec<(gpui::Point<Pixels>, Pixels, Hsla)>,
    /// Problem messages drawn after the end of their line.
    messages: Vec<(gpui::Point<Pixels>, ShapedLine)>,
    /// Fold chevrons in the gutter: bounds and icon path.
    fold_icons: Vec<(Bounds<Pixels>, &'static str)>,
    /// Backgrounds of the "⋯" after folded lines.
    fold_markers: Vec<PaintQuad>,
    /// Placeholder text and its origin, for empty inputs.
    placeholder: Option<(gpui::Point<Pixels>, ShapedLine)>,
    cursors: Vec<PaintQuad>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn plain_run(len: usize, color: Hsla, bold: bool) -> TextRun {
    let mut run_font = font(theme::mono_font());
    if bold {
        run_font.weight = FontWeight::BOLD;
    }
    TextRun {
        len,
        font: run_font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

fn shape(window: &Window, text: String, color: Hsla) -> ShapedLine {
    let run = plain_run(text.len(), color, false);
    window
        .text_system()
        .shape_line(text.into(), theme::editor_font_size(), &[run], None)
}

/// Widest line number in a diff, in digits.
fn diff_digits(rows: &[DiffRow]) -> usize {
    let max = rows
        .iter()
        .filter_map(|row| row.old_line.max(row.new_line))
        .max()
        .unwrap_or(0);
    max.to_string().len().max(MIN_GUTTER_DIGITS)
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let line_height = theme::editor_line_height();
        let settings = settings::get(cx);
        let tab_width = settings.tab_size();
        let show_git_markers = settings.git_gutter;

        // Gutter wide enough for the largest line number.
        let digit_width = shape(window, "0".into(), theme::text()).width;
        let editor = self.editor.read(cx);
        let single_line = editor.single_line;
        // Multi-line inputs (commit message) have no gutter either.
        let input = single_line || editor.input;
        let diff_rows = editor.diff_rows.clone();
        let wraps = editor.wraps(cx);
        let digits = match &diff_rows {
            Some(rows) => diff_digits(rows),
            None => editor
                .buffer
                .line_count()
                .to_string()
                .len()
                .max(MIN_GUTTER_DIGITS),
        };
        let gutter_width = if input {
            px(INPUT_PADDING_LEFT)
        } else if diff_rows.is_some() && editor.diff_side {
            digit_width * digits as f32 + px(GUTTER_PADDING_LEFT + GUTTER_PADDING_RIGHT)
        } else if diff_rows.is_some() {
            digit_width * (digits * 2) as f32
                + px(GUTTER_PADDING_LEFT + DIFF_COLUMN_GAP + GUTTER_PADDING_RIGHT)
        } else {
            digit_width * digits as f32 + px(GUTTER_PADDING_LEFT + GUTTER_PADDING_RIGHT)
        };
        let text_left = bounds.left() + gutter_width;
        let text_area = Bounds::from_corners(point(text_left, bounds.top()), bounds.bottom_right());
        let viewport_width = text_area.size.width;
        let viewport_height = bounds.size.height;

        // Soft wrap at the right edge, leaving room for the cursor.
        let wrap_cols = ((viewport_width / digit_width).floor() as usize).saturating_sub(1);
        self.editor.update(cx, |editor, _| {
            let cols = wraps.then_some(wrap_cols);
            editor.wrap.update(&editor.buffer, cols, tab_width);
        });
        let editor = self.editor.read(cx);
        let buffer = &editor.buffer;
        let wrap = &editor.wrap;
        let wrapping = wrap.wraps_lines();
        let can_fold = editor.can_fold();
        let line_count = buffer.line_count();
        let display_rows = wrap.row_count(line_count);

        // Resolve scrolling: keep the cursor visible after edits, then clamp.
        let mut scroll = editor.scroll;
        let (cursor_row, cursor_col) = buffer.row_col(buffer.cursor());
        if editor.autoscroll {
            let cursor_top = line_height * wrap.display_row(cursor_row, cursor_col) as f32;
            if cursor_top < scroll.y {
                scroll.y = cursor_top;
            } else if cursor_top + line_height > scroll.y + viewport_height {
                scroll.y = cursor_top + line_height - viewport_height;
            }
            if !wrapping {
                let display = if editor.secret {
                    DisplayLine::masked(&buffer.line_text(cursor_row))
                } else {
                    DisplayLine::new(&buffer.line_text(cursor_row), tab_width)
                };
                let shaped = shape(window, display.text.clone(), theme::text());
                let cursor_x = shaped.x_for_index(display.byte_for_col(cursor_col));
                let margin = digit_width * 4.;
                if cursor_x < scroll.x + margin {
                    scroll.x = cursor_x - margin;
                } else if cursor_x > scroll.x + viewport_width - margin {
                    scroll.x = cursor_x - viewport_width + margin;
                }
            }
        }
        // Allow scrolling until the last line reaches the top of the viewport.
        let max_scroll_y = line_height * display_rows.saturating_sub(1) as f32;
        scroll.y = scroll.y.clamp(px(0.), max_scroll_y);
        scroll.x = if wrapping {
            px(0.)
        } else {
            scroll.x.max(px(0.))
        };

        let first_display_row = (scroll.y / line_height).floor() as usize;
        let rows_in_view = (viewport_height / line_height).ceil() as usize + 1;
        let last_display_row = (first_display_row + rows_in_view).min(display_rows);
        // Inputs center their one line vertically.
        let top_offset = if single_line {
            (viewport_height - line_height) / 2.
        } else {
            px(0.)
        };
        let row_top = |display_row: usize| {
            bounds.top() + top_offset + line_height * display_row as f32 - scroll.y
        };
        let line_origin_x = text_left - scroll.x;

        let first_row = wrap.segment(first_display_row).0;
        let last_row = if last_display_row > first_display_row {
            wrap.segment(last_display_row - 1).0 + 1
        } else {
            first_row
        };
        let highlights = editor.highlights_for_rows(first_row..last_row);
        let mut highlights = highlights.as_slice();
        let rope = buffer.rope();
        let mut lines = Vec::with_capacity(last_display_row - first_display_row);
        let mut gutter_numbers = Vec::with_capacity(last_row - first_row);
        let mut row_backgrounds = Vec::new();
        let mut fold_icons = Vec::new();
        // The current buffer line: its text and highlight spans (in columns).
        let mut current: Option<(usize, Vec<char>)> = None;
        let mut row_spans = Vec::new();
        let mut segment_spans = Vec::new();
        let old_column_right =
            bounds.left() + px(GUTTER_PADDING_LEFT) + digit_width * digits as f32;
        let new_column_right = old_column_right + px(DIFF_COLUMN_GAP) + digit_width * digits as f32;
        for display_row in first_display_row..last_display_row {
            let (row, start_col, end_col) = wrap.segment(display_row);
            if current.as_ref().is_none_or(|(current, _)| *current != row) {
                current = Some((row, buffer.line_text(row).chars().collect()));
                // Highlight spans on this line, converted from bytes to columns.
                let line_start_byte = rope.line_to_byte(row);
                let line_end_byte =
                    rope.char_to_byte(buffer.line_start(row) + buffer.line_len(row));
                let line_start_char = buffer.line_start(row);
                row_spans.clear();
                while let Some((range, highlight)) = highlights.first() {
                    if range.start >= line_end_byte {
                        break;
                    }
                    let start = range.start.max(line_start_byte);
                    let end = range.end.min(line_end_byte);
                    if start < end {
                        let col = |byte: usize| rope.byte_to_char(byte) - line_start_char;
                        row_spans.push((col(start)..col(end), *highlight));
                    }
                    if range.end > line_end_byte {
                        break;
                    }
                    highlights = &highlights[1..];
                }
            }
            let chars = current
                .as_ref()
                .map_or(&[][..], |(_, chars)| chars.as_slice());
            let segment_end = end_col.unwrap_or(chars.len()).min(chars.len());
            let text: String = chars[start_col.min(segment_end)..segment_end]
                .iter()
                .collect();
            let display = if editor.secret {
                DisplayLine::masked(&text)
            } else {
                DisplayLine::new(&text, tab_width)
            };
            segment_spans.clear();
            for (range, highlight) in &row_spans {
                let start = range.start.max(start_col);
                let end = range.end.min(segment_end);
                if start < end {
                    segment_spans.push((start - start_col..end - start_col, *highlight));
                }
            }

            let diff_row = diff_rows.as_ref().and_then(|rows| rows.get(row));
            let len = display.text.len();
            let runs = match diff_row.map(|diff_row| diff_row.kind) {
                Some(DiffRowKind::FileHeader) => vec![plain_run(len, theme::text(), true)],
                Some(DiffRowKind::HunkHeader) => vec![plain_run(len, theme::text_muted(), false)],
                Some(DiffRowKind::Meta) => vec![plain_run(len, theme::text_muted(), false)],
                _ => line_runs(&display, &segment_spans),
            };
            if let Some(diff_row) = diff_row {
                let background = match diff_row.kind {
                    DiffRowKind::FileHeader => Some(theme::elevated_bg()),
                    DiffRowKind::HunkHeader => Some(theme::diff_hunk_bg()),
                    DiffRowKind::Added => Some(theme::diff_added_bg()),
                    DiffRowKind::Removed => Some(theme::diff_removed_bg()),
                    DiffRowKind::Filler => Some(theme::panel_bg()),
                    DiffRowKind::Meta | DiffRowKind::Context => None,
                };
                if let Some(background) = background {
                    row_backgrounds.push(fill(
                        Bounds::new(
                            point(bounds.left(), row_top(display_row)),
                            size(bounds.size.width, line_height),
                        ),
                        background,
                    ));
                }
            }
            let shaped = window.text_system().shape_line(
                display.text.clone().into(),
                theme::editor_font_size(),
                &runs,
                None,
            );
            let color = if row == cursor_row {
                theme::text_muted()
            } else {
                theme::text_faint()
            };
            if let Some(diff_row) = diff_row {
                // A side of a side-by-side diff has one column, for its side.
                let new_right = if editor.diff_side {
                    old_column_right
                } else {
                    new_column_right
                };
                for (line, right) in [
                    (diff_row.old_line, old_column_right),
                    (diff_row.new_line, new_right),
                ] {
                    if let Some(line) = line {
                        let number = shape(window, line.to_string(), color);
                        gutter_numbers.push((right - number.width, row_top(display_row), number));
                    }
                }
            } else if !input && start_col == 0 {
                let number = shape(window, (row + 1).to_string(), color);
                let number_x = text_left - px(GUTTER_PADDING_RIGHT) - number.width;
                gutter_numbers.push((number_x, row_top(display_row), number));
                // Fold chevrons: always on folded lines, on foldable ones
                // while the mouse is over the gutter.
                let folded = can_fold && buffer.is_folded(row);
                if folded || (can_fold && editor.gutter_hovered && buffer.is_foldable(row)) {
                    let icon_size = px(FOLD_ICON_SIZE);
                    let icon = if folded {
                        ui::IconName::ChevronRight
                    } else {
                        ui::IconName::ChevronDown
                    };
                    fold_icons.push((
                        Bounds::new(
                            point(
                                text_left - px(GUTTER_PADDING_RIGHT) + px(3.),
                                row_top(display_row) + (line_height - icon_size) / 2.,
                            ),
                            size(icon_size, icon_size),
                        ),
                        icon.path(),
                    ));
                }
            }
            lines.push(VisibleLine {
                row,
                display_row,
                start_col,
                end_col,
                display,
                shaped,
            });
        }

        // Changes since the last commit, as bars at the gutter's left edge.
        let mut git_markers = Vec::new();
        let hunks = if show_git_markers {
            editor.git_hunks_in(first_row..last_row)
        } else {
            &[]
        };
        for hunk in hunks {
            let marker_left = bounds.left() + px(MARKER_LEFT);
            let start = wrap.row_start(hunk.new.start as usize);
            let (top, height, color) = match hunk.change() {
                LineChange::DeletedAbove => (row_top(start) - px(2.), px(4.), theme::git_deleted()),
                change => {
                    let rows = wrap.row_start(hunk.new.end as usize) - start;
                    let color = if change == LineChange::Added {
                        theme::git_added()
                    } else {
                        theme::git_changed_marker()
                    };
                    (row_top(start), line_height * rows as f32, color)
                }
            };
            git_markers.push(fill(
                Bounds::new(point(marker_left, top), size(px(MARKER_WIDTH), height)),
                color,
            ));
        }

        // Folded lines end in a "⋯" marker.
        let mut messages = Vec::new();
        let mut fold_markers = Vec::new();
        if can_fold {
            for line in lines
                .iter()
                .filter(|line| line.end_col.is_none() && buffer.is_folded(line.row))
            {
                let text = shape(window, "⋯".to_owned(), theme::text_muted());
                let x = line_origin_x + line.shaped.width + digit_width;
                let top = row_top(line.display_row);
                let width = text.width + digit_width;
                fold_markers.push(fill(
                    Bounds::new(point(x, top + px(3.)), size(width, line_height - px(6.))),
                    theme::hover_bg(),
                ));
                messages.push((point(x + digit_width / 2., top), text));
            }
        }
        let folded_extra = |row: usize| {
            if can_fold && buffer.is_folded(row) {
                digit_width * 3.
            } else {
                px(0.)
            }
        };

        // Problems: underline the word, and show the message after the line.
        let mut underlines = Vec::new();
        for diagnostic in editor.diagnostics_in(first_row..last_row) {
            let color = diagnostic.severity.color();
            let chars: Vec<char> = buffer.line_text(diagnostic.row).chars().collect();
            if let Some((from, to)) = crate::diagnostics::underline_cols(&chars, diagnostic.column)
            {
                for line in lines.iter().filter(|line| line.row == diagnostic.row) {
                    let segment_end = line.end_col.unwrap_or(chars.len());
                    let (from, to) = (from.max(line.start_col), to.min(segment_end));
                    if from >= to {
                        continue;
                    }
                    let x = line_origin_x + line.x_for_col(from);
                    let width = line.x_for_col(to) - line.x_for_col(from);
                    let y = row_top(line.display_row) + line_height - px(3.);
                    underlines.push((point(x, y), width, color));
                }
            }
            let last = lines
                .iter()
                .rev()
                .find(|line| line.row == diagnostic.row && line.end_col.is_none());
            if let Some(line) = last {
                let message: String = diagnostic.message.lines().next().unwrap_or_default().into();
                let text = shape(window, format!("● {message}"), color.opacity(0.75));
                let x =
                    line_origin_x + line.shaped.width + digit_width * 3. + folded_extra(line.row);
                messages.push((point(x, row_top(line.display_row)), text));
            }
        }

        // The part of each drawn segment that a char range covers, as
        // (line, from col, to col, continues onto the next line).
        let spans_of = |range: Range<usize>| {
            let (start_row, start_col) = buffer.row_col(range.start);
            let (end_row, end_col) = buffer.row_col(range.end);
            lines.iter().filter_map(move |line| {
                if line.row < start_row || line.row > end_row {
                    return None;
                }
                let line_end = buffer.line_len(line.row);
                let segment_end = line.end_col.unwrap_or(line_end);
                let from = if line.row == start_row { start_col } else { 0 };
                let to = if line.row == end_row {
                    end_col
                } else {
                    line_end
                };
                let (from, to) = (from.max(line.start_col), to.min(segment_end));
                if from > to || (from == to && line.end_col.is_some()) {
                    return None;
                }
                let breaks = line.row < end_row && line.end_col.is_none();
                Some((line, from, to, breaks))
            })
        };

        // Find matches, drawn under the selection.
        let mut search_matches = Vec::new();
        if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
            let visible = buffer.line_start(first.row)
                ..buffer.line_start(last.row) + buffer.line_len(last.row) + 1;
            for range in editor.search_matches_in(visible) {
                for (line, from, to, _) in spans_of(range.clone()) {
                    let top = row_top(line.display_row);
                    search_matches.push(fill(
                        Bounds::from_corners(
                            point(line_origin_x + line.x_for_col(from), top),
                            point(line_origin_x + line.x_for_col(to), top + line_height),
                        ),
                        theme::search_match(),
                    ));
                }
            }
        }

        // Selection highlight, one quad per visible segment it covers.
        let mut selections = Vec::new();
        let selection = buffer.selection.range();
        let all_selections = buffer.selections();
        for range in all_selections
            .iter()
            .map(|s| s.range())
            .filter(|r| !r.is_empty())
        {
            for (line, from, to, breaks) in spans_of(range) {
                let x0 = line.x_for_col(from);
                let mut x1 = line.x_for_col(to);
                if breaks {
                    // Show the selected line break.
                    x1 += digit_width * 0.6;
                }
                if x1 > x0 {
                    let top = row_top(line.display_row);
                    selections.push(fill(
                        Bounds::from_corners(
                            point(line_origin_x + x0, top),
                            point(line_origin_x + x1, top + line_height),
                        ),
                        theme::selection(),
                    ));
                }
            }
        }

        let cursor_line = lines
            .iter()
            .find(|line| line.row == cursor_row && line.contains_col(cursor_col));
        let mut cursors = Vec::new();
        for selection in &all_selections {
            let (row, col) = buffer.row_col(selection.head);
            let line = lines
                .iter()
                .find(|line| line.row == row && line.contains_col(col));
            if let Some(line) = line {
                cursors.push(fill(
                    Bounds::new(
                        point(
                            line_origin_x + line.x_for_col(col),
                            row_top(line.display_row),
                        ),
                        size(px(CURSOR_WIDTH), line_height),
                    ),
                    theme::accent(),
                ));
            }
        }
        let current_line = cursor_line
            .filter(|_| !input && selection.is_empty() && all_selections.len() == 1)
            .map(|line| {
                fill(
                    Bounds::new(
                        point(bounds.left(), row_top(line.display_row)),
                        size(bounds.size.width, line_height),
                    ),
                    theme::current_line(),
                )
            });

        let placeholder =
            (input && buffer.is_empty() && !editor.placeholder.is_empty()).then(|| {
                let text = shape(window, editor.placeholder.to_string(), theme::text_faint());
                (point(line_origin_x, row_top(0)), text)
            });

        let layout = EditorLayout {
            bounds,
            text_left,
            line_height,
            lines,
        };
        let partner = self.editor.update(cx, |editor, _| {
            editor.scroll = scroll;
            editor.autoscroll = false;
            editor.scroll_partner.clone()
        });
        // The other side of a side-by-side diff follows (a frame later).
        if let Some(partner) = partner.and_then(|partner| partner.upgrade()) {
            partner.update(cx, |partner, cx| {
                if partner.scroll.y != scroll.y {
                    partner.scroll.y = scroll.y;
                    partner.autoscroll = false;
                    cx.notify();
                }
            });
        }

        PrepaintState {
            layout: Some(layout),
            gutter_numbers,
            row_backgrounds,
            git_markers,
            text_area,
            line_origin_x,
            top_offset,
            current_line,
            search_matches,
            selections,
            underlines,
            messages,
            fold_icons,
            fold_markers,
            placeholder,
            cursors,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        let line_height = theme::editor_line_height();

        for quad in prepaint.row_backgrounds.drain(..) {
            window.paint_quad(quad);
        }
        if focused && let Some(current_line) = prepaint.current_line.take() {
            window.paint_quad(current_line);
        }
        for quad in prepaint.git_markers.drain(..) {
            window.paint_quad(quad);
        }
        for (x, y, number) in &prepaint.gutter_numbers {
            number.paint(point(*x, *y), line_height, window, cx).ok();
        }
        for (bounds, path) in prepaint.fold_icons.drain(..) {
            window
                .paint_svg(
                    bounds,
                    path.into(),
                    gpui::TransformationMatrix::unit(),
                    theme::text_muted(),
                    cx,
                )
                .ok();
        }

        let layout = prepaint
            .layout
            .take()
            .expect("prepaint always builds a layout");
        window.with_content_mask(
            Some(ContentMask {
                bounds: prepaint.text_area,
            }),
            |window| {
                for quad in prepaint.search_matches.drain(..) {
                    window.paint_quad(quad);
                }
                for selection in prepaint.selections.drain(..) {
                    window.paint_quad(selection);
                }
                if let Some((origin, placeholder)) = &prepaint.placeholder {
                    placeholder.paint(*origin, line_height, window, cx).ok();
                }
                let scroll_y = self.editor.read(cx).scroll.y;
                for line in &layout.lines {
                    let y =
                        bounds.top() + prepaint.top_offset + line_height * line.display_row as f32
                            - scroll_y;
                    line.shaped
                        .paint(point(prepaint.line_origin_x, y), line_height, window, cx)
                        .ok();
                }
                for quad in prepaint.fold_markers.drain(..) {
                    window.paint_quad(quad);
                }
                for (origin, width, color) in prepaint.underlines.drain(..) {
                    let style = gpui::UnderlineStyle {
                        thickness: px(1.),
                        color: Some(color),
                        wavy: true,
                    };
                    window.paint_underline(origin, width, &style);
                }
                for (origin, message) in &prepaint.messages {
                    message.paint(*origin, line_height, window, cx).ok();
                }
                if focused {
                    for cursor in prepaint.cursors.drain(..) {
                        window.paint_quad(cursor);
                    }
                }
            },
        );

        self.editor
            .update(cx, |editor, _| editor.layout = Some(layout));
    }
}
