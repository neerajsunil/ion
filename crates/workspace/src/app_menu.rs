//! The macOS menu bar. Windows has no menu bar; there the same commands live
//! in the title bar's app menu.

use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

use crate::workspace::*;

actions!(app, [Hide, HideOthers, ShowAll, Minimize, Zoom]);

/// The standard macOS window shortcuts.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]
}

/// Sets the menu bar. Call after binding keys: menu items show their
/// shortcuts from the keymap.
pub fn init_app_menu(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            window
                .update(cx, |_, window, _| window.minimize_window())
                .ok();
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.zoom_window()).ok();
        }
    });
    cx.set_menus(menus());
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Ion".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Ion", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Ion", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New File", NewFile),
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::action("Connect to SSH…", ConnectSsh),
                MenuItem::separator(),
                MenuItem::action("Save", editor::Save),
                MenuItem::action("Save As…", SaveAs),
                MenuItem::separator(),
                MenuItem::action("Reopen Closed Tab", ReopenClosedTab),
                MenuItem::action("Close Tab", CloseTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", editor::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", editor::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", editor::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", editor::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", editor::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", editor::SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Find", Find),
                MenuItem::action("Replace", FindReplace),
                MenuItem::action("Find in Files", SearchProject),
                MenuItem::separator(),
                MenuItem::action("Toggle Comment", editor::ToggleComment),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Command Palette", CommandPalette),
                MenuItem::action("Go to File…", ToggleFileFinder),
                MenuItem::action("Go to Line…", GoToLine),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Explorer", ShowFiles),
                MenuItem::action("Source Control", ShowGit),
                MenuItem::action("Agents", ShowAgents),
                MenuItem::action("Problems", ShowProblems),
                MenuItem::separator(),
                MenuItem::action("Split Right", SplitRight),
                MenuItem::action("Split Down", SplitDown),
                MenuItem::action("Toggle Word Wrap", editor::ToggleWordWrap),
                MenuItem::action("Markdown Preview", OpenMarkdownPreview),
                MenuItem::separator(),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Reset Zoom", ResetZoom),
            ],
        },
        Menu {
            name: "Terminal".into(),
            items: vec![
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("Toggle Terminal", ToggleTerminal),
                MenuItem::separator(),
                MenuItem::action("Run…", RunTask),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        },
    ]
}
