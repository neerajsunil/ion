//! The palette: find a file by name (Ctrl+P), or a command after ">"
//! (Ctrl+Shift+P).

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
    OpenFile(PathBuf),
    Run(Box<dyn Action>),
    Dismissed,
}

enum Choice {
    File(usize),
    Command(usize),
}

pub struct Palette {
    query: Entity<Editor>,
    index: Option<Arc<FileIndex>>,
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
        initial: &str,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            let mut input = Editor::single_line("Search files by name — type > for commands", cx);
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
        let commands = commands::all_for_workspace(remote);
        let shortcuts = commands
            .iter()
            .map(|command| command.shortcut(cx))
            .collect();
        let mut palette = Self {
            query,
            index,
            commands,
            shortcuts,
            choices: Vec::new(),
            selected: 0,
            scroll_handle: UniformListScrollHandle::new(),
            _query_subscription: subscription,
        };
        palette.update_choices(cx);
        palette
    }

    /// Supplies the file index when it finishes building after opening.
    pub fn set_index(&mut self, index: Arc<FileIndex>, cx: &mut Context<Self>) {
        self.index = Some(index);
        self.update_choices(cx);
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
                let query = self.query.read(cx).text();
                match &self.index {
                    Some(index) => fuzzy::match_paths(
                        index.files.iter().map(|file| fuzzy::Candidate {
                            path_lower: &file.path_lower,
                            name_start: file.name_start,
                        }),
                        &query,
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
                    cx.emit(PaletteEvent::OpenFile(index.absolute(&index.files[*file])));
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
                    Choice::File(file) => {
                        let file = &self.index.as_ref()?.files[*file];
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
