//! The Keyboard Shortcuts page: every command and its shortcut. Click one to
//! record a new shortcut; changes go to `keybindings` in settings.json.

use std::collections::HashMap;

use gpui::{
    Action, AnyElement, App, ClickEvent, Context, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Subscription, div,
    prelude::FluentBuilder, px,
};
use ui::IconName;

use super::{SettingsView, framed, group_title, matches};

/// Most shortcut matches shown in search results across all pages.
const MAX_SEARCH_RESULTS: usize = 30;

struct Entry {
    category: SharedString,
    label: SharedString,
    name: &'static str,
    action: Box<dyn Action>,
    /// The built-in shortcut, if there is one.
    default: Option<SharedString>,
}

pub(super) struct Shortcuts {
    entries: Vec<Entry>,
    /// The action waiting for its new shortcut.
    recording: Option<&'static str>,
    _recorder: Option<Subscription>,
}

/// "AddNextOccurrence" → "Add Next Occurrence".
fn words(name: &str) -> String {
    let mut text = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            text.push(' ');
        }
        text.push(if c == '_' { ' ' } else { c });
    }
    text
}

/// A category for an action that isn't in the command palette, from its
/// namespace: `file_tree::Copy` → "File Tree".
fn category(namespace: &str) -> String {
    match namespace {
        "app" => "Window".into(),
        namespace => namespace
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

impl Shortcuts {
    pub(super) fn new(cx: &mut Context<SettingsView>) -> Self {
        let defaults = crate::keymap::defaults();
        let mut default_keys: HashMap<&'static str, SharedString> = HashMap::new();
        for binding in &defaults {
            default_keys
                .entry(binding.action().name())
                .or_insert_with(|| ui::keystrokes_text(binding.keystrokes()));
        }

        let mut entries: Vec<Entry> = Vec::new();
        // Palette commands first, with their friendly names. Actions that
        // can't be built by name (like starting a specific agent) can't be
        // rebound.
        for command in
            crate::commands::all_for_workspace(false, &[], &settings::Settings::default())
        {
            let name = command.action.name();
            if cx.build_action(name, None).is_err() || entries.iter().any(|e| e.name == name) {
                continue;
            }
            entries.push(Entry {
                category: command.category.into(),
                label: command.label.trim_end_matches('…').to_owned().into(),
                name,
                action: command.action,
                default: default_keys.get(name).cloned(),
            });
        }
        // Then everything else that has a shortcut.
        for binding in defaults {
            let name = binding.action().name();
            if entries.iter().any(|e| e.name == name) {
                continue;
            }
            let (namespace, action) = name.rsplit_once("::").unwrap_or(("", name));
            entries.push(Entry {
                category: category(namespace).into(),
                label: words(action).into(),
                name,
                action: binding.action().boxed_clone(),
                default: default_keys.get(name).cloned(),
            });
        }
        // Group by category, in the order categories first appear.
        let mut order: Vec<SharedString> = Vec::new();
        for entry in &entries {
            if !order.contains(&entry.category) {
                order.push(entry.category.clone());
            }
        }
        entries.sort_by_key(|entry| order.iter().position(|c| *c == entry.category));

        Self {
            entries,
            recording: None,
            _recorder: None,
        }
    }

    pub(super) fn stop_recording(&mut self) {
        self.recording = None;
        self._recorder = None;
    }

    /// Records the next keystroke made while `settings_focus` (the Settings
    /// tab) has focus.
    fn start_recording(
        &mut self,
        name: &'static str,
        settings_focus: FocusHandle,
        cx: &mut Context<SettingsView>,
    ) {
        let view = cx.entity().downgrade();
        // Interceptors run before the keymap, so the keys being recorded
        // don't also run whatever they're bound to now.
        let recorder = cx.intercept_keystrokes(move |event, window, cx| {
            // Moved on to another tab or window: stop, and let the key through.
            if !settings_focus.contains_focused(window, cx) {
                view.update(cx, |view, cx| {
                    view.shortcuts.stop_recording();
                    cx.notify();
                })
                .ok();
                return;
            }
            let keystroke = &event.keystroke;
            let key = keystroke.key.as_str();
            if matches!(
                key,
                "shift" | "control" | "alt" | "platform" | "function" | "cmd" | "ctrl" | "fn"
            ) {
                return;
            }
            cx.stop_propagation();
            let cancel = key == "escape" && !keystroke.modifiers.modified();
            let keys = keystroke.unparse();
            view.update(cx, |view, cx| {
                view.shortcuts.stop_recording();
                cx.notify();
            })
            .ok();
            if !cancel {
                settings::update(cx, |s| {
                    s.keybindings.insert(name.to_owned(), keys);
                });
            }
        });
        self.recording = Some(name);
        self._recorder = Some(recorder);
        cx.notify();
    }

    /// The page's rows. `filter` narrows them to a search; `in_search` puts
    /// them under a "Keyboard Shortcuts" heading among other results.
    pub(super) fn render(
        &self,
        filter: Option<&str>,
        in_search: bool,
        cx: &mut Context<SettingsView>,
    ) -> Vec<AnyElement> {
        let overrides = settings::get(cx).keybindings.clone();
        let current: Vec<Option<SharedString>> = self
            .entries
            .iter()
            .map(|entry| ui::shortcut(entry.action.as_ref(), cx))
            .collect();
        let mut users: HashMap<&str, Vec<usize>> = HashMap::new();
        for (ix, keys) in current.iter().enumerate() {
            if let Some(keys) = keys {
                users.entry(keys.as_ref()).or_default().push(ix);
            }
        }

        let shown: Vec<usize> = (0..self.entries.len())
            .filter(|&ix| {
                let entry = &self.entries[ix];
                filter.is_none_or(|query| {
                    matches(
                        query,
                        &format!(
                            "{} {} {} {}",
                            entry.category,
                            entry.label,
                            current[ix].as_ref().map_or("", |keys| keys.as_ref()),
                            entry.default.as_ref().map_or("", |keys| keys.as_ref())
                        ),
                    )
                })
            })
            .take(if in_search {
                MAX_SEARCH_RESULTS
            } else {
                usize::MAX
            })
            .collect();

        let mut elements = Vec::new();
        if in_search {
            if !shown.is_empty() {
                elements.push(group_title("Keyboard Shortcuts").into_any_element());
            }
        } else if !overrides.is_empty() {
            elements.push(
                div()
                    .mt_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().text_color(theme::text_muted()).child(
                        match overrides.len() {
                            1 => "You've changed 1 shortcut.".to_owned(),
                            n => format!("You've changed {n} shortcuts."),
                        },
                    ))
                    .child(
                        ui::secondary_button("reset-all-shortcuts", "Reset All").on_click(
                            cx.listener(|_, _: &ClickEvent, _, cx| {
                                settings::update(cx, |s| s.keybindings.clear())
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }

        let mut last_category: Option<&SharedString> = None;
        for ix in shown {
            let entry = &self.entries[ix];
            if !in_search && last_category != Some(&entry.category) {
                elements.push(group_title(entry.category.clone()).into_any_element());
                last_category = Some(&entry.category);
            }
            let changed = overrides.contains_key(entry.name);
            let conflicts: Vec<SharedString> = match (&current[ix], changed) {
                (Some(keys), true) => users[keys.as_ref()]
                    .iter()
                    .filter(|&&other| other != ix)
                    .map(|&other| self.entries[other].label.clone())
                    .collect(),
                _ => Vec::new(),
            };
            elements.push(self.render_entry(
                ix,
                entry,
                current[ix].clone(),
                changed,
                conflicts,
                in_search,
                cx,
            ));
        }
        elements
    }

    #[allow(clippy::too_many_arguments)]
    fn render_entry(
        &self,
        ix: usize,
        entry: &Entry,
        current: Option<SharedString>,
        changed: bool,
        conflicts: Vec<SharedString>,
        in_search: bool,
        cx: &mut Context<SettingsView>,
    ) -> AnyElement {
        let name = entry.name;
        let recording = self.recording == Some(name);
        let has_default = entry.default.is_some();
        let small = |text: SharedString| {
            div()
                .text_size(theme::ui_font_size_small())
                .text_color(theme::text_muted())
                .child(text)
        };

        let keys = if recording {
            framed()
                .id(("shortcut-keys", ix))
                .h(px(26.))
                .px_2()
                .flex()
                .items_center()
                .border_color(theme::accent())
                .text_size(theme::ui_font_size_small())
                .text_color(theme::text())
                .child("Press a shortcut…")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.shortcuts.stop_recording();
                    cx.notify();
                }))
        } else {
            div()
                .id(("shortcut-keys", ix))
                .h(px(26.))
                .px_1()
                .flex()
                .items_center()
                .rounded(px(5.))
                .cursor_pointer()
                .hover(|keys| keys.bg(theme::hover_bg()))
                .child(match current.clone() {
                    Some(keys) => ui::kbd(keys).into_any_element(),
                    None => div()
                        .px_1()
                        .text_size(theme::ui_font_size_small())
                        .text_color(theme::text_faint())
                        .child("Add shortcut")
                        .into_any_element(),
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let focus = this.focus_handle.clone();
                    window.focus(&focus);
                    this.shortcuts.start_recording(name, focus, cx)
                }))
        };

        div()
            .py_2()
            .flex()
            .items_center()
            .gap_4()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_color(theme::text()).child(if in_search {
                        SharedString::from(format!("{}: {}", entry.category, entry.label))
                    } else {
                        entry.label.clone()
                    }))
                    .when(changed, |text| {
                        text.child(small(match &entry.default {
                            Some(keys) => format!("Default: {keys}").into(),
                            None => "Added by you".into(),
                        }))
                    })
                    .when(!conflicts.is_empty(), |text| {
                        text.child(
                            div()
                                .text_size(theme::ui_font_size_small())
                                .text_color(theme::git_modified())
                                .child(format!("Also used by {}", conflicts.join(", "))),
                        )
                    }),
            )
            .child(div().flex_none().child(keys))
            .child(
                div()
                    .flex_none()
                    .w(px(52.))
                    .flex()
                    .justify_end()
                    .gap_1()
                    .when(changed, |buttons| {
                        buttons.child(
                            ui::small_icon_button(("shortcut-reset", ix), IconName::Undo2)
                                .tooltip(ui::text_tooltip("Reset to default"))
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    settings::update(cx, |s| {
                                        s.keybindings.remove(name);
                                    })
                                })),
                        )
                    })
                    .when(current.is_some(), |buttons| {
                        buttons.child(
                            ui::small_icon_button(("shortcut-remove", ix), IconName::X)
                                .tooltip(ui::text_tooltip("Remove shortcut"))
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    remove(name, has_default, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// Removes a shortcut. A built-in one is turned off with an empty entry;
/// one the user added is simply forgotten.
fn remove(name: &'static str, has_default: bool, cx: &mut App) {
    settings::update(cx, |s| {
        if has_default {
            s.keybindings.insert(name.to_owned(), String::new());
        } else {
            s.keybindings.remove(name);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_actions_for_people() {
        assert_eq!(words("AddNextOccurrence"), "Add Next Occurrence");
        assert_eq!(category("file_tree"), "File Tree");
        assert_eq!(category("app"), "Window");
    }
}
