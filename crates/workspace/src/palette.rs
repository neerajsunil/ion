//! The palette: find a file by name (Ctrl+P), or a command after ">"
//! (Ctrl+Shift+P). Recently used and agent-changed files come first, and a
//! pasted `path:line:column` opens at that position.

use std::path::PathBuf;
use std::sync::Arc;

use editor::{Editor, EditorEvent};
use gpui::{
    Action, AppContext, ClickEvent, Context, Div, Entity, EventEmitter, Focusable,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Render, ScrollStrategy,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Subscription,
    UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder, px, uniform_list,
};
use project::FileIndex;
use ui::IconName;

use crate::commands::{self, Command};

actions!(palette, [Dismiss]);

const CONTEXT: &str = "Palette";
const ROW_HEIGHT: f32 = 32.;
const MAX_VISIBLE_ROWS: usize = 12;
const MAX_FILES: usize = 200;

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Dismiss, Some(CONTEXT))]
}

pub enum PaletteEvent {
    /// A file, and a zero-based (line, column) to put the cursor at.
    OpenFile(PathBuf, Option<(usize, usize)>),
    Run(Box<dyn Action>),
    Dismissed,
}

/// A file used recently, most recent first in the list given to the palette.
pub struct RecentFile {
    pub path: PathBuf,
    /// Why it's there, shown on its row: "changed by agent", "open".
    pub label: Option<&'static str>,
}

/// A recent file found in the index: (file, position in the recent list, label).
type RecentEntry = (usize, u32, Option<&'static str>);

enum Choice {
    File(usize),
    Command(usize),
}

pub struct Palette {
    query: Entity<Editor>,
    index: Option<Arc<FileIndex>>,
    recent: Vec<RecentFile>,
    /// `recent` located in `index`, sorted by file.
    recent_entries: Vec<RecentEntry>,
    /// The one-based line and column typed after the file name.
    position: Option<(usize, Option<usize>)>,
    commands: Vec<Command>,
    /// Shortcut text per command, looked up once when opened.
    shortcuts: Vec<Option<SharedString>>,
    choices: Vec<Choice>,
    selected: usize,
    scroll_handle: UniformListScrollHandle,
    _query_subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    /// `initial` is "" for files, ">" for commands.
    pub fn new(
        index: Option<Arc<FileIndex>>,
        recent: Vec<RecentFile>,
        initial: &str,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            let mut input = Editor::single_line(
                "Search files by name or path:line — type > for commands",
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
        let commands = commands::all_for_workspace(remote, settings::get(cx));
        let shortcuts = commands
            .iter()
            .map(|command| command.shortcut(cx))
            .collect();
        let mut palette = Self {
            query,
            index: None,
            recent,
            recent_entries: Vec::new(),
            position: None,
            commands,
            shortcuts,
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
        self.recent_entries = locate_recent(&index, &self.recent);
        self.index = Some(index);
        self.update_choices(cx);
    }

    fn recent_entry(&self, file: usize) -> Option<&RecentEntry> {
        self.recent_entries
            .binary_search_by_key(&file, |entry| entry.0)
            .ok()
            .map(|ix| &self.recent_entries[ix])
    }

    fn command_query(&self, cx: &gpui::App) -> Option<String> {
        let text = self.query.read(cx).text();
        text.strip_prefix('>').map(str::to_owned)
    }

    fn update_choices(&mut self, cx: &mut Context<Self>) {
        self.choices = match self.command_query(cx) {
            Some(query) => commands::matching(&self.commands, &query)
                .into_iter()
                .map(Choice::Command)
                .collect(),
            None => {
                let text = self.query.read(cx).text();
                let (query, position) = fuzzy::split_position(&text);
                self.position = position;
                match &self.index {
                    Some(index) => fuzzy::match_paths(
                        index
                            .files
                            .iter()
                            .enumerate()
                            .map(|(ix, file)| fuzzy::Candidate {
                                path_lower: &file.path_lower,
                                name_start: file.name_start,
                                recent: self.recent_entry(ix).map(|entry| entry.1),
                            }),
                        within_root(index, query),
                        MAX_FILES,
                    )
                    .into_iter()
                    .map(|m| Choice::File(m.index))
                    .collect(),
                    None => Vec::new(),
                }
            }
        };
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
                        index.absolute(&index.files[*file]),
                        position,
                    ));
                }
            }
            Some(Choice::Command(command)) => {
                cx.emit(PaletteEvent::Run(
                    self.commands[*command].action.boxed_clone(),
                ));
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
                        let label = self.recent_entry(*ix).and_then(|entry| entry.2);
                        let file = &self.index.as_ref()?.files[*ix];
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
                })
            })
            .collect()
    }
}

/// Finds each recent file in the index: (file, recency, label), by file.
fn locate_recent(index: &FileIndex, recent: &[RecentFile]) -> Vec<RecentEntry> {
    let mut entries: Vec<RecentEntry> = recent
        .iter()
        .enumerate()
        .filter_map(|(recency, file)| {
            let relative = file.path.strip_prefix(&index.root).ok()?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            let ix = index
                .files
                .binary_search_by(|file| (*file.path).cmp(relative.as_str()))
                .ok()?;
            Some((ix, recency as u32, file.label))
        })
        .collect();
    entries.sort_unstable_by_key(|entry| entry.0);
    entries.dedup_by_key(|entry| entry.0);
    entries
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
            locate_recent(&index, &recent),
            [(0, 2, None), (2, 0, Some("open"))]
        );
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
