//! The window layout: sidebar, tabs, editor area, terminal panel and status bar.

mod agents;
mod auto_save;
mod branch_picker;
mod chrome;
mod commands;
mod diff_view;
mod file_ops;
mod find_bar;
mod fs_sync;
mod git_panel;
mod git_state;
mod ide;
mod image_view;
mod markdown_preview;
mod notify;
mod palette;
mod pane;
mod pane_ops;
mod pane_view;
mod persist;
mod problems;
mod project_search;
mod prompt;
mod remote_paths;
mod remote_window;
mod run;
mod send;
mod session;
mod settings_view;
mod ssh_view;
mod workspace;

pub use ide::{init as init_ide, refresh_folders as refresh_ide_folders};
pub use workspace::*;

/// Key bindings for the workspace and the panels it owns.
pub fn all_key_bindings() -> Vec<gpui::KeyBinding> {
    let mut bindings = key_bindings();
    bindings.extend(find_bar::key_bindings());
    bindings.extend(prompt::key_bindings());
    bindings.extend(git_panel::key_bindings());
    bindings.extend(branch_picker::key_bindings());
    bindings.extend(palette::key_bindings());
    bindings.extend(ssh_view::key_bindings());
    bindings
}
