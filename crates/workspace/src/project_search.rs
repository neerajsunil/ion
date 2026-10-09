//! Project-wide search (Ctrl+Shift+F), shown in the sidebar.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;

use editor::{Editor, EditorEvent};
use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, StyledText, Subscription, Task, UniformListScrollHandle,
    Window, div, prelude::FluentBuilder, px, uniform_list,
};
use project::FileIndex;
use project::search::{FileMatches, Sources, stream_with_filesystem};

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

/// What a search looks at before the rest of the project, taken from the
/// workspace when the search starts.
#[derive(Default)]
pub struct SearchContext {
    /// Absolute paths, most relevant first.
    pub first: Vec<PathBuf>,
    /// Open files with unsaved changes, searched instead of the disk.
    pub buffers: Vec<(PathBuf, text::Rope)>,
}

type ContextSource = Box<dyn Fn(&App) -> SearchContext>;

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
    context: Option<ContextSource>,
    /// Files with matches, in search order. `None` before the first search.
    results: Option<Vec<FileMatches>>,
    truncated: bool,
    /// The results shown are from the previous search: the next batch
    /// replaces them.
    stale: bool,
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
            context: None,
            results: None,
            truncated: false,
            stale: false,
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

    /// Where searches get the files to look at first.
    pub fn set_context(&mut self, context: impl Fn(&App) -> SearchContext + 'static) {
        self.context = Some(Box::new(context));
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
        self.stale = true;
        cx.notify();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let Ok(context) = this.update(cx, |this, cx| {
                this.context
                    .as_ref()
                    .map(|context| context(cx))
                    .unwrap_or_default()
            }) else {
                return;
            };
            let (sender, mut batches) = futures::channel::mpsc::unbounded();
            let search = cx.background_spawn(async move {
                let sources = Sources {
                    first: context.first,
                    buffers: context
                        .buffers
                        .into_iter()
                        .map(|(path, rope)| (path, rope.to_string()))
                        .collect(),
                };
                stream_with_filesystem(
                    &filesystem,
                    &index,
                    &query,
                    case_sensitive,
                    &sources,
                    &cancel,
                    &|batch| {
                        sender.unbounded_send(batch).ok();
                    },
                )
            });
            while let Some(batch) = batches.next().await {
                if this
                    .update(cx, |this, cx| this.add_results(batch, cx))
                    .is_err()
                {
                    return;
                }
            }
            let outcome = search.await;
            this.update(cx, |this, cx| {
                if this.stale {
                    this.add_results(Vec::new(), cx);
                }
                this.searching = false;
                match outcome {
                    Ok(truncated) => this.truncated = truncated,
                    Err(error) => {
                        this.error = Some(error.to_string());
                        this.results = None;
                        this.rows.clear();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Adds a batch of results in search order, replacing the previous
    /// search's results on the first one.
    fn add_results(&mut self, batch: Vec<FileMatches>, cx: &mut Context<Self>) {
        let results = self.results.get_or_insert_with(Vec::new);
        if self.stale {
            self.stale = false;
            self.truncated = false;
            results.clear();
            self.scroll_handle
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
        results.extend(batch);
        results.sort_by_key(|file| file.order);
        self.rows.clear();
        for (file_ix, file) in results.iter().enumerate() {
            self.rows.push(Row::File(file_ix));
            self.rows
                .extend((0..file.lines.len()).map(|line_ix| Row::Line(file_ix, line_ix)));
        }
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
        let file = &results[file_ix];
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
        let results = match &self.results {
            Some(results) if !self.stale => results,
            _ if self.searching => return "Searching…".into(),
            Some(results) => results,
            None if self.index.is_none() => return "Indexing files…".into(),
            None => return "".into(),
        };
        let lines: usize = results.iter().map(|file| file.lines.len()).sum();
        if lines == 0 {
            return if self.searching {
                "Searching…".into()
            } else {
                "No results".into()
            };
        }
        let more = if self.truncated { "+" } else { "" };
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let files = results.len();
        let searching = if self.searching { ", searching…" } else { "" };
        format!(
            "{lines}{more} result{} in {files} file{}{searching}",
            plural(lines),
            plural(files)
        )
        .into()
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let Some(results) = &self.results else {
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
                        let file = &results[*file_ix];
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
                        let line = &results[*file_ix].lines[*line_ix];
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
