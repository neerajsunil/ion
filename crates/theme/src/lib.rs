//! Colors and metrics shared across the UI.
//!
//! The palette (dark or light) and the font sizes can change at runtime from
//! settings; every accessor reads the current values, so callers just call
//! `theme::bg()` and redraw after a change.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use gpui::{Hsla, Pixels, SharedString, px, rgba};

/// Colors as 0xRRGGBBAA.
pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub elevated: u32,
    pub border: u32,
    pub text: u32,
    pub text_muted: u32,
    pub text_faint: u32,
    pub accent: u32,
    /// Text on an accent background (primary buttons).
    pub on_accent: u32,
    pub hover: u32,
    pub active: u32,
    pub selection: u32,
    pub search_match: u32,
    pub current_line: u32,
    pub git_added: u32,
    pub git_modified: u32,
    pub git_deleted: u32,
    pub git_changed: u32,
    pub diff_added_bg: u32,
    pub diff_removed_bg: u32,
    pub diff_hunk_bg: u32,
    pub shadow: u32,
    pub syntax: SyntaxPalette,
    /// The 16 ANSI colors: normal, then bright.
    pub ansi: [u32; 16],
}

/// Syntax highlighting colors as 0xRRGGBB.
pub struct SyntaxPalette {
    pub keyword: u32,
    pub function: u32,
    pub ty: u32,
    pub string: u32,
    pub escape: u32,
    pub number: u32,
    pub comment: u32,
    pub property: u32,
    pub operator: u32,
    pub punctuation: u32,
    pub tag: u32,
    pub attribute: u32,
    pub link: u32,
}

pub static DARK: Palette = Palette {
    bg: 0x1b1d22ff,
    panel: 0x16181cff,
    elevated: 0x23262cff,
    border: 0x2a2d34ff,
    text: 0xd4d7ddff,
    text_muted: 0x8b919bff,
    text_faint: 0x50555eff,
    accent: 0xff5b2eff,
    on_accent: 0x14151aff,
    hover: 0xffffff0d,
    active: 0xff5b2e26,
    selection: 0xff5b2e40,
    search_match: 0xe5c07b38,
    current_line: 0xffffff08,
    git_added: 0x98c379ff,
    git_modified: 0xe5c07bff,
    git_deleted: 0xe06c75ff,
    git_changed: 0x61afefff,
    diff_added_bg: 0x98c3791c,
    diff_removed_bg: 0xe06c7520,
    diff_hunk_bg: 0xff5b2e12,
    shadow: 0x00000073,
    syntax: SyntaxPalette {
        keyword: 0xc678dd,
        function: 0x61afef,
        ty: 0xe5c07b,
        string: 0x98c379,
        escape: 0x56b6c2,
        number: 0xd19a66,
        comment: 0x6b717d,
        property: 0xe06c75,
        operator: 0x56b6c2,
        punctuation: 0x8b919b,
        tag: 0xe06c75,
        attribute: 0xd19a66,
        link: 0x61afef,
    },
    ansi: [
        0x3b3f47, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, //
        0x5c6370, 0xf0838c, 0xb5e08f, 0xf2d48f, 0x82c3ff, 0xd9a0ec, 0x7fd3dc, 0xe6e8ec,
    ],
};

pub static LIGHT: Palette = Palette {
    bg: 0xffffffff,
    panel: 0xf6f7f9ff,
    elevated: 0xffffffff,
    border: 0xe2e4e8ff,
    text: 0x1f2328ff,
    text_muted: 0x59616bff,
    text_faint: 0xa1a8b1ff,
    accent: 0xe04a1fff,
    on_accent: 0xffffffff,
    hover: 0x0000000b,
    active: 0xe04a1f1c,
    selection: 0xff5b2e33,
    search_match: 0xf2b70040,
    current_line: 0x00000008,
    git_added: 0x2f9e44ff,
    git_modified: 0xb7791fff,
    git_deleted: 0xd63c4aff,
    git_changed: 0x2f6fdfff,
    diff_added_bg: 0x2f9e441f,
    diff_removed_bg: 0xd63c4a1c,
    diff_hunk_bg: 0xe04a1f12,
    shadow: 0x14203238,
    syntax: SyntaxPalette {
        keyword: 0x8839ef,
        function: 0x1e66f5,
        ty: 0xb7791f,
        string: 0x2f9e44,
        escape: 0x0e7c86,
        number: 0xd4610b,
        comment: 0x8c939c,
        property: 0xd63c4a,
        operator: 0x0e7c86,
        punctuation: 0x6b7280,
        tag: 0xd63c4a,
        attribute: 0xd4610b,
        link: 0x1e66f5,
    },
    ansi: [
        0x383a42, 0xd63c4a, 0x2f9e44, 0xb7791f, 0x2f6fdf, 0x8839ef, 0x0e7c86, 0x6e7781, //
        0x59616b, 0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0x383a42,
    ],
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

static LIGHT_MODE: AtomicBool = AtomicBool::new(false);
static UI_SIZE: AtomicU32 = AtomicU32::new(13);
static EDITOR_SIZE: AtomicU32 = AtomicU32::new(14);
static TERMINAL_SIZE: AtomicU32 = AtomicU32::new(14);
/// Editor line height as a multiple of the font size, times 100.
static LINE_SPACING: AtomicU32 = AtomicU32::new(150);
static EDITOR_FONT: RwLock<Option<SharedString>> = RwLock::new(None);

pub fn set_appearance(appearance: Appearance) {
    LIGHT_MODE.store(appearance == Appearance::Light, Ordering::Relaxed);
}

pub fn appearance() -> Appearance {
    if LIGHT_MODE.load(Ordering::Relaxed) {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

/// Font sizes in pixels; values are clamped to a usable range.
pub fn set_font_sizes(ui: u32, editor: u32, terminal: u32) {
    UI_SIZE.store(ui.clamp(10, 20), Ordering::Relaxed);
    EDITOR_SIZE.store(editor.clamp(8, 32), Ordering::Relaxed);
    TERMINAL_SIZE.store(terminal.clamp(8, 32), Ordering::Relaxed);
}

/// The editor and terminal font; `None` uses the platform default.
pub fn set_mono_font(family: Option<String>) {
    let family = family.filter(|family| !family.trim().is_empty());
    *EDITOR_FONT.write().expect("font lock poisoned") = family.map(Into::into);
}

pub fn palette() -> &'static Palette {
    match appearance() {
        Appearance::Dark => &DARK,
        Appearance::Light => &LIGHT,
    }
}

fn color(value: u32) -> Hsla {
    rgba(value).into()
}

macro_rules! colors {
    ($($(#[$doc:meta])* $name:ident => $field:ident),* $(,)?) => {
        $( $(#[$doc])* pub fn $name() -> Hsla { color(palette().$field) } )*
    };
}

colors! {
    bg => bg,
    panel_bg => panel,
    elevated_bg => elevated,
    border => border,
    text => text,
    text_muted => text_muted,
    text_faint => text_faint,
    accent => accent,
    on_accent => on_accent,
    hover_bg => hover,
    active_bg => active,
    selection => selection,
    search_match => search_match,
    current_line => current_line,
    /// Gutter markers and file status colors.
    git_added => git_added,
    git_modified => git_modified,
    git_deleted => git_deleted,
    /// Changed-line marker in the editor gutter.
    git_changed_marker => git_changed,
    diff_added_bg => diff_added_bg,
    diff_removed_bg => diff_removed_bg,
    diff_hunk_bg => diff_hunk_bg,
    shadow => shadow,
}

/// Syntax colors for the current appearance.
pub fn syntax() -> &'static SyntaxPalette {
    &palette().syntax
}

/// Terminal colors as 0xRRGGBB: foreground, background, cursor.
pub fn terminal_colors() -> (u32, u32, u32) {
    let palette = palette();
    (palette.text >> 8, palette.bg >> 8, palette.accent >> 8)
}

pub fn terminal_ansi() -> &'static [u32; 16] {
    &palette().ansi
}

pub fn ui_font_size() -> Pixels {
    px(UI_SIZE.load(Ordering::Relaxed) as f32)
}

/// A smaller UI size for secondary text (status bar, section headers).
pub fn ui_font_size_small() -> Pixels {
    px((UI_SIZE.load(Ordering::Relaxed) as f32 - 1.).max(10.))
}

pub fn editor_font_size() -> Pixels {
    px(EDITOR_SIZE.load(Ordering::Relaxed) as f32)
}

pub fn editor_line_height() -> Pixels {
    let factor = LINE_SPACING.load(Ordering::Relaxed) as f32 / 100.;
    px((EDITOR_SIZE.load(Ordering::Relaxed) as f32 * factor).round())
}

/// Editor line height as a multiple of the font size.
pub fn set_line_spacing(factor: f32) {
    LINE_SPACING.store((factor * 100.).round() as u32, Ordering::Relaxed);
}

pub fn terminal_font_size() -> Pixels {
    px(TERMINAL_SIZE.load(Ordering::Relaxed) as f32)
}

pub fn terminal_line_height() -> Pixels {
    px((TERMINAL_SIZE.load(Ordering::Relaxed) as f32 * 1.36).round())
}

pub fn ui_font() -> &'static str {
    if cfg!(target_os = "windows") {
        "Segoe UI"
    } else if cfg!(target_os = "macos") {
        ".SystemUIFont"
    } else {
        "Inter"
    }
}

pub fn mono_font() -> SharedString {
    if let Some(family) = EDITOR_FONT.read().expect("font lock poisoned").clone() {
        return family;
    }
    if cfg!(target_os = "windows") {
        "Consolas".into()
    } else if cfg!(target_os = "macos") {
        "Menlo".into()
    } else {
        "DejaVu Sans Mono".into()
    }
}
