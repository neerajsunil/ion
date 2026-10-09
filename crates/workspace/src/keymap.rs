//! The keymap: every panel's default shortcuts, with the changes from
//! `keybindings` in settings.json on top. Rebuilt whenever those change.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate};
use settings::Settings;

/// Every default binding. Later bindings win ties, so panel bindings come
/// after the editor's.
pub(crate) fn defaults() -> Vec<KeyBinding> {
    let mut bindings = editor::key_bindings();
    bindings.extend(crate::all_key_bindings());
    bindings.extend(file_tree::key_bindings());
    bindings.extend(terminal::key_bindings());
    if cfg!(target_os = "macos") {
        bindings.extend(crate::app_menu::key_bindings());
    }
    bindings
}

/// Binds the keys and keeps them in step with settings.json.
pub fn init_keymap(cx: &mut App) {
    apply(cx);
    let mut last = settings::get(cx).keybindings.clone();
    cx.observe_global::<Settings>(move |cx| {
        let current = &settings::get(cx).keybindings;
        if *current != last {
            last = current.clone();
            apply(cx);
        }
    })
    .detach();
}

fn apply(cx: &mut App) {
    let overrides = settings::get(cx).keybindings.clone();
    let bindings = with_overrides(defaults(), &overrides, |name| {
        cx.build_action(name, None).ok()
    });
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

/// The defaults minus every action the user rebound, plus the user's keys
/// for those actions. A rebound action keeps the contexts its defaults had,
/// so a new editor shortcut still only works in the editor. An empty value
/// removes the shortcut.
fn with_overrides(
    defaults: Vec<KeyBinding>,
    overrides: &BTreeMap<String, String>,
    build: impl Fn(&str) -> Option<Box<dyn Action>>,
) -> Vec<KeyBinding> {
    let mut contexts: BTreeMap<&str, Vec<Option<Rc<KeyBindingContextPredicate>>>> = BTreeMap::new();
    let mut bindings = Vec::new();
    for binding in defaults {
        let name = binding.action().name();
        if overrides.contains_key(name) {
            let predicates = contexts.entry(name).or_default();
            let predicate = binding.predicate();
            if !predicates.contains(&predicate) {
                predicates.push(predicate);
            }
        } else {
            bindings.push(binding);
        }
    }
    for (name, keys) in overrides {
        let keys = keys.trim();
        if keys.is_empty() {
            continue;
        }
        let predicates = contexts.remove(name.as_str()).unwrap_or_else(|| vec![None]);
        for predicate in predicates {
            let Some(action) = build(name) else {
                break;
            };
            match KeyBinding::load(keys, action, predicate, false, None, &DummyKeyboardMapper) {
                Ok(binding) => bindings.push(binding),
                Err(_) => break,
            }
        }
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CloseTab, NextTab, ToggleTerminal};

    fn build(name: &str) -> Option<Box<dyn Action>> {
        match name {
            "workspace::ToggleTerminal" => Some(Box::new(ToggleTerminal)),
            "workspace::NextTab" => Some(Box::new(NextTab)),
            _ => None,
        }
    }

    fn keys_for(bindings: &[KeyBinding], name: &str) -> Vec<String> {
        bindings
            .iter()
            .filter(|binding| binding.action().name() == name)
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|stroke| stroke.unparse())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn overrides_replace_remove_and_keep_context() {
        let defaults = vec![
            KeyBinding::new("ctrl-`", ToggleTerminal, Some("Workspace")),
            KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
            KeyBinding::new("ctrl-w", CloseTab, Some("Workspace")),
        ];
        let overrides = BTreeMap::from([
            ("workspace::ToggleTerminal".to_owned(), "ctrl-j".to_owned()),
            ("workspace::NextTab".to_owned(), String::new()),
            ("nothing::Here".to_owned(), "ctrl-q".to_owned()),
            ("workspace::CloseTab".to_owned(), "not a key!!".to_owned()),
        ]);
        let bindings = with_overrides(defaults, &overrides, build);
        assert_eq!(keys_for(&bindings, "workspace::ToggleTerminal"), ["ctrl-j"]);
        assert!(keys_for(&bindings, "workspace::NextTab").is_empty());
        assert!(keys_for(&bindings, "workspace::CloseTab").is_empty());
        let terminal = bindings
            .iter()
            .find(|binding| binding.action().name() == "workspace::ToggleTerminal")
            .unwrap();
        assert!(terminal.predicate().is_some());
    }
}
