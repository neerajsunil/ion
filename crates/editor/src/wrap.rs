//! Display rows: soft wrapping splits buffer lines into several rows, and
//! folds hide lines (they take no rows).
//!
//! Wrapping is by columns (the editor font is monospace) at the last space
//! before the edge, else mid-word. The map is rebuilt only when the text,
//! the width, the tab size or the folds change, and costs one pass over
//! the text.

use std::collections::HashMap;

use text::Buffer;

/// Files with more lines than this aren't wrapped, so a huge log can't
/// make every keystroke rescan it.
const MAX_WRAPPED_LINES: usize = 20_000;
/// Folding alone is cheaper (no text scan), so it goes further.
const MAX_FOLDED_LINES: usize = 200_000;

#[derive(Default)]
pub(crate) struct WrapMap {
    /// (buffer version, columns, tab size, folds version) the map was built for.
    key: Option<(u64, Option<usize>, usize, u64)>,
    /// Whether lines wrap (else the map only hides folded lines).
    wrapping: bool,
    /// Display row where each buffer line starts, plus the total at the end.
    /// Empty when not wrapping: then display rows are buffer rows.
    starts: Vec<usize>,
    /// Columns where continuation rows start, for lines that wrap.
    breaks: HashMap<usize, Vec<usize>>,
}

impl WrapMap {
    pub fn is_active(&self) -> bool {
        !self.starts.is_empty()
    }

    /// Whether long lines wrap (no horizontal scrolling).
    pub fn wraps_lines(&self) -> bool {
        self.wrapping
    }

    /// Rebuilds the map, wrapping at `cols` columns if given, if anything
    /// changed.
    pub fn update(&mut self, buffer: &Buffer, cols: Option<usize>, tab_width: usize) {
        let cols = cols.map(|cols| cols.max(8));
        let key = (buffer.version(), cols, tab_width, buffer.folds_version());
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.starts.clear();
        self.breaks.clear();
        self.wrapping = false;
        let hidden = buffer.hidden_ranges();
        if cols.is_none() && hidden.is_empty() {
            return;
        }
        let line_count = buffer.line_count();
        let limit = if cols.is_some() {
            MAX_WRAPPED_LINES
        } else {
            MAX_FOLDED_LINES
        };
        if line_count > limit {
            return;
        }
        self.wrapping = cols.is_some();
        self.starts.reserve(line_count + 1);
        let mut hidden = hidden.into_iter().peekable();
        let mut total = 0;
        for (row, line) in buffer.rope().lines().enumerate().take(line_count) {
            self.starts.push(total);
            while hidden.peek().is_some_and(|&(_, last)| last < row) {
                hidden.next();
            }
            if hidden.peek().is_some_and(|&(first, _)| first <= row) {
                // Folded away: no rows.
                continue;
            }
            match cols {
                Some(cols) => {
                    let breaks = line_breaks(line.chars(), cols, tab_width);
                    total += breaks.len() + 1;
                    if !breaks.is_empty() {
                        self.breaks.insert(row, breaks);
                    }
                }
                None => total += 1,
            }
        }
        self.starts.push(total);
    }

    fn breaks(&self, row: usize) -> &[usize] {
        self.breaks.get(&row).map_or(&[], Vec::as_slice)
    }

    /// Display rows in total.
    pub fn row_count(&self, line_count: usize) -> usize {
        self.starts.last().copied().unwrap_or(line_count)
    }

    /// Display row of the first segment of buffer `row`.
    pub fn row_start(&self, row: usize) -> usize {
        match self.starts.get(row) {
            Some(start) => *start,
            None if self.is_active() => self.row_count(0),
            None => row,
        }
    }

    /// Display row showing (row, col).
    pub fn display_row(&self, row: usize, col: usize) -> usize {
        let segment = self.breaks(row).partition_point(|&start| start <= col);
        self.row_start(row) + segment
    }

    /// The buffer row shown on `display_row` and the column its segment
    /// starts at, with the next segment's start (`None` on a line's last).
    pub fn segment(&self, display_row: usize) -> (usize, usize, Option<usize>) {
        if !self.is_active() {
            return (display_row, 0, None);
        }
        let row = self
            .starts
            .partition_point(|&start| start <= display_row)
            .saturating_sub(1)
            .min(self.starts.len() - 2);
        let index = display_row - self.starts[row];
        let breaks = self.breaks(row);
        let start = if index == 0 { 0 } else { breaks[index - 1] };
        (row, start, breaks.get(index).copied())
    }
}

/// Columns where continuation rows start, wrapping at `cols` visual columns.
fn line_breaks(chars: impl Iterator<Item = char>, cols: usize, tab_width: usize) -> Vec<usize> {
    let tab_width = tab_width.max(1);
    let mut breaks = Vec::new();
    let mut visual = 0;
    // Column after the last space in the current segment.
    let mut after_space = None;
    let mut segment_start = 0;
    let chars = chars.into_iter();
    let mut col = 0;
    for ch in chars {
        if ch == '\n' || ch == '\r' {
            break;
        }
        let width = if ch == '\t' {
            tab_width - visual % tab_width
        } else {
            1
        };
        while visual + width > cols && col > segment_start {
            // Break after the last space, unless that leaves nothing.
            let at = after_space.filter(|&at| at > segment_start).unwrap_or(col);
            breaks.push(at);
            segment_start = at;
            after_space = None;
            // Re-measure the carried-over part of the word.
            visual = col - at;
        }
        visual += width;
        col += 1;
        if ch == ' ' || ch == '\t' {
            after_space = Some(col);
        }
    }
    breaks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn breaks(text: &str, cols: usize) -> Vec<usize> {
        line_breaks(text.chars(), cols, 4)
    }

    #[test]
    fn wraps_at_spaces_then_mid_word() {
        assert_eq!(breaks("hello world", 20), Vec::<usize>::new());
        assert_eq!(breaks("hello world", 8), vec![6]);
        assert_eq!(breaks("abcdefghij", 4), vec![4, 8]);
        assert_eq!(breaks("aa bbbbbbbbbb", 5), vec![3, 8]);
    }

    #[test]
    fn maps_display_rows_both_ways() {
        let buffer = Buffer::new("short\nhello world again\nend");
        let mut map = WrapMap::default();
        map.update(&buffer, Some(8), 4);
        // "hello world again" breaks into "hello ", "world ", "again".
        assert_eq!(map.row_count(3), 5);
        assert_eq!(map.row_start(1), 1);
        assert_eq!(map.display_row(1, 7), 2);
        assert_eq!(map.display_row(1, 12), 3);
        assert_eq!(map.segment(2), (1, 6, Some(12)));
        assert_eq!(map.segment(3), (1, 12, None));
        assert_eq!(map.segment(4), (2, 0, None));
        assert_eq!(map.row_start(2), 4);
    }

    #[test]
    fn folded_lines_take_no_rows() {
        let mut buffer = Buffer::new("a {\n  b\n  c\n}\nd");
        let mut map = WrapMap::default();
        map.update(&buffer, None, 4);
        assert!(!map.is_active());
        buffer.fold_at(0);
        map.update(&buffer, None, 4);
        assert!(map.is_active() && !map.wraps_lines());
        assert_eq!(map.row_count(5), 3);
        assert_eq!(map.segment(0), (0, 0, None));
        assert_eq!(map.segment(1), (3, 0, None));
        assert_eq!(map.segment(2), (4, 0, None));
        assert_eq!(map.row_start(3), 1);
    }
}
