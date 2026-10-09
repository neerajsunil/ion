//! The window chrome: title bar with the app menu, sidebar header, welcome
//! page and status bar.

use std::path::PathBuf;

use file_tree::FileTree;
use gpui::{
    Action, AnyElement, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Point, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Window, WindowControlArea, div, point, prelude::FluentBuilder, px,
};
use ui::{IconName, Menu, MenuEntry};

use crate::diff_view::DiffTarget;
use crate::workspace::*;

pub(crate) const TITLE_HEIGHT: f32 = 38.;
pub(crate) const STATUS_HEIGHT: f32 = 26.;

impl Workspace {
    // ---- title bar -----------------------------------------------------------

    pub(crate) fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let project = self.root.as_ref().map(|root| {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string())
        });
        let toggle = |id: &'static str,
                      name: IconName,
                      label: &'static str,
                      on: bool,
                      action: Box<dyn Action>| {
            ui::toggle_icon_button(id, name, on)
                .occlude()
                .tooltip(ui::tooltip(label, Some(action.boxed_clone())))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };

        let drag_area = div()
            .id("title-drag")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .gap_1()
            .pl_2()
            .pr_1()
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .id("app-menu")
                    .occlude()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|button| button.bg(theme::hover_bg()))
                    .when(self.menu_kind == Some(MenuKind::App), |button| {
                        button.bg(theme::hover_bg())
                    })
                    .child(ui::logo(20.))
                    .child(ui::icon_sized(
                        IconName::ChevronDown,
                        px(14.),
                        theme::text_muted(),
                    ))
                    .tooltip(ui::tooltip("Menu", None))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.show_app_menu(menu_position(event, 30.), cx)
                    })),
            )
            .children(project.map(|name| {
                div()
                    .id("project-menu")
                    .occlude()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(theme::text())
                    .font_weight(FontWeight::MEDIUM)
                    .hover(|button| button.bg(theme::hover_bg()))
                    .child(name)
                    .child(ui::icon_sized(
                        IconName::ChevronDown,
                        px(14.),
                        theme::text_faint(),
                    ))
                    .tooltip(ui::tooltip("Switch project", None))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.show_project_menu(menu_position(event, 30.), cx)
                    }))
            }))
            .child(div().flex_1())
            .child(
                div()
                    .id("title-search")
                    .occlude()
                    .w(px(420.))
                    .min_w(px(160.))
                    .flex_shrink()
                    .h(px(28.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::bg())
                    .cursor_pointer()
                    .text_color(theme::text_faint())
                    .hover(|search| search.border_color(theme::text_faint()))
                    .child(ui::icon_sized(
                        IconName::Search,
                        px(14.),
                        theme::text_faint(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child("Search files and commands"),
                    )
                    .children(ui::shortcut(&ToggleFileFinder, cx).map(ui::kbd))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleFileFinder), cx)
                    }),
            )
            .child(div().flex_1())
            .when(self.root.is_some(), |bar| {
                bar.child(
                    ui::toggle_icon_button(
                        "title-run",
                        IconName::Play,
                        self.menu_kind == Some(MenuKind::Run),
                    )
                    .occlude()
                    .tooltip(ui::tooltip("Run…", Some(Box::new(RunTask))))
                    .on_click(cx.listener(
                        |this, event: &ClickEvent, _, cx| {
                            this.show_run_menu(menu_position(event, 30.), cx)
                        },
                    )),
                )
            })
            .child(toggle(
                "toggle-sidebar",
                IconName::PanelLeft,
                "Toggle Sidebar",
                self.sidebar_visible,
                Box::new(ToggleSidebar),
            ))
            .child(toggle(
                "toggle-terminal",
                IconName::PanelBottom,
                "Toggle Terminal",
                self.dock_visible,
                Box::new(ToggleTerminal),
            ));

        div()
            // Keep the workspace's focus-on-click handler from preventing
            // native title-bar dragging and caption-button actions on Windows.
            .occlude()
            .h(px(TITLE_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .bg(theme::panel_bg())
            .border_b_1()
            .border_color(theme::border())
            .child(drag_area)
            .when(cfg!(windows), |bar| bar.child(window_controls(window)))
    }

    pub(crate) fn show_app_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        use IconName as I;
        let action = |label: &'static str, icon: IconName, action: Box<dyn Action>| {
            let shortcut = ui::shortcut(action.as_ref(), cx);
            MenuEntry::item(label, move |_, window, cx| {
                window.dispatch_action(action.boxed_clone(), cx)
            })
            .icon(icon)
            .shortcut(shortcut)
        };
        let mut entries = vec![
            action("New File", I::FilePlus, Box::new(NewFile)),
            action("Open Folder…", I::FolderOpen, Box::new(OpenFolder)),
            action("Connect to SSH…", I::Monitor, Box::new(ConnectSsh)),
            action("Refresh Project", I::RefreshCw, Box::new(RefreshProject)),
            action("Save", I::Save, Box::new(editor::Save)),
            MenuEntry::Separator,
            action("Command Palette", I::Command, Box::new(CommandPalette)),
            action("Go to File…", I::Search, Box::new(ToggleFileFinder)),
            action("Find in Files", I::FolderSearch, Box::new(SearchProject)),
            MenuEntry::Separator,
            action("New Terminal", I::SquareTerminal, Box::new(NewTerminal)),
            action("Split Right", I::Columns2, Box::new(SplitRight)),
            action("Split Down", I::Rows2, Box::new(SplitDown)),
            MenuEntry::Separator,
            action("Settings", I::Settings, Box::new(OpenSettings)),
            action("Quit", I::LogOut, Box::new(Quit)),
        ];
        if self.filesystem.remote().is_some() {
            entries.insert(
                3,
                action("Disconnect SSH", I::LogOut, Box::new(DisconnectSsh)),
            );
        }
        self.open_menu(
            MenuKind::App,
            Menu::new(position, entries).min_width(px(260.)),
            cx,
        );
    }

    fn show_project_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let remote = self
            .filesystem
            .remote()
            .map(|connection| connection.options().clone());
        let recent_remote: Vec<crate::session::RemoteProject> = crate::session::read(cx)
            .recent_remote
            .iter()
            .filter(|project| {
                // A remote window lists folders on its own host.
                remote
                    .as_ref()
                    .is_none_or(|options| *options == project.connection)
                    && Some(&project.folder) != self.root.as_ref()
            })
            .take(if remote.is_some() { 8 } else { 4 })
            .cloned()
            .collect();
        let recent: Vec<PathBuf> = crate::session::read(cx)
            .recent_folders
            .iter()
            .filter(|_| remote.is_none())
            .filter(|folder| folder.is_dir() && Some(*folder) != self.root.as_ref())
            .take(8)
            .cloned()
            .collect();
        let mut entries = Vec::new();
        if !recent.is_empty() {
            entries.push(MenuEntry::Header("Recent".into()));
        }
        for folder in recent {
            let name = folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.display().to_string());
            entries.push(
                MenuEntry::item(
                    name,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_path(folder.clone(), window, cx)
                    }),
                )
                .icon(IconName::Folder),
            );
        }
        if !recent_remote.is_empty() {
            entries.push(MenuEntry::Header(
                match &remote {
                    Some(options) => format!("Recent on {}", options.host),
                    None => "Remote".into(),
                }
                .into(),
            ));
        }
        for project in recent_remote {
            let title = if remote.is_some() {
                project.folder.to_string_lossy().replace('\\', "/")
            } else {
                project.title()
            };
            entries.push(
                MenuEntry::item(
                    title,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_recent_remote(project.clone(), window, cx)
                    }),
                )
                .icon(IconName::Monitor),
            );
        }
        if !entries.is_empty() {
            entries.push(MenuEntry::Separator);
        }
        let shortcut = ui::shortcut(&OpenFolder, cx);
        entries.push(
            MenuEntry::item("Open Folder…", |_, window, cx| {
                window.dispatch_action(Box::new(OpenFolder), cx)
            })
            .icon(IconName::FolderOpen)
            .shortcut(shortcut),
        );
        self.open_menu(MenuKind::Project, Menu::new(position, entries), cx);
    }

    // ---- sidebar -------------------------------------------------------------

    pub(crate) fn render_sidebar(
        &self,
        file_tree: &Entity<FileTree>,
        cx: &mut Context<Self>,
    ) -> Div {
        let changes = self.git.as_ref().map_or(0, |git| git.status.entries.len());
        let mode_button = |id: &'static str,
                           name: IconName,
                           label: &'static str,
                           mode: SidebarMode,
                           action: Box<dyn Action>| {
            ui::toggle_icon_button(id, name, self.sidebar_mode == mode)
                .relative()
                .tooltip(ui::tooltip(label, Some(action.boxed_clone())))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        let git_button = mode_button(
            "sidebar-git",
            IconName::GitBranch,
            "Source Control",
            SidebarMode::Git,
            Box::new(ShowGit),
        )
        .when(changes > 0, |button| {
            button.child(
                div()
                    .absolute()
                    .top(px(-2.))
                    .right(px(-4.))
                    .min_w(px(15.))
                    .h(px(15.))
                    .px(px(3.))
                    .rounded_full()
                    .bg(theme::accent())
                    .text_color(theme::on_accent())
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if changes > 99 {
                        "99+".to_owned()
                    } else {
                        changes.to_string()
                    }),
            )
        });

        let mut actions: Vec<Stateful<Div>> = Vec::new();
        match self.sidebar_mode {
            SidebarMode::Files => {
                actions.push(
                    ui::icon_button("tree-new-file", IconName::FilePlus)
                        .tooltip(ui::tooltip("New File…", None))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.new_entry_in_selection(false, window, cx)
                        })),
                );
                actions.push(
                    ui::icon_button("tree-new-folder", IconName::FolderPlus)
                        .tooltip(ui::tooltip("New Folder…", None))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.new_entry_in_selection(true, window, cx)
                        })),
                );
                let tree = file_tree.clone();
                actions.push(
                    ui::icon_button("tree-collapse", IconName::ChevronsDownUp)
                        .tooltip(ui::tooltip("Collapse All", None))
                        .on_click(move |_, _, cx| {
                            tree.update(cx, |tree, cx| tree.collapse_all(cx))
                        }),
                );
            }
            SidebarMode::Git => {
                actions.push(
                    ui::icon_button("git-refresh", IconName::RefreshCw)
                        .tooltip(ui::tooltip("Refresh", None))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.refresh_git_status(cx)),
                        ),
                );
            }
            SidebarMode::Search => {}
            SidebarMode::Problems => actions.extend(self.problem_actions(cx)),
            SidebarMode::Agents => {
                actions.push(
                    ui::icon_button("agents-new", IconName::Plus)
                        .tooltip(ui::tooltip("New Agent or Terminal…", None))
                        .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                            let pane = this.terminal_pane();
                            this.show_shell_menu(pane, event.position(), cx)
                        })),
                );
            }
        }

        let title = match self.sidebar_mode {
            SidebarMode::Files => "Explorer",
            SidebarMode::Search => "Search",
            SidebarMode::Git => "Source Control",
            SidebarMode::Agents => "Agents",
            SidebarMode::Problems => "Problems",
        };
        div()
            .w(self.sidebar_width)
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::panel_bg())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .h(px(BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .child(mode_button(
                        "sidebar-files",
                        IconName::Files,
                        "Explorer",
                        SidebarMode::Files,
                        Box::new(ShowFiles),
                    ))
                    .child(mode_button(
                        "sidebar-search",
                        IconName::Search,
                        "Search",
                        SidebarMode::Search,
                        Box::new(SearchProject),
                    ))
                    .child(git_button)
                    .child(mode_button(
                        "sidebar-agents",
                        IconName::Bot,
                        "Agents",
                        SidebarMode::Agents,
                        Box::new(ShowAgents),
                    ))
                    .child(mode_button(
                        "sidebar-problems",
                        IconName::TriangleAlert,
                        "Problems",
                        SidebarMode::Problems,
                        Box::new(ShowProblems),
                    ))
                    .child(div().flex_1())
                    .children(actions),
            )
            .child(
                div()
                    .px_4()
                    .pb_1()
                    .text_size(theme::ui_font_size_small())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_faint())
                    .child(title.to_uppercase()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .map(|body| match self.sidebar_mode {
                        SidebarMode::Files => body.child(file_tree.clone()),
                        SidebarMode::Search => body.child(self.project_search.clone()),
                        SidebarMode::Git => body.child(self.git_panel.clone()),
                        SidebarMode::Agents => body.child(self.render_agents(cx)),
                        SidebarMode::Problems => body.child(self.render_problems(cx)),
                    }),
            )
    }

    /// New file or folder from the sidebar buttons: inside the selected
    /// folder, next to the selected file, or at the root.
    fn new_entry_in_selection(
        &mut self,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let selected = self
            .file_tree
            .as_ref()
            .and_then(|tree| tree.read(cx).selection().into_iter().next());
        let dir = match selected {
            Some(path)
                if self
                    .file_tree
                    .as_ref()
                    .is_some_and(|tree| tree.read(cx).is_directory(&path)) =>
            {
                path
            }
            Some(path) => path.parent().map_or(root.clone(), PathBuf::from),
            None => root.clone(),
        };
        let shown = dir
            .strip_prefix(&root)
            .ok()
            .map(|relative| relative.display().to_string().replace('\\', "/"))
            .filter(|relative| !relative.is_empty())
            .unwrap_or_else(|| ".".into());
        let (title, purpose) = if folder {
            (
                format!("New folder in {shown}"),
                PromptPurpose::NewFolder { dir },
            )
        } else {
            (
                format!("New file in {shown}"),
                PromptPurpose::NewFile { dir },
            )
        };
        self.show_prompt(title, "", 0..0, purpose, window, cx);
    }

    // ---- welcome -------------------------------------------------------------

    pub(crate) fn render_empty_state(&self, cx: &mut Context<Self>) -> Div {
        let session = crate::session::read(cx);
        let recent: Vec<PathBuf> = session
            .recent_folders
            .iter()
            .filter(|folder| folder.is_dir())
            .take(6)
            .cloned()
            .collect();
        let recent_remote: Vec<crate::session::RemoteProject> =
            session.recent_remote.iter().take(4).cloned().collect();
        let row = |id: SharedString,
                   name: IconName,
                   label: SharedString,
                   trailing: Option<AnyElement>| {
            div()
                .id(id)
                .h(px(32.))
                .px_2()
                .mx(px(-8.))
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|row| row.bg(theme::hover_bg()))
                .child(ui::icon_sized(name, px(16.), theme::accent()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text())
                        .child(label),
                )
                .children(trailing)
        };
        let action_row =
            |id: &'static str, name: IconName, label: &'static str, action: Box<dyn Action>| {
                let shortcut =
                    ui::shortcut(action.as_ref(), cx).map(|text| ui::kbd(text).into_any_element());
                row(id.into(), name, label.into(), shortcut)
                    .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
            };
        let heading = |text: &'static str| {
            div()
                .mb_1()
                .text_size(theme::ui_font_size_small())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_faint())
                .child(text.to_uppercase())
        };

        // A folder is open: a quiet hint instead of the full welcome.
        if self.root.is_some() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(300.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(action_row(
                            "hint-file",
                            IconName::Search,
                            "Go to File",
                            Box::new(ToggleFileFinder),
                        ))
                        .child(action_row(
                            "hint-commands",
                            IconName::Command,
                            "All Commands",
                            Box::new(CommandPalette),
                        ))
                        .child(action_row(
                            "hint-terminal",
                            IconName::SquareTerminal,
                            "Open Terminal",
                            Box::new(ToggleTerminal),
                        ))
                        .child(action_row(
                            "hint-new",
                            IconName::FilePlus,
                            "New File",
                            Box::new(NewFile),
                        )),
                );
        }

        let remote_rows: Vec<_> = recent_remote
            .into_iter()
            .enumerate()
            .map(|(ix, project)| {
                let trailing = div()
                    .flex_none()
                    .max_w(px(160.))
                    .truncate()
                    .text_size(theme::ui_font_size_small())
                    .text_color(theme::text_faint())
                    .child(project.connection.host.clone())
                    .into_any_element();
                let name = project
                    .title()
                    .split(" — ")
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                row(
                    format!("recent-remote-{ix}").into(),
                    IconName::Monitor,
                    name.into(),
                    Some(trailing),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_recent_remote(project.clone(), window, cx)
                }))
            })
            .collect();
        let has_recent = !recent.is_empty() || !remote_rows.is_empty();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(560.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(ui::logo(40.))
                            .child(
                                div()
                                    .text_size(px(26.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Ion"),
                            ),
                    )
                    .child(
                        div()
                            .mt_2()
                            .mb_8()
                            .text_color(theme::text_muted())
                            .child("The lightweight IDE built around your terminal agent."),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_10()
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .child(heading("Start"))
                                    .child(action_row(
                                        "start-open",
                                        IconName::FolderOpen,
                                        "Open Folder…",
                                        Box::new(OpenFolder),
                                    ))
                                    .child(action_row(
                                        "start-ssh",
                                        IconName::Monitor,
                                        "Connect to SSH…",
                                        Box::new(ConnectSsh),
                                    ))
                                    .child(action_row(
                                        "start-new",
                                        IconName::FilePlus,
                                        "New File",
                                        Box::new(NewFile),
                                    ))
                                    .child(action_row(
                                        "start-terminal",
                                        IconName::SquareTerminal,
                                        "Open Terminal",
                                        Box::new(ToggleTerminal),
                                    ))
                                    .child(action_row(
                                        "start-commands",
                                        IconName::Command,
                                        "All Commands",
                                        Box::new(CommandPalette),
                                    )),
                            )
                            .when(has_recent, |columns| {
                                columns.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(heading("Recent"))
                                        .children(recent.into_iter().enumerate().map(
                                            |(ix, folder)| {
                                                let name = folder
                                                    .file_name()
                                                    .map(|name| name.to_string_lossy().into_owned())
                                                    .unwrap_or_default();
                                                let parent = folder
                                                    .parent()
                                                    .map(|parent| parent.display().to_string())
                                                    .unwrap_or_default();
                                                let trailing = div()
                                                    .flex_none()
                                                    .max_w(px(140.))
                                                    .truncate()
                                                    .text_size(theme::ui_font_size_small())
                                                    .text_color(theme::text_faint())
                                                    .child(parent)
                                                    .into_any_element();
                                                row(
                                                    format!("recent-{ix}").into(),
                                                    IconName::Folder,
                                                    name.into(),
                                                    Some(trailing),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _: &ClickEvent, window, cx| {
                                                        this.open_path(folder.clone(), window, cx)
                                                    },
                                                ))
                                            },
                                        ))
                                        .children(remote_rows),
                                )
                            }),
                    ),
            )
    }

    // ---- status bar ----------------------------------------------------------

    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> Div {
        let active_editor = self.active_editor();
        let editor = active_editor.as_ref().map(|editor| editor.read(cx));
        let item = |id: &'static str| {
            div()
                .id(id)
                .h(px(20.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(4.))
                .whitespace_nowrap()
        };
        let branch = self.git.as_ref().and_then(|git| {
            let branch = &git.status.branch;
            let mut label = match (&branch.head, &branch.oid) {
                (Some(head), _) => head.clone(),
                (None, Some(oid)) => oid[..oid.len().min(7)].to_owned(),
                (None, None) => return None,
            };
            if branch.ahead > 0 {
                label.push_str(&format!(" ↑{}", branch.ahead));
            }
            if branch.behind > 0 {
                label.push_str(&format!(" ↓{}", branch.behind));
            }
            Some(label)
        });
        let blame = self.line_blame(cx).filter(|_| settings::get(cx).git_blame);
        let position = editor.map(|editor| {
            let (line, col) = editor.cursor_position();
            format!("Ln {line}, Col {col}")
        });
        let tab_size = settings::get(cx).tab_size();
        let details = editor.map(|editor| {
            let language = editor.language_name().unwrap_or("Plain Text");
            [
                format!("Spaces: {tab_size}"),
                editor.line_ending().to_owned(),
                language.to_owned(),
            ]
        });
        let attention = self.hidden_attention(cx);
        let following = self.following_label(cx);

        div()
            .h(px(STATUS_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .bg(theme::panel_bg())
            .border_t_1()
            .border_color(theme::border())
            .text_size(theme::ui_font_size_small())
            .text_color(theme::text_muted())
            .children(self.filesystem.remote().map(|connection| {
                let label = connection.label();
                match &self.remote_state {
                    Some(remote::ConnectionState::Reconnecting(reason)) => item("ssh-status")
                        .text_color(theme::git_modified())
                        .child(ui::icon_sized(
                            IconName::RefreshCw,
                            px(13.),
                            theme::git_modified(),
                        ))
                        .child(format!("Reconnecting to {label}…"))
                        .tooltip(ui::text_tooltip(reason.clone())),
                    Some(remote::ConnectionState::Disconnected(reason)) => item("ssh-status")
                        .cursor_pointer()
                        .text_color(theme::git_deleted())
                        .hover(|item| item.bg(theme::hover_bg()))
                        .child(ui::icon_sized(
                            IconName::Monitor,
                            px(13.),
                            theme::git_deleted(),
                        ))
                        .child(format!("Disconnected from {label} · Reconnect"))
                        .tooltip(ui::text_tooltip(reason.clone()))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.reconnect_remote(cx)),
                        ),
                    _ => item("ssh-status")
                        .text_color(theme::accent())
                        .child(ui::icon_sized(IconName::Monitor, px(13.), theme::accent()))
                        .child(format!("SSH: {label}"))
                        .tooltip(ui::tooltip("Connected over SSH", None)),
                }
            }))
            .children(branch.map(|branch| {
                item("branch")
                    .cursor_pointer()
                    .hover(|item| item.bg(theme::hover_bg()).text_color(theme::text()))
                    .child(ui::icon_sized(
                        IconName::GitBranch,
                        px(14.),
                        theme::text_muted(),
                    ))
                    .child(branch)
                    .tooltip(ui::tooltip("Switch Branch", Some(Box::new(SwitchBranch))))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.show_branch_picker(window, cx)
                    }))
            }))
            .children(self.render_problem_counts(cx))
            .children(self.status.clone().map(|status| {
                div()
                    .min_w_0()
                    .truncate()
                    .px(px(6.))
                    .text_color(theme::accent())
                    .child(status)
            }))
            .child(div().flex_1())
            .children(self.render_ide_status(cx))
            .when_some(following, |bar, label| {
                bar.child(
                    item("following")
                        .max_w(px(300.))
                        .min_w_0()
                        .text_color(theme::accent())
                        .child(ui::icon_sized(IconName::Monitor, px(13.), theme::accent()))
                        .child(div().min_w_0().truncate().child(label))
                        .child(
                            ui::small_icon_button("stop-following", IconName::X)
                                .tooltip(ui::tooltip(
                                    "Stop Following",
                                    Some(Box::new(StopFollowing)),
                                ))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.stop_following(window, cx)
                                })),
                        ),
                )
            })
            .when_some(attention, |bar, label| {
                bar.child(
                    item("terminal-attention")
                        .max_w(px(360.))
                        .min_w_0()
                        .cursor_pointer()
                        .text_color(theme::accent())
                        .hover(|item| item.bg(theme::hover_bg()))
                        .child(div().size(px(6.)).rounded_full().bg(theme::accent()))
                        .child(div().min_w_0().truncate().child(label))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.show_attention(window, cx)
                        })),
                )
            })
            .children(blame.map(|blame| {
                let tooltip = blame.tooltip.clone();
                item("blame")
                    .max_w(px(420.))
                    .min_w_0()
                    .text_color(theme::text_faint())
                    .when(blame.oid.is_some(), |item| {
                        item.cursor_pointer().hover(|item| {
                            item.bg(theme::hover_bg()).text_color(theme::text_muted())
                        })
                    })
                    .tooltip(ui::text_tooltip(tooltip))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(oid) = blame.oid.clone() {
                            this.open_diff(DiffTarget::Commit(oid), window, cx);
                        }
                    }))
                    .child(div().min_w_0().truncate().child(blame.text))
            }))
            .children(position.map(|position| item("position").child(position)))
            .children(details.into_iter().flatten().enumerate().map(|(ix, text)| {
                item(["status-indent", "status-eol", "status-language"][ix]).child(text)
            }))
    }
}

/// Below the clicked element, aligned to its left edge.
fn menu_position(event: &ClickEvent, below: f32) -> Point<Pixels> {
    let position = event.position();
    point(position.x - px(12.), px(below + 2.))
}

/// Minimize, maximize and close, drawn with the system icon font.
fn window_controls(window: &Window) -> Div {
    let control = |id: &'static str, glyph: &'static str, area: WindowControlArea, close: bool| {
        div()
            .id(id)
            .w(px(46.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family("Segoe Fluent Icons")
            .text_size(px(10.))
            .text_color(theme::text_muted())
            .window_control_area(area)
            .map(|button| {
                if close {
                    button.hover(|button| button.bg(gpui::rgb(0xc42b1c)).text_color(gpui::white()))
                } else {
                    button.hover(|button| button.bg(theme::hover_bg()).text_color(theme::text()))
                }
            })
            .child(glyph)
    };
    let maximize = if window.is_maximized() {
        "\u{E923}"
    } else {
        "\u{E922}"
    };
    div()
        .h_full()
        .flex()
        .flex_none()
        .child(control(
            "window-min",
            "\u{E921}",
            WindowControlArea::Min,
            false,
        ))
        .child(control(
            "window-max",
            maximize,
            WindowControlArea::Max,
            false,
        ))
        .child(control(
            "window-close",
            "\u{E8BB}",
            WindowControlArea::Close,
            true,
        ))
}
