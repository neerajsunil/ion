//! Popup menus (app menu, context menus, dropdowns), all drawn the same way.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, InteractiveElement, IntoElement, MouseDownEvent, ParentElement,
    Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window, anchored, deferred,
    div, point, prelude::FluentBuilder, px,
};

use crate::components::kbd;
use crate::icons::{IconName, icon_sized};

pub type MenuHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

pub enum MenuEntry {
    Item {
        icon: Option<IconName>,
        label: SharedString,
        shortcut: Option<SharedString>,
        /// Greyed out and not clickable.
        disabled: bool,
        handler: MenuHandler,
    },
    /// A small group title.
    Header(SharedString),
    Separator,
}

impl MenuEntry {
    pub fn item(
        label: impl Into<SharedString>,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self::Item {
            icon: None,
            label: label.into(),
            shortcut: None,
            disabled: false,
            handler: Rc::new(handler),
        }
    }

    pub fn icon(mut self, name: IconName) -> Self {
        if let Self::Item { icon, .. } = &mut self {
            *icon = Some(name);
        }
        self
    }

    pub fn shortcut(mut self, text: Option<SharedString>) -> Self {
        if let Self::Item { shortcut, .. } = &mut self {
            *shortcut = text;
        }
        self
    }

    pub fn disabled(mut self, value: bool) -> Self {
        if let Self::Item { disabled, .. } = &mut self {
            *disabled = value;
        }
        self
    }
}

/// Where a menu opens and what's in it. The owner keeps an
/// `Option<Menu>` and draws it with [`render_menu`].
pub struct Menu {
    pub position: Point<Pixels>,
    pub entries: Vec<MenuEntry>,
    pub min_width: Pixels,
}

impl Menu {
    pub fn new(position: Point<Pixels>, entries: Vec<MenuEntry>) -> Self {
        Self {
            position,
            entries,
            min_width: px(220.),
        }
    }

    pub fn min_width(mut self, width: Pixels) -> Self {
        self.min_width = width;
        self
    }
}

/// Draws a menu above everything. `dismiss` runs on a click outside the
/// menu and before an item's handler.
pub fn render_menu(
    menu: &Menu,
    dismiss: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
) -> AnyElement {
    let dismiss = Rc::new(dismiss);
    let any_shortcut = menu.entries.iter().any(|entry| {
        matches!(
            entry,
            MenuEntry::Item {
                shortcut: Some(_),
                ..
            }
        )
    });
    let list = div()
        .occlude()
        .min_w(menu.min_width)
        .p_1()
        .flex()
        .flex_col()
        .bg(theme::elevated_bg())
        .border_1()
        .border_color(theme::border())
        .rounded(px(8.))
        .shadow_lg()
        .font_family(theme::ui_font())
        .text_size(theme::ui_font_size())
        .text_color(theme::text())
        // Clicks inside the menu don't reach the dismiss layer.
        .on_any_mouse_down(|_: &MouseDownEvent, _, cx| cx.stop_propagation())
        .children(menu.entries.iter().enumerate().map(|(ix, entry)| {
            match entry {
                MenuEntry::Separator => div()
                    .my_1()
                    .mx_1()
                    .h(px(1.))
                    .bg(theme::border())
                    .into_any_element(),
                MenuEntry::Header(title) => div()
                    .px_2()
                    .pt(px(6.))
                    .pb(px(2.))
                    .text_size(theme::ui_font_size_small())
                    .text_color(theme::text_faint())
                    .child(title.clone())
                    .into_any_element(),
                MenuEntry::Item {
                    icon,
                    label,
                    shortcut,
                    disabled,
                    handler,
                } => {
                    let handler = handler.clone();
                    let dismiss = dismiss.clone();
                    let disabled = *disabled;
                    div()
                        .id(("menu-item", ix))
                        .h(px(28.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(5.))
                        .when(disabled, |row| row.text_color(theme::text_faint()))
                        .when(!disabled, |row| {
                            row.cursor_pointer()
                                .hover(|row| row.bg(theme::active_bg()))
                                .on_click(move |event, window, cx| {
                                    dismiss(window, cx);
                                    handler(event, window, cx);
                                })
                        })
                        .child(match icon {
                            Some(name) => {
                                icon_sized(*name, px(16.), theme::text_muted()).into_any_element()
                            }
                            None => div().w(px(16.)).flex_none().into_any_element(),
                        })
                        .child(div().flex_1().child(label.clone()))
                        .when(any_shortcut, |row| {
                            row.child(match shortcut {
                                Some(text) => kbd(text.clone()).ml_4().into_any_element(),
                                None => div().into_any_element(),
                            })
                        })
                        .into_any_element()
                }
            }
        }));

    let viewport = window.viewport_size();
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            div()
                .id("menu-layer")
                .w(viewport.width)
                .h(viewport.height)
                .on_any_mouse_down(move |_: &MouseDownEvent, window, cx| dismiss(window, cx))
                .child(
                    anchored()
                        .position(menu.position)
                        .snap_to_window()
                        .child(list),
                ),
        ),
    )
    .with_priority(2)
    .into_any_element()
}
