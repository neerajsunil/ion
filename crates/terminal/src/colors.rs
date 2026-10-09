//! Terminal color palette: 16 themed ANSI colors, the xterm 256-color cube and
//! grayscale ramp, and the default foreground/background.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, rgb};

fn from_hex(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

pub(crate) fn dim(color: Rgb) -> Rgb {
    let scale = |c: u8| (c as f32 * 0.66) as u8;
    Rgb {
        r: scale(color.r),
        g: scale(color.g),
        b: scale(color.b),
    }
}

fn named(color: NamedColor) -> Rgb {
    let index = color as usize;
    if index < 16 {
        return from_hex(theme::terminal_ansi()[index]);
    }
    match color {
        NamedColor::Foreground | NamedColor::BrightForeground => {
            from_hex(theme::terminal_colors().0)
        }
        NamedColor::Background => from_hex(theme::terminal_colors().1),
        NamedColor::Cursor => from_hex(theme::terminal_colors().2),
        NamedColor::DimForeground => dim(from_hex(theme::terminal_colors().0)),
        // DimBlack..DimWhite follow the normal colors in order.
        dim_color => dim(from_hex(
            theme::terminal_ansi()[dim_color as usize - NamedColor::DimBlack as usize],
        )),
    }
}

fn indexed(index: u8) -> Rgb {
    match index {
        0..16 => from_hex(theme::terminal_ansi()[index as usize]),
        16..232 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232.. => {
            let v = 8 + (index - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}

/// Resolves a cell color, honoring palette overrides set by programs (OSC 4/10/11).
pub(crate) fn resolve(color: Color, overrides: &Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Named(name) => overrides[name].unwrap_or_else(|| named(name)),
        Color::Indexed(index) => overrides[index as usize].unwrap_or_else(|| indexed(index)),
    }
}

/// Palette lookup by index for color queries (OSC 4/10/11/12).
pub(crate) fn by_index(index: usize, overrides: &Colors) -> Rgb {
    overrides[index].unwrap_or_else(|| match index {
        0..256 => indexed(index as u8),
        _ => match index {
            256 => named(NamedColor::Foreground),
            257 => named(NamedColor::Background),
            _ => named(NamedColor::Cursor),
        },
    })
}

pub(crate) fn to_hsla(color: Rgb) -> Hsla {
    rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_cube_and_grayscale() {
        assert_eq!(indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            indexed(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            indexed(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }

    #[test]
    fn dim_colors_map_to_their_base() {
        assert_eq!(
            named(NamedColor::DimRed),
            dim(from_hex(theme::terminal_ansi()[1]))
        );
    }
}
