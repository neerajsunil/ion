//! The definitions in a file (functions, types, headings), for Go to Symbol.
//! Uses each grammar's tags query, so it is per file and needs no language
//! server.

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, QueryCursor, Tree};

use crate::Language;

/// Names longer than this are cut (a heading that is a whole paragraph).
const MAX_NAME_CHARS: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Interface,
    Type,
    Module,
    Macro,
    Constant,
    Field,
    Heading,
    Other,
}

impl SymbolKind {
    fn from_capture_name(name: &str) -> Option<Self> {
        Some(match name.strip_prefix("definition.")? {
            "function" => Self::Function,
            "method" => Self::Method,
            "class" => Self::Class,
            "interface" => Self::Interface,
            "type" => Self::Type,
            "module" => Self::Module,
            "macro" => Self::Macro,
            "constant" => Self::Constant,
            "field" | "property" => Self::Field,
            "heading" => Self::Heading,
            _ => Self::Other,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::Module => "module",
            Self::Macro => "macro",
            Self::Constant => "constant",
            Self::Field => "field",
            Self::Heading => "heading",
            Self::Other => "symbol",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Where the name starts: zero-based row and char column.
    pub row: usize,
    pub col: usize,
    /// How many other symbols contain this one (a method in a class is 1).
    pub depth: usize,
}

/// Every definition in the file, in document order. Walks the whole tree, so
/// call it off the UI thread.
pub fn symbols(language: &Language, tree: &Tree, rope: &Rope) -> Vec<Symbol> {
    let Some(query) = language.tags_query() else {
        return Vec::new();
    };
    let Some(name_capture) = query.capture_index_for_name("name") else {
        return Vec::new();
    };
    let kinds: Vec<Option<SymbolKind>> = query
        .capture_names()
        .iter()
        .map(|name| SymbolKind::from_capture_name(name))
        .collect();
    let text = |node: Node| {
        rope.get_byte_slice(node.byte_range())
            .into_iter()
            .flat_map(|slice| slice.chunks())
            .map(str::as_bytes)
    };

    // (definition range, name range, pattern, kind)
    let mut found = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text);
    while let Some(query_match) = matches.next() {
        let mut name = None;
        let mut definition = None;
        for capture in query_match.captures() {
            if capture.index == name_capture {
                name = Some(capture.node);
            } else if let Some(kind) = kinds[capture.index as usize] {
                definition = Some((capture.node, kind));
            }
        }
        if let (Some(name), Some((node, kind))) = (name, definition) {
            found.push((
                node.byte_range(),
                name.byte_range(),
                query_match.pattern_index,
                kind,
            ));
        }
    }

    // One symbol per name: when several patterns match it (a method is
    // also a function), the earlier, more specific pattern wins.
    found.sort_by_key(|(_, name, pattern, _)| (name.start, name.end, *pattern));
    found.dedup_by_key(|(_, name, _, _)| name.clone());
    found.sort_by_key(|(definition, name, _, _)| (definition.start, name.start));

    let mut open: Vec<usize> = Vec::new();
    found
        .into_iter()
        .filter_map(|(definition, name, _, kind)| {
            while open.last().is_some_and(|end| *end <= definition.start) {
                open.pop();
            }
            let depth = open.len();
            open.push(definition.end);
            let text: String = rope.get_byte_slice(name.clone())?.chars().collect();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                return None;
            }
            let start = rope.byte_to_char(name.start);
            let row = rope.char_to_line(start);
            Some(Symbol {
                name: text.chars().take(MAX_NAME_CHARS).collect(),
                kind,
                row,
                col: start - rope.line_to_char(row),
                depth,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect, load, parse};
    use std::path::Path;

    fn symbols_of(file: &str, source: &str) -> Vec<(String, SymbolKind, usize, usize)> {
        let language = load(detect(Path::new(file)).unwrap()).unwrap();
        let rope = Rope::from_str(source);
        let tree = parse(&language, &rope, None).unwrap();
        symbols(&language, &tree, &rope)
            .into_iter()
            .map(|symbol| (symbol.name, symbol.kind, symbol.row, symbol.depth))
            .collect()
    }

    #[test]
    fn every_tags_query_compiles() {
        for id in crate::LanguageId::all() {
            if let Some(err) = load(id).unwrap().tags_error() {
                panic!("{err}");
            }
        }
    }

    #[test]
    fn rust_symbols_nest_and_prefer_methods() {
        let source = "struct A;\nimpl A {\n    fn new() -> Self { A }\n}\nfn main() {}\n";
        assert_eq!(
            symbols_of("a.rs", source),
            [
                ("A".to_owned(), SymbolKind::Class, 0, 0),
                ("new".to_owned(), SymbolKind::Method, 2, 0),
                ("main".to_owned(), SymbolKind::Function, 4, 0),
            ]
        );
    }

    #[test]
    fn python_methods_are_inside_their_class() {
        let source = "class A:\n    def f(self):\n        pass\n\ndef g():\n    pass\n";
        assert_eq!(
            symbols_of("a.py", source),
            [
                ("A".to_owned(), SymbolKind::Class, 0, 0),
                ("f".to_owned(), SymbolKind::Function, 1, 1),
                ("g".to_owned(), SymbolKind::Function, 4, 0),
            ]
        );
    }

    #[test]
    fn typescript_includes_interfaces() {
        let source = "interface Shape { area(): number }\nclass Box {\n  size() { return 1 }\n}\n";
        let found = symbols_of("a.ts", source);
        assert!(
            found.contains(&("Shape".to_owned(), SymbolKind::Interface, 0, 0)),
            "{found:?}"
        );
        assert!(
            found.contains(&("size".to_owned(), SymbolKind::Method, 2, 1)),
            "{found:?}"
        );
    }

    #[test]
    fn markdown_headings_nest_by_section() {
        let source = "# Title\n\ntext\n\n## Part\n\nmore\n\n# Next\n";
        assert_eq!(
            symbols_of("a.md", source),
            [
                ("Title".to_owned(), SymbolKind::Heading, 0, 0),
                ("Part".to_owned(), SymbolKind::Heading, 4, 1),
                ("Next".to_owned(), SymbolKind::Heading, 8, 0),
            ]
        );
    }

    #[test]
    fn columns_count_chars() {
        let language = load(detect(Path::new("a.rs")).unwrap()).unwrap();
        let source = "/* é */ fn f() {}";
        let rope = Rope::from_str(source);
        let tree = parse(&language, &rope, None).unwrap();
        let found = symbols(&language, &tree, &rope);
        assert_eq!((found[0].row, found[0].col), (0, 11));
    }
}
