//! Drawing the pane layout: splits with draggable dividers, tab bars,
//! drop targets for dragged tabs, and the pane menu.

use std::collections::HashMap;

use gpui::{
    AnyElement, AppContext, Bounds, ClickEvent, Context, CursorStyle, Div, DragMoveEvent, EntityId,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div,
    prelude::FluentBuilder, px, relative,
};
use ui::{IconName, Menu, MenuEntry};

use crate::pane::{Axis, DragPreview, DraggedTab, PaneId, PaneNode, Region, Side};
use crate::workspace::{BAR_HEIGHT, MenuKind, PromptPurpose, Workspace};

/// Width of the grab area between split panes.
/// The space between split panes: a 1px line with room either side to grab.
const DIVIDER: f32 = 6.;

/// Screen positions of panes and splits from the last frame, for dragging
/// dividers and moving focus between panes.
#[derive(Default)]
pub(crate) struct LayoutBounds {
    pub panes: HashMap<PaneId, Bounds<Pixels>>,
    pub splits: HashMap<(Region, Vec<usize>), Bounds<Pixels>>,
}

/// A divider being dragged.
pub(crate) enum Resize {
    Sidebar,
    Dock,
    Split {
        region: Region,
        path: Vec<usize>,
        divider: usize,
        axis: Axis,
    },
}

#[derive(Clone)]
pub(crate) enum MenuAction {
    NewFile(PaneId),
    NewTerminal(PaneId),
    NewTerminalWith(PaneId, terminal::ShellProfile),
    NewAgent(PaneId, terminal::Harness),
    Follow(EntityId),
    StopFollowing,
    RestartAgent(EntityId),
    HidePanel,
    Split(PaneId, Side),
    MoveToSplit(EntityId, PaneId, Side),
    Zoom(PaneId),
    ClosePane(PaneId),
    CloseItem(EntityId),
    CloseOthers(EntityId),
    CloseAll(PaneId),
    RenameTerminal(EntityId),
}

/// The drop target under `position`: within a quarter of an edge splits
/// toward that edge (the left and right strips run full height), anywhere
/// else moves the tab into the pane. Matches the zones in
/// `render_drop_zones`.
fn drop_side(bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<Side> {
    let x = (position.x - bounds.origin.x) / bounds.size.width;
    let y = (position.y - bounds.origin.y) / bounds.size.height;
    if x < 0.25 {
        Some(Side::Left)
    } else if x > 0.75 {
        Some(Side::Right)
    } else if y < 0.25 {
        Some(Side::Up)
    } else if y > 0.75 {
        Some(Side::Down)
    } else {
        None
    }
}

impl Workspace {
    pub(crate) fn render_tree(
        &self,
        region: Region,
        node: &PaneNode,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let PaneNode::Split {
            axis,
            children,
            flexes,
        } = node
        else {
            let PaneNode::Pane(id) = node else {
                unreachable!()
            };
            return self.render_pane(*id, region, cx);
        };
        let axis = *axis;
        let bounds = self.layout_bounds.clone();
        let key = (region, path.clone());
        let mut container = div()
            .relative()
            .size_full()
            .flex()
            .map(|el| match axis {
                Axis::Row => el.flex_row(),
                Axis::Column => el.flex_col(),
            })
            .child(
                canvas(
                    move |b, _, _| {
                        bounds.borrow_mut().splits.insert(key, b);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        for (ix, (child, flex)) in children.iter().zip(flexes).enumerate() {
            if ix > 0 {
                container = container.child(self.render_divider(region, &path, ix - 1, axis, cx));
            }
            let mut child_path = path.clone();
            child_path.push(ix);
            let mut cell = div().min_w_0().min_h_0().flex().overflow_hidden();
            let style = cell.style();
            style.flex_grow = Some(*flex);
            style.flex_shrink = Some(1.);
            style.flex_basis = Some(relative(0.).into());
            container =
                container.child(cell.child(self.render_tree(region, child, child_path, cx)));
        }
        container.into_any_element()
    }

    fn render_divider(
        &self,
        region: Region,
        path: &[usize],
        divider: usize,
        axis: Axis,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = SharedString::from(format!("divider-{region:?}-{path:?}-{divider}"));
        let dragging = matches!(&self.resize, Some(Resize::Split {
            region: r, path: p, divider: d, ..
        }) if *r == region && p.as_slice() == path && *d == divider);
        let path = path.to_vec();
        let line = div()
            .bg(if dragging {
                theme::accent()
            } else {
                theme::border()
            })
            .group_hover(id.clone(), |line| line.bg(theme::accent()))
            .map(|line| match axis {
                Axis::Row => line.w(px(if dragging { 2. } else { 1. })).h_full(),
                Axis::Column => line.h(px(if dragging { 2. } else { 1. })).w_full(),
            });
        div()
            .id(id.clone())
            .group(id)
            .flex_none()
            .flex()
            .justify_center()
            .items_center()
            .bg(theme::bg())
            .child(line)
            .map(|el| match axis {
                Axis::Row => el
                    .w(px(DIVIDER))
                    .h_full()
                    .cursor(CursorStyle::ResizeLeftRight),
                Axis::Column => el.h(px(DIVIDER)).w_full().cursor(CursorStyle::ResizeUpDown),
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.equalize_split(region, &path, cx);
                        return;
                    }
                    this.resize = Some(Resize::Split {
                        region,
                        path: path.clone(),
                        divider,
                        axis,
                    });
                }),
            )
    }

    pub(crate) fn render_pane(
        &self,
        id: PaneId,
        region: Region,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(pane) = self.panes.get(&id) else {
            return div().into_any_element();
        };
        let bounds = self.layout_bounds.clone();
        let show_find = self.find_visible
            && self.editor_pane == id
            && pane
                .active_item()
                .is_some_and(|item| item.editor().is_some());
        let proposal_bar = pane
            .active_item()
            .and_then(|item| self.render_proposal_bar(item, cx));
        let content = match pane.active_item() {
            Some(item) => div().size_full().child(item.view()),
            None if region == Region::Center && self.center == PaneNode::Pane(id) => {
                self.render_empty_state(cx)
            }
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .gap_2()
                .items_center()
                .justify_center()
                .text_size(theme::ui_font_size_small())
                .text_color(theme::text_faint())
                .child(if region == Region::Center {
                    "Open a file with Ctrl+P, or drag a tab here"
                } else {
                    "Drag a tab here"
                })
                // An empty split has no tab bar, so it needs its own way out.
                .child(
                    div()
                        .id(("close-split", id as usize))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_color(theme::text_muted())
                        .hover(|button| button.bg(theme::hover_bg()).text_color(theme::text()))
                        .child(ui::icon_sized(IconName::X, px(14.), theme::text_muted()))
                        .child("Close Split")
                        .tooltip(ui::tooltip("Close Split", Some(Box::new(crate::CloseTab))))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.close_pane(id, window, cx)
                        })),
                ),
        };
        div()
            .id(("pane", id as usize))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .capture_any_mouse_down(cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                if this.active_pane != id {
                    this.pane_focused(id, cx);
                }
            }))
            .when(!pane.items.is_empty() || region == Region::Dock, |pane| {
                pane.child(self.render_tab_bar(id, region, cx))
            })
            .when(show_find, |pane| pane.child(self.find_bar.clone()))
            .children(proposal_bar)
            .child(div().flex_1().min_h_0().child(content))
            .child(
                canvas(
                    move |b, _, _| {
                        bounds.borrow_mut().panes.insert(id, b);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when(cx.has_active_drag(), |pane| {
                pane.child(self.render_drop_zones(id, cx))
            })
            .into_any_element()
    }

    fn render_tab_bar(&self, id: PaneId, region: Region, cx: &mut Context<Self>) -> Div {
        let pane = &self.panes[&id];
        let pane_active = id == self.active_pane;
        let tools = match region {
            Region::Center => self.center_pane_tools(id, cx),
            Region::Dock => self.dock_pane_tools(id, cx),
        };
        let tabs: Vec<_> = pane
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let item_id = item.id();
                let title = item.title(cx);
                let is_active = ix == pane.active;
                let dirty = item.is_dirty(cx);
                let attention = item.needs_attention(cx);
                let notification = item.notification(cx);
                let progress = item.progress(cx);
                let is_terminal = item.terminal().is_some();
                let icon = item.icon(cx);
                let dragged = DraggedTab {
                    item: item_id,
                    title: title.clone(),
                };
                let text_color = match (is_active, pane_active) {
                    (true, true) => theme::text(),
                    (true, false) => theme::text_muted(),
                    _ => theme::text_muted(),
                };
                div()
                    .id(SharedString::from(format!("tab-{item_id:?}")))
                    .group("tab")
                    .relative()
                    .h_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pl_3()
                    .pr_1()
                    .border_r_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .text_color(text_color)
                    .when(!is_active, |tab| {
                        tab.hover(|tab| tab.text_color(theme::text()))
                    })
                    .when(is_active, |tab| tab.bg(theme::bg()))
                    .when_some(notification, |tab, text| {
                        tab.tooltip(ui::text_tooltip(text))
                    })
                    .when_some(progress, |tab, progress| tab.child(progress_line(progress)))
                    // The focused pane's current tab gets an accent line on top.
                    .when(is_active && pane_active, |tab| {
                        tab.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(2.))
                                .bg(theme::accent()),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some((pane, ix)) = this.find_item(item_id) {
                            this.activate_item(pane, ix, window, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if event.click_count == 2 && is_terminal {
                                this.rename_terminal(item_id, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            if let Some((pane, ix)) = this.find_item(item_id) {
                                this.close_item(pane, ix, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.show_tab_menu(id, item_id, is_terminal, event.position, cx);
                        }),
                    )
                    .on_drag(dragged, |tab, _, _, cx| {
                        cx.new(|_| DragPreview::new(tab.title.clone()))
                    })
                    .drag_over::<DraggedTab>(|tab, _, _, _| tab.bg(theme::active_bg()))
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                        this.move_item(dragged.item, id, Some(ix), window, cx);
                    }))
                    .child(if attention {
                        div()
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(7.)).rounded_full().bg(theme::accent()))
                            .into_any_element()
                    } else {
                        let color = if is_active {
                            theme::text_muted()
                        } else {
                            theme::text_faint()
                        };
                        ui::icon_sized(icon, px(14.), color).into_any_element()
                    })
                    .child(div().max_w(px(220.)).truncate().child(title))
                    .child(
                        div()
                            .id(SharedString::from(format!("close-{item_id:?}")))
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .hover(|button| button.bg(theme::hover_bg()))
                            .map(|button| {
                                if dirty {
                                    // A dot for unsaved changes, a close button on hover.
                                    button
                                        .child(
                                            div()
                                                .size(px(8.))
                                                .rounded_full()
                                                .bg(theme::text_muted())
                                                .group_hover("tab", |dot| dot.invisible()),
                                        )
                                        .child(
                                            ui::icon_sized(
                                                IconName::X,
                                                px(14.),
                                                theme::text_muted(),
                                            )
                                            .absolute()
                                            .invisible()
                                            .group_hover("tab", |x| x.visible()),
                                        )
                                } else {
                                    button.child(
                                        ui::icon_sized(IconName::X, px(14.), theme::text_muted())
                                            .when(!is_active, |x| {
                                                x.invisible().group_hover("tab", |x| x.visible())
                                            }),
                                    )
                                }
                            })
                            .tooltip(ui::tooltip("Close", Some(Box::new(crate::CloseTab))))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                if let Some((pane, ix)) = this.find_item(item_id) {
                                    this.close_item(pane, ix, window, cx);
                                }
                            })),
                    )
            })
            .collect();

        div()
            .h(px(BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .bg(theme::panel_bg())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .id(("tabs", id as usize))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .overflow_x_scroll()
                    .children(tabs)
                    // Dropping on the empty part of the bar adds the tab at the end.
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(24.))
                            .h_full()
                            .drag_over::<DraggedTab>(|space, _, _, _| space.bg(theme::active_bg()))
                            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                                this.move_item(dragged.item, id, None, window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .px_1()
                    .children(tools),
            )
    }

    fn center_pane_tools(&self, id: PaneId, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let markdown = self
            .panes
            .get(&id)
            .and_then(|pane| pane.active_item())
            .filter(|item| item.diff().is_none())
            .and_then(|item| item.editor())
            .is_some_and(|editor| editor.read(cx).is_markdown());
        let preview = markdown.then(|| {
            ui::icon_button(("md-preview", id as usize), IconName::Eye)
                .tooltip(ui::tooltip(
                    "Open Preview",
                    Some(Box::new(crate::OpenMarkdownPreview)),
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.editor_pane = id;
                    this.open_markdown_preview(window, cx)
                }))
        });
        let diff_editor = self
            .panes
            .get(&id)
            .and_then(|pane| pane.active_item())
            .filter(|item| item.diff().is_some())
            .and_then(|item| item.editor())
            .cloned();
        let layout = diff_editor.map(|editor| {
            let split = editor.read(cx).is_split_diff();
            ui::toggle_icon_button(("diff-layout", id as usize), IconName::Columns2, split)
                .tooltip(ui::tooltip(
                    if split {
                        "Show Inline"
                    } else {
                        "Show Side by Side"
                    },
                    Some(Box::new(editor::ToggleDiffLayout)),
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.diff_split = !split;
                    editor.update(cx, |editor, cx| editor.set_split_diff(!split, window, cx));
                    cx.notify();
                }))
        });
        preview
            .into_iter()
            .chain(layout)
            .chain([
                ui::icon_button(("split", id as usize), IconName::Columns2)
                    .tooltip(ui::tooltip(
                        "Split Right",
                        Some(Box::new(crate::SplitRight)),
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.run_menu_action(MenuAction::Split(id, Side::Right), window, cx)
                    })),
                ui::icon_button(("pane-more", id as usize), IconName::Ellipsis)
                    .tooltip(ui::tooltip("More Actions", None))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.show_pane_menu(id, event.position(), cx)
                    })),
            ])
            .collect()
    }

    /// New terminal (with a shell picker), split, maximize and hide.
    fn dock_pane_tools(&self, id: PaneId, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let last = self.dock.panes().last() == Some(&id);
        let zoomed = self.zoomed == Some(id);
        let mut tools = vec![
            ui::icon_button(("new-terminal", id as usize), IconName::Plus)
                .tooltip(ui::tooltip(
                    "New Terminal",
                    Some(Box::new(crate::NewTerminal)),
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.new_terminal_in(id, window, cx)
                })),
            ui::small_icon_button(("shell-menu", id as usize), IconName::ChevronDown)
                .tooltip(ui::tooltip("New Terminal With…", None))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.show_shell_menu(id, event.position(), cx)
                })),
            ui::icon_button(("split-terminal", id as usize), IconName::Columns2)
                .tooltip(ui::tooltip(
                    "Split Terminal",
                    Some(Box::new(crate::SplitRight)),
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.run_menu_action(MenuAction::Split(id, Side::Right), window, cx)
                })),
        ];
        if last {
            tools.push(
                ui::icon_button(
                    "dock-maximize",
                    if zoomed {
                        IconName::Minimize2
                    } else {
                        IconName::Maximize2
                    },
                )
                .tooltip(ui::tooltip(
                    if zoomed {
                        "Restore Panel"
                    } else {
                        "Maximize Panel"
                    },
                    Some(Box::new(crate::ToggleMaximizePane)),
                ))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.run_menu_action(MenuAction::Zoom(id), window, cx)
                })),
            );
            tools.push(
                ui::icon_button("dock-hide", IconName::X)
                    .tooltip(ui::tooltip(
                        "Hide Panel",
                        Some(Box::new(crate::ToggleTerminal)),
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.run_menu_action(MenuAction::HidePanel, window, cx)
                    })),
            );
        }
        tools
    }

    pub(crate) fn menu_entry(
        &self,
        label: impl Into<SharedString>,
        icon: IconName,
        action: MenuAction,
        shortcut: Option<Box<dyn gpui::Action>>,
        cx: &mut Context<Self>,
    ) -> MenuEntry {
        let shortcut = shortcut.and_then(|action| ui::shortcut(action.as_ref(), cx));
        MenuEntry::item(
            label,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.run_menu_action(action.clone(), window, cx)
            }),
        )
        .icon(icon)
        .shortcut(shortcut)
    }

    fn show_pane_menu(&mut self, id: PaneId, position: Point<Pixels>, cx: &mut Context<Self>) {
        let zoomed = self.zoomed == Some(id);
        let entries = vec![
            self.menu_entry(
                "New File",
                IconName::FilePlus,
                MenuAction::NewFile(id),
                Some(Box::new(crate::NewFile)),
                cx,
            ),
            self.menu_entry(
                "New Terminal",
                IconName::SquareTerminal,
                MenuAction::NewTerminal(id),
                None,
                cx,
            ),
            MenuEntry::Separator,
            self.menu_entry(
                "Split Right",
                IconName::Columns2,
                MenuAction::Split(id, Side::Right),
                Some(Box::new(crate::SplitRight)),
                cx,
            ),
            self.menu_entry(
                "Split Down",
                IconName::Rows2,
                MenuAction::Split(id, Side::Down),
                Some(Box::new(crate::SplitDown)),
                cx,
            ),
            self.menu_entry(
                if zoomed {
                    "Restore Pane"
                } else {
                    "Maximize Pane"
                },
                if zoomed {
                    IconName::Minimize2
                } else {
                    IconName::Maximize2
                },
                MenuAction::Zoom(id),
                Some(Box::new(crate::ToggleMaximizePane)),
                cx,
            ),
            MenuEntry::Separator,
            self.menu_entry(
                "Close Pane",
                IconName::X,
                MenuAction::ClosePane(id),
                None,
                cx,
            ),
        ];
        self.open_menu(MenuKind::Pane, Menu::new(position, entries), cx);
    }

    pub(crate) fn show_shell_menu(
        &mut self,
        id: PaneId,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let mut entries = Vec::new();
        // Harnesses run on this machine, so remote projects don't offer them.
        let settings = settings::get(cx);
        let harnesses: Vec<_> = match self.filesystem.remote() {
            Some(_) => Vec::new(),
            None => terminal::available_harnesses()
                .into_iter()
                .filter(|harness| settings.shows_terminal(harness.kind.name()))
                .collect(),
        };
        let shells: Vec<_> = terminal::available_shells()
            .into_iter()
            .filter(|shell| settings.shows_terminal(&shell.name))
            .collect();
        if !harnesses.is_empty() {
            entries.push(MenuEntry::Header("New agent".into()));
            for harness in harnesses {
                entries.push(self.menu_entry(
                    harness.kind.name(),
                    IconName::Bot,
                    MenuAction::NewAgent(id, harness),
                    None,
                    cx,
                ));
            }
            entries.push(MenuEntry::Separator);
        }
        entries.push(MenuEntry::Header("New terminal with".into()));
        for shell in shells {
            entries.push(self.menu_entry(
                shell.name.clone(),
                IconName::SquareTerminal,
                MenuAction::NewTerminalWith(id, shell),
                None,
                cx,
            ));
        }
        entries.push(MenuEntry::Separator);
        entries.push(
            MenuEntry::item("Choose Shells and Agents…", |_, window, cx| {
                window.dispatch_action(
                    Box::new(crate::settings_view::OpenSettingsAt(
                        crate::settings_view::Page::Terminal,
                    )),
                    cx,
                )
            })
            .icon(IconName::Settings),
        );
        self.open_menu(MenuKind::Shell, Menu::new(position, entries), cx);
    }

    fn show_tab_menu(
        &mut self,
        pane: PaneId,
        item: EntityId,
        is_terminal: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let mut entries = vec![
            self.menu_entry(
                "Close",
                IconName::X,
                MenuAction::CloseItem(item),
                Some(Box::new(crate::CloseTab)),
                cx,
            ),
            self.menu_entry(
                "Close Others",
                IconName::X,
                MenuAction::CloseOthers(item),
                None,
                cx,
            ),
            self.menu_entry(
                "Close All",
                IconName::X,
                MenuAction::CloseAll(pane),
                None,
                cx,
            ),
            MenuEntry::Separator,
            self.menu_entry(
                "Move to Split Right",
                IconName::Columns2,
                MenuAction::MoveToSplit(item, pane, Side::Right),
                None,
                cx,
            ),
            self.menu_entry(
                "Move to Split Down",
                IconName::Rows2,
                MenuAction::MoveToSplit(item, pane, Side::Down),
                None,
                cx,
            ),
        ];
        if is_terminal {
            entries.push(MenuEntry::Separator);
            entries.push(self.menu_entry(
                "Rename…",
                IconName::Pencil,
                MenuAction::RenameTerminal(item),
                None,
                cx,
            ));
        }
        self.open_menu(MenuKind::Tab, Menu::new(position, entries), cx);
    }

    /// Overlay while dragging a tab: drop on an edge to split, in the
    /// middle to move the tab into this pane. The part of the pane the tab
    /// would take is shaded.
    fn render_drop_zones(&self, id: PaneId, cx: &mut Context<Self>) -> Div {
        let preview = self
            .drop_target
            .filter(|(pane, _)| *pane == id)
            .map(|(_, side)| {
                let shade = div()
                    .absolute()
                    .bg(theme::accent().opacity(0.12))
                    .border_2()
                    .border_color(theme::accent().opacity(0.6))
                    .rounded(px(4.));
                match side {
                    Some(Side::Left) => shade.left_0().top_0().h_full().w(relative(0.5)),
                    Some(Side::Right) => shade.right_0().top_0().h_full().w(relative(0.5)),
                    Some(Side::Up) => shade.left_0().top_0().w_full().h(relative(0.5)),
                    Some(Side::Down) => shade.left_0().bottom_0().w_full().h(relative(0.5)),
                    None => shade.size_full(),
                }
            });
        let zone = |side: Option<Side>| {
            let base = div()
                .absolute()
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedTab, window, cx| match side {
                        Some(side) => this.move_item_to_split(dragged.item, id, side, window, cx),
                        None => this.move_item(dragged.item, id, None, window, cx),
                    }),
                )
                // Files from the tree or the OS: anywhere in the pane.
                .on_drop(
                    cx.listener(move |this, files: &file_tree::DraggedFiles, window, cx| {
                        this.drop_files(id, files.paths.clone(), window, cx)
                    }),
                )
                .on_drop(
                    cx.listener(move |this, files: &gpui::ExternalPaths, window, cx| {
                        this.drop_files(id, files.paths().to_vec(), window, cx)
                    }),
                );
            match side {
                Some(Side::Left) => base.left_0().top_0().h_full().w(relative(0.25)),
                Some(Side::Right) => base.right_0().top_0().h_full().w(relative(0.25)),
                Some(Side::Up) => base
                    .top_0()
                    .left(relative(0.25))
                    .w(relative(0.5))
                    .h(relative(0.25)),
                Some(Side::Down) => base
                    .bottom_0()
                    .left(relative(0.25))
                    .w(relative(0.5))
                    .h(relative(0.25)),
                None => base
                    .top(relative(0.25))
                    .left(relative(0.25))
                    .w(relative(0.5))
                    .h(relative(0.5)),
            }
        };
        div()
            .absolute()
            .top(px(BAR_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    let target = event
                        .bounds
                        .contains(&event.event.position)
                        .then(|| (id, drop_side(event.bounds, event.event.position)));
                    let changed = match target {
                        Some(target) => this.drop_target != Some(target),
                        // Leaving this pane clears its preview only.
                        None => this.drop_target.is_some_and(|(pane, _)| pane == id),
                    };
                    if changed {
                        this.drop_target = target;
                        cx.notify();
                    }
                }),
            )
            .children(preview)
            .child(zone(Some(Side::Left)))
            .child(zone(Some(Side::Right)))
            .child(zone(Some(Side::Up)))
            .child(zone(Some(Side::Down)))
            .child(zone(None))
    }

    fn run_menu_action(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            MenuAction::NewFile(pane) => {
                self.editor_pane = pane;
                self.new_file(&crate::NewFile, window, cx);
            }
            MenuAction::NewTerminal(pane) => self.new_terminal_in(pane, window, cx),
            MenuAction::NewTerminalWith(pane, shell) => {
                let launch = crate::pane_ops::Launch::Shell(shell);
                let item = self.terminal_item(self.root.clone(), None, launch, window, cx);
                self.insert_item(pane, item, window, cx);
            }
            MenuAction::NewAgent(pane, harness) => self.new_agent_in(pane, harness, window, cx),
            MenuAction::Follow(terminal) => self.follow(terminal, window, cx),
            MenuAction::StopFollowing => self.stop_following(window, cx),
            MenuAction::RestartAgent(terminal) => {
                let view = self
                    .find_item(terminal)
                    .and_then(|(pane, ix)| self.panes[&pane].items[ix].terminal().cloned());
                if let Some(view) = view {
                    view.update(cx, |view, cx| view.restart(window, cx));
                }
            }
            MenuAction::HidePanel => {
                if self.dock_visible {
                    self.toggle_terminal(&crate::ToggleTerminal, window, cx);
                }
            }
            MenuAction::Split(pane, side) => {
                self.active_pane = pane;
                self.split_active_pane(side, window, cx);
            }
            MenuAction::MoveToSplit(item, pane, side) => {
                self.move_item_to_split(item, pane, side, window, cx)
            }
            MenuAction::Zoom(pane) => {
                self.active_pane = pane;
                self.toggle_zoom(cx);
            }
            MenuAction::ClosePane(pane) => self.close_pane(pane, window, cx),
            MenuAction::CloseItem(item) => {
                if let Some((pane, ix)) = self.find_item(item) {
                    self.close_item(pane, ix, window, cx);
                }
            }
            MenuAction::CloseAll(pane) => {
                let items: Vec<EntityId> = self
                    .panes
                    .get(&pane)
                    .map(|pane| pane.items.iter().map(|item| item.id()).collect())
                    .unwrap_or_default();
                for item in items {
                    if let Some((pane, ix)) = self.find_item(item) {
                        self.close_item(pane, ix, window, cx);
                    }
                }
            }
            MenuAction::CloseOthers(item) => {
                let Some((pane, _)) = self.find_item(item) else {
                    return;
                };
                let others: Vec<EntityId> = self.panes[&pane]
                    .items
                    .iter()
                    .map(|other| other.id())
                    .filter(|other| *other != item)
                    .collect();
                for other in others {
                    if let Some((pane, ix)) = self.find_item(other) {
                        self.close_item(pane, ix, window, cx);
                    }
                }
            }
            MenuAction::RenameTerminal(item) => self.rename_terminal(item, window, cx),
        }
        cx.notify();
    }

    fn rename_terminal(&mut self, item: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane, ix)) = self.find_item(item) else {
            return;
        };
        let current = self.panes[&pane].items[ix].title(cx).to_string();
        let len = current.chars().count();
        self.show_prompt(
            "Rename terminal".into(),
            &current,
            0..len,
            PromptPurpose::RenameTerminal { item },
            window,
            cx,
        );
    }
}

/// A static line along the bottom of a terminal tab while its program
/// reports progress (`OSC 9;4`). It never animates, so a busy agent costs
/// no frames.
fn progress_line(progress: terminal::Progress) -> Div {
    use terminal::ProgressState;
    let line = div().absolute().bottom_0().left_0().h(px(2.));
    let line = match progress.percent {
        Some(percent) if progress.state != ProgressState::Indeterminate => {
            line.w(relative(f32::from(percent.max(5)) / 100.))
        }
        _ => line.right_0().opacity(0.6),
    };
    line.bg(match progress.state {
        ProgressState::Normal | ProgressState::Indeterminate => theme::accent(),
        ProgressState::Paused => theme::git_modified(),
        ProgressState::Error => theme::git_deleted(),
    })
}
