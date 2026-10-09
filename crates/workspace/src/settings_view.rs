//! The Settings tab: pages in a side menu, a search box that looks through
//! every page, and a live control for each setting. Changes are saved to
//! settings.json right away.

mod shortcuts;

use std::rc::Rc;

use editor::{Editor, EditorEvent};
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle,
    Focusable, FontWeight, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px,
};
use settings::{
    AutoSave, CursorStyle, LineNumbers, LineSpacing, Settings, Side, ThemeChoice, WordWrap,
};
use ui::{IconName, Menu, MenuEntry};

use shortcuts::Shortcuts;

/// A page in the side menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    General,
    Editor,
    Files,
    Terminal,
    Git,
    Shortcuts,
}

impl Page {
    const ALL: [Page; 6] = [
        Page::General,
        Page::Editor,
        Page::Files,
        Page::Terminal,
        Page::Git,
        Page::Shortcuts,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Editor => "Editor",
            Page::Files => "Files",
            Page::Terminal => "Terminal",
            Page::Git => "Git",
            Page::Shortcuts => "Keyboard Shortcuts",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Page::General => IconName::Settings,
            Page::Editor => IconName::FileCode,
            Page::Files => IconName::Save,
            Page::Terminal => IconName::SquareTerminal,
            Page::Git => IconName::GitBranch,
            Page::Shortcuts => IconName::Keyboard,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Page::General => "Theme, layout and what Ion does when it starts.",
            Page::Editor => "How code looks and behaves while you type.",
            Page::Files => "Saving, and tidying files when you save.",
            Page::Terminal => "Your shell, and which shells and agents the + menu offers.",
            Page::Git => "What Ion shows about your changes.",
            Page::Shortcuts => {
                "Click a shortcut to record a new one. Esc cancels, and chords like \
                 Ctrl+K Ctrl+S can be set in settings.json."
            }
        }
    }
}

/// Opens Settings at a page.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct OpenSettingsAt(pub Page);

pub struct SettingsView {
    focus_handle: FocusHandle,
    page: Page,
    search: Entity<Editor>,
    font_input: Entity<Editor>,
    shells: Vec<String>,
    agents: Vec<&'static str>,
    shortcuts: Shortcuts,
    menu: Option<Menu>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SettingsView {
    // Typing right away searches.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

/// One setting: where it lives, what it's called and its control.
struct Row {
    page: Page,
    group: &'static str,
    title: SharedString,
    description: SharedString,
    /// Extra words search should find it by.
    keywords: &'static str,
    control: AnyElement,
}

/// Every word of `query` appears somewhere in `text`.
fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| text.contains(word))
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let current = settings::get(cx).editor_font_family.clone();
        let font_input = cx.new(|cx| {
            let mut input = Editor::single_line(theme::mono_font(), cx);
            input.set_text(&current, cx);
            input
        });
        let search = cx.new(|cx| Editor::single_line("Search settings", cx));
        let subscriptions = vec![
            cx.subscribe(&font_input, |_, input, event, cx| {
                if let EditorEvent::Edited = event {
                    let family = input.read(cx).text().trim().to_owned();
                    settings::update(cx, |s| s.editor_font_family = family);
                }
            }),
            cx.subscribe(&search, |this, _, event, cx| {
                if let EditorEvent::Edited = event {
                    this.shortcuts.stop_recording();
                    cx.notify();
                }
            }),
            // Shortcut labels follow the keymap.
            cx.observe_global::<Settings>(|_, cx| cx.notify()),
        ];
        Self {
            focus_handle: cx.focus_handle(),
            page: Page::General,
            search,
            font_input,
            shells: terminal::available_shells()
                .into_iter()
                .map(|shell| shell.name)
                .collect(),
            agents: terminal::available_harnesses()
                .into_iter()
                .map(|harness| harness.kind.name())
                .collect(),
            shortcuts: Shortcuts::new(cx),
            menu: None,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn show_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.search.update(cx, |search, cx| search.set_text("", cx));
        self.shortcuts.stop_recording();
        cx.notify();
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).text().trim().to_owned()
    }
}

// ---- controls ---------------------------------------------------------------

fn group_title(title: impl Into<SharedString>) -> Div {
    let title: SharedString = title.into();
    div()
        .mt_6()
        .mb_1()
        .text_size(theme::ui_font_size_small())
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_faint())
        .child(title.to_uppercase())
}

fn render_row(title: SharedString, description: SharedString, control: AnyElement) -> Div {
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

fn option_button(id: impl Into<ElementId>, label: SharedString, on: bool) -> Stateful<Div> {
    div()
        .id(id)
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
}

fn framed() -> Div {
    div()
        .rounded(px(7.))
        .border_1()
        .border_color(theme::border())
}

impl SettingsView {
    /// A few short options side by side; the selected one is highlighted.
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: Vec<(&'static str, T)>,
        selected: T,
        apply: fn(&mut Settings, T),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        framed()
            .flex()
            .gap(px(2.))
            .p(px(2.))
            .children(options.into_iter().enumerate().map(|(ix, (label, value))| {
                option_button((id, ix), label.into(), value == selected).on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| settings::update(cx, |s| apply(s, value)),
                ))
            }))
            .into_any_element()
    }

    /// A button showing the current choice that opens a menu of all of
    /// them, for lists too long to show side by side.
    fn dropdown<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        options: Vec<(SharedString, T)>,
        selected: T,
        apply: impl Fn(&mut Settings, T) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let apply = Rc::new(apply);
        let label = options
            .iter()
            .find(|(_, value)| *value == selected)
            .map(|(label, _)| label.clone())
            .unwrap_or_default();
        framed()
            .id(id)
            .h(px(30.))
            .min_w(px(160.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(|button| button.bg(theme::hover_bg()))
            .child(div().flex_1().child(label))
            .child(ui::icon_sized(
                IconName::ChevronDown,
                px(14.),
                theme::text_muted(),
            ))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let entries = options
                    .iter()
                    .map(|(label, value)| {
                        let (value, apply) = (*value, apply.clone());
                        let entry = MenuEntry::item(label.clone(), move |_, _, cx| {
                            settings::update(cx, |s| apply(s, value))
                        });
                        if value == selected {
                            entry.icon(IconName::Check)
                        } else {
                            entry
                        }
                    })
                    .collect();
                this.menu = Some(Menu::new(event.position(), entries).min_width(px(200.)));
                cx.notify();
            }))
            .into_any_element()
    }

    /// − value +
    fn stepper(
        &self,
        id: &'static str,
        value: u32,
        range: (u32, u32),
        apply: fn(&mut Settings, u32),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let button =
            |ix: usize, name: IconName, next: u32| -> Stateful<Div> {
                ui::small_icon_button((id, ix), name).on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| settings::update(cx, |s| apply(s, next)),
                ))
            };
        framed()
            .h(px(30.))
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .child(button(
                0,
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
            .child(button(1, IconName::Plus, (value + 1).min(range.1)))
            .into_any_element()
    }

    fn switch(
        &self,
        id: impl Into<ElementId>,
        on: bool,
        toggle: impl Fn(&mut Settings, bool) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                    settings::update(cx, |s| toggle(s, !on))
                }),
            )
            .into_any_element()
    }

    /// Shows or hides a shell or agent in the + menu.
    fn menu_switch(
        &self,
        id: ElementId,
        name: SharedString,
        s: &Settings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = s.shows_terminal(&name);
        self.switch(
            id,
            shown,
            move |s, show| {
                s.hidden_terminals
                    .retain(|hidden| !hidden.eq_ignore_ascii_case(&name));
                if !show {
                    s.hidden_terminals.push(name.to_string());
                }
            },
            cx,
        )
    }
}

// ---- pages ------------------------------------------------------------------

impl SettingsView {
    /// Every setting, in page order.
    fn rows(&self, s: &Settings, cx: &mut Context<Self>) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut add = |page: Page,
                       group: &'static str,
                       title: &str,
                       description: &str,
                       keywords: &'static str,
                       control: AnyElement| {
            rows.push(Row {
                page,
                group,
                title: SharedString::from(title.to_owned()),
                description: SharedString::from(description.to_owned()),
                keywords,
                control,
            })
        };

        // General
        add(
            Page::General,
            "Appearance",
            "Theme",
            "System follows your computer's light or dark mode.",
            "dark light color mode",
            self.segmented(
                "theme",
                vec![
                    ("Dark", ThemeChoice::Dark),
                    ("Light", ThemeChoice::Light),
                    ("System", ThemeChoice::System),
                ],
                s.theme,
                |s, v| s.theme = v,
                cx,
            ),
        );
        add(
            Page::General,
            "Appearance",
            "Interface text size",
            "Sidebar, tabs, menus and status bar.",
            "font ui zoom",
            self.stepper(
                "ui-size",
                s.ui_font_size,
                (10, 20),
                |s, v| s.ui_font_size = v,
                cx,
            ),
        );
        add(
            Page::General,
            "Layout",
            "Sidebar position",
            "Which side the files, search and Git panel sit on.",
            "explorer panel left right",
            self.segmented(
                "sidebar-side",
                vec![("Left", Side::Left), ("Right", Side::Right)],
                s.sidebar_side,
                |s, v| s.sidebar_side = v,
                cx,
            ),
        );
        add(
            Page::General,
            "Startup",
            "Reopen last project",
            "Start where you left off, with your files and terminals.",
            "restore session startup",
            self.switch(
                "restore-session",
                s.restore_session,
                |s, v| s.restore_session = v,
                cx,
            ),
        );
        add(
            Page::General,
            "Startup",
            "Desktop notifications",
            "Tell me when an agent finishes or needs input while Ion is in the background.",
            "notify alert agent done",
            self.switch(
                "desktop-notifications",
                s.desktop_notifications,
                |s, v| s.desktop_notifications = v,
                cx,
            ),
        );

        // Editor
        add(
            Page::Editor,
            "Text",
            "Font",
            "Any installed monospace font. Leave empty for the default.",
            "family typeface monospace",
            framed()
                .w(px(220.))
                .h(px(30.))
                .child(self.font_input.clone())
                .into_any_element(),
        );
        add(
            Page::Editor,
            "Text",
            "Text size",
            "",
            "font size zoom",
            self.stepper(
                "editor-size",
                s.editor_font_size,
                (8, 32),
                |s, v| s.editor_font_size = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Text",
            "Line spacing",
            "Room between lines.",
            "line height density",
            self.segmented(
                "line-spacing",
                vec![
                    ("Compact", LineSpacing::Compact),
                    ("Normal", LineSpacing::Normal),
                    ("Relaxed", LineSpacing::Relaxed),
                ],
                s.line_spacing,
                |s, v| s.line_spacing = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Text",
            "Word wrap",
            "Wrap long lines at the window edge. Alt+Z toggles it for one file.",
            "wrap lines soft",
            self.segmented(
                "word-wrap",
                vec![
                    ("Off", WordWrap::Off),
                    ("Text files", WordWrap::Prose),
                    ("On", WordWrap::On),
                ],
                s.word_wrap,
                |s, v| s.word_wrap = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Display",
            "Line numbers",
            "Relative counts lines up and down from the cursor.",
            "gutter relative vim",
            self.segmented(
                "line-numbers",
                vec![
                    ("On", LineNumbers::On),
                    ("Relative", LineNumbers::Relative),
                    ("Off", LineNumbers::Off),
                ],
                s.line_numbers,
                |s, v| s.line_numbers = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Display",
            "Highlight current line",
            "",
            "cursor line band",
            self.switch(
                "highlight-line",
                s.highlight_current_line,
                |s, v| s.highlight_current_line = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Display",
            "Cursor style",
            "",
            "caret block bar underline",
            self.segmented(
                "cursor-style",
                vec![
                    ("Bar", CursorStyle::Bar),
                    ("Block", CursorStyle::Block),
                    ("Underline", CursorStyle::Underline),
                ],
                s.cursor_style,
                |s, v| s.cursor_style = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Typing",
            "Indent with",
            "What Tab inserts.",
            "tabs spaces indentation",
            self.segmented(
                "indent",
                vec![("Spaces", true), ("Tabs", false)],
                s.indent_with_spaces,
                |s, v| s.indent_with_spaces = v,
                cx,
            ),
        );
        add(
            Page::Editor,
            "Typing",
            "Tab size",
            "How wide an indent is.",
            "indent width spaces",
            self.stepper("tab-size", s.tab_size, (1, 8), |s, v| s.tab_size = v, cx),
        );
        add(
            Page::Editor,
            "Typing",
            "Close brackets and quotes",
            "Typing ( adds the ) for you.",
            "auto pair autoclose parentheses",
            self.switch(
                "auto-close",
                s.auto_close_brackets,
                |s, v| s.auto_close_brackets = v,
                cx,
            ),
        );

        // Files
        add(
            Page::Files,
            "Saving",
            "Auto save",
            "Save files without pressing Ctrl+S.",
            "autosave save",
            self.dropdown(
                "auto-save",
                vec![
                    ("Off".into(), AutoSave::Off),
                    ("After a short delay".into(), AutoSave::AfterDelay),
                    ("When you switch away".into(), AutoSave::OnFocusChange),
                ],
                s.auto_save,
                |s, v| s.auto_save = v,
                cx,
            ),
        );
        add(
            Page::Files,
            "When saving",
            "Trim trailing whitespace",
            "Remove spaces at the ends of lines. The line you're on is left alone.",
            "whitespace format clean",
            self.switch(
                "trim-whitespace",
                s.trim_trailing_whitespace,
                |s, v| s.trim_trailing_whitespace = v,
                cx,
            ),
        );
        add(
            Page::Files,
            "When saving",
            "End with a newline",
            "Add a line break at the end of the file if it's missing.",
            "final newline eof format",
            self.switch(
                "final-newline",
                s.insert_final_newline,
                |s, v| s.insert_final_newline = v,
                cx,
            ),
        );

        // Terminal
        let mut shells: Vec<(SharedString, usize)> = vec![("System default".into(), 0)];
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
        add(
            Page::Terminal,
            "Terminal",
            "Default shell",
            "Used by new terminals.",
            "powershell bash zsh cmd wsl fish",
            self.shell_dropdown(shells, shell_ix, shell_names, cx),
        );
        add(
            Page::Terminal,
            "Terminal",
            "Text size",
            "",
            "font size",
            self.stepper(
                "terminal-size",
                s.terminal_font_size,
                (8, 32),
                |s, v| s.terminal_font_size = v,
                cx,
            ),
        );
        add(
            Page::Terminal,
            "Terminal",
            "Copy on select",
            "Selecting text copies it, like most Linux terminals.",
            "clipboard selection",
            self.switch(
                "copy-on-select",
                s.terminal_copy_on_select,
                |s, v| s.terminal_copy_on_select = v,
                cx,
            ),
        );
        for (ix, agent) in self.agents.iter().enumerate() {
            let control = self.menu_switch(("agent", ix).into(), (*agent).into(), s, cx);
            add(
                Page::Terminal,
                "Agents in the + menu",
                agent,
                "",
                "new agent menu hide show",
                control,
            );
        }
        for (ix, shell) in self.shells.iter().enumerate() {
            let control = self.menu_switch(("shell", ix).into(), shell.clone().into(), s, cx);
            add(
                Page::Terminal,
                "Shells in the + menu",
                shell,
                "",
                "new terminal menu hide show",
                control,
            );
        }

        // Git
        add(
            Page::Git,
            "Editor",
            "Line blame",
            "Who last changed the cursor line, in the status bar.",
            "blame author annotate",
            self.switch("git-blame", s.git_blame, |s, v| s.git_blame = v, cx),
        );
        add(
            Page::Git,
            "Editor",
            "Change markers",
            "Added and changed lines marked next to the line numbers.",
            "gutter diff decorations",
            self.switch("git-gutter", s.git_gutter, |s, v| s.git_gutter = v, cx),
        );
        rows
    }

    fn shell_dropdown(
        &self,
        options: Vec<(SharedString, usize)>,
        selected: usize,
        names: Vec<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 0 is the system default, then the shells in order.
        self.dropdown(
            "shell",
            options,
            selected,
            move |s, ix| {
                s.terminal_shell = ix
                    .checked_sub(1)
                    .and_then(|ix| names.get(ix).cloned())
                    .unwrap_or_default();
            },
            cx,
        )
    }

    fn render_rows(rows: Vec<Row>, show_page: bool) -> Vec<AnyElement> {
        let mut elements = Vec::new();
        let mut last: Option<(Page, &'static str)> = None;
        for row in rows {
            if last != Some((row.page, row.group)) {
                let heading = if show_page && last.map(|(page, _)| page) != Some(row.page) {
                    row.page.title()
                } else if show_page {
                    ""
                } else {
                    row.group
                };
                if !heading.is_empty() {
                    elements.push(group_title(heading).into_any_element());
                }
                last = Some((row.page, row.group));
            }
            elements.push(render_row(row.title, row.description, row.control).into_any_element());
        }
        elements
    }

    fn render_nav(&self, query: &str, cx: &mut Context<Self>) -> Div {
        let searching = !query.is_empty();
        div()
            .w(px(220.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p_3()
            .bg(theme::panel_bg())
            .border_r_1()
            .border_color(theme::border())
            .child(
                framed()
                    .mb_3()
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(theme::bg())
                    .child(ui::icon_sized(
                        IconName::Search,
                        px(14.),
                        theme::text_muted(),
                    ))
                    .child(div().flex_1().min_w_0().child(self.search.clone())),
            )
            .children(Page::ALL.into_iter().map(|page| {
                let on = !searching && page == self.page;
                div()
                    .id(("settings-page", page as usize))
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(if on {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .when(on, |item| item.bg(theme::active_bg()))
                    .when(!on, |item| item.hover(|item| item.bg(theme::hover_bg())))
                    .child(ui::icon_sized(
                        page.icon(),
                        px(15.),
                        if on {
                            theme::text()
                        } else {
                            theme::text_muted()
                        },
                    ))
                    .child(page.title())
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.show_page(page, cx)),
                    )
            }))
            .child(div().flex_1())
            .child(
                div()
                    .id("open-settings-json")
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(theme::text_muted())
                    .hover(|item| item.bg(theme::hover_bg()))
                    .child(ui::icon_sized(
                        IconName::FileCode,
                        px(15.),
                        theme::text_muted(),
                    ))
                    .child("Open settings.json")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(crate::OpenSettingsFile), cx)
                    }),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = settings::get(cx).clone();
        let query = self.query(cx);
        let searching = !query.is_empty();

        let (title, description): (SharedString, SharedString) = if searching {
            (
                format!("Results for “{query}”").into(),
                SharedString::default(),
            )
        } else {
            (self.page.title().into(), self.page.description().into())
        };

        let mut body: Vec<AnyElement> = Vec::new();
        if searching || self.page != Page::Shortcuts {
            let rows: Vec<Row> = self
                .rows(&s, cx)
                .into_iter()
                .filter(|row| {
                    if searching {
                        matches(
                            &query,
                            &format!(
                                "{} {} {} {} {}",
                                row.page.title(),
                                row.group,
                                row.title,
                                row.description,
                                row.keywords
                            ),
                        )
                    } else {
                        row.page == self.page
                    }
                })
                .collect();
            body.extend(Self::render_rows(rows, searching));
        }
        if searching || self.page == Page::Shortcuts {
            let filter = searching.then_some(query.as_str());
            body.extend(self.shortcuts.render(filter, searching, cx));
        }
        if searching && body.is_empty() {
            body.push(
                div()
                    .mt_6()
                    .text_color(theme::text_muted())
                    .child("No settings or shortcuts match.")
                    .into_any_element(),
            );
        }

        let this = cx.entity().downgrade();
        let menu = self.menu.as_ref().map(|menu| {
            ui::render_menu(
                menu,
                move |_, cx| {
                    this.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    })
                    .ok();
                },
                window,
            )
        });

        div()
            .id("settings")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .bg(theme::bg())
            .child(self.render_nav(&query, cx))
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .max_w(px(760.))
                            .mx_auto()
                            .px_8()
                            .py_6()
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .when(!description.is_empty(), |page| {
                                page.child(
                                    div()
                                        .mt_1()
                                        .text_color(theme::text_muted())
                                        .child(description),
                                )
                            })
                            .children(body),
                    ),
            )
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_needs_every_word() {
        assert!(matches("tab size", "Editor Typing Tab size How wide"));
        assert!(matches("SIZE", "Tab size"));
        assert!(!matches("tab font", "Tab size"));
    }
}
