//! Problems shown in the editor: a wavy underline and the message at the
//! end of the line. They come from outside (build output in a terminal) and
//! hide once the file is edited, since their positions may no longer match.

use gpui::{Context, Hsla, SharedString};

use crate::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

impl DiagnosticSeverity {
    pub fn color(self) -> Hsla {
        match self {
            DiagnosticSeverity::Error => theme::git_deleted(),
            DiagnosticSeverity::Warning => theme::git_modified(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Zero-based row.
    pub row: usize,
    /// Zero-based column, if the tool printed one.
    pub column: Option<usize>,
    pub severity: DiagnosticSeverity,
    pub message: SharedString,
}

#[derive(Default)]
pub(crate) struct Diagnostics {
    /// Sorted by row, then severity.
    items: Vec<Diagnostic>,
    /// The buffer version they were reported for.
    version: u64,
}

impl Editor {
    pub fn set_diagnostics(&mut self, mut items: Vec<Diagnostic>, cx: &mut Context<Self>) {
        items.sort_by_key(|a| (a.row, a.severity, a.column));
        let version = self.buffer.version();
        if self.diagnostics.items == items && self.diagnostics.version == version {
            return;
        }
        self.diagnostics = Diagnostics { items, version };
        cx.notify();
    }

    /// The most severe diagnostic on each row in `rows`, unless the text
    /// changed since they were reported.
    pub(crate) fn diagnostics_in(
        &self,
        rows: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &Diagnostic> {
        let current = self.diagnostics.version == self.buffer.version();
        let items = if current {
            self.diagnostics.items.as_slice()
        } else {
            &[]
        };
        let start = items.partition_point(|item| item.row < rows.start);
        let end = items.partition_point(|item| item.row < rows.end);
        let mut last_row = None;
        items[start..end].iter().filter(move |item| {
            let first = last_row != Some(item.row);
            last_row = Some(item.row);
            first
        })
    }

    /// The diagnostic message at the cursor's row, for the status bar.
    pub fn diagnostic_at_cursor(&self) -> Option<&Diagnostic> {
        let row = self.cursor_row();
        self.diagnostics_in(row..row + 1).next()
    }
}

/// Columns to underline on `line` for a problem at `column`: the word (or
/// symbol) there, or the line's text when the column is unknown.
pub(crate) fn underline_cols(line: &[char], column: Option<usize>) -> Option<(usize, usize)> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let end_of_text = line.len() - line.iter().rev().take_while(|c| c.is_whitespace()).count();
    let (start, end) = match column {
        Some(start) if start < end_of_text => {
            let end = if is_word(line[start]) {
                start + line[start..].iter().take_while(|c| is_word(**c)).count()
            } else {
                start + 1
            };
            (start, end)
        }
        _ => {
            let start = line.iter().take_while(|c| c.is_whitespace()).count();
            (start, end_of_text)
        }
    };
    (start < end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(line: &str, column: Option<usize>) -> Option<(usize, usize)> {
        underline_cols(&line.chars().collect::<Vec<_>>(), column)
    }

    #[test]
    fn underlines_the_word_or_the_line() {
        assert_eq!(cols("    let x_1 = y;", Some(8)), Some((8, 11)));
        assert_eq!(cols("    foo(;", Some(7)), Some((7, 8)));
        assert_eq!(cols("    foo();  ", None), Some((4, 10)));
        assert_eq!(cols("   ", None), None);
        assert_eq!(cols("abc", Some(10)), Some((0, 3)));
    }
}
