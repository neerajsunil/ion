//! Project-wide search (Ctrl+Shift+F), shown in the sidebar.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use editor::{Editor, EditorEvent};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, StyledText, Subscription, Task, UniformListScrollHandle,
    Window, div, prelude::FluentBuilder, px, uniform_list,
};
use project::FileIndex;
use project::search::{SearchResults, search_with_filesystem};

const ROW_HEIGHT: f32 = 22.;
/// Wait this long after typing stops before searching.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub enum ProjectSearchEvent {
    Open {
        path: PathBuf,
        line: usize,
        columns: Range<usize>,
    },
}

enum Row {
    File(usize),
    Line(usize, usize),
}

pub struct ProjectSearch {
    filesystem: project::FileSystem,
    error: Option<String>,
    query: Entity<Editor>,
    case_sensitive: bool,
    index: Option<Arc<FileIndex>>,
    results: Option<Arc<SearchResults>>,
    rows: Vec<Row>,
    searching: bool,
    /// Set to stop the running search when a newer one starts.
    cancel: Arc<AtomicBool>,
    search_task: Option<Task<()>>,
    scroll_handle: UniformListScrollHandle,
    _query_subscription: Subscription,
}

impl EventEmitter<ProjectSearchEvent> for ProjectSearch {}

impl ProjectSearch {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Editor::single_line("Search in project", cx));
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let EditorEvent::Edited = event {
                this.schedule_search(cx);
            }
        });
        Self {
            filesystem: project::FileSystem::Local,
            error: None,
            query,
            case_sensitive: false,
            index: None,
            results: None,
            rows: Vec::new(),
            searching: false,
            cancel: Arc::new(AtomicBool::new(false)),
            search_task: None,
            scroll_handle: UniformListScrollHandle::new(),
            _query_subscription: subscription,
        }
    }

    pub fn set_index(&mut self, index: Option<Arc<FileIndex>>, cx: &mut Context<Self>) {
        self.index = index;
        if !self.query.read(cx).text().is_empty() {
            self.schedule_search(cx);
        }
    }

    pub fn set_filesystem(&mut self, filesystem: project::FileSystem) {
        self.filesystem = filesystem;
    }

    /// Focuses the query, optionally replacing it with `text`.
    pub fn focus(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |query, cx| {
            let text = text.unwrap_or_else(|| query.text());
            query.set_text(&text, cx);
        });
        window.focus(&gpui::Focusable::focus_handle(self.query.read(cx), cx));
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(connection) = self.filesystem.remote() {
            connection.cancel_jobs();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();

        let query = self.query.read(cx).text();
        let Some(index) = self.index.clone().filter(|_| !query.is_empty()) else {
            self.search_task = None;
            self.results = None;
            self.rows.clear();
            self.searching = false;
            cx.notify();
            return;
        };
        let case_sensitive = self.case_sensitive;
        let filesystem = self.filesystem.clone();
        self.error = None;
        self.searching = true;
        cx.notify();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let results = cx
                .background_spawn(async move {
                    search_with_filesystem(&filesystem, &index, &query, case_sensitive, &cancel)
                })
                .await;
            this.update(cx, |this, cx| match results {
                Ok(results) => this.show_results(results, cx),
                Err(error) => {
                    this.error = Some(error.to_string());
                    this.searching = false;
                    this.results = None;
                    this.rows.clear();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn show_results(&mut self, results: SearchResults, cx: &mut Context<Self>) {
        self.rows.clear();
        for (file_ix, file) in results.files.iter().enumerate() {
            self.rows.push(Row::File(file_ix));
            self.rows
                .extend((0..file.lines.len()).map(|line_ix| Row::Line(file_ix, line_ix)));
        }
        self.results = Some(Arc::new(results));
        self.searching = false;
        self.scroll_handle
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn open_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(results) = &self.results else {
            return;
        };
        let (file_ix, line_ix) = match self.rows.get(ix) {
            Some(Row::File(file_ix)) => (*file_ix, 0),
            Some(Row::Line(file_ix, line_ix)) => (*file_ix, *line_ix),
            None => return,
        };
        let file = &results.files[file_ix];
        let line = &file.lines[line_ix];
        cx.emit(ProjectSearchEvent::Open {
            path: file.absolute.clone(),
            line: line.line,
            columns: line.columns.clone(),
        });
    }

    fn summary(&self) -> SharedString {
        if let Some(error) = &self.error {
            return error.clone().into();
        }
        if self.searching {
            return "Searching…".into();
        }
        let Some(results) = &self.results else {
            return if self.index.is_none() {
                "Indexing files…".into()
            } else {
                "".into()
            };
        };
        let lines: usize = results.files.iter().map(|file| file.lines.len()).sum();
        if lines == 0 {
            return "No results".into();
        }
        let more = if results.truncated { "+" } else { "" };
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let files = results.files.len();
        format!(
            "{lines}{more} result{} in {files} file{}",
            plural(lines),
            plural(files)
        )
        .into()
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let Some(results) = self.results.clone() else {
            return Vec::new();
        };
        range
            .filter_map(|ix| {
                let row = div()
                    .id(ix)
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme::hover_bg()))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.open_row(ix, cx)),
                    );
                Some(match self.rows.get(ix)? {
                    Row::File(file_ix) => {
                        let file = &results.files[*file_ix];
                        let (dir, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
                        row.px_2()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme::text())
                                    .child(name.to_owned()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text_faint())
                                    .child(dir.to_owned()),
                            )
                    }
                    Row::Line(file_ix, line_ix) => {
                        let line = &results.files[*file_ix].lines[*line_ix];
                        let highlight = HighlightStyle {
                            color: Some(theme::text()),
                            font_weight: Some(FontWeight::BOLD),
                            background_color: Some(theme::search_match()),
                            ..Default::default()
                        };
                        row.pl_4()
                            .pr_2()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(32.))
                                    .text_color(theme::text_faint())
                                    .child((line.line + 1).to_string()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text_muted())
                                    .child(StyledText::new(line.preview.clone()).with_highlights(
                                        [(line.preview_range.clone(), highlight)],
                                    )),
                            )
                    }
                })
            })
            .collect()
    }
}

impl Render for ProjectSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("ProjectSearch")
            .size_full()
            .flex()
            .flex_col()
            .text_xs()
            .on_action(cx.listener(|this, _: &editor::Newline, _, cx| this.schedule_search(cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pb_1()
                    .child(
                        div()
                            .flex_1()
                            .h(px(26.))
                            .rounded_sm()
                            .bg(theme::bg())
                            .border_1()
                            .border_color(theme::border())
                            .child(self.query.clone()),
                    )
                    .child(
                        div()
                            .id("search-case")
                            .h(px(22.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_color(theme::text_muted())
                            .when(self.case_sensitive, |button| {
                                button.bg(theme::active_bg()).text_color(theme::text())
                            })
                            .hover(|button| button.bg(theme::hover_bg()))
                            .child("Aa")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.case_sensitive = !this.case_sensitive;
                                this.schedule_search(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_color(theme::text_faint())
                    .child(self.summary()),
            )
            .child(
                uniform_list(
                    "project-search-results",
                    self.rows.len(),
                    cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
                )
                .track_scroll(self.scroll_handle.clone())
                .flex_1(),
            )
    }
}
