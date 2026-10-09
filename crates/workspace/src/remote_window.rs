//! Remote windows: opening them, login questions during reconnects, the
//! connection state, and commands that behave differently over SSH.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Context, Entity, PromptLevel, TitlebarOptions, Window, WindowBounds,
    WindowOptions, point, px, size,
};
use project::FileSystem;
use remote::{Connection, ConnectionState};

use crate::session::RemoteProject;
use crate::ssh_view::{PromptRequest, SshEvent, SshView};
use crate::workspace::{ConnectSsh, DisconnectSsh, Modal, RefreshProject, Workspace};

impl Workspace {
    pub(crate) fn connect_ssh(
        &mut self,
        _: &ConnectSsh,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| SshView::new(window, cx));
        self.show_ssh(view, window, cx);
    }

    /// Reconnects to a recent remote project.
    pub(crate) fn open_recent_remote(
        &mut self,
        project: RemoteProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Already connected to that host here: just switch folders.
        if let Some(connection) = self.filesystem.remote()
            && *connection.options() == project.connection
        {
            let connection = connection.clone();
            let folder = project.folder.clone();
            let check = cx.background_spawn(async move {
                FileSystem::Ssh(connection).is_dir(&folder).unwrap_or(false)
            });
            cx.spawn_in(window, async move |this, cx| {
                let is_dir = check.await;
                this.update_in(cx, |this, window, cx| {
                    if is_dir {
                        this.set_root(project.folder, window, cx);
                    } else {
                        this.status = Some("That folder no longer exists on the server".into());
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        let view = cx.new(|cx| SshView::reopen(project, window, cx));
        self.show_ssh(view, window, cx);
    }

    /// Open Folder in a remote window browses the server.
    pub(crate) fn pick_remote_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(connection) = self.filesystem.remote().cloned() else {
            return;
        };
        let current = self.root.clone();
        let view = cx.new(|cx| SshView::pick_folder(connection, current, window, cx));
        self.show_ssh(view, window, cx);
    }

    fn show_ssh(&mut self, view: Entity<SshView>, window: &mut Window, cx: &mut Context<Self>) {
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                SshEvent::Dismissed => this.dismiss_modal(window, cx),
                SshEvent::Connected(connection, root) => {
                    let (connection, root) = (connection.clone(), root.clone());
                    this.dismiss_modal(window, cx);
                    let same = this
                        .filesystem
                        .remote()
                        .is_some_and(|current| Arc::ptr_eq(current, &connection));
                    if same {
                        this.set_root(root, window, cx);
                    } else {
                        Self::open_remote_window(connection, root, cx);
                    }
                }
            });
        self.set_modal(Modal::Ssh(view), subscription, cx);
    }

    fn open_remote_window(connection: Arc<Connection>, root: PathBuf, cx: &mut App) {
        let opened = cx.open_window(window_options(cx), move |window, cx| {
            cx.new(|cx| {
                let mut workspace = Workspace::new(window, cx);
                workspace.attach_remote(connection, window, cx);
                workspace.set_root(root, window, cx);
                workspace.new_terminal(&crate::NewTerminal, window, cx);
                workspace
            })
        });
        if let Err(error) = opened {
            log_error(&format!("Can't open the remote window: {error}"));
        }
    }

    /// Makes this window use `connection` for everything.
    fn attach_remote(
        &mut self,
        connection: Arc<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filesystem = FileSystem::Ssh(connection.clone());
        // Login questions during a reconnect are asked in this window.
        let (prompter, mut requests) = crate::ssh_view::prompter();
        connection.set_prompter(Some(prompter));
        self.remote_state = Some(connection.state());
        let mut states = connection.subscribe();
        self.remote_tasks = vec![
            cx.spawn_in(window, async move |this, cx| {
                while let Some(request) = requests.next().await {
                    if this
                        .update_in(cx, |this, window, cx| {
                            this.show_remote_prompt(request, window, cx)
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }),
            cx.spawn_in(window, async move |this, cx| {
                while let Some(state) = states.next().await {
                    if this
                        .update_in(cx, |this, window, cx| {
                            this.remote_state_changed(state, window, cx)
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }),
        ];
    }

    fn show_remote_prompt(
        &mut self,
        request: PromptRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.activate_window();
        let view = cx.new(|cx| SshView::prompt_only(request, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            if let SshEvent::Dismissed = event {
                this.dismiss_modal(window, cx);
            }
        });
        self.set_modal(Modal::Ssh(view), subscription, cx);
    }

    fn remote_state_changed(
        &mut self,
        state: ConnectionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_connected = self.remote_state == Some(ConnectionState::Connected);
        self.remote_state = Some(state.clone());
        // Back online: restart live updates and catch up on what changed.
        if state == ConnectionState::Connected && !was_connected {
            self.status = None;
            if let Some(root) = self.root.clone() {
                self.start_watching(&root, window, cx);
            }
            self.refresh_project(&RefreshProject, window, cx);
        }
        cx.notify();
    }

    /// The status bar's Reconnect.
    pub(crate) fn reconnect_remote(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.filesystem.remote().cloned() else {
            return;
        };
        self.remote_state = Some(ConnectionState::Reconnecting("Reconnecting".into()));
        cx.notify();
        cx.background_spawn(async move { connection.reconnect() })
            .detach();
    }

    pub(crate) fn disconnect_ssh(
        &mut self,
        _: &DisconnectSsh,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filesystem.remote().is_none() {
            return;
        }
        if self.confirm_quit(window, cx) {
            if let Some(connection) = self.filesystem.remote() {
                connection.disconnect();
            }
            window.remove_window();
        }
    }

    pub(crate) fn refresh_project(
        &mut self,
        _: &RefreshProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.refresh_loaded(cx));
        }
        self.rebuild_index(false, cx);
        self.refresh_git_status(cx);
        for editor in self.editors() {
            if let Some(path) = editor.read(cx).path().map(std::path::Path::to_path_buf) {
                self.sync_editor_with_disk(editor, path, window, cx);
            }
        }
    }

    /// Opens a local folder or file in a new window (from a remote window).
    pub(crate) fn open_local_window(path: PathBuf, cx: &mut App) {
        let opened = cx.open_window(window_options(cx), move |window, cx| {
            cx.new(|cx| {
                let mut workspace = Workspace::new(window, cx);
                workspace.open_path(path, window, cx);
                workspace
            })
        });
        if let Err(error) = opened {
            log_error(&format!("Can't open a window: {error}"));
        }
    }

    /// Quit: asks once about unsaved changes in every window.
    pub(crate) fn quit_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_session(cx);
        let mut dirty = self.items().filter(|(_, item)| item.is_dirty(cx)).count();
        let this_window = window.window_handle().window_id();
        let others: Vec<_> = cx
            .windows()
            .into_iter()
            .filter(|handle| handle.window_id() != this_window)
            .filter_map(|handle| handle.downcast::<Workspace>())
            .collect();
        for handle in &others {
            if let Ok(count) = handle.update(cx, |workspace, _, cx| {
                workspace.save_session(cx);
                workspace
                    .items()
                    .filter(|(_, item)| item.is_dirty(cx))
                    .count()
            }) {
                dirty += count;
            }
        }
        if dirty == 0 {
            cx.quit();
            return;
        }
        let windows = others.len() + 1;
        let message = format!(
            "You have unsaved changes in {dirty} file{}{}.",
            if dirty == 1 { "" } else { "s" },
            if windows > 1 {
                format!(" across {windows} windows")
            } else {
                String::new()
            }
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Quit anyway and lose them?"),
            &["Quit Without Saving", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if let Ok(0) = answer.await {
                cx.update(|cx| cx.quit()).ok();
            }
        })
        .detach();
    }
}

/// A new Ion window, centered, with Ion's own title bar.
pub fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(800.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Ion".into()),
            appears_transparent: true,
            // Centers macOS's window buttons in Ion's title bar.
            traffic_light_position: Some(point(
                px(crate::chrome::TRAFFIC_LIGHTS_X),
                px((crate::chrome::TITLE_HEIGHT - 12.) / 2.),
            )),
        }),
        ..Default::default()
    }
}

fn log_error(message: &str) {
    eprintln!("{message}");
}
