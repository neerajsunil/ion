//! Small building blocks used everywhere: buttons, tooltips, shortcut chips.

use gpui::{
    Action, AnyView, App, AppContext, Context, Div, ElementId, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::icons::{IconName, icon_sized};

/// A square, borderless icon button. Add `.tooltip(ui::tooltip(..))`.
pub fn icon_button(id: impl Into<ElementId>, name: IconName) -> Stateful<Div> {
    icon_button_sized(id, name, 26., 16., false)
}

/// An icon button that shows whether something is on (a panel is open, a
/// view is selected).
pub fn toggle_icon_button(id: impl Into<ElementId>, name: IconName, on: bool) -> Stateful<Div> {
    icon_button_sized(id, name, 26., 16., on)
}

/// A smaller icon button for dense rows (tree, git changes).
pub fn small_icon_button(id: impl Into<ElementId>, name: IconName) -> Stateful<Div> {
    icon_button_sized(id, name, 22., 14., false)
}

fn icon_button_sized(
    id: impl Into<ElementId>,
    name: IconName,
    box_size: f32,
    icon: f32,
    on: bool,
) -> Stateful<Div> {
    let color = if on {
        theme::text()
    } else {
        theme::text_muted()
    };
    div()
        .id(id)
        .group("icon-button")
        .flex_none()
        .size(px(box_size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .when(on, |button| button.bg(theme::active_bg()))
        .hover(|button| button.bg(theme::hover_bg()))
        .child(
            icon_sized(name, px(icon), color)
                .group_hover("icon-button", |icon| icon.text_color(theme::text())),
        )
}

/// The prominent call to action (Commit, Open Folder).
pub fn primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(6.))
        .bg(theme::accent())
        .text_color(theme::on_accent())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|button| button.opacity(0.9))
        .child(label.into())
}

/// A quiet button with a border.
pub fn secondary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(6.))
        .border_1()
        .border_color(theme::border())
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|button| button.bg(theme::hover_bg()))
        .child(label.into())
}

/// A keyboard shortcut chip, e.g. "Ctrl+P".
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(theme::border())
        .text_size(theme::ui_font_size_small())
        .text_color(theme::text_muted())
        .child(text.into())
}

/// Text for the main binding of an action, e.g. "Ctrl+Shift+P".
pub fn shortcut(action: &dyn Action, cx: &App) -> Option<SharedString> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let binding = keymap.bindings_for_action(action).next()?;
    Some(keystrokes_text(binding.keystrokes()))
}

/// "Ctrl+K Ctrl+S" for a binding's keystrokes.
pub fn keystrokes_text(keystrokes: &[gpui::KeybindingKeystroke]) -> SharedString {
    let strokes: Vec<String> = keystrokes
        .iter()
        .map(|stroke| {
            let modifiers = stroke.modifiers();
            let mut text = String::new();
            if modifiers.control {
                text.push_str("Ctrl+");
            }
            if modifiers.platform {
                text.push_str(if cfg!(target_os = "macos") {
                    "Cmd+"
                } else {
                    "Win+"
                });
            }
            if modifiers.alt {
                text.push_str("Alt+");
            }
            if modifiers.shift {
                text.push_str("Shift+");
            }
            text.push_str(&key_name(stroke.key()));
            text
        })
        .collect();
    strokes.join(" ").into()
}

fn key_name(key: &str) -> String {
    match key {
        "escape" => "Esc".into(),
        "enter" => "Enter".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Del".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// A tooltip with a label and, optionally, a shortcut chip.
pub struct Tooltip {
    label: SharedString,
    shortcut: Option<SharedString>,
}

impl Tooltip {
    pub fn new(label: impl Into<SharedString>, shortcut: Option<SharedString>) -> Self {
        Self {
            label: label.into(),
            shortcut,
        }
    }
}

/// A tooltip builder for `.tooltip(..)`: the label, plus the shortcut of
/// `action` if it has one.
pub fn tooltip(
    label: impl Into<SharedString>,
    action: Option<Box<dyn Action>>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let label = label.into();
    move |_, cx| {
        let shortcut = action.as_deref().and_then(|action| shortcut(action, cx));
        cx.new(|_| Tooltip::new(label.clone(), shortcut)).into()
    }
}

/// A plain multi-line text tooltip.
pub fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip::new(text.clone(), None)).into()
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(420.))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::border())
            .rounded(px(6.))
            .shadow_md()
            .font_family(theme::ui_font())
            .text_size(theme::ui_font_size_small())
            .text_color(theme::text())
            .child(
                div().flex().flex_col().children(
                    self.label
                        .lines()
                        .map(|line| div().min_h(px(14.)).child(line.to_owned())),
                ),
            )
            .when_some(self.shortcut.clone(), |tip, shortcut| {
                tip.child(kbd(shortcut))
            })
    }
}
