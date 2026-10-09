//! Live Markdown preview: a tab that renders an open Markdown editor's text
//! and follows its edits.
//!
//! The text is parsed again only when the editor's version changed, and only
//! while the preview is drawn.

use std::ops::Range;
use std::path::{Path, PathBuf};

use editor::Editor;
use gpui::{
    AnyElement, Context, ElementId, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight,
    HighlightStyle, InteractiveText, IntoElement, ParentElement, Render, SharedString,
    StrikethroughStyle, Styled, StyledText, Subscription, UnderlineStyle, WeakEntity, Window, div,
    prelude::*, px, relative,
};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Inline formatting within a run of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Strong,
    Emphasis,
    Code,
    Strike,
}

/// Text with formatting and links, as byte ranges into `text`.
#[derive(Debug, Default, PartialEq)]
struct Inline {
    text: String,
    marks: Vec<(Range<usize>, Mark)>,
    links: Vec<(Range<usize>, String)>,
}

#[derive(Debug, PartialEq)]
enum Block {
    Heading(u8, Inline),
    Paragraph(Inline),
    Code(String),
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Rule,
    Table {
        header: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
}

#[derive(Debug, Default, PartialEq)]
struct ListItem {
    /// A task list checkbox.
    checked: Option<bool>,
    blocks: Vec<Block>,
}

enum Container {
    Quote(Vec<Block>),
    List(Option<u64>, Vec<ListItem>),
    Item(ListItem),
    Table {
        header: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
        row: Vec<Inline>,
    },
}

#[derive(Default)]
struct Builder {
    root: Vec<Block>,
    stack: Vec<Container>,
    inline: Option<Inline>,
    heading: Option<u8>,
    open_marks: Vec<(Mark, usize)>,
    open_links: Vec<(String, usize)>,
    code: Option<String>,
}

impl Builder {
    fn push_block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Container::Quote(blocks)) => blocks.push(block),
            Some(Container::Item(item)) => item.blocks.push(block),
            _ => self.root.push(block),
        }
    }

    /// Ends the current paragraph (tight list items have no paragraph tags).
    fn flush(&mut self) {
        if let Some(inline) = self.inline.take() {
            let block = match self.heading.take() {
                Some(level) => Block::Heading(level, inline),
                None => Block::Paragraph(inline),
            };
            self.push_block(block);
        }
    }

    fn inline(&mut self) -> &mut Inline {
        self.inline.get_or_insert_default()
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.code {
                Some(code) => code.push_str(&text),
                None => self.inline().text.push_str(&text),
            },
            Event::Code(text) => {
                let inline = self.inline();
                let start = inline.text.len();
                inline.text.push_str(&text);
                let end = inline.text.len();
                inline.marks.push((start..end, Mark::Code));
            }
            Event::SoftBreak => self.inline().text.push(' '),
            Event::HardBreak => self.inline().text.push('\n'),
            Event::Rule => {
                self.flush();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(checked) => {
                if let Some(Container::Item(item)) = self.stack.last_mut() {
                    item.checked = Some(checked);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        let position = self.inline.as_ref().map_or(0, |inline| inline.text.len());
        match tag {
            Tag::Paragraph | Tag::TableCell => {
                self.flush();
                self.inline = Some(Inline::default());
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level as u8);
                self.inline = Some(Inline::default());
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.stack.push(Container::Quote(Vec::new()));
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                self.stack.push(Container::List(start, Vec::new()));
            }
            Tag::Item => self.stack.push(Container::Item(ListItem::default())),
            Tag::Table(_) => {
                self.flush();
                self.stack.push(Container::Table {
                    header: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::Emphasis => self.open_marks.push((Mark::Emphasis, position)),
            Tag::Strong => self.open_marks.push((Mark::Strong, position)),
            Tag::Strikethrough => self.open_marks.push((Mark::Strike, position)),
            Tag::Link { dest_url, .. } => self.open_links.push((dest_url.to_string(), position)),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        let position = self.inline.as_ref().map_or(0, |inline| inline.text.len());
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush(),
            TagEnd::TableCell => {
                let cell = self.inline.take().unwrap_or_default();
                if let Some(Container::Table { row, .. }) = self.stack.last_mut() {
                    row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(Container::Table { header, rows, row }) = self.stack.last_mut() {
                    let cells = std::mem::take(row);
                    if matches!(tag, TagEnd::TableHead) {
                        *header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
            }
            TagEnd::Table => {
                if let Some(Container::Table { header, rows, .. }) = self.stack.pop() {
                    self.push_block(Block::Table { header, rows });
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                if let Some(Container::Quote(blocks)) = self.stack.pop() {
                    self.push_block(Block::Quote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                let mut code = self.code.take().unwrap_or_default();
                if code.ends_with('\n') {
                    code.pop();
                }
                self.push_block(Block::Code(code));
            }
            TagEnd::List(_) => {
                self.flush();
                if let Some(Container::List(start, items)) = self.stack.pop() {
                    self.push_block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                self.flush();
                if let Some(Container::Item(item)) = self.stack.pop()
                    && let Some(Container::List(_, items)) = self.stack.last_mut()
                {
                    items.push(item);
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                if let Some((mark, start)) = self.open_marks.pop()
                    && let Some(inline) = &mut self.inline
                {
                    inline.marks.push((start..position, mark));
                }
            }
            TagEnd::Link => {
                if let Some((url, start)) = self.open_links.pop()
                    && let Some(inline) = &mut self.inline
                {
                    inline.links.push((start..position, url));
                }
            }
            _ => {}
        }
    }
}

fn parse(text: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut builder = Builder::default();
    for event in Parser::new_ext(text, options) {
        builder.event(event);
    }
    builder.flush();
    builder.root
}

pub(crate) enum PreviewEvent {
    /// A link to a file in the project was clicked.
    OpenFile(PathBuf),
}

pub(crate) struct MarkdownPreview {
    focus_handle: FocusHandle,
    editor: WeakEntity<Editor>,
    path: Option<PathBuf>,
    title: SharedString,
    version: Option<u64>,
    blocks: Vec<Block>,
    _observe: Subscription,
}

impl EventEmitter<PreviewEvent> for MarkdownPreview {}

impl Focusable for MarkdownPreview {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl MarkdownPreview {
    pub fn new(editor: &gpui::Entity<Editor>, cx: &mut Context<Self>) -> Self {
        let read = editor.read(cx);
        Self {
            focus_handle: cx.focus_handle(),
            editor: editor.downgrade(),
            path: read.path().map(Path::to_path_buf),
            title: format!("Preview {}", read.title()).into(),
            version: None,
            blocks: Vec::new(),
            _observe: cx.observe(editor, |_, _, cx| cx.notify()),
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The editor this previews, while it's open.
    pub fn editor(&self) -> Option<gpui::Entity<Editor>> {
        self.editor.upgrade()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let editor = editor.read(cx);
        if self.version != Some(editor.version()) {
            self.version = Some(editor.version());
            self.blocks = parse(&editor.text());
        }
    }

    /// Follows a clicked link: web links open in the browser, relative ones
    /// open the file in Ion.
    fn open_link(&self, url: &str, cx: &mut Context<Self>) {
        if url.contains("://") || url.starts_with("mailto:") {
            cx.open_url(url);
            return;
        }
        let file = url.split('#').next().unwrap_or_default();
        if file.is_empty() {
            return;
        }
        let file = crate::ide::percent_decode(file);
        if let Some(dir) = self.path.as_deref().and_then(Path::parent) {
            cx.emit(PreviewEvent::OpenFile(dir.join(file)));
        }
    }
}

/// Builds element ids unique within one render.
struct Ids(usize);

impl Ids {
    fn next(&mut self) -> ElementId {
        self.0 += 1;
        ElementId::NamedInteger("md".into(), self.0 as u64)
    }
}

fn mark_style(mark: Mark) -> HighlightStyle {
    match mark {
        Mark::Strong => HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        },
        Mark::Emphasis => HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..Default::default()
        },
        Mark::Code => HighlightStyle {
            background_color: Some(theme::elevated_bg()),
            color: Some(gpui::rgba(theme::syntax().string).into()),
            ..Default::default()
        },
        Mark::Strike => HighlightStyle {
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
            ..Default::default()
        },
    }
}

impl MarkdownPreview {
    fn inline(&self, inline: &Inline, ids: &mut Ids, cx: &mut Context<Self>) -> AnyElement {
        let link_style = HighlightStyle {
            color: Some(theme::accent()),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            }),
            ..Default::default()
        };
        let highlights: Vec<(Range<usize>, HighlightStyle)> = inline
            .marks
            .iter()
            .map(|(range, mark)| (range.clone(), mark_style(*mark)))
            .chain(
                inline
                    .links
                    .iter()
                    .map(|(range, _)| (range.clone(), link_style)),
            )
            .filter(|(range, _)| !range.is_empty())
            .collect();
        // GPUI wants highlights sorted and not overlapping: split at every
        // boundary and combine the styles covering each piece.
        let mut points: Vec<usize> = highlights
            .iter()
            .flat_map(|(range, _)| [range.start, range.end])
            .collect();
        points.sort_unstable();
        points.dedup();
        let merged: Vec<(Range<usize>, HighlightStyle)> = points
            .windows(2)
            .filter_map(|pair| {
                let piece = pair[0]..pair[1];
                highlights
                    .iter()
                    .filter(|(range, _)| range.start <= piece.start && piece.end <= range.end)
                    .map(|(_, style)| *style)
                    .reduce(HighlightStyle::highlight)
                    .map(|style| (piece, style))
            })
            .collect();
        let text = StyledText::new(SharedString::from(inline.text.clone())).with_highlights(merged);
        if inline.links.is_empty() {
            return text.into_any_element();
        }
        let ranges = inline
            .links
            .iter()
            .map(|(range, _)| range.clone())
            .collect();
        let urls: Vec<String> = inline.links.iter().map(|(_, url)| url.clone()).collect();
        let this = cx.weak_entity();
        InteractiveText::new(ids.next(), text)
            .on_click(ranges, move |ix, _, cx| {
                if let Some(url) = urls.get(ix) {
                    this.update(cx, |this, cx| this.open_link(url, cx)).ok();
                }
            })
            .into_any_element()
    }

    fn block(&self, block: &Block, ids: &mut Ids, cx: &mut Context<Self>) -> AnyElement {
        let base = theme::ui_font_size();
        match block {
            Block::Heading(level, inline) => {
                let (scale, rule) = match level {
                    1 => (2.0, true),
                    2 => (1.5, true),
                    3 => (1.25, false),
                    _ => (1.1, false),
                };
                div()
                    .pt(px(8.))
                    .pb(px(4.))
                    .when(rule, |heading| {
                        heading.border_b_1().border_color(theme::border())
                    })
                    .text_size(base * scale)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(self.inline(inline, ids, cx))
                    .into_any_element()
            }
            Block::Paragraph(inline) => div()
                .line_height(relative(1.6))
                .child(self.inline(inline, ids, cx))
                .into_any_element(),
            Block::Code(code) => div()
                .p(px(12.))
                .rounded(px(6.))
                .bg(theme::elevated_bg())
                .font_family(theme::mono_font())
                .text_size(theme::editor_font_size() * 0.92)
                .line_height(relative(1.5))
                .child(SharedString::from(code.clone()))
                .into_any_element(),
            Block::Quote(blocks) => div()
                .pl(px(14.))
                .border_l_4()
                .border_color(theme::border())
                .text_color(theme::text_muted())
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(blocks.iter().map(|block| self.block(block, ids, cx)))
                .into_any_element(),
            Block::List { start, items } => div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(items.iter().enumerate().map(|(ix, item)| {
                    let marker: SharedString = match (item.checked, start) {
                        (Some(true), _) => "☑".into(),
                        (Some(false), _) => "☐".into(),
                        (None, Some(start)) => format!("{}.", start + ix as u64).into(),
                        (None, None) => "•".into(),
                    };
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(18.))
                                .text_color(theme::text_muted())
                                .line_height(relative(1.6))
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .children(
                                    item.blocks.iter().map(|block| self.block(block, ids, cx)),
                                ),
                        )
                }))
                .into_any_element(),
            Block::Rule => div()
                .h(px(1.))
                .my(px(8.))
                .bg(theme::border())
                .into_any_element(),
            Block::Table { header, rows } => {
                let cell = |inline: &Inline, bold: bool, ids: &mut Ids, cx: &mut Context<Self>| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(10.))
                        .py(px(6.))
                        .border_r_1()
                        .border_color(theme::border())
                        .when(bold, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                        .child(self.inline(inline, ids, cx))
                };
                let mut table = div()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(theme::border())
                    .rounded(px(4.));
                let mut header_row = div().flex().bg(theme::elevated_bg());
                for inline in header {
                    header_row = header_row.child(cell(inline, true, ids, cx));
                }
                table = table.child(header_row);
                for row in rows {
                    let mut line = div().flex().border_t_1().border_color(theme::border());
                    for inline in row {
                        line = line.child(cell(inline, false, ids, cx));
                    }
                    table = table.child(line);
                }
                table.into_any_element()
            }
        }
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        let mut ids = Ids(0);
        let blocks: Vec<AnyElement> = self
            .blocks
            .iter()
            .map(|block| self.block(block, &mut ids, cx))
            .collect();
        div()
            .id("markdown-preview")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .bg(theme::bg())
            .text_color(theme::text())
            .text_size(theme::ui_font_size())
            .child(
                div()
                    .w_full()
                    .max_w(px(860.))
                    .mx_auto()
                    .px(px(32.))
                    .py(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(blocks),
            )
    }
}

impl crate::workspace::Workspace {
    /// Ctrl+Shift+V: previews the active Markdown file beside it, or shows
    /// the preview that's already open.
    pub(crate) fn open_markdown_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::pane::{Item, ItemKind};

        let Some(editor) = self.active_editor() else {
            return;
        };
        if !editor.read(cx).is_markdown() {
            self.status = Some("Preview works on Markdown files".into());
            cx.notify();
            return;
        }
        let existing = self.items().find_map(|(_, item)| match &item.kind {
            ItemKind::Preview(view) if view.read(cx).editor().as_ref() == Some(&editor) => {
                Some(item.id())
            }
            _ => None,
        });
        if let Some((pane, ix)) = existing.and_then(|id| self.find_item(id)) {
            self.activate_item(pane, ix, window, cx);
            return;
        }
        let view = cx.new(|cx| MarkdownPreview::new(&editor, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let PreviewEvent::OpenFile(path) = event;
            this.editor_pane = this.file_pane();
            this.open_file(path.clone(), window, cx);
        });
        let item = Item {
            kind: ItemKind::Preview(view),
            _subscriptions: vec![subscription],
        };
        // Beside the file, in a new split.
        let source = self
            .find_item(editor.entity_id())
            .map_or_else(|| self.file_pane(), |(pane, _)| pane);
        let pane = self.new_pane();
        self.center.split(source, pane, crate::pane::Side::Right);
        self.zoomed = None;
        self.insert_item_with_focus(pane, item, false, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_markdown() {
        let blocks = parse(
            "# Title\n\nSome **bold** and [a link](docs/a.md).\n\n- one\n- [x] two\n\n```rs\nfn main() {}\n```\n",
        );
        assert!(matches!(&blocks[0], Block::Heading(1, inline) if inline.text == "Title"));
        let Block::Paragraph(paragraph) = &blocks[1] else {
            panic!("expected a paragraph: {blocks:?}");
        };
        assert_eq!(paragraph.text, "Some bold and a link.");
        assert_eq!(paragraph.marks, vec![(5..9, Mark::Strong)]);
        assert_eq!(paragraph.links, vec![(14..20, "docs/a.md".to_owned())]);
        let Block::List { start: None, items } = &blocks[2] else {
            panic!("expected a list: {blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].checked, Some(true));
        assert!(matches!(&items[0].blocks[0], Block::Paragraph(inline) if inline.text == "one"));
        assert_eq!(blocks[3], Block::Code("fn main() {}".into()));
    }

    #[test]
    fn parses_tables() {
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let Block::Table { header, rows } = &blocks[0] else {
            panic!("expected a table: {blocks:?}");
        };
        assert_eq!(header.len(), 2);
        assert_eq!(rows[0][1].text, "2");
    }
}
