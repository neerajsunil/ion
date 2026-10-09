//! A tab that shows an image file, scaled down to fit.

use std::path::{Path, PathBuf};

use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, ObjectFit, ParentElement, Render,
    RetainAllImageCache, SharedString, Styled, StyledImage, Window, div, img, prelude::*, px,
};

/// Extensions opened as images instead of text (SVG stays text: it's code
/// people edit).
pub(crate) fn is_image(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    matches!(
        extension.as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff")
    )
}

pub(crate) struct ImageView {
    focus_handle: FocusHandle,
    path: PathBuf,
    /// File size, shown under the image.
    size: Option<u64>,
    /// The decoded image, while the tab is shown. GPUI's global cache would
    /// keep it until Ion exits.
    cache: Option<Entity<RetainAllImageCache>>,
}

impl Focusable for ImageView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ImageView {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let size = std::fs::metadata(&path).ok().map(|meta| meta.len());
        Self {
            focus_handle: cx.focus_handle(),
            path,
            size,
            cache: None,
        }
    }

    /// Frees the decoded image of a hidden tab; it's decoded again when shown.
    pub fn release_image(&mut self) {
        self.cache = None;
    }

    pub fn holds_image(&self) -> bool {
        self.cache.is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> SharedString {
        self.path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
            .into()
    }
}

fn format_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

impl Render for ImageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cache = self
            .cache
            .get_or_insert_with(|| RetainAllImageCache::new(cx))
            .clone();
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(self.path.clone())
                            .image_cache(&cache)
                            .max_w_full()
                            .max_h_full()
                            .object_fit(ObjectFit::ScaleDown)
                            .with_fallback(|| {
                                div()
                                    .text_color(theme::text_muted())
                                    .child("Can't show this image")
                                    .into_any_element()
                            }),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(theme::border())
                    .text_size(theme::ui_font_size_small())
                    .text_color(theme::text_muted())
                    .child(match self.size {
                        Some(size) => format!("{} · {}", self.title(), format_size(size)),
                        None => self.title().to_string(),
                    }),
            )
    }
}
