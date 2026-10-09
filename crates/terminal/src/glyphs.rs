//! Box drawing (U+2500–U+257F) and block elements (U+2580–U+259F), drawn as
//! shapes that fill the cell instead of font glyphs.
//!
//! Font glyphs for these are only as tall as the font, so with the terminal's
//! taller line height they leave gaps between rows: borders of full-screen
//! apps break up and pictures like an agent's logo get stripes. Terminal.app,
//! Windows Terminal, kitty and others draw them themselves for the same reason.
//!
//! Everything here is plain geometry in logical pixels; edges snap to device
//! pixels so lines stay crisp and neighbouring cells join without seams.

/// A cell on screen, in logical pixels.
#[derive(Clone, Copy)]
pub(crate) struct Cell {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Device pixels per logical pixel.
    pub scale: f32,
}

/// `[left, top, right, bottom]`
pub(crate) type Rect = [f32; 4];
pub(crate) type Point = (f32, f32);

/// A curve or line drawn with a stroke of `Glyph::stroke_width`.
pub(crate) enum Stroke {
    /// A cubic Bézier: start, two control points, end.
    Curve([Point; 4]),
    Line([Point; 2]),
}

pub(crate) struct Glyph {
    pub rects: Vec<Rect>,
    pub strokes: Vec<Stroke>,
    pub stroke_width: f32,
    /// Below 1 for the shade characters.
    pub alpha: f32,
}

/// Whether `c` is drawn here rather than by the font.
pub(crate) fn is_builtin(c: char) -> bool {
    ('\u{2500}'..='\u{259F}').contains(&c)
}

pub(crate) fn glyph(c: char, cell: Cell) -> Option<Glyph> {
    let geometry = Geometry::new(cell);
    let mut glyph = Glyph {
        rects: Vec::new(),
        strokes: Vec::new(),
        stroke_width: geometry.t,
        alpha: 1.,
    };
    if let Some(arms) = arms(c) {
        glyph.rects = geometry.lines(arms);
    } else if let Some((horizontal, weight, dashes)) = dashed(c) {
        glyph.rects = geometry.dashes(horizontal, weight, dashes);
    } else if let Some(block) = block(c) {
        glyph.rects = block
            .rects
            .iter()
            .map(|eighths| geometry.eighths(*eighths))
            .collect();
        glyph.alpha = block.alpha;
    } else {
        glyph.strokes = geometry.arc(c).or_else(|| geometry.diagonal(c))?;
    }
    Some(glyph)
}

// ---- line weights --------------------------------------------------------------

const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// The weight of the line towards each edge: `[up, right, down, left]`.
type Arms = [u8; 4];

/// Weights for U+2500–U+254B, U+2550–U+256C and U+2574–U+257F, written as
/// "up right down left". Dashed lines (`-`) and arcs are handled separately.
const LINES_2500: [&str; 76] = [
    "0101", "0202", "1010", "2020", "-", "-", "-", "-", "-", "-", "-", "-", // 2500
    "0110", "0210", "0120", "0220", "0011", "0012", "0021", "0022", // 250C
    "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", // 2514
    "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", // 251C
    "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", // 2524
    "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222", // 252C
    "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", // 2534
    "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", // 253C
    "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", // 2544
];
const LINES_2550: [&str; 29] = [
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", // 2550
    "1300", "3100", "3300", "1003", "3001", "3003", // 2558
    "1310", "3130", "3330", "1013", "3031", "3033", // 255E
    "0313", "0131", "0333", "1303", "3101", "3303", // 2564
    "1313", "3131", "3333", // 256A
];
const LINES_2574: [&str; 12] = [
    "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", // 2574
    "0201", "1020", "0102", "2010", // 257C
];

fn arms(c: char) -> Option<Arms> {
    let code = c as u32;
    let text = match code {
        0x2500..=0x254B => LINES_2500[(code - 0x2500) as usize],
        0x2550..=0x256C => LINES_2550[(code - 0x2550) as usize],
        0x2574..=0x257F => LINES_2574[(code - 0x2574) as usize],
        _ => return None,
    };
    let bytes = text.as_bytes();
    (bytes.len() == 4).then(|| [0, 1, 2, 3].map(|i| bytes[i] - b'0'))
}

/// Dashed lines: (horizontal, weight, number of dashes).
fn dashed(c: char) -> Option<(bool, u8, u32)> {
    Some(match c {
        '┄' => (true, LIGHT, 3),
        '┅' => (true, HEAVY, 3),
        '┆' => (false, LIGHT, 3),
        '┇' => (false, HEAVY, 3),
        '┈' => (true, LIGHT, 4),
        '┉' => (true, HEAVY, 4),
        '┊' => (false, LIGHT, 4),
        '┋' => (false, HEAVY, 4),
        '╌' => (true, LIGHT, 2),
        '╍' => (true, HEAVY, 2),
        '╎' => (false, LIGHT, 2),
        '╏' => (false, HEAVY, 2),
        _ => return None,
    })
}

// ---- block elements ------------------------------------------------------------

struct Block {
    /// `[left, top, right, bottom]` in eighths of the cell.
    rects: &'static [[u8; 4]],
    alpha: f32,
}

const UL: [u8; 4] = [0, 0, 4, 4];
const UR: [u8; 4] = [4, 0, 8, 4];
const LL: [u8; 4] = [0, 4, 4, 8];
const LR: [u8; 4] = [4, 4, 8, 8];
const FULL: [u8; 4] = [0, 0, 8, 8];

fn block(c: char) -> Option<Block> {
    let solid = |rects| Some(Block { rects, alpha: 1. });
    let shade = |alpha| {
        Some(Block {
            rects: &[FULL],
            alpha,
        })
    };
    match c {
        '▀' => solid(&[[0, 0, 8, 4]]),
        '▁' => solid(&[[0, 7, 8, 8]]),
        '▂' => solid(&[[0, 6, 8, 8]]),
        '▃' => solid(&[[0, 5, 8, 8]]),
        '▄' => solid(&[[0, 4, 8, 8]]),
        '▅' => solid(&[[0, 3, 8, 8]]),
        '▆' => solid(&[[0, 2, 8, 8]]),
        '▇' => solid(&[[0, 1, 8, 8]]),
        '█' => solid(&[FULL]),
        '▉' => solid(&[[0, 0, 7, 8]]),
        '▊' => solid(&[[0, 0, 6, 8]]),
        '▋' => solid(&[[0, 0, 5, 8]]),
        '▌' => solid(&[[0, 0, 4, 8]]),
        '▍' => solid(&[[0, 0, 3, 8]]),
        '▎' => solid(&[[0, 0, 2, 8]]),
        '▏' => solid(&[[0, 0, 1, 8]]),
        '▐' => solid(&[[4, 0, 8, 8]]),
        '░' => shade(0.25),
        '▒' => shade(0.5),
        '▓' => shade(0.75),
        '▔' => solid(&[[0, 0, 8, 1]]),
        '▕' => solid(&[[7, 0, 8, 8]]),
        '▖' => solid(&[LL]),
        '▗' => solid(&[LR]),
        '▘' => solid(&[UL]),
        '▙' => solid(&[UL, LL, LR]),
        '▚' => solid(&[UL, LR]),
        '▛' => solid(&[UL, UR, LL]),
        '▜' => solid(&[UL, UR, LR]),
        '▝' => solid(&[UR]),
        '▞' => solid(&[UR, LL]),
        '▟' => solid(&[UR, LL, LR]),
        _ => None,
    }
}

// ---- geometry ------------------------------------------------------------------

struct Geometry {
    cell: Cell,
    /// Light line thickness, a whole number of device pixels.
    t: f32,
    /// The cell's center, on the device pixel grid.
    cx: f32,
    cy: f32,
}

/// A line segment along one axis: `(along_start, along_end, across_start, across_end)`.
type Segment = (f32, f32, f32, f32);

impl Geometry {
    fn new(cell: Cell) -> Self {
        let device = |logical: f32| (logical * cell.scale).round().max(1.) / cell.scale;
        let mut geometry = Self {
            cell,
            t: device(cell.width / 8.),
            cx: 0.,
            cy: 0.,
        };
        geometry.cx = geometry.round(cell.x + cell.width / 2.);
        geometry.cy = geometry.round(cell.y + cell.height / 2.);
        geometry
    }

    fn round(&self, v: f32) -> f32 {
        (v * self.cell.scale).round() / self.cell.scale
    }

    fn floor(&self, v: f32) -> f32 {
        (v * self.cell.scale).floor() / self.cell.scale
    }

    fn ceil(&self, v: f32) -> f32 {
        (v * self.cell.scale).ceil() / self.cell.scale
    }

    fn thickness(&self, weight: u8) -> f32 {
        if weight == HEAVY { 2. * self.t } else { self.t }
    }

    /// How far a line of `weight` reaches to each side of its center.
    fn half_width(&self, weight: u8) -> f32 {
        match weight {
            NONE => 0.,
            LIGHT => self.t / 2.,
            HEAVY => self.t,
            _ => 1.5 * self.t,
        }
    }

    /// `[start, end)` of a band of `thickness` centered on `center`.
    fn band(&self, center: f32, thickness: f32) -> (f32, f32) {
        let start = self.round(center - thickness / 2.);
        (start, start + thickness)
    }

    /// A rect given in eighths of the cell.
    fn eighths(&self, [left, top, right, bottom]: [u8; 4]) -> Rect {
        let x = |e: u8| self.round(self.cell.x + self.cell.width * f32::from(e) / 8.);
        let y = |e: u8| self.round(self.cell.y + self.cell.height * f32::from(e) / 8.);
        [x(left), y(top), x(right), y(bottom)]
    }

    /// Lines from the center towards each edge with a nonzero weight.
    fn lines(&self, [up, right, down, left]: Arms) -> Vec<Rect> {
        let Cell {
            x,
            y,
            width,
            height,
            ..
        } = self.cell;
        let mut rects = Vec::new();
        let horizontal = |(a0, a1, c0, c1): Segment| [a0, c0, a1, c1];
        let vertical = |(a0, a1, c0, c1): Segment| [c0, a0, c1, a1];
        // Horizontal arms cross the vertical ones (up, down), and vice versa.
        for (weight, end) in [(right, x + width), (left, x)] {
            let arm = self.arm(weight, self.cx, end, self.cy, up, down);
            rects.extend(arm.into_iter().map(horizontal));
        }
        for (weight, end) in [(down, y + height), (up, y)] {
            let arm = self.arm(weight, self.cy, end, self.cx, left, right);
            rects.extend(arm.into_iter().map(vertical));
        }
        rects
    }

    /// One arm, from the center (reaching back far enough to join the
    /// crossing lines, `before` and `after` on either side) to `end`.
    fn arm(
        &self,
        weight: u8,
        center: f32,
        end: f32,
        across: f32,
        before: u8,
        after: u8,
    ) -> Vec<Segment> {
        let span = |reach: f32| {
            if end > center {
                (self.floor(center - reach), end)
            } else {
                (end, self.ceil(center + reach))
            }
        };
        match weight {
            NONE => Vec::new(),
            DOUBLE => {
                let t = self.t;
                // Each of the two lines stops at the crossing double line on
                // its own side, or joins the outer line of the other side.
                let reach = |own: u8, other: u8| match own {
                    DOUBLE => -t / 2.,
                    NONE if other == DOUBLE => 1.5 * t,
                    _ => self.half_width(own),
                };
                let (b0, b1) = self.band(across - t, t);
                let (a0, a1) = span(reach(before, after));
                let (c0, c1) = self.band(across + t, t);
                let (d0, d1) = span(reach(after, before));
                vec![(a0, a1, b0, b1), (d0, d1, c0, c1)]
            }
            _ => {
                let (c0, c1) = self.band(across, self.thickness(weight));
                let reach = self.half_width(before).max(self.half_width(after));
                let (a0, a1) = span(reach);
                vec![(a0, a1, c0, c1)]
            }
        }
    }

    fn dashes(&self, horizontal: bool, weight: u8, dashes: u32) -> Vec<Rect> {
        let Cell {
            x,
            y,
            width,
            height,
            ..
        } = self.cell;
        let (start, length, across) = if horizontal {
            (x, width, self.cy)
        } else {
            (y, height, self.cx)
        };
        let (c0, c1) = self.band(across, self.thickness(weight));
        let slot = length / dashes as f32;
        (0..dashes)
            .map(|i| {
                let a0 = self.round(start + slot * (i as f32 + 0.2));
                let a1 = self.round(start + slot * (i as f32 + 0.8));
                if horizontal {
                    [a0, c0, a1, c1]
                } else {
                    [c0, a0, c1, a1]
                }
            })
            .collect()
    }

    /// Rounded corners: a quarter ellipse between two edge midpoints.
    fn arc(&self, c: char) -> Option<Vec<Stroke>> {
        let Cell {
            x,
            y,
            width,
            height,
            ..
        } = self.cell;
        let (edge_y, edge_x) = match c {
            '╭' => (y + height, x + width),
            '╮' => (y + height, x),
            '╯' => (y, x),
            '╰' => (y, x + width),
            _ => return None,
        };
        // Centers of the light lines it joins, so it lines up with them.
        let (v0, v1) = self.band(self.cx, self.t);
        let (h0, h1) = self.band(self.cy, self.t);
        let (cx, cy) = ((v0 + v1) / 2., (h0 + h1) / 2.);
        // Control points for a quarter circle (or ellipse) as a cubic Bézier.
        const K: f32 = 0.552_284_8;
        Some(vec![Stroke::Curve([
            (cx, edge_y),
            (cx, edge_y + (cy - edge_y) * K),
            (edge_x + (cx - edge_x) * K, cy),
            (edge_x, cy),
        ])])
    }

    fn diagonal(&self, c: char) -> Option<Vec<Stroke>> {
        let Cell {
            x,
            y,
            width,
            height,
            ..
        } = self.cell;
        let rising = Stroke::Line([(x, y + height), (x + width, y)]);
        let falling = Stroke::Line([(x, y), (x + width, y + height)]);
        match c {
            '╱' => Some(vec![rising]),
            '╲' => Some(vec![falling]),
            '╳' => Some(vec![rising, falling]),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Cell = Cell {
        x: 10.,
        y: 20.,
        width: 8.,
        height: 18.,
        scale: 2.,
    };

    fn rects(c: char) -> Vec<Rect> {
        glyph(c, CELL).expect("builtin").rects
    }

    fn covers(rects: &[Rect], (px, py): Point) -> bool {
        rects
            .iter()
            .any(|[l, t, r, b]| (*l..*r).contains(&px) && (*t..*b).contains(&py))
    }

    #[test]
    fn draws_every_character_in_both_ranges() {
        for c in '\u{2500}'..='\u{259F}' {
            assert!(is_builtin(c));
            let glyph = glyph(c, CELL).unwrap_or_else(|| panic!("{c} is not drawn"));
            assert!(!glyph.rects.is_empty() || !glyph.strokes.is_empty(), "{c}");
            for [l, t, r, b] in glyph.rects {
                assert!(l < r && t < b, "{c} has an empty rect");
                assert!(
                    l >= CELL.x && r <= CELL.x + CELL.width,
                    "{c} leaves its cell"
                );
                assert!(
                    t >= CELL.y && b <= CELL.y + CELL.height,
                    "{c} leaves its cell"
                );
            }
        }
        assert!(!is_builtin('M'));
    }

    #[test]
    fn vertical_lines_span_the_whole_row() {
        // The bug this fixes: font glyphs left gaps between rows.
        for c in ['│', '┃', '║', '┼', '╬', '█', '▐', '▌'] {
            let rects = rects(c);
            let top = rects.iter().map(|r| r[1]).fold(f32::MAX, f32::min);
            let bottom = rects.iter().map(|r| r[3]).fold(f32::MIN, f32::max);
            assert_eq!((top, bottom), (CELL.y, CELL.y + CELL.height), "{c}");
        }
    }

    #[test]
    fn corners_join_without_a_notch() {
        let (cx, cy) = (CELL.x + CELL.width / 2., CELL.y + CELL.height / 2.);
        for c in ['┌', '┐', '└', '┘', '┏', '╔', '╝'] {
            assert!(covers(&rects(c), (cx, cy)) || c == '╔' || c == '╝', "{c}");
        }
        // Double corners: the outer lines meet, the gap between them stays open.
        let double = rects('╔');
        let t = 1.;
        assert!(covers(&double, (cx - t, cy - t)), "outer corner");
        assert!(!covers(&double, (cx, cy)), "gap between the lines");
        assert!(covers(&double, (cx + t, cy + t)), "inner corner");
    }

    #[test]
    fn heavy_is_thicker_than_light() {
        let width = |c| {
            let r = rects(c)[0];
            r[3] - r[1]
        };
        assert!(width('━') > width('─'));
    }
}
