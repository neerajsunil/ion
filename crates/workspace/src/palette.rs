//! The palette: find a file by name (Ctrl+P), a command after ">"
//! (Ctrl+Shift+P), or a symbol in the current file after "@" (Ctrl+Shift+O). With no query, recently used and agent-changed files come
//! first; among equally good matches, the most used ones (frecency) win. A
//! pasted `path:line:column` opens at that position.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor::{Editor, EditorEvent};
use gpui::{
    Action, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, ScrollStrategy,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription, Task,
    UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder, px, uniform_list,
};
use project::FileIndex;
use syntax::Symbol;
use ui::IconName;

use crate::commands::{self, Command};
use crate::file_history::FileUse;

actions!(palette, [Dismiss]);

const CONTEXT: &str = "Palette";
const ROW_HEIGHT: f32 = 32.;
const MAX_VISIBLE_ROWS: usize = 12;
const MAX_FILES: usize = 200;
/// Matching more files than this happens in the background, so typing never
/// waits on a large project.
const MAX_FILES_MATCHED_INLINE: usize = 20_000;

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Dismiss, Some(CONTEXT))]
}

pub enum PaletteEvent {
    /// A file, and a zero-based (line, column) to put the cursor at.
    OpenFile(PathBuf, Option<(usize, usize)>),
    Run(Box<dyn Action>),
    /// A symbol in the editor the palette was opened from: zero-based
    /// (line, column).
    GoTo(Entity<Editor>, (usize, usize)),
    Dismissed,
}

/// A file used recently, most recent first in the list given to the palette.
pub struct RecentFile {
    pub path: PathBuf,
    /// Why it's there, shown on its row: "changed by agent", "open".
    pub label: Option<&'static str>,
}

/// A file used before, found in the index.
#[derive(Debug, PartialEq)]
struct RecentEntry {
    file: usize,
    /// Position when ordered most recently used first (for an empty query).
    recency: u32,
    /// Position when ordered most used first (to break ties in a query).
    frecency: u32,
    label: Option<&'static str>,
}

/// A file query and the position of every file it matched.
type Narrowed = (String, Vec<usize>);

enum Choice {
    File(usize),
    Command(usize),
    Symbol(usize),
}

/// The symbols of the editor the palette was opened from.
enum Symbols {
    NoFile,
    /// Dropping it stops the search.
    Loading {
        _task: Task<()>,
    },
    Ready(Vec<Symbol>),
}

pub struct Palette {
    query: Entity<Editor>,
    index: Option<Arc<FileIndex>>,
    recent: Vec<RecentFile>,
    /// Files used in earlier sessions too.
    history: Vec<FileUse>,
    /// `recent` and `history` located in `index`, sorted by file.
    recent_entries: Arc<[RecentEntry]>,
    /// The last file query and every file it matched: a longer query only
    /// searches those.
    narrowed: Option<Arc<Narrowed>>,
    /// Matching the current query in the background; replacing it drops
    /// the result of an older query.
    match_task: Option<Task<()>>,
    /// The one-based line and column typed after the file name.
    position: Option<(usize, Option<usize>)>,
    commands: Vec<Command>,
    /// Shortcut text per command, looked up once when opened.
    shortcuts: Vec<Option<SharedString>>,
    /// The editor the palette was opened from, for symbols.
    editor: Option<Entity<Editor>>,
    /// Found the first time "@" is typed.
    symbols: Option<Symbols>,
    choices: Vec<Choice>,
    selected: usize,
    scroll_handle: UniformListScrollHandle,
    _query_subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    /// `initial` is "" for files, ">" for commands, "@" for symbols in
    /// `editor`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        index: Option<Arc<FileIndex>>,
        recent: Vec<RecentFile>,
        history: Vec<FileUse>,
        editor: Option<Entity<Editor>>,
        initial: &str,
        remote: bool,
        agents: Vec<terminal::HarnessKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            let mut input = Editor::single_line(
                "Search files by name or path:line — > for commands, @ for symbols",
                cx,
            );
            input.set_text(initial, cx);
            let end = initial.chars().count();
            input.select_range((0, end), (0, end), cx);
            input
        });
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let EditorEvent::Edited = event {
                this.update_choices(cx);
            }
        });
        window.focus(&query.focus_handle(cx));
        let commands = commands::all_for_workspace(remote, &agents, settings::get(cx));
        let shortcuts = commands
            .iter()
            .map(|command| command.shortcut(cx))
            .collect();
        let mut palette = Self {
            query,
            index: None,
            recent,
            history,
            recent_entries: Arc::from([]),
            narrowed: None,
            match_task: None,
            position: None,
            commands,
            shortcuts,
            editor,
            symbols: None,
            choices: Vec::new(),
            selected: 0,
            scroll_handle: UniformListScrollHandle::new(),
            _query_subscription: subscription,
        };
        match index {
            Some(index) => palette.set_index(index, cx),
            None => palette.update_choices(cx),
        }
        palette
    }

    /// Supplies the file index when it finishes building after opening.
    pub fn set_index(&mut self, index: Arc<FileIndex>, cx: &mut Context<Self>) {
        self.recent_entries = locate_recent(
            &index,
            &self.recent,
            &self.history,
            crate::file_history::now(),
        )
        .into();
        self.narrowed = None;
        self.index = Some(index);
        self.update_choices(cx);
    }

    fn recent_entry(&self, file: usize) -> Option<&RecentEntry> {
        recent_entry(&self.recent_entries, file)
    }

    fn command_query(&self, cx: &gpui::App) -> Option<String> {
        let text = self.query.read(cx).text();
        text.strip_prefix('>').map(str::to_owned)
    }

    fn symbol_query(&self, cx: &gpui::App) -> Option<String> {
        let text = self.query.read(cx).text();
        text.strip_prefix('@').map(str::to_owned)
    }

    /// Starts finding the editor's symbols, once.
    fn load_symbols(&mut self, cx: &mut Context<Self>) {
        if self.symbols.is_some() {
            return;
        }
        let task = self
            .editor
            .as_ref()
            .and_then(|editor| editor.read(cx).symbols(cx));
        self.symbols = Some(match task {
            Some(task) => Symbols::Loading {
                _task: cx.spawn(async move |this, cx| {
                    let symbols = task.await;
                    this.update(cx, |this, cx| {
                        this.symbols = Some(Symbols::Ready(symbols));
                        this.update_choices(cx);
                    })
                    .ok();
                }),
            },
            None => Symbols::NoFile,
        });
    }

    fn update_choices(&mut self, cx: &mut Context<Self>) {
        self.match_query(false, cx);
    }

    /// Matches the query and shows the results: right away for commands and
    /// small projects, else once a background match finishes (or now, with
    /// `wait`).
    fn match_query(&mut self, wait: bool, cx: &mut Context<Self>) {
        self.match_task = None;
        if let Some(query) = self.command_query(cx) {
            let choices = commands::matching(&self.commands, &query)
                .into_iter()
                .map(Choice::Command)
                .collect();
            self.show(choices, cx);
            return;
        }
        if let Some(query) = self.symbol_query(cx) {
            self.load_symbols(cx);
            let choices = match &self.symbols {
                Some(Symbols::Ready(symbols)) => matching_symbols(symbols, &query)
                    .into_iter()
                    .map(Choice::Symbol)
                    .collect(),
                _ => Vec::new(),
            };
            self.show(choices, cx);
            return;
        }
        let text = self.query.read(cx).text();
        let (query, position) = fuzzy::split_position(&text);
        self.position = position;
        let Some(index) = self.index.clone() else {
            self.show(Vec::new(), cx);
            return;
        };
        let query = within_root(&index, query).to_owned();
        let recent = self.recent_entries.clone();
        let narrowed = self
            .narrowed
            .clone()
            .filter(|narrowed| fuzzy::narrows(&narrowed.0, &query));
        let count = narrowed
            .as_ref()
            .map_or(index.len(), |narrowed| narrowed.1.len());
        if wait || count <= MAX_FILES_MATCHED_INLINE {
            let (files, narrowed) = match_files(&index, &recent, narrowed.as_deref(), &query);
            self.narrowed = narrowed.map(Arc::new);
            self.show(files.into_iter().map(Choice::File).collect(), cx);
            return;
        }
        let matching = cx.background_spawn(async move {
            match_files(&index, &recent, narrowed.as_deref(), &query)
        });
        self.match_task = Some(cx.spawn(async move |this, cx| {
            let (files, narrowed) = matching.await;
            this.update(cx, |this, cx| {
                this.match_task = None;
                this.narrowed = narrowed.map(Arc::new);
                this.show(files.into_iter().map(Choice::File).collect(), cx);
            })
            .ok();
        }));
    }

    fn show(&mut self, choices: Vec<Choice>, cx: &mut Context<Self>) {
        self.choices = choices;
        self.selected = 0;
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.choices.is_empty() {
            return;
        }
        let len = self.choices.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.scroll_handle
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.choices.get(ix) {
            Some(Choice::File(file)) => {
                if let Some(index) = &self.index {
                    // One-based, as printed, to the editor's zero-based.
                    let position = self
                        .position
                        .map(|(line, column)| (line.max(1) - 1, column.unwrap_or(1).max(1) - 1));
                    cx.emit(PaletteEvent::OpenFile(
                        index.absolute(&index.file(*file)),
                        position,
                    ));
                }
            }
            Some(Choice::Command(command)) => {
                cx.emit(PaletteEvent::Run(
                    self.commands[*command].action.boxed_clone(),
                ));
            }
            Some(Choice::Symbol(symbol)) => {
                if let (Some(editor), Some(Symbols::Ready(symbols))) = (&self.editor, &self.symbols)
                {
                    let symbol = &symbols[*symbol];
                    cx.emit(PaletteEvent::GoTo(editor.clone(), (symbol.row, symbol.col)));
                }
            }
            None => {}
        }
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        range
            .filter_map(|ix| {
                let row = div()
                    .id(ix)
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|row| row.bg(theme::hover_bg()))
                    .when(ix == self.selected, |row| row.bg(theme::active_bg()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.confirm(ix, cx)));
                Some(match self.choices.get(ix)? {
                    Choice::File(ix) => {
                        let label = self.recent_entry(*ix).and_then(|entry| entry.label);
                        let file = self.index.as_ref()?.file(*ix);
                        let dir = file.path[..file.name_start]
                            .trim_end_matches('/')
                            .to_owned();
                        row.child(ui::icon(IconName::File))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme::text())
                                    .child(file.name().to_owned()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(theme::ui_font_size_small())
                                    .text_color(theme::text_faint())
                                    .child(dir),
                            )
                            .children(label.map(|label| {
                                div()
                                    .ml_auto()
                                    .flex_none()
                                    .text_size(theme::ui_font_size_small())
                                    .text_color(theme::text_muted())
                                    .child(label)
                            }))
                    }
                    Choice::Command(command) => {
                        let shortcut = self.shortcuts[*command].clone();
                        let command = &self.commands[*command];
                        row.child(match command.icon {
                            Some(name) => ui::icon(name).into_any_element(),
                            None => div().w(px(16.)).flex_none().into_any_element(),
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme::text_faint())
                                .child(format!("{}:", command.category)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::text())
                                .child(command.label),
                        )
                        .children(shortcut.map(ui::kbd))
                    }
                    Choice::Symbol(symbol) => {
                        let Some(Symbols::Ready(symbols)) = &self.symbols else {
                            return None;
                        };
                        let symbol = &symbols[*symbol];
                        // Nesting shows only in the full outline.
                        let nested = self.symbol_query(cx).is_some_and(|q| q.trim().is_empty());
                        let indent = if nested { symbol.depth.min(6) } else { 0 };
                        row.child(div().flex_none().w(px(14. * indent as f32)))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text())
                                    .child(symbol.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(theme::ui_font_size_small())
                                    .text_color(theme::text_faint())
                                    .child(symbol.kind.label()),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .flex_none()
                                    .text_size(theme::ui_font_size_small())
                                    .text_color(theme::text_muted())
                                    .child(format!("{}", symbol.row + 1)),
                            )
                    }
                })
            })
            .collect()
    }
}

/// Positions of the symbols matching `query`: all of them in document order
/// for an empty query, else names containing it (ignoring case), exact
/// names first, then names starting with it, then the rest, each in
/// document order.
fn matching_symbols(symbols: &[Symbol], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut found: Vec<(u8, usize)> = symbols
        .iter()
        .enumerate()
        .filter_map(|(ix, symbol)| {
            let name = symbol.name.to_lowercase();
            let rank = if name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if name.contains(&query) {
                2
            } else {
                return None;
            };
            Some((rank, ix))
        })
        .collect();
    found.sort_by_key(|(rank, ix)| (*rank, *ix));
    found.into_iter().map(|(_, ix)| ix).collect()
}

fn recent_entry(entries: &[RecentEntry], file: usize) -> Option<&RecentEntry> {
    entries
        .binary_search_by_key(&file, |entry| entry.file)
        .ok()
        .map(|ix| &entries[ix])
}

/// The best files for `query` (positions in `index`), and what it matched
/// for narrowing the next query. When `narrowed` is given (an earlier query
/// this one extends), only its matches are searched.
fn match_files(
    index: &FileIndex,
    recent: &[RecentEntry],
    narrowed: Option<&Narrowed>,
    query: &str,
) -> (Vec<usize>, Option<Narrowed>) {
    let only = narrowed.map(|(_, files)| files);
    let file_at = |position: usize| only.map_or(position, |files| files[position]);
    let by_recency = query.trim().is_empty();
    let count = only.map_or(index.len(), Vec::len);
    let candidates = (0..count).map(|position| {
        let ix = file_at(position);
        let file = index.file(ix);
        fuzzy::Candidate {
            path_lower: file.path_lower,
            name_start: file.name_start,
            recent: recent_entry(recent, ix).map(|entry| {
                if by_recency {
                    entry.recency
                } else {
                    entry.frecency
                }
            }),
        }
    });
    let (top, all) = fuzzy::match_paths_with_all(candidates, query, MAX_FILES);
    let files = top.into_iter().map(|m| file_at(m.index)).collect();
    // An empty query narrows nothing.
    let narrowed =
        (!by_recency).then(|| (query.to_owned(), all.into_iter().map(file_at).collect()));
    (files, narrowed)
}

/// Finds the recent files (this session, in order) and the files used in
/// earlier sessions in the index, and ranks them, sorted by file.
fn locate_recent(
    index: &FileIndex,
    recent: &[RecentFile],
    history: &[FileUse],
    now: u64,
) -> Vec<RecentEntry> {
    let scores: HashMap<&Path, f64> = history
        .iter()
        .map(|file| (file.path.as_path(), file.frecency(now)))
        .collect();
    let mut older: Vec<&FileUse> = history.iter().collect();
    older.sort_by_key(|file| std::cmp::Reverse(file.last));
    // Most recent first: this session's files (one opened or changed now is
    // worth at least one use), then the rest of the history.
    let candidates = recent
        .iter()
        .map(|file| {
            let score = scores.get(file.path.as_path()).copied().unwrap_or(0.);
            (&file.path, file.label, score.max(1.))
        })
        .chain(
            older
                .into_iter()
                .map(|file| (&file.path, None, file.frecency(now))),
        );
    let mut seen = HashSet::new();
    let mut entries: Vec<(usize, Option<&'static str>, f64)> = Vec::new();
    for (path, label, score) in candidates {
        if let Some(file) = index.position(path)
            && seen.insert(file)
        {
            entries.push((file, label, score));
        }
    }
    let mut by_score: Vec<usize> = (0..entries.len()).collect();
    // Stable: equal scores stay most recent first.
    by_score.sort_by(|&a, &b| entries[b].2.total_cmp(&entries[a].2));
    let mut frecency = vec![0; entries.len()];
    for (rank, &entry) in by_score.iter().enumerate() {
        frecency[entry] = rank as u32;
    }
    let mut located: Vec<RecentEntry> = entries
        .into_iter()
        .enumerate()
        .map(|(recency, (file, label, _))| RecentEntry {
            file,
            recency: recency as u32,
            frecency: frecency[recency],
            label,
        })
        .collect();
    located.sort_unstable_by_key(|entry| entry.file);
    located
}

/// A pasted absolute path inside the project, as a path relative to it.
fn within_root<'a>(index: &FileIndex, query: &'a str) -> &'a str {
    let root = index.root.to_string_lossy().replace('\\', "/");
    let root = root.trim_end_matches('/');
    let normalized = query.replace('\\', "/");
    match normalized.get(..root.len()) {
        Some(prefix)
            if !root.is_empty()
                && prefix.eq_ignore_ascii_case(root)
                && normalized[root.len()..].starts_with('/') =>
        {
            &query[root.len() + 1..]
        }
        _ => query,
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.choices.len().min(MAX_VISIBLE_ROWS);
        let empty = if self.command_query(cx).is_some() {
            "No matching commands"
        } else if self.symbol_query(cx).is_some() {
            match &self.symbols {
                Some(Symbols::NoFile) => "Open a code file to see its symbols",
                Some(Symbols::Loading { .. }) | None => "Finding symbols…",
                Some(Symbols::Ready(symbols)) if symbols.is_empty() => "No symbols in this file",
                Some(Symbols::Ready(_)) => "No matching symbols",
            }
        } else if self.index.is_none() {
            "Indexing files…"
        } else {
            "No matching files"
        };
        div()
            .key_context(CONTEXT)
            .w(px(620.))
            .flex()
            .flex_col()
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::border())
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &editor::MoveDown, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &editor::MoveUp, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &editor::Newline, _, cx| {
                // Enter right after typing opens the best match for what
                // was typed, not for the query before it.
                if this.match_task.is_some() {
                    this.match_query(true, cx);
                }
                let selected = this.selected;
                this.confirm(selected, cx)
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(PaletteEvent::Dismissed)))
            .child(
                div()
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_3()
                    .pr_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(ui::icon(IconName::Search))
                    .child(div().flex_1().h_full().child(self.query.clone())),
            )
            .map(|palette| {
                if self.choices.is_empty() {
                    palette.child(
                        div()
                            .px_4()
                            .py_3()
                            .text_color(theme::text_muted())
                            .child(empty),
                    )
                } else {
                    palette.child(
                        uniform_list(
                            "palette-results",
                            self.choices.len(),
                            cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
                        )
                        .track_scroll(self.scroll_handle.clone())
                        .h(px(ROW_HEIGHT * rows as f32 + 8.))
                        .p_1(),
                    )
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> FileIndex {
        FileIndex::from_paths(
            PathBuf::from("/srv/app"),
            ["src/a.rs", "src/b.rs", "README.md"].map(String::from),
        )
    }

    fn entry(file: usize, recency: u32, frecency: u32, label: Option<&'static str>) -> RecentEntry {
        RecentEntry {
            file,
            recency,
            frecency,
            label,
        }
    }

    #[test]
    fn locates_recent_files_in_the_index() {
        let index = index();
        let recent = [
            ("/srv/app/src/b.rs", Some("open")),
            ("/elsewhere/x.rs", None),
            ("/srv/app/README.md", None),
        ]
        .map(|(path, label)| RecentFile {
            path: PathBuf::from(path),
            label,
        });
        // Sorted: README.md, src/a.rs, src/b.rs.
        assert_eq!(
            locate_recent(&index, &recent, &[], 0),
            [entry(0, 1, 1, None), entry(2, 0, 0, Some("open"))]
        );
    }

    #[test]
    fn ranks_by_recency_and_by_use() {
        let index = index();
        let day = 24 * 60 * 60;
        let now = 10 * day;
        let used = |path: &str, score: f64, last: u64| FileUse {
            path: PathBuf::from(path),
            score,
            last,
        };
        // a.rs: used a lot yesterday. README.md: once, an hour ago.
        let history = [
            used("/srv/app/src/a.rs", 10., now - day),
            used("/srv/app/README.md", 1., now - 3600),
            used("/srv/app/gone.rs", 50., now),
        ];
        // b.rs: opened this session, never before.
        let recent = [RecentFile {
            path: PathBuf::from("/srv/app/src/b.rs"),
            label: None,
        }];
        assert_eq!(
            locate_recent(&index, &recent, &history, now),
            [
                // README.md: second most recent, tied on use with b.rs.
                entry(0, 1, 2, None),
                // a.rs: least recent, most used.
                entry(1, 2, 0, None),
                entry(2, 0, 1, None),
            ]
        );
    }

    #[test]
    fn matches_symbols_best_first_in_document_order() {
        let symbol = |name: &str| Symbol {
            name: name.to_owned(),
            kind: syntax::SymbolKind::Function,
            row: 0,
            col: 0,
            depth: 0,
        };
        let symbols = [
            symbol("load_file"),
            symbol("Load"),
            symbol("reload"),
            symbol("loader"),
        ];
        assert_eq!(matching_symbols(&symbols, ""), [0, 1, 2, 3]);
        assert_eq!(matching_symbols(&symbols, "load"), [1, 0, 3, 2]);
        assert_eq!(matching_symbols(&symbols, "x"), [] as [usize; 0]);
    }

    #[test]
    fn pasted_absolute_paths_become_relative() {
        let index = index();
        assert_eq!(within_root(&index, "/srv/app/src/a.rs"), "src/a.rs");
        assert_eq!(
            within_root(&index, "/srv/application/a.rs"),
            "/srv/application/a.rs"
        );
        assert_eq!(within_root(&index, "a.rs"), "a.rs");
    }
}
