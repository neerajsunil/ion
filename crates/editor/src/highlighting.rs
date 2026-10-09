//! Syntax highlighting for the editor: keeps a tree-sitter tree up to date in
//! the background and turns highlight spans into styled text runs.

use std::ops::Range;
use std::sync::Arc;

use gpui::{AppContext, Context, FontStyle, FontWeight, Hsla, Task, TextRun, font, rgb};
use syntax::{Highlight, Language, LanguageId, Tree};
use text::TextEdit;

use crate::Editor;
use crate::layout::DisplayLine;

/// Files larger than this aren't highlighted.
const MAX_HIGHLIGHT_FILE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct SyntaxState {
    id: LanguageId,
    /// Loaded (query compiled) on the first background parse.
    language: Option<Arc<Language>>,
    tree: Option<Tree>,
    /// The tree no longer matches the text and can't be reused incrementally
    /// (after undo/redo). It is still drawn until the next parse lands.
    tree_stale: bool,
    /// Edits made while a parse was running, replayed onto its result.
    edits_during_parse: Vec<TextEdit>,
    /// Undo/redo happened while a parse was running, so its result is stale.
    reset_during_parse: bool,
    parse_task: Option<Task<()>>,
}

impl Editor {
    pub(crate) fn init_syntax(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .path()
            .or(self.language_hint.as_deref())
            .and_then(syntax::detect)
        else {
            return;
        };
        if self.buffer.rope().len_bytes() > MAX_HIGHLIGHT_FILE_BYTES {
            return;
        }
        self.buffer.track_edits();
        self.syntax = Some(SyntaxState {
            id,
            language: None,
            tree: None,
            tree_stale: false,
            edits_during_parse: Vec::new(),
            reset_during_parse: false,
            parse_task: None,
        });
        self.schedule_parse(cx);
    }

    pub fn language_name(&self) -> Option<&'static str> {
        self.syntax.as_ref().map(|syntax| syntax.id.name())
    }

    /// Call after every buffer change: keeps the tree's positions in step with
    /// the text and re-parses in the background.
    pub(crate) fn buffer_changed(&mut self, cx: &mut Context<Self>) {
        let batch = self.buffer.take_edits();
        let Some(syntax) = &mut self.syntax else {
            return;
        };
        if batch.edits.is_empty() && !batch.reset {
            return;
        }
        let parsing = syntax.parse_task.is_some();
        if batch.reset {
            syntax.tree_stale = true;
            syntax.reset_during_parse |= parsing;
            syntax.edits_during_parse.clear();
        }
        if !syntax.tree_stale {
            if let Some(tree) = &mut syntax.tree {
                for edit in &batch.edits {
                    syntax::apply_edit(tree, edit);
                }
            }
            if parsing {
                syntax.edits_during_parse.extend(batch.edits);
            }
        }
        self.schedule_parse(cx);
    }

    fn schedule_parse(&mut self, cx: &mut Context<Self>) {
        let Some(syntax) = &mut self.syntax else {
            return;
        };
        if syntax.parse_task.is_some() {
            return;
        }
        let rope = self.buffer.rope().clone();
        let version = self.buffer.version();
        let old_tree = syntax.tree.clone().filter(|_| !syntax.tree_stale);
        let language = syntax.language.clone();
        let id = syntax.id;
        syntax.edits_during_parse.clear();
        syntax.reset_during_parse = false;

        let parse = cx.background_spawn(async move {
            let language = match language {
                Some(language) => language,
                None => syntax::load(id).ok()?,
            };
            let tree = syntax::parse(&language, &rope, old_tree.as_ref());
            Some((language, tree))
        });
        syntax.parse_task = Some(cx.spawn(async move |this, cx| {
            let result = parse.await;
            this.update(cx, |editor, cx| editor.finish_parse(result, version, cx))
                .ok();
        }));
    }

    fn finish_parse(
        &mut self,
        result: Option<(Arc<Language>, Option<Tree>)>,
        version: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(syntax) = &mut self.syntax else {
            return;
        };
        syntax.parse_task = None;
        let Some((language, tree)) = result else {
            // The language failed to load; turn highlighting off for this file.
            self.syntax = None;
            return;
        };
        syntax.language = Some(language);
        if let Some(mut tree) = tree {
            if syntax.reset_during_parse {
                // Undo/redo replaced the text mid-parse: show this result for
                // now, but re-parse from scratch.
                syntax.tree_stale = true;
            } else {
                for edit in syntax.edits_during_parse.drain(..) {
                    syntax::apply_edit(&mut tree, &edit);
                }
                syntax.tree_stale = false;
            }
            syntax.tree = Some(tree);
        }
        if self.buffer.version() != version {
            self.schedule_parse(cx);
        }
        cx.notify();
    }

    /// Highlight spans (byte ranges) for the given rows.
    pub(crate) fn highlights_for_rows(&self, rows: Range<usize>) -> Vec<(Range<usize>, Highlight)> {
        let Some(syntax) = &self.syntax else {
            return Vec::new();
        };
        let (Some(language), Some(tree)) = (&syntax.language, &syntax.tree) else {
            return Vec::new();
        };
        let rope = self.buffer.rope();
        let start = rope.line_to_byte(rows.start.min(rope.len_lines()));
        let end = rope.line_to_byte(rows.end.min(rope.len_lines()));
        syntax::highlight_spans(language, tree, rope, start..end)
    }
}

#[derive(Clone, Copy)]
struct TextStyle {
    color: Hsla,
    italic: bool,
    bold: bool,
}

fn style_for(highlight: Option<Highlight>) -> TextStyle {
    let palette = theme::syntax();
    let plain = TextStyle {
        color: theme::text(),
        italic: false,
        bold: false,
    };
    let color = |hex: u32| TextStyle {
        color: rgb(hex).into(),
        ..plain
    };
    let Some(highlight) = highlight else {
        return plain;
    };
    match highlight {
        Highlight::Keyword => color(palette.keyword),
        Highlight::Function => color(palette.function),
        Highlight::Type => color(palette.ty),
        Highlight::String => color(palette.string),
        Highlight::Escape => color(palette.escape),
        Highlight::Number | Highlight::Constant => color(palette.number),
        Highlight::Comment => TextStyle {
            italic: true,
            ..color(palette.comment)
        },
        Highlight::Property | Highlight::Label => color(palette.property),
        Highlight::Operator => color(palette.operator),
        Highlight::Punctuation => color(palette.punctuation),
        Highlight::Tag => color(palette.tag),
        Highlight::Attribute => color(palette.attribute),
        Highlight::Heading => TextStyle {
            bold: true,
            ..color(palette.property)
        },
        Highlight::Link => color(palette.link),
        Highlight::Emphasis => TextStyle {
            italic: true,
            ..plain
        },
    }
}

fn run(len: usize, style: TextStyle) -> TextRun {
    let mut run_font = font(theme::mono_font());
    if style.italic {
        run_font.style = FontStyle::Italic;
    }
    if style.bold {
        run_font.weight = FontWeight::BOLD;
    }
    TextRun {
        len,
        font: run_font,
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Text runs covering a drawn line. `spans` are char-column ranges within the
/// line, sorted and non-overlapping.
pub(crate) fn line_runs(
    display: &DisplayLine,
    spans: &[(Range<usize>, Highlight)],
) -> Vec<TextRun> {
    let len = display.text.len();
    let mut runs = Vec::with_capacity(spans.len() * 2 + 1);
    let mut pos = 0;
    for (cols, highlight) in spans {
        let start = display.byte_for_col(cols.start).max(pos);
        let end = display.byte_for_col(cols.end);
        if start > pos {
            runs.push(run(start - pos, style_for(None)));
        }
        if end > start {
            runs.push(run(end - start, style_for(Some(*highlight))));
            pos = end;
        }
    }
    if pos < len || runs.is_empty() {
        runs.push(run(len - pos, style_for(None)));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens(text: &str, spans: &[(Range<usize>, Highlight)]) -> Vec<usize> {
        line_runs(&DisplayLine::new(text, 4), spans)
            .iter()
            .map(|run| run.len)
            .collect()
    }

    #[test]
    fn runs_cover_the_whole_line() {
        assert_eq!(lens("let x = 1", &[(0..3, Highlight::Keyword)]), [3, 6]);
        assert_eq!(lens("x = 1", &[(4..5, Highlight::Number)]), [4, 1]);
        assert_eq!(lens("", &[]), [0]);
    }

    #[test]
    fn runs_account_for_expanded_tabs() {
        // The tab draws as four spaces, so the span starts at display byte 4.
        assert_eq!(lens("\tx", &[(1..2, Highlight::Keyword)]), [4, 1]);
    }
}
