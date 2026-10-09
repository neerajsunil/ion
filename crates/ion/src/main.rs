// Release builds on Windows are GUI apps; dev builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod login_path;

use std::path::PathBuf;

use gpui::{App, AppContext, Application, Focusable};
use workspace::Workspace;

fn main() {
    // `ssh` runs this program to ask login questions (see `remote::askpass_main`).
    if let Some(code) = remote::askpass_main() {
        std::process::exit(code);
    }
    #[cfg(target_os = "macos")]
    login_path::restart_with_login_path();

    // `ion <folder>` or `ion <file>` opens it right away.
    let open_path = std::env::args_os().nth(1).map(PathBuf::from);

    Application::new()
        .with_assets(ui::Assets)
        .run(move |cx: &mut App| {
            settings::init(cx);
            workspace::init_keymap(cx);
            if cfg!(target_os = "macos") {
                workspace::init_app_menu(cx);
            }
            // Claude Code's /ide connects to Ion through this.
            workspace::init_ide(cx);

            // Quit when the last window closes (the default on Windows and Linux).
            cx.on_window_closed(|cx| {
                workspace::refresh_ide_folders(cx);
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let window = cx
                .open_window(workspace::window_options(cx), |window, cx| {
                    cx.new(|cx| Workspace::new(window, cx))
                })
                .expect("failed to open the main window");

            window
                .update(cx, |workspace, window, cx| {
                    window.focus(&workspace.focus_handle(cx));
                    match open_path {
                        Some(path) => workspace.open_path(path, window, cx),
                        None if settings::get(cx).restore_session => {
                            workspace.restore_session(window, cx);
                        }
                        None => {}
                    }
                })
                .ok();
            cx.activate(true);
        });
}
