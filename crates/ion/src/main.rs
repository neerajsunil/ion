// Release builds on Windows are GUI apps; dev builds keep the console for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use gpui::{
    App, AppContext, Application, Bounds, Focusable, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};
use workspace::Workspace;

fn main() {
    // `ssh` runs this program to ask login questions (see `remote::askpass_main`).
    if let Some(code) = remote::askpass_main() {
        std::process::exit(code);
    }

    // `ion <folder>` or `ion <file>` opens it right away.
    let open_path = std::env::args_os().nth(1).map(PathBuf::from);

    Application::new()
        .with_assets(ui::Assets)
        .run(move |cx: &mut App| {
            settings::init(cx);
            // Later bindings win ties, so panel bindings come after the editor's.
            cx.bind_keys(editor::key_bindings());
            cx.bind_keys(workspace::all_key_bindings());
            cx.bind_keys(file_tree::key_bindings());
            cx.bind_keys(terminal::key_bindings());
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

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        // Ion draws its own title bar (app menu, search, controls).
                        titlebar: Some(TitlebarOptions {
                            title: Some("Ion".into()),
                            appears_transparent: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| Workspace::new(window, cx)),
                )
                .expect("failed to open the main window");

            window
                .update(cx, |workspace, window, cx| {
                    window.focus(&workspace.focus_handle(cx));
                    match open_path {
                        Some(path) => workspace.open_path(path, window, cx),
                        None => {
                            workspace.restore_session(window, cx);
                        }
                    }
                })
                .ok();
            cx.activate(true);
        });
}
