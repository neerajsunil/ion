//! A small modal asking for one line of text (new file name, rename...).

use editor::Editor;
use gpui::{
    AppContext, Context, Entity, EventEmitter, Focusable, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div, px,
};

actions!(input_prompt, [Dismiss]);

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Dismiss, Some("InputPrompt"))]
}

pub enum InputPromptEvent {
    Confirmed(String),
    Dismissed,
}

pub struct InputPrompt {
    title: SharedString,
    input: Entity<Editor>,
}

impl EventEmitter<InputPromptEvent> for InputPrompt {}

impl InputPrompt {
    /// `selection` is the char range of `initial` to select, e.g. the file
    /// stem when renaming so the extension is kept.
    pub fn new(
        title: impl Into<SharedString>,
        initial: &str,
        selection: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = Editor::single_line("", cx);
            input.set_text(initial, cx);
            input.select_range((0, selection.start), (0, selection.end), cx);
            input
        });
        window.focus(&input.focus_handle(cx));
        Self {
            title: title.into(),
            input,
        }
    }
}

impl Render for InputPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("InputPrompt")
            .w(px(420.))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::border())
            .rounded_lg()
            .shadow_lg()
            .on_action(cx.listener(|this, _: &editor::Newline, _, cx| {
                let text = this.input.read(cx).text().trim().to_owned();
                if !text.is_empty() {
                    cx.emit(InputPromptEvent::Confirmed(text));
                }
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(InputPromptEvent::Dismissed)))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .h(px(30.))
                    .rounded_sm()
                    .bg(theme::bg())
                    .border_1()
                    .border_color(theme::accent())
                    .child(self.input.clone()),
            )
    }
}
