//! Ion's UI kit: icons and the small components every view is built from,
//! so buttons, menus and tooltips look and behave the same everywhere.

mod components;
mod file_icons;
mod icons;
mod menu;

pub use components::*;
pub use file_icons::{file_icon, file_icon_for};
pub use icons::{Assets, IconName, icon, icon_sized, logo};
pub use menu::{Menu, MenuEntry, MenuHandler, render_menu};
