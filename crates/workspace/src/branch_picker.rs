//! Switching branches (click the branch in the status bar).

use editor::{Editor, EditorEvent};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, EventEmitter, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, ScrollStrategy, Stateful,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, Window, actions,
    div, prelude::FluentBuilder, px, uniform_list,
};

actions!(branch_picker, [Dismiss]);

const CONTEXT: &str = "BranchPicker";
const ROW_HEIGHT: f32 = 28.;
const MAX_VISIBLE_ROWS: usize = 12;

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Dismiss, Some(CONTEXT))]
}

pub enum BranchPickerEvent {
    Switch { name: String, create: bool },
    Dismissed,
}

enum Choice {
    Branch(usize),
    Create(String),
}

pub struct BranchPicker {
    query: Entity<Editor>,
    branches: Vec<String>,
    current: Option<String>,
    choices: Vec<Choice>,
    selected: usize,
    scroll_handle: UniformListScrollHandle,
    _query_subscription: Subscription,
}

impl EventEmitter<BranchPickerEvent> for BranchPicker {}

impl BranchPicker {
    pub fn new(
        branches: Vec<String>,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| Editor::single_line("Switch to branch, or type a new name", cx));
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let EditorEvent::Edited = event {
                this.update_choices(cx);
            }
        });
        window.focus(&query.focus_handle(cx));
        let mut picker = Self {
            query,
            branches,
            current,
            choices: Vec::new(),
            selected: 0,
            scroll_handle: UniformListScrollHandle::new(),
            _query_subscription: subscription,
        };
        picker.update_choices(cx);
        picker
    }

    fn update_choices(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_owned();
        let lower = query.to_lowercase();
        self.choices = self
            .branches
            .iter()
            .enumerate()
            .filter(|(_, name)| name.to_lowercase().contains(&lower))
            .map(|(ix, _)| Choice::Branch(ix))
            .collect();
        let exists = self.branches.contains(&query);
        if !query.is_empty() && !exists && !query.contains(char::is_whitespace) {
            self.choices.push(Choice::Create(query));
        }
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
        let event = match self.choices.get(ix) {
            Some(Choice::Branch(branch)) => {
                let name = self.branches[*branch].clone();
                if Some(&name) == self.current.as_ref() {
                    BranchPickerEvent::Dismissed
                } else {
                    BranchPickerEvent::Switch {
                        name,
                        create: false,
                    }
                }
            }
            Some(Choice::Create(name)) => BranchPickerEvent::Switch {
                name: name.clone(),
                create: true,
            },
            None => return,
        };
        cx.emit(event);
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        range
            .filter_map(|ix| {
                let (label, note) = match self.choices.get(ix)? {
                    Choice::Branch(branch) => {
                        let name = &self.branches[*branch];
                        let current = Some(name) == self.current.as_ref();
                        (name.clone(), current.then_some("current"))
                    }
                    Choice::Create(name) => (format!("Create branch “{name}”"), None),
                };
                Some(
                    div()
                        .id(ix)
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .cursor_pointer()
                        .hover(|row| row.bg(theme::hover_bg()))
                        .when(ix == self.selected, |row| row.bg(theme::active_bg()))
                        .child(div().min_w_0().truncate().child(label))
                        .children(note.map(|note| {
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::text_faint())
                                .child(note)
                        }))
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.confirm(ix, cx)),
                        ),
                )
            })
            .collect()
    }
}

impl Render for BranchPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.choices.len().min(MAX_VISIBLE_ROWS);
        div()
            .key_context(CONTEXT)
            .w(px(480.))
            .flex()
            .flex_col()
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::border())
            .rounded_lg()
            .shadow_lg()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &editor::MoveDown, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &editor::MoveUp, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &editor::Newline, _, cx| {
                let selected = this.selected;
                this.confirm(selected, cx)
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(BranchPickerEvent::Dismissed)))
            .child(
                div()
                    .h(px(40.))
                    .px_2()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(self.query.clone()),
            )
            .map(|picker| {
                if self.choices.is_empty() {
                    picker.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_color(theme::text_muted())
                            .child("No matching branches"),
                    )
                } else {
                    picker.child(
                        uniform_list(
                            "branch-picker",
                            self.choices.len(),
                            cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
                        )
                        .track_scroll(self.scroll_handle.clone())
                        .h(px(ROW_HEIGHT * rows as f32))
                        .py_1(),
                    )
                }
            })
    }
}
