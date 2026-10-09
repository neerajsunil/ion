//! Syntax highlighting with tree-sitter.
//!
//! Pure logic with no UI dependencies. Parsing is meant to run on a background
//! thread: it reads an O(1) rope snapshot and reuses the previous tree, so
//! re-parsing after an edit only touches the changed region. Highlighting
//! queries only the visible byte range.

mod highlight;
mod languages;

use std::ops::Range;

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use text::TextEdit;
use tree_sitter::{InputEdit, Node, Parser, Point, QueryCursor};

pub use highlight::Highlight;
pub use languages::{Language, LanguageId, detect, load};
pub use tree_sitter::Tree;

/// Ranges larger than this aren't highlighted (e.g. a giant minified line).
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;

/// Parses `rope`, reusing `old_tree` (already adjusted with [`apply_edit`]) so
/// only changed regions are re-parsed.
pub fn parse(language: &Language, rope: &Rope, old_tree: Option<&Tree>) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&language.grammar).ok()?;
    let len = rope.len_bytes();
    let mut read = |byte: usize, _: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - chunk_start..]
    };
    parser.parse_with_options(&mut read, old_tree, None)
}

/// Adjusts `tree` for an edit made after it was parsed.
pub fn apply_edit(tree: &mut Tree, edit: &TextEdit) {
    let point = |(row, column): (usize, usize)| Point { row, column };
    tree.edit(&InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: point(edit.start_point),
        old_end_position: point(edit.old_end_point),
        new_end_position: point(edit.new_end_point),
    });
}

/// Non-overlapping highlighted byte ranges within `range`, in order.
pub fn highlight_spans(
    language: &Language,
    tree: &Tree,
    rope: &Rope,
    range: Range<usize>,
) -> Vec<(Range<usize>, Highlight)> {
    let range = range.start.min(rope.len_bytes())..range.end.min(rope.len_bytes());
    if range.is_empty() || range.len() > MAX_HIGHLIGHT_BYTES {
        return Vec::new();
    }

    let text = |node: Node| {
        rope.get_byte_slice(node.byte_range())
            .into_iter()
            .flat_map(|slice| slice.chunks())
            .map(str::as_bytes)
    };
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(range.clone());
    let mut captures = cursor.captures(&language.query, tree.root_node(), text);
    let mut found = Vec::new();
    while let Some((query_match, index)) = captures.next() {
        let capture = query_match.captures()[*index];
        if let Some(Some(highlight)) = language.capture_highlights.get(capture.index as usize) {
            found.push((
                capture.node.byte_range(),
                query_match.pattern_index,
                *highlight,
            ));
        }
    }

    // Paint outer nodes first so nested ones (an escape inside a string) win.
    // For the same node, later patterns paint last and win: queries list
    // general rules first and specific ones after.
    found.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(&b.1)));
    let mut paint: Vec<Option<Highlight>> = vec![None; range.len()];
    for (node_range, _, highlight) in found {
        let start = node_range.start.max(range.start) - range.start;
        let end = node_range.end.min(range.end).saturating_sub(range.start);
        if start < end {
            paint[start..end].fill(Some(highlight));
        }
    }

    let mut spans: Vec<(Range<usize>, Highlight)> = Vec::new();
    for (offset, highlight) in paint.into_iter().enumerate() {
        let Some(highlight) = highlight else {
            continue;
        };
        let byte = range.start + offset;
        match spans.last_mut() {
            Some((last, last_highlight)) if last.end == byte && *last_highlight == highlight => {
                last.end += 1;
            }
            _ => spans.push((byte..byte + 1, highlight)),
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_language_loads() {
        for id in LanguageId::all() {
            if let Err(err) = load(id) {
                panic!("{err}");
            }
        }
    }

    #[test]
    fn detects_by_extension_and_name() {
        assert_eq!(
            detect(Path::new("src/main.rs")).map(LanguageId::name),
            Some("Rust")
        );
        assert_eq!(
            detect(Path::new("App.TSX")).map(LanguageId::name),
            Some("TSX")
        );
        assert_eq!(
            detect(Path::new("Cargo.lock")).map(LanguageId::name),
            Some("TOML")
        );
        assert_eq!(detect(Path::new("notes.txt")), None);
    }

    fn highlighted<'a>(
        source: &'a str,
        spans: &[(Range<usize>, Highlight)],
    ) -> Vec<(&'a str, Highlight)> {
        spans
            .iter()
            .map(|(range, highlight)| (&source[range.clone()], *highlight))
            .collect()
    }

    #[test]
    fn highlights_rust() {
        let language = load(detect(Path::new("a.rs")).unwrap()).unwrap();
        let source = "fn main() { let s = \"hi\\n\"; } // done";
        let rope = Rope::from_str(source);
        let tree = parse(&language, &rope, None).unwrap();
        let spans = highlighted(
            source,
            &highlight_spans(&language, &tree, &rope, 0..source.len()),
        );
        assert!(spans.contains(&("fn", Highlight::Keyword)), "{spans:?}");
        assert!(spans.contains(&("main", Highlight::Function)), "{spans:?}");
        assert!(spans.contains(&("\\n", Highlight::Escape)), "{spans:?}");
        assert!(
            spans.contains(&("// done", Highlight::Comment)),
            "{spans:?}"
        );
    }

    #[test]
    fn specific_patterns_override_general_ones() {
        // `(field_identifier) @property` comes before the method-call pattern.
        let language = load(detect(Path::new("a.rs")).unwrap()).unwrap();
        let source = "fn f() { x.call(); }";
        let rope = Rope::from_str(source);
        let tree = parse(&language, &rope, None).unwrap();
        let spans = highlighted(
            source,
            &highlight_spans(&language, &tree, &rope, 0..source.len()),
        );
        assert!(spans.contains(&("call", Highlight::Function)), "{spans:?}");
    }

    #[test]
    fn incremental_reparse_matches_full_parse() {
        let language = load(detect(Path::new("a.py")).unwrap()).unwrap();
        let mut buffer = text::Buffer::new("def f():\n    return 1\n");
        buffer.track_edits();
        let mut tree = parse(&language, buffer.rope(), None).unwrap();
        buffer.move_to(4, false);
        buffer.insert("long_");
        for edit in buffer.take_edits().edits {
            apply_edit(&mut tree, &edit);
        }
        let incremental = parse(&language, buffer.rope(), Some(&tree)).unwrap();
        let full = parse(&language, buffer.rope(), None).unwrap();
        assert_eq!(
            incremental.root_node().to_sexp(),
            full.root_node().to_sexp()
        );
    }
}
