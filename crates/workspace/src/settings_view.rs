//! The Settings tab: every setting with a live control. Changes are saved
//! to settings.json right away.

use editor::{Editor, EditorEvent};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};
use settings::{AutoSave, Settings, ThemeChoice, WordWrap};
use ui::IconName;

pub struct SettingsView {
    focus_handle: FocusHandle,
    font_input: Entity<Editor>,
    shells: Vec<String>,
    _subscription: Subscription,
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let current = settings::get(cx).editor_font_family.clone();
        let font_input = cx.new(|cx| {
            let mut input = Editor::single_line(theme::mono_font(), cx);
            input.set_text(&current, cx);
            input
        });
        let subscription = cx.subscribe(&font_input, |_, input, event, cx| {
            if let EditorEvent::Edited = event {
                let family = input.read(cx).text().trim().to_owned();
                settings::update(cx, |s| s.editor_font_family = family);
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            font_input,
            shells: terminal::available_shells()
                .into_iter()
                .map(|shell| shell.name)
                .collect(),
            _subscription: subscription,
        }
    }
}

fn section(title: &'static str) -> Div {
    div()
        .mt_8()
        .mb_1()
        .text_size(theme::ui_font_size_small())
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_faint())
        .child(title.to_uppercase())
}

fn row(title: &'static str, description: &'static str, control: impl IntoElement) -> Div {
    div()
        .py_3()
        .flex()
        .items_center()
        .gap_6()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_color(theme::text())
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .when(!description.is_empty(), |text| {
                    text.child(
                        div()
                            .text_size(theme::ui_font_size_small())
                            .text_color(theme::text_muted())
                            .child(description),
                    )
                }),
        )
        .child(div().flex_none().child(control))
}

impl SettingsView {
    /// A row of options; the selected one is highlighted.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: Vec<(SharedString, T)>,
        selected: T,
        apply: fn(&mut Settings, T),
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(7.))
            .border_1()
            .border_color(theme::border())
            .children(options.into_iter().enumerate().map(|(ix, (label, value))| {
                let on = value == selected;
                div()
                    .id((id, ix))
                    .h(px(26.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_color(if on {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .when(on, |option| option.bg(theme::active_bg()))
                    .when(!on, |option| {
                        option.hover(|option| option.bg(theme::hover_bg()))
                    })
                    .child(label)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        settings::update(cx, |s| apply(s, value))
                    }))
            }))
    }

    /// − value +
    fn stepper(
        &self,
        id: &'static str,
        value: u32,
        range: (u32, u32),
        apply: fn(&mut Settings, u32),
        cx: &mut Context<Self>,
    ) -> Div {
        let button = |suffix: &'static str, name: IconName, next: u32| -> Stateful<Div> {
            ui::small_icon_button((id, if suffix == "-" { 0usize } else { 1 }), name).on_click(
                cx.listener(move |_, _: &ClickEvent, _, cx| {
                    settings::update(cx, |s| apply(s, next))
                }),
            )
        };
        div()
            .h(px(30.))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .rounded(px(7.))
            .border_1()
            .border_color(theme::border())
            .child(button(
                "-",
                IconName::Minus,
                value.saturating_sub(1).max(range.0),
            ))
            .child(
                div()
                    .w(px(32.))
                    .flex()
                    .justify_center()
                    .child(value.to_string()),
            )
            .child(button("+", IconName::Plus, (value + 1).min(range.1)))
    }

    fn switch(
        &self,
        id: &'static str,
        on: bool,
        apply: fn(&mut Settings, bool),
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .w(px(36.))
            .h(px(20.))
            .rounded_full()
            .relative()
            .cursor_pointer()
            .bg(if on {
                theme::accent()
            } else {
                theme::text_faint()
            })
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(if on { 18. } else { 2. }))
                    .size(px(16.))
                    .rounded_full()
                    .bg(gpui::white()),
            )
            .on_click(
                cx.listener(move |_, _: &ClickEvent, _, cx| {
                    settings::update(cx, |s| apply(s, !on))
                }),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = settings::get(cx).clone();
        let mut shells: Vec<(SharedString, usize)> = vec![("Default".into(), 0)];
        shells.extend(
            self.shells
                .iter()
                .enumerate()
                .map(|(ix, name)| (SharedString::from(name.clone()), ix + 1)),
        );
        let shell_ix = self
            .shells
            .iter()
            .position(|name| name.eq_ignore_ascii_case(s.terminal_shell.trim()))
            .map_or(0, |ix| ix + 1);
        let shell_names = self.shells.clone();
        let shell_picker = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .max_w(px(420.))
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(7.))
            .border_1()
            .border_color(theme::border())
            .children(shells.into_iter().map(|(label, ix)| {
                let on = ix == shell_ix;
                let names = shell_names.clone();
                div()
                    .id(("shell", ix))
                    .h(px(26.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_color(if on {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .when(on, |option| option.bg(theme::active_bg()))
                    .when(!on, |option| {
                        option.hover(|option| option.bg(theme::hover_bg()))
                    })
                    .child(label)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        let name = ix
                            .checked_sub(1)
                            .map(|ix| names[ix].clone())
                            .unwrap_or_default();
                        settings::update(cx, |s| s.terminal_shell = name)
                    }))
            }));

        div()
            .id("settings")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .bg(theme::bg())
            .child(
                div()
                    .max_w(px(760.))
                    .mx_auto()
                    .px_10()
                    .py_8()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_size(px(22.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Settings"),
                                    )
                                    .child(div().mt_1().text_color(theme::text_muted()).child(
                                        "Changes apply right away and are saved to settings.json.",
                                    )),
                            )
                            .child(
                                ui::secondary_button("open-settings-json", "Open settings.json")
                                    .on_click(|_, window, cx| {
                                        window
                                            .dispatch_action(Box::new(crate::OpenSettingsFile), cx)
                                    }),
                            ),
                    )
                    .child(section("Appearance"))
                    .child(row(
                        "Theme",
                        "System follows the Windows light or dark app mode.",
                        self.segmented(
                            "theme",
                            vec![
                                ("Dark".into(), ThemeChoice::Dark),
                                ("Light".into(), ThemeChoice::Light),
                                ("System".into(), ThemeChoice::System),
                            ],
                            s.theme,
                            |s, v| s.theme = v,
                            cx,
                        ),
                    ))
                    .child(row(
                        "Interface text size",
                        "Sidebar, tabs, menus and status bar.",
                        self.stepper(
                            "ui-size",
                            s.ui_font_size,
                            (10, 20),
                            |s, v| s.ui_font_size = v,
                            cx,
                        ),
                    ))
                    .child(row(
                        "Editor font",
                        "Any installed monospace font. Empty uses the default.",
                        div()
                            .w(px(220.))
                            .h(px(30.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(theme::border())
                            .child(self.font_input.clone()),
                    ))
                    .child(row(
                        "Editor text size",
                        "",
                        self.stepper(
                            "editor-size",
                            s.editor_font_size,
                            (8, 32),
                            |s, v| s.editor_font_size = v,
                            cx,
                        ),
                    ))
                    .child(section("Editor"))
                    .child(row(
                        "Tab size",
                        "Spaces inserted by Tab, and the width of tab characters.",
                        self.stepper("tab-size", s.tab_size, (1, 8), |s, v| s.tab_size = v, cx),
                    ))
                    .child(row(
                        "Auto save",
                        "Save files without pressing Ctrl+S.",
                        self.segmented(
                            "auto-save",
                            vec![
                                ("Off".into(), AutoSave::Off),
                                ("After a delay".into(), AutoSave::AfterDelay),
                                ("On focus change".into(), AutoSave::OnFocusChange),
                            ],
                            s.auto_save,
                            |s, v| s.auto_save = v,
                            cx,
                        ),
                    ))
                    .child(row(
                        "Word wrap",
                        "Wrap long lines at the window edge. Alt+Z toggles it for one file.",
                        self.segmented(
                            "word-wrap",
                            vec![
                                ("Off".into(), WordWrap::Off),
                                ("Text files".into(), WordWrap::Prose),
                                ("On".into(), WordWrap::On),
                            ],
                            s.word_wrap,
                            |s, v| s.word_wrap = v,
                            cx,
                        ),
                    ))
                    .child(section("Terminal"))
                    .child(row("Shell", "Used by new terminals.", shell_picker))
                    .child(row(
                        "Desktop notifications",
                        "Notify when an agent finishes or needs input while Ion is in the background.",
                        self.switch(
                            "desktop-notifications",
                            s.desktop_notifications,
                            |s, v| s.desktop_notifications = v,
                            cx,
                        ),
                    ))
                    .child(row(
                        "Terminal text size",
                        "",
                        self.stepper(
                            "terminal-size",
                            s.terminal_font_size,
                            (8, 32),
                            |s, v| s.terminal_font_size = v,
                            cx,
                        ),
                    ))
                    .child(section("Git"))
                    .child(row(
                        "Line blame",
                        "Who last changed the cursor line, in the status bar.",
                        self.switch("git-blame", s.git_blame, |s, v| s.git_blame = v, cx),
                    ))
                    .child(row(
                        "Change markers",
                        "Added and changed lines marked next to the line numbers.",
                        self.switch("git-gutter", s.git_gutter, |s, v| s.git_gutter = v, cx),
                    )),
            )
    }
}
