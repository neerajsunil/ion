//! Find and replace in the active file (Ctrl+F / Ctrl+H).

use editor::{Editor, EditorEvent};
use gpui::{
    AppContext, ClickEvent, Context, Entity, EventEmitter, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, WeakEntity, Window, actions, div, prelude::FluentBuilder, px,
};

actions!(
    find_bar,
    [Dismiss, FindPrevious, ToggleCaseSensitive, ReplaceAll]
);

const CONTEXT: &str = "FindBar";

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        // Shift+Enter in the query goes backwards. Bound on the input itself
        // so it takes precedence over the editor's own Shift+Enter.
        KeyBinding::new("shift-enter", FindPrevious, Some("FindBar > Editor")),
        KeyBinding::new("alt-c", ToggleCaseSensitive, Some(CONTEXT)),
        KeyBinding::new("ctrl-alt-enter", ReplaceAll, Some(CONTEXT)),
    ]
}

pub enum FindBarEvent {
    Dismissed,
}

pub struct FindBar {
    query: Entity<Editor>,
    replacement: Entity<Editor>,
    show_replace: bool,
    case_sensitive: bool,
    target: Option<WeakEntity<Editor>>,
    _subscriptions: [Subscription; 1],
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| Editor::single_line("Find", cx));
        let replacement = cx.new(|cx| Editor::single_line("Replace", cx));
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let EditorEvent::Edited = event {
                this.search(true, cx);
            }
        });
        Self {
            query,
            replacement,
            show_replace: false,
            case_sensitive: false,
            target: None,
            _subscriptions: [subscription],
        }
    }

    /// Opens the bar for `target`, seeding the query from the selection.
    pub fn show(
        &mut self,
        target: Entity<Editor>,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = target.read(cx).selected_text();
        self.retarget(target, cx);
        self.show_replace = replace;
        if !selected.is_empty() && !selected.contains('\n') {
            self.query
                .update(cx, |query, cx| query.set_text(&selected, cx));
        } else {
            self.query
                .update(cx, |query, cx| query.set_text(&query.text(), cx));
        }
        let field = if replace && !selected.is_empty() {
            &self.replacement
        } else {
            &self.query
        };
        window.focus(&field.focus_handle(cx));
        cx.notify();
    }

    /// Points the bar at another editor (the active tab changed).
    pub fn retarget(&mut self, target: Entity<Editor>, cx: &mut Context<Self>) {
        if let Some(old) = self.target.take().and_then(|old| old.upgrade())
            && old != target
        {
            old.update(cx, |editor, cx| editor.clear_search(cx));
        }
        self.target = Some(target.downgrade());
        self.search(false, cx);
    }

    /// Clears highlights in the target; call when the bar closes.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.target.take().and_then(|target| target.upgrade()) {
            target.update(cx, |editor, cx| editor.clear_search(cx));
        }
    }

    fn target(&self) -> Option<Entity<Editor>> {
        self.target.as_ref().and_then(WeakEntity::upgrade)
    }

    fn search(&mut self, jump: bool, cx: &mut Context<Self>) {
        let Some(target) = self.target() else {
            return;
        };
        let query = self.query.read(cx).text();
        let case_sensitive = self.case_sensitive;
        target.update(cx, |editor, cx| {
            editor.set_search(&query, case_sensitive, cx);
            if jump {
                editor.select_nearest_match(cx);
            }
        });
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(target) = self.target() {
            target.update(cx, |editor, cx| editor.select_match(forward, cx));
            cx.notify();
        }
    }

    fn replace_one(&mut self, cx: &mut Context<Self>) {
        let replacement = self.replacement.read(cx).text();
        if let Some(target) = self.target() {
            target.update(cx, |editor, cx| editor.replace_match(&replacement, cx));
            cx.notify();
        }
    }

    fn replace_all(&mut self, _: &ReplaceAll, _: &mut Window, cx: &mut Context<Self>) {
        let replacement = self.replacement.read(cx).text();
        if let Some(target) = self.target() {
            target.update(cx, |editor, cx| editor.replace_all(&replacement, cx));
            cx.notify();
        }
    }

    fn toggle_case(&mut self, _: &ToggleCaseSensitive, _: &mut Window, cx: &mut Context<Self>) {
        self.case_sensitive = !self.case_sensitive;
        self.search(false, cx);
    }

    /// Enter: next match in the query field, replace in the replace field.
    fn confirm(&mut self, _: &editor::Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.replacement.focus_handle(cx).is_focused(window) {
            self.replace_one(cx);
        } else {
            self.step(true, cx);
        }
    }

    fn status(&self, cx: &gpui::App) -> SharedString {
        let query_empty = self.query.read(cx).text().is_empty();
        match self
            .target()
            .and_then(|target| target.read(cx).search_position())
        {
            _ if query_empty => "".into(),
            Some((_, 0)) | None => "No results".into(),
            Some((0, total)) => format!("{total} results").into(),
            Some((current, total)) => format!("{current} of {total}").into(),
        }
    }
}

fn button(id: &'static str, label: &'static str, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(22.))
        .min_w(px(22.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .text_color(if active {
            theme::text()
        } else {
            theme::text_muted()
        })
        .when(active, |button| button.bg(theme::active_bg()))
        .hover(|button| button.bg(theme::hover_bg()).text_color(theme::text()))
        .child(label)
}

fn field(input: Entity<Editor>) -> gpui::Div {
    div()
        .w(px(280.))
        .h(px(26.))
        .rounded_sm()
        .bg(theme::bg())
        .border_1()
        .border_color(theme::border())
        .child(input)
}

impl Render for FindBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status(cx);
        div()
            .key_context(CONTEXT)
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .bg(theme::panel_bg())
            .border_b_1()
            .border_color(theme::border())
            .text_xs()
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(Self::toggle_case))
            .on_action(cx.listener(Self::replace_all))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(FindBarEvent::Dismissed)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(field(self.query.clone()))
                    .child(
                        button("case", "Aa", self.case_sensitive).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.toggle_case(&ToggleCaseSensitive, window, cx)
                            },
                        )),
                    )
                    .child(
                        div()
                            .min_w(px(80.))
                            .px_2()
                            .text_color(theme::text_muted())
                            .child(status),
                    )
                    .child(
                        button("previous", "↑", false).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.step(false, cx)),
                        ),
                    )
                    .child(
                        button("next", "↓", false).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.step(true, cx)),
                        ),
                    )
                    .child(
                        button("toggle-replace", "Replace", self.show_replace).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.show_replace = !this.show_replace;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(div().flex_1())
                    .child(button("close-find", "×", false).on_click(
                        cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(FindBarEvent::Dismissed)),
                    )),
            )
            .when(self.show_replace, |bar| {
                bar.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(field(self.replacement.clone()))
                        .child(button("replace-one", "Replace", false).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.replace_one(cx)),
                        ))
                        .child(
                            button("replace-all", "Replace All", false).on_click(cx.listener(
                                |this, _: &ClickEvent, window, cx| {
                                    this.replace_all(&ReplaceAll, window, cx)
                                },
                            )),
                        ),
                )
            })
    }
}
