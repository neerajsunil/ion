//! The Run button: the project's scripts and build commands (found by
//! [`project::detect_tasks`]), each run in a terminal named after it.
//! Running a task again replaces its last run.

use gpui::{Context, Pixels, Point, SharedString, Window, point, px};
use project::RunTask;
use ui::{IconName, Menu, MenuEntry};

use crate::pane_ops::Launch;
use crate::workspace::{MenuKind, Workspace};

impl Workspace {
    /// Finds the project's tasks, then shows them in a menu at `position`.
    pub(crate) fn show_run_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let filesystem = self.filesystem.clone();
        cx.spawn(async move |this, cx| {
            let tasks = cx
                .background_executor()
                .spawn(async move { project::detect_tasks(&filesystem, &root) })
                .await;
            this.update(cx, |this, cx| this.open_run_menu(tasks, position, cx))
                .ok();
        })
        .detach();
    }

    /// The Run menu from the command palette, under the title bar's button.
    pub(crate) fn show_run_menu_at_title(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width = window.viewport_size().width;
        self.show_run_menu(point(width - px(360.), px(40.)), cx);
    }

    fn open_run_menu(
        &mut self,
        tasks: Vec<RunTask>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let mut entries = Vec::new();
        let mut source = None;
        for task in tasks {
            if source != Some(task.source) {
                if source.is_some() {
                    entries.push(MenuEntry::Separator);
                }
                source = Some(task.source);
                entries.push(MenuEntry::Header(task.source.into()));
            }
            let running = self.task_terminals.contains_key(&task.command);
            let label = if running {
                format!("{} (restart)", task.label)
            } else {
                task.label.clone()
            };
            let this = this.clone();
            entries.push(
                MenuEntry::item(label, move |_, window, cx| {
                    this.update(cx, |this, cx| this.run_task(task.clone(), window, cx))
                        .ok();
                })
                .icon(IconName::Play),
            );
        }
        if entries.is_empty() {
            entries.push(MenuEntry::Header(
                "No scripts found (package.json, Cargo.toml, go.mod, Makefile)".into(),
            ));
        }
        self.open_menu(MenuKind::Run, Menu::new(position, entries), cx);
    }

    /// Runs a task in its own terminal, closing the one from its last run.
    pub(crate) fn run_task(&mut self, task: RunTask, window: &mut Window, cx: &mut Context<Self>) {
        let mut pane = None;
        if let Some(id) = self.task_terminals.remove(&task.command)
            && let Some((last_pane, ix)) = self.find_item(id)
        {
            self.remove_item(last_pane, ix, window, cx);
            pane = Some(last_pane).filter(|pane| self.panes.contains_key(pane));
        }
        let pane = pane.unwrap_or_else(|| self.terminal_pane());
        let name = SharedString::from(task.label.clone());
        let item = self.terminal_item(self.root.clone(), Some(name), Launch::Default, window, cx);
        let Some(view) = item.terminal().cloned() else {
            return;
        };
        self.task_terminals.insert(task.command.clone(), item.id());
        self.insert_item(pane, item, window, cx);
        view.update(cx, |view, cx| view.run_command(&task.command, cx));
    }
}
