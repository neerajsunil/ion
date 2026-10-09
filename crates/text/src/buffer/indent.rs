//! Automatic indentation: Enter indents after an opening bracket and
//! outdents after a block ends, and a closing bracket typed on a blank line
//! moves back a level.

use super::{Buffer, EditKind, Selection};

/// Lines looked at, around the cursor, to tell how the file is indented.
const INDENT_SAMPLE_ROWS: usize = 500;

/// How a language's blocks are indented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndentRules {
    /// A line ending in `:` opens a block (Python, YAML).
    pub colon_opens: bool,
    /// Statements after which the block is over, so the next line is one
    /// level less (Python's `return`).
    pub block_enders: &'static [&'static str],
}

fn closer_for(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn leading_whitespace(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// `indent` one level less: a tab, or up to `unit`'s width of spaces.
fn outdent(indent: &str, unit: &str) -> String {
    if let Some(rest) = indent.strip_suffix('\t') {
        return rest.to_owned();
    }
    let width = unit.chars().count().max(1);
    let spaces = indent.chars().rev().take_while(|c| *c == ' ').count();
    indent[..indent.len() - spaces.min(width)].to_owned()
}

impl Buffer {
    /// The indent step this file uses near `row`: a tab if its lines are
    /// indented with tabs, else the most common increase in spaces from one
    /// line to the next. `fallback` when nothing is indented.
    pub fn indent_unit_near(&self, row: usize, fallback: &str) -> String {
        let first = row.saturating_sub(INDENT_SAMPLE_ROWS);
        let last = (row + INDENT_SAMPLE_ROWS).min(self.line_count());
        let mut tabs = 0;
        let mut spaced = 0;
        let mut steps = [0usize; 9];
        let mut previous = 0;
        for row in first..last {
            let mut spaces = 0;
            let mut next = None;
            for c in self.rope.line(row).chars() {
                if c != ' ' {
                    next = Some(c);
                    break;
                }
                spaces += 1;
            }
            match next {
                Some('\t') if spaces == 0 => {
                    tabs += 1;
                    continue;
                }
                // Blank.
                None | Some('\n' | '\r') => continue,
                _ => {}
            }
            if spaces > 0 {
                spaced += 1;
            }
            if spaces > previous && spaces - previous < steps.len() {
                steps[spaces - previous] += 1;
            }
            previous = spaces;
        }
        if tabs > spaced {
            return "\t".to_owned();
        }
        let best = (2..steps.len())
            .filter(|step| steps[*step] > 0)
            .max_by_key(|step| (steps[*step], std::cmp::Reverse(*step)));
        match best {
            Some(step) if spaced > 0 => " ".repeat(step),
            _ => fallback.to_owned(),
        }
    }

    /// Enter, indenting by the language's rules: one level more after an
    /// opening bracket (and the closer moved to its own line when the cursor
    /// is between a pair), one level less after a block ends. Whitespace left
    /// on an otherwise blank line is removed. `fallback_unit` is the indent
    /// step when the file doesn't show one.
    pub fn newline_indented(&mut self, fallback_unit: &str, rules: IndentRules) {
        let range = self.selection.range();
        let (row, col) = self.row_col(range.start);
        let line = self.line_text(row);
        let before: String = line.chars().take(col).collect();
        let (end_row, end_col) = self.row_col(range.end);
        let after: String = self.line_text(end_row).chars().skip(end_col).collect();
        let base = leading_whitespace(&before).to_owned();
        let code = before.trim_end();
        let unit = || self.indent_unit_near(row, fallback_unit);

        // A blank line keeps no trailing whitespace.
        let start = if code.is_empty() && after.trim().is_empty() {
            self.line_start(row)
        } else {
            range.start
        };
        let last = code.chars().last();
        let closer = after.trim_start().chars().next();
        let text = if let Some(close) = last.and_then(closer_for) {
            let inner = format!("{base}{}", unit());
            if closer == Some(close) {
                // `{|}`: the closer goes on its own line, the cursor between.
                let skipped = after.chars().count() - after.trim_start().chars().count();
                let end = range.end + skipped;
                let text = format!("{le}{inner}{le}{base}", le = self.line_ending);
                self.edit(start..end, &text, EditKind::Other);
                let cursor = start + self.line_ending.chars().count() + inner.chars().count();
                self.selection = Selection::cursor(cursor);
                return;
            }
            inner
        } else if rules.colon_opens && last == Some(':') {
            format!("{base}{}", unit())
        } else if code
            .split_whitespace()
            .next()
            .is_some_and(|word| rules.block_enders.contains(&word))
        {
            outdent(&base, &unit())
        } else {
            base
        };
        let text = format!("{}{text}", self.line_ending);
        self.edit(start..range.end, &text, EditKind::Other);
    }

    /// Typing a closing bracket on a line that is otherwise blank moves it
    /// back one level. Returns false (nothing done) in any other case.
    pub fn type_closer_outdented(&mut self, c: char, fallback_unit: &str) -> bool {
        if !matches!(c, ')' | ']' | '}') || !self.selection.is_empty() {
            return false;
        }
        let (row, col) = self.row_col(self.cursor());
        let line = self.line_text(row);
        let before: String = line.chars().take(col).collect();
        let after: String = line.chars().skip(col).collect();
        if before.is_empty() || !before.trim().is_empty() || !after.trim().is_empty() {
            return false;
        }
        let indent = outdent(&before, &self.indent_unit_near(row, fallback_unit));
        let start = self.line_start(row);
        let text = format!("{indent}{c}");
        self.edit(start..self.cursor(), &text, EditKind::Insert);
        self.selection = Selection::cursor(start + text.chars().count());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C_LIKE: IndentRules = IndentRules {
        colon_opens: false,
        block_enders: &[],
    };
    const PYTHON: IndentRules = IndentRules {
        colon_opens: true,
        block_enders: &["return", "pass"],
    };

    fn at_end(text: &str) -> Buffer {
        let mut b = Buffer::new(text);
        b.move_doc_end(false);
        b
    }

    #[test]
    fn indents_after_an_opening_bracket() {
        let mut b = at_end("fn f() {");
        b.newline_indented("    ", C_LIKE);
        b.insert("x");
        assert_eq!(b.text(), "fn f() {\n    x");
    }

    #[test]
    fn splits_a_bracket_pair_onto_three_lines() {
        let mut b = Buffer::new("  call(\n");
        b.move_to(7, false);
        b.insert(")");
        b.move_to(7, false);
        b.newline_indented("  ", C_LIKE);
        b.insert("x");
        assert_eq!(b.text(), "  call(\n    x\n  )\n");
    }

    #[test]
    fn python_colon_indents_and_return_outdents() {
        let mut b = at_end("def f():");
        b.newline_indented("    ", PYTHON);
        b.insert("return 1");
        b.newline_indented("    ", PYTHON);
        b.insert("x");
        assert_eq!(b.text(), "def f():\n    return 1\nx");
    }

    #[test]
    fn colon_is_ordinary_without_the_rule() {
        let mut b = at_end("  label:");
        b.newline_indented("    ", C_LIKE);
        b.insert("x");
        assert_eq!(b.text(), "  label:\n  x");
    }

    #[test]
    fn enter_on_a_blank_line_drops_its_whitespace() {
        let mut b = at_end("{\n    ");
        b.newline_indented("    ", C_LIKE);
        assert_eq!(b.text(), "{\n\n    ");
    }

    #[test]
    fn follows_the_file_indent_over_the_setting() {
        let mut b = at_end("a {\n  b {\n    c\n  }\n}\nif x {");
        b.newline_indented("    ", C_LIKE);
        b.insert("y");
        assert!(b.text().ends_with("if x {\n  y"), "{:?}", b.text());
    }

    #[test]
    fn detects_tabs() {
        let b = Buffer::new("a {\n\tb\n\tc\n}\n");
        assert_eq!(b.indent_unit_near(0, "    "), "\t");
        assert_eq!(Buffer::new("plain\ntext\n").indent_unit_near(0, "  "), "  ");
    }

    #[test]
    fn closer_on_a_blank_line_outdents() {
        let mut b = at_end("if x {\n    y\n    ");
        assert!(b.type_closer_outdented('}', "    "));
        assert_eq!(b.text(), "if x {\n    y\n}");
        let mut b = at_end("    y");
        assert!(!b.type_closer_outdented('}', "    "));
    }

    #[test]
    fn indent_and_closer_undo_in_one_step() {
        let mut b = at_end("{");
        b.newline_indented("    ", C_LIKE);
        b.undo();
        assert_eq!(b.text(), "{");
    }
}
