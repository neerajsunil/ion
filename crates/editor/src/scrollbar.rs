//! Overlay scrollbars and the horizontal scroll limit.
//!
//! The bars are drawn over the text and only while they're useful: for a
//! moment after the wheel scrolls, while the mouse is over a bar's track (at
//! the right or bottom edge), and while one is dragged.

use std::time::Duration;

use gpui::{Bounds, Context, Pixels, Point, Task, point, px, size};
use text::Buffer;

use crate::Editor;
use crate::layout::MAX_DRAWN_CHARS;

/// Thickness of a bar's track.
pub(crate) const SCROLLBAR_WIDTH: f32 = 12.;
/// Space between the thumb and the track edges.
const THUMB_INSET: f32 = 3.;
const MIN_THUMB_LENGTH: f32 = 24.;
/// How long the bars stay after the last wheel scroll.
const HIDE_DELAY: Duration = Duration::from_millis(1200);
/// Files with more lines than this aren't measured on every edit; they
/// scroll as far as the widest line drawn so far.
const MAX_MEASURED_LINES: usize = 50_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

/// One bar's geometry in the last frame.
pub(crate) struct ScrollbarLayout {
    pub axis: Axis,
    pub track: Bounds<Pixels>,
    pub thumb: Bounds<Pixels>,
    max_scroll: Pixels,
}

impl ScrollbarLayout {
    /// A bar for `viewport` pixels of content scrolled by `scroll` out of
    /// `max_scroll`, or `None` when everything fits.
    pub fn new(
        axis: Axis,
        track: Bounds<Pixels>,
        viewport: Pixels,
        scroll: Pixels,
        max_scroll: Pixels,
    ) -> Option<Self> {
        if max_scroll <= px(0.) {
            return None;
        }
        let track_length = along(axis, track.size);
        let length = (track_length * (viewport / (viewport + max_scroll)))
            .max(px(MIN_THUMB_LENGTH))
            .min(track_length);
        let start = (track_length - length) * (scroll / max_scroll).clamp(0., 1.);
        let inset = px(THUMB_INSET);
        let thumb = match axis {
            Axis::Vertical => Bounds::new(
                point(track.left() + inset, track.top() + start),
                size(track.size.width - inset * 2., length),
            ),
            Axis::Horizontal => Bounds::new(
                point(track.left() + start, track.top() + inset),
                size(length, track.size.height - inset * 2.),
            ),
        };
        Some(Self {
            axis,
            track,
            thumb,
            max_scroll,
        })
    }

    /// The scroll offset that puts the thumb's start `offset` pixels into
    /// the track.
    fn scroll_for_thumb_start(&self, offset: Pixels) -> Pixels {
        let room = along(self.axis, self.track.size) - along(self.axis, self.thumb.size);
        if room <= px(0.) {
            return px(0.);
        }
        self.max_scroll * (offset / room).clamp(0., 1.)
    }

    /// Distance of `position` from the start of the track, along the bar.
    fn offset_in_track(&self, position: Point<Pixels>) -> Pixels {
        match self.axis {
            Axis::Vertical => position.y - self.track.top(),
            Axis::Horizontal => position.x - self.track.left(),
        }
    }
}

fn along(axis: Axis, size: gpui::Size<Pixels>) -> Pixels {
    match axis {
        Axis::Vertical => size.height,
        Axis::Horizontal => size.width,
    }
}

#[derive(Default)]
pub(crate) struct ScrollbarState {
    /// Shown after a wheel scroll, until `hide_task` fires.
    pub revealed: bool,
    hide_task: Option<Task<()>>,
    /// The bar whose track is under the mouse.
    pub hovered: Option<Axis>,
    /// The bar being dragged, and where in its thumb it was grabbed.
    pub drag: Option<(Axis, Pixels)>,
    /// Widest line in columns, for the (buffer version, tab size) it's for.
    widest: Option<((u64, usize), usize)>,
    /// Widest line drawn so far, standing in for measuring huge files.
    pub widest_drawn: Pixels,
}

impl ScrollbarState {
    /// Whether `axis`'s bar is drawn.
    pub fn visible(&self, axis: Axis) -> bool {
        self.revealed || self.hovered == Some(axis) || self.drag.is_some_and(|(a, _)| a == axis)
    }

    /// Columns of the widest line, measured again only after edits. `None`
    /// for huge files (see [`MAX_MEASURED_LINES`]).
    pub fn widest_line(&mut self, buffer: &Buffer, tab_width: usize) -> Option<usize> {
        if buffer.line_count() > MAX_MEASURED_LINES {
            return None;
        }
        let key = (buffer.version(), tab_width);
        match self.widest {
            Some((cached, cols)) if cached == key => Some(cols),
            _ => {
                let cols = widest_line(buffer, tab_width);
                self.widest = Some((key, cols));
                Some(cols)
            }
        }
    }
}

/// Columns of the widest line, as drawn (tabs expanded, long lines cut).
/// Tabs count as a full tab stop, which is exact for indentation.
fn widest_line(buffer: &Buffer, tab_width: usize) -> usize {
    let extra_per_tab = tab_width.max(1) - 1;
    buffer
        .rope()
        .lines()
        .map(|line| {
            let (mut chars, mut tabs) = (0, 0);
            for byte in line.bytes() {
                match byte {
                    b'\n' | b'\r' => {}
                    b'\t' => {
                        chars += 1;
                        tabs += 1;
                    }
                    // Count chars by their first byte.
                    _ => chars += usize::from(byte & 0xC0 != 0x80),
                }
            }
            chars.min(MAX_DRAWN_CHARS) + tabs.min(MAX_DRAWN_CHARS) * extra_per_tab
        })
        .max()
        .unwrap_or(0)
}

impl Editor {
    /// The bar under `position` in the last frame.
    fn scrollbar_at(&self, position: Point<Pixels>) -> Option<&ScrollbarLayout> {
        let layout = self.layout.as_ref()?;
        layout
            .scrollbars
            .iter()
            .find(|bar| bar.track.contains(&position))
    }

    /// Shows the bars, hiding them again after a pause.
    pub(crate) fn reveal_scrollbars(&mut self, cx: &mut Context<Self>) {
        self.scrollbars.revealed = true;
        self.scrollbars.hide_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDE_DELAY).await;
            this.update(cx, |editor, cx| {
                editor.scrollbars.revealed = false;
                editor.scrollbars.hide_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Starts dragging the bar under `position`, if any. Clicking the track
    /// outside the thumb jumps there. Returns whether a bar was hit.
    pub(crate) fn scrollbar_mouse_down(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(bar) = self.scrollbar_at(position) else {
            return false;
        };
        let axis = bar.axis;
        let thumb_start = bar.offset_in_track(bar.thumb.origin);
        let thumb_length = along(axis, bar.thumb.size);
        let offset = bar.offset_in_track(position);
        let grab = if offset >= thumb_start && offset <= thumb_start + thumb_length {
            offset - thumb_start
        } else {
            thumb_length / 2.
        };
        self.scrollbars.drag = Some((axis, grab));
        self.scrollbar_drag_to(position, cx);
        true
    }

    /// Follows the mouse while a bar is dragged. Returns whether one is.
    pub(crate) fn scrollbar_drag_to(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((axis, grab)) = self.scrollbars.drag else {
            return false;
        };
        let Some(bar) = self
            .layout
            .as_ref()
            .and_then(|layout| layout.scrollbars.iter().find(|bar| bar.axis == axis))
        else {
            return true;
        };
        let scroll = bar.scroll_for_thumb_start(bar.offset_in_track(position) - grab);
        match axis {
            Axis::Vertical => self.scroll.y = scroll,
            Axis::Horizontal => self.scroll.x = scroll,
        }
        self.autoscroll = false;
        self.clamp_scroll();
        self.sync_partner_scroll(cx);
        cx.notify();
        true
    }

    /// Ends a bar drag. Returns whether one was in progress.
    pub(crate) fn scrollbar_mouse_up(&mut self, cx: &mut Context<Self>) -> bool {
        if self.scrollbars.drag.take().is_none() {
            return false;
        }
        self.reveal_scrollbars(cx);
        cx.notify();
        true
    }

    /// Shows a bar while the mouse is over its track.
    pub(crate) fn update_scrollbar_hover(
        &mut self,
        position: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let hovered = position.and_then(|position| self.scrollbar_at(position).map(|bar| bar.axis));
        if hovered != self.scrollbars.hovered {
            // Leaving a bar lets it linger like after a scroll.
            if hovered.is_none() {
                self.reveal_scrollbars(cx);
            }
            self.scrollbars.hovered = hovered;
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widest_line_expands_tabs_and_skips_line_breaks() {
        let buffer = Buffer::new("ab\r\n\tabc\nxy");
        assert_eq!(widest_line(&buffer, 4), 7);
        assert_eq!(widest_line(&Buffer::new(""), 4), 0);
    }

    #[test]
    fn thumb_tracks_scroll_and_maps_back() {
        let track = Bounds::new(point(px(0.), px(0.)), size(px(12.), px(100.)));
        let bar = ScrollbarLayout::new(Axis::Vertical, track, px(100.), px(150.), px(300.))
            .expect("content overflows");
        // A quarter of the content is visible, and it's halfway down.
        assert_eq!(bar.thumb.size.height, px(25.));
        assert_eq!(bar.thumb.origin.y, px(37.5));
        assert_eq!(bar.scroll_for_thumb_start(px(37.5)), px(150.));
        assert_eq!(bar.scroll_for_thumb_start(px(500.)), px(300.));
        assert!(ScrollbarLayout::new(Axis::Vertical, track, px(100.), px(0.), px(0.)).is_none());
    }
}
