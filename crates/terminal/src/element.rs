//! Draws a terminal grid: cell backgrounds, text, selection and cursor.
//!
//! Text is shaped in runs of identically styled cells and each run is placed at
//! its exact cell position, so columns stay aligned even when a fallback font
//! has slightly different glyph widths.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, ElementInputHandler, Entity, FontStyle,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PaintQuad,
    Pixels, Point, ShapedLine, StrikethroughStyle, Style, TextRun, UnderlineStyle, Window, fill,
    font, outline, point, px, relative, size,
};

use crate::TerminalView;
use crate::colors::{dim, resolve, to_hsla};
use crate::pty::GridSize;

pub(crate) const PADDING_X: f32 = 10.;
pub(crate) const PADDING_Y: f32 = 6.;

/// Geometry of the last frame, used to map mouse positions to cells.
#[derive(Clone, Copy)]
pub(crate) struct GridLayout {
    pub origin: Point<Pixels>,
    pub cell: gpui::Size<Pixels>,
    pub cursor: Option<Bounds<Pixels>>,
}

#[derive(Clone, Copy, PartialEq)]
struct RunStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
}

struct TextRunData {
    row: usize,
    col: usize,
    /// Columns covered so far; runs only grow over narrow cells.
    end_col: usize,
    text: String,
    style: RunStyle,
}

struct BackgroundRect {
    row: usize,
    col: usize,
    end_col: usize,
    color: Hsla,
}

struct CursorData {
    row: usize,
    col: usize,
    width: usize,
    shape: CursorShape,
    ch: char,
    style: RunStyle,
}

/// Shaped text and where to draw it.
type PositionedLine = (Point<Pixels>, ShapedLine);

pub(crate) struct PrepaintState {
    backgrounds: Vec<PaintQuad>,
    text: Vec<PositionedLine>,
    /// The cursor shape, plus the glyph under a block cursor.
    cursor: Option<(PaintQuad, Option<PositionedLine>)>,
}

pub(crate) struct TerminalElement {
    view: Entity<TerminalView>,
    focused: bool,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>, focused: bool) -> Self {
        Self { view, focused }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn shape(window: &Window, text: String, style: RunStyle) -> ShapedLine {
    let mut run_font = font(theme::mono_font());
    if style.bold {
        run_font.weight = FontWeight::BOLD;
    }
    if style.italic {
        run_font.style = FontStyle::Italic;
    }
    let run = TextRun {
        len: text.len(),
        font: run_font,
        color: style.fg,
        background_color: None,
        underline: style.underline.then_some(UnderlineStyle {
            color: Some(style.fg),
            thickness: px(1.),
            wavy: false,
        }),
        strikethrough: style.strikeout.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(style.fg),
        }),
    };
    window
        .text_system()
        .shape_line(text.into(), theme::terminal_font_size(), &[run], None)
}

impl Element for TerminalElement {
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
        let plain = RunStyle {
            fg: theme::text(),
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
        };
        let cell_width = shape(window, "M".into(), plain).width;
        let cell_height = theme::terminal_line_height();
        let origin = point(bounds.left() + px(PADDING_X), bounds.top() + px(PADDING_Y));
        let cols = ((bounds.size.width - px(2. * PADDING_X)) / cell_width)
            .floor()
            .max(2.) as usize;
        let rows = ((bounds.size.height - px(2. * PADDING_Y)) / cell_height)
            .floor()
            .max(1.) as usize;
        let grid_size = GridSize {
            cols,
            rows,
            cell_width: f32::from(cell_width) as u16,
            cell_height: f32::from(cell_height) as u16,
        };
        self.view.update(cx, |view, _| view.resize(grid_size));

        let view = self.view.read(cx);
        let Some(term) = view.term() else {
            return PrepaintState {
                backgrounds: Vec::new(),
                text: Vec::new(),
                cursor: None,
            };
        };

        // Collect everything under the lock, then shape after releasing it so
        // the PTY thread is never blocked on text shaping.
        let mut runs: Vec<TextRunData> = Vec::new();
        let mut backgrounds: Vec<BackgroundRect> = Vec::new();
        let cursor_data;
        {
            let term = term.lock();
            let content = term.renderable_content();
            let display_offset = content.display_offset as i32;
            let palette = content.colors;
            let selection = content.selection;
            let screen_lines = term.screen_lines();

            for indexed in content.display_iter {
                let cell = indexed.cell;
                let flags = cell.flags;
                if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let row = (indexed.point.line.0 + display_offset) as usize;
                let col = indexed.point.column.0;
                let width = if flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };

                let mut fg = resolve(cell.fg, palette);
                let mut bg = resolve(cell.bg, palette);
                if flags.contains(Flags::DIM) {
                    fg = dim(fg);
                }
                let inverse = flags.contains(Flags::INVERSE);
                if inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if flags.contains(Flags::HIDDEN) {
                    fg = bg;
                }

                let selected = selection.is_some_and(|range| range.contains(indexed.point));
                let background = if selected {
                    Some(theme::selection())
                } else if inverse || cell.bg != Color::Named(NamedColor::Background) {
                    Some(to_hsla(bg))
                } else {
                    None
                };
                if let Some(color) = background {
                    match backgrounds.last_mut() {
                        Some(last)
                            if last.row == row && last.end_col == col && last.color == color =>
                        {
                            last.end_col = col + width;
                        }
                        _ => backgrounds.push(BackgroundRect {
                            row,
                            col,
                            end_col: col + width,
                            color,
                        }),
                    }
                }

                let style = RunStyle {
                    fg: to_hsla(fg),
                    bold: flags.contains(Flags::BOLD),
                    italic: flags.contains(Flags::ITALIC),
                    underline: flags.intersects(Flags::ALL_UNDERLINES),
                    strikeout: flags.contains(Flags::STRIKEOUT),
                };
                if cell.c == ' ' && !style.underline && !style.strikeout {
                    continue;
                }
                let extends_last = width == 1
                    && runs.last().is_some_and(|last| {
                        last.row == row && last.end_col == col && last.style == style
                    });
                if extends_last {
                    let last = runs.last_mut().expect("checked above");
                    last.text.push(cell.c);
                    last.end_col += 1;
                } else {
                    runs.push(TextRunData {
                        row,
                        col,
                        // Wide chars get their own run so the next cell starts fresh.
                        end_col: if width == 1 { col + 1 } else { usize::MAX },
                        text: cell.c.to_string(),
                        style,
                    });
                }
                if let Some(zero_width) = cell.zerowidth() {
                    runs.last_mut()
                        .expect("just pushed")
                        .text
                        .extend(zero_width);
                }
            }

            let cursor = content.cursor;
            let cursor_row = cursor.point.line.0 + display_offset;
            cursor_data = (cursor.shape != CursorShape::Hidden
                && (0..screen_lines as i32).contains(&cursor_row))
            .then(|| {
                let cell = &term.grid()[cursor.point];
                CursorData {
                    row: cursor_row as usize,
                    col: cursor.point.column.0,
                    width: if cell.flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    },
                    shape: cursor.shape,
                    ch: cell.c,
                    style: RunStyle {
                        fg: theme::bg(),
                        bold: cell.flags.contains(Flags::BOLD),
                        ..plain
                    },
                }
            });
        }

        let cell_origin = |row: usize, col: usize| {
            point(
                origin.x + cell_width * col as f32,
                origin.y + cell_height * row as f32,
            )
        };
        let background_quads = backgrounds
            .into_iter()
            .map(|rect| {
                fill(
                    Bounds::new(
                        cell_origin(rect.row, rect.col),
                        size(cell_width * (rect.end_col - rect.col) as f32, cell_height),
                    ),
                    rect.color,
                )
            })
            .collect();
        let text = runs
            .into_iter()
            .map(|run| {
                (
                    cell_origin(run.row, run.col),
                    shape(window, run.text, run.style),
                )
            })
            .collect();

        let mut cursor_bounds = None;
        let cursor = cursor_data.map(|cursor| {
            let top_left = cell_origin(cursor.row, cursor.col);
            let block = Bounds::new(
                top_left,
                size(cell_width * cursor.width as f32, cell_height),
            );
            cursor_bounds = Some(block);
            let accent = theme::accent();
            match cursor.shape {
                CursorShape::Block if self.focused => {
                    let glyph = (cursor.ch != ' ')
                        .then(|| (top_left, shape(window, cursor.ch.to_string(), cursor.style)));
                    (fill(block, accent), glyph)
                }
                CursorShape::Beam => (
                    fill(Bounds::new(top_left, size(px(2.), cell_height)), accent),
                    None,
                ),
                CursorShape::Underline => (
                    fill(
                        Bounds::new(
                            point(top_left.x, top_left.y + cell_height - px(2.)),
                            size(block.size.width, px(2.)),
                        ),
                        accent,
                    ),
                    None,
                ),
                _ => (outline(block, accent, BorderStyle::Solid), None),
            }
        });

        let layout = GridLayout {
            origin,
            cell: size(cell_width, cell_height),
            cursor: cursor_bounds,
        };
        self.view.update(cx, |view, _| view.layout = Some(layout));

        PrepaintState {
            backgrounds: background_quads,
            text,
            cursor,
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
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let line_height = theme::terminal_line_height();
        for quad in prepaint.backgrounds.drain(..) {
            window.paint_quad(quad);
        }
        for (origin, line) in &prepaint.text {
            line.paint(*origin, line_height, window, cx).ok();
        }
        if let Some((quad, glyph)) = prepaint.cursor.take() {
            window.paint_quad(quad);
            if let Some((origin, line)) = glyph {
                line.paint(origin, line_height, window, cx).ok();
            }
        }
    }
}
