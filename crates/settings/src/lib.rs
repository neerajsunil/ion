//! User settings: `settings.json` in the config folder, applied live.
//!
//! The settings page writes through [`update`]; editing the file by hand
//! works too, since the file is watched and reloaded.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{App, AppContext, AsyncApp, Global, WindowAppearance};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
    /// Follow the Windows (or macOS) app mode.
    System,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoSave {
    #[default]
    Off,
    /// A second after you stop typing.
    AfterDelay,
    /// When you switch to another tab or window.
    OnFocusChange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordWrap {
    Off,
    On,
    /// Markdown and plain text wrap, code doesn't.
    #[default]
    Prose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub ui_font_size: u32,
    /// Editor and terminal font; empty uses the platform default.
    pub editor_font_family: String,
    pub editor_font_size: u32,
    /// Spaces per indent (Tab inserts this many) and the width of tab characters.
    pub tab_size: u32,
    pub auto_save: AutoSave,
    /// Long lines wrap at the window edge (Alt+Z toggles per file).
    pub word_wrap: WordWrap,
    /// Shell for new terminals: a name from the shell list ("Git Bash") or a
    /// program. Empty picks PowerShell 7, then Windows PowerShell.
    pub terminal_shell: String,
    pub terminal_font_size: u32,
    /// Who last changed the cursor line, in the status bar.
    pub git_blame: bool,
    /// Added and changed lines marked in the editor gutter.
    pub git_gutter: bool,
    /// A desktop notification when a terminal asks for attention (an agent
    /// finished or needs input) while Ion is in the background.
    pub desktop_notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            ui_font_size: 13,
            editor_font_family: String::new(),
            editor_font_size: 14,
            tab_size: 4,
            auto_save: AutoSave::Off,
            word_wrap: WordWrap::Prose,
            terminal_shell: String::new(),
            terminal_font_size: 14,
            git_blame: true,
            git_gutter: true,
            desktop_notifications: true,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Tab size kept within a sensible range.
    pub fn tab_size(&self) -> usize {
        self.tab_size.clamp(1, 16) as usize
    }

    /// Pushes colors and fonts into the theme.
    fn apply(&self, cx: &App) {
        let appearance = match self.theme {
            ThemeChoice::Dark => theme::Appearance::Dark,
            ThemeChoice::Light => theme::Appearance::Light,
            ThemeChoice::System => match cx.window_appearance() {
                WindowAppearance::Light | WindowAppearance::VibrantLight => {
                    theme::Appearance::Light
                }
                WindowAppearance::Dark | WindowAppearance::VibrantDark => theme::Appearance::Dark,
            },
        };
        theme::set_appearance(appearance);
        theme::set_font_sizes(
            self.ui_font_size,
            self.editor_font_size,
            self.terminal_font_size,
        );
        theme::set_mono_font(Some(self.editor_font_family.clone()));
    }
}

pub fn get(cx: &App) -> &Settings {
    cx.global::<Settings>()
}

/// Changes settings, applies them everywhere and saves the file.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut settings = get(cx).clone();
    change(&mut settings);
    if settings == *get(cx) {
        return;
    }
    let snapshot = settings.clone();
    set(settings, cx);
    cx.background_spawn(async move { save(&snapshot).ok() })
        .detach();
}

fn set(settings: Settings, cx: &mut App) {
    settings.apply(cx);
    cx.set_global(settings);
    cx.refresh_windows();
}

/// Re-applies the theme, e.g. after the system switched light/dark mode.
pub fn reapply(cx: &mut App) {
    get(cx).apply(cx);
    cx.refresh_windows();
}

/// Loads settings, applies them and starts watching the file for edits.
pub fn init(cx: &mut App) {
    let settings = load();
    set(settings, cx);
    watch(cx);
}

pub fn settings_file() -> Option<PathBuf> {
    Some(config_dir()?.join("settings.json"))
}

fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(base.join("Ion"))
}

/// The saved settings, or defaults if there are none. Unknown or invalid
/// fields fall back to their defaults instead of failing the whole file.
pub fn load() -> Settings {
    settings_file()
        .and_then(|path| std::fs::read(path).ok())
        .map(|bytes| parse(&bytes))
        .unwrap_or_default()
}

fn parse(bytes: &[u8]) -> Settings {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        // Keep the fields that are valid.
        let Ok(serde_json::Value::Object(fields)) = serde_json::from_slice(bytes) else {
            return Settings::default();
        };
        let mut settings = serde_json::to_value(Settings::default()).expect("settings serialize");
        for (key, value) in fields {
            let mut candidate = settings.clone();
            candidate[&key] = value;
            if serde_json::from_value::<Settings>(candidate.clone()).is_ok() {
                settings = candidate;
            }
        }
        serde_json::from_value(settings).unwrap_or_default()
    })
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let Some(path) = settings_file() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    // Write a temporary file and rename it, so a crash can't leave half a file.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, json + "\n")?;
    std::fs::rename(temp, path)
}

/// Reloads settings when the file changes (edited by hand, or by another
/// Ion window).
fn watch(cx: &mut App) {
    let Some(path) = settings_file() else {
        return;
    };
    let Some(dir) = path.parent().map(PathBuf::from) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let (tx, mut rx) = unbounded();
    let file = path.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event.paths.contains(&file)
        {
            let _ = tx.unbounded_send(());
        }
    });
    let Ok(mut watcher) = watcher else {
        return;
    };
    if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
        return;
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        // The watcher lives as long as this task.
        let _watcher = watcher;
        while rx.next().await.is_some() {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            while rx.try_recv().is_ok() {}
            let settings = cx.background_spawn(async { load() }).await;
            let applied = cx.update(|cx| {
                if settings != *get(cx) {
                    set(settings, cx);
                }
            });
            if applied.is_err() {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_bad_fields_fall_back_to_defaults() {
        let settings = parse(br#"{"tab_size": 2, "theme": "nonsense", "git_blame": false}"#);
        assert_eq!(settings.tab_size, 2);
        assert_eq!(settings.theme, ThemeChoice::Dark);
        assert!(!settings.git_blame);
        assert_eq!(settings.editor_font_size, 14);
        assert_eq!(parse(b"not json"), Settings::default());
    }

    #[test]
    fn round_trips() {
        let settings = Settings {
            theme: ThemeChoice::Light,
            auto_save: AutoSave::AfterDelay,
            ..Default::default()
        };
        let json = serde_json::to_vec(&settings).unwrap();
        assert_eq!(parse(&json), settings);
    }
}
