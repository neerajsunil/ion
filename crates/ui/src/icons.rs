//! The icon set: Lucide icons (ISC license, see `icons/LICENSE`), embedded
//! in the binary and drawn as tinted masks.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Pixels, SharedString, Styled, Svg, px, svg};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconName {
    ArrowDown,
    ArrowUp,
    Bot,
    Check,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    ChevronsDownUp,
    CircleHelp,
    CircleX,
    ClipboardPaste,
    Columns2,
    Command,
    Copy,
    Ellipsis,
    ExternalLink,
    Eye,
    File,
    FileCode,
    FilePlus,
    FileText,
    Files,
    Folder,
    FolderOpen,
    FolderPlus,
    FolderSearch,
    GitBranch,
    GitCommitHorizontal,
    GitCompare,
    History,
    Image,
    Keyboard,
    LayoutTemplate,
    LogOut,
    Maximize2,
    Menu,
    Minimize2,
    Minus,
    Monitor,
    Moon,
    PanelBottom,
    PanelLeft,
    Pencil,
    Play,
    Plus,
    RefreshCw,
    Rows2,
    Save,
    Scissors,
    Search,
    Settings,
    Square,
    SquareTerminal,
    Sun,
    Trash2,
    TriangleAlert,
    Undo2,
    X,
}

impl IconName {
    pub fn path(self) -> &'static str {
        match self {
            IconName::ArrowDown => "icons/arrow-down.svg",
            IconName::ArrowUp => "icons/arrow-up.svg",
            IconName::Bot => "icons/bot.svg",
            IconName::Check => "icons/check.svg",
            IconName::ChevronDown => "icons/chevron-down.svg",
            IconName::ChevronRight => "icons/chevron-right.svg",
            IconName::ChevronUp => "icons/chevron-up.svg",
            IconName::ChevronsDownUp => "icons/chevrons-down-up.svg",
            IconName::CircleHelp => "icons/circle-help.svg",
            IconName::ClipboardPaste => "icons/clipboard-paste.svg",
            IconName::Columns2 => "icons/columns-2.svg",
            IconName::Command => "icons/command.svg",
            IconName::Copy => "icons/copy.svg",
            IconName::Ellipsis => "icons/ellipsis.svg",
            IconName::ExternalLink => "icons/external-link.svg",
            IconName::File => "icons/file.svg",
            IconName::FileCode => "icons/file-code.svg",
            IconName::FilePlus => "icons/file-plus.svg",
            IconName::FileText => "icons/file-text.svg",
            IconName::Files => "icons/files.svg",
            IconName::Folder => "icons/folder.svg",
            IconName::FolderOpen => "icons/folder-open.svg",
            IconName::FolderPlus => "icons/folder-plus.svg",
            IconName::FolderSearch => "icons/folder-search.svg",
            IconName::GitBranch => "icons/git-branch.svg",
            IconName::GitCommitHorizontal => "icons/git-commit-horizontal.svg",
            IconName::GitCompare => "icons/git-compare.svg",
            IconName::History => "icons/history.svg",
            IconName::Keyboard => "icons/keyboard.svg",
            IconName::LayoutTemplate => "icons/layout-template.svg",
            IconName::LogOut => "icons/log-out.svg",
            IconName::Maximize2 => "icons/maximize-2.svg",
            IconName::Menu => "icons/menu.svg",
            IconName::Minimize2 => "icons/minimize-2.svg",
            IconName::Minus => "icons/minus.svg",
            IconName::Monitor => "icons/monitor.svg",
            IconName::Moon => "icons/moon.svg",
            IconName::PanelBottom => "icons/panel-bottom.svg",
            IconName::PanelLeft => "icons/panel-left.svg",
            IconName::Pencil => "icons/pencil.svg",
            IconName::Plus => "icons/plus.svg",
            IconName::RefreshCw => "icons/refresh-cw.svg",
            IconName::Rows2 => "icons/rows-2.svg",
            IconName::Save => "icons/save.svg",
            IconName::Scissors => "icons/scissors.svg",
            IconName::Search => "icons/search.svg",
            IconName::Settings => "icons/settings.svg",
            IconName::SquareTerminal => "icons/square-terminal.svg",
            IconName::Sun => "icons/sun.svg",
            IconName::Trash2 => "icons/trash-2.svg",
            IconName::Undo2 => "icons/undo-2.svg",
            IconName::CircleX => "icons/circle-x.svg",
            IconName::Eye => "icons/eye.svg",
            IconName::Image => "icons/image.svg",
            IconName::Play => "icons/play.svg",
            IconName::Square => "icons/square.svg",
            IconName::TriangleAlert => "icons/triangle-alert.svg",
            IconName::X => "icons/x.svg",
        }
    }
}

/// An icon at the standard 16px size in the muted text color.
pub fn icon(name: IconName) -> Svg {
    icon_sized(name, px(16.), theme::text_muted())
}

pub fn icon_sized(name: IconName, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// Serves the embedded icons to GPUI.
pub struct Assets;

/// The Ion logo, in colour.
const LOGO: &str = "brand/ion-logo.svg";

/// The Ion logo at `size` pixels.
pub fn logo(size: f32) -> gpui::Img {
    gpui::img(LOGO).size(px(size)).flex_none()
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/arrow-down.svg" => include_bytes!("../icons/arrow-down.svg"),
            "icons/arrow-up.svg" => include_bytes!("../icons/arrow-up.svg"),
            "icons/bot.svg" => include_bytes!("../icons/bot.svg"),
            "icons/check.svg" => include_bytes!("../icons/check.svg"),
            "icons/chevron-down.svg" => include_bytes!("../icons/chevron-down.svg"),
            "icons/chevron-right.svg" => include_bytes!("../icons/chevron-right.svg"),
            "icons/chevron-up.svg" => include_bytes!("../icons/chevron-up.svg"),
            "icons/chevrons-down-up.svg" => include_bytes!("../icons/chevrons-down-up.svg"),
            "icons/circle-help.svg" => include_bytes!("../icons/circle-help.svg"),
            "icons/clipboard-paste.svg" => include_bytes!("../icons/clipboard-paste.svg"),
            "icons/columns-2.svg" => include_bytes!("../icons/columns-2.svg"),
            "icons/command.svg" => include_bytes!("../icons/command.svg"),
            "icons/copy.svg" => include_bytes!("../icons/copy.svg"),
            "icons/ellipsis.svg" => include_bytes!("../icons/ellipsis.svg"),
            "icons/external-link.svg" => include_bytes!("../icons/external-link.svg"),
            "icons/file.svg" => include_bytes!("../icons/file.svg"),
            "icons/file-code.svg" => include_bytes!("../icons/file-code.svg"),
            "icons/file-plus.svg" => include_bytes!("../icons/file-plus.svg"),
            "icons/file-text.svg" => include_bytes!("../icons/file-text.svg"),
            "icons/files.svg" => include_bytes!("../icons/files.svg"),
            "icons/folder.svg" => include_bytes!("../icons/folder.svg"),
            "icons/folder-open.svg" => include_bytes!("../icons/folder-open.svg"),
            "icons/folder-plus.svg" => include_bytes!("../icons/folder-plus.svg"),
            "icons/folder-search.svg" => include_bytes!("../icons/folder-search.svg"),
            "icons/git-branch.svg" => include_bytes!("../icons/git-branch.svg"),
            "icons/git-commit-horizontal.svg" => {
                include_bytes!("../icons/git-commit-horizontal.svg")
            }
            "icons/git-compare.svg" => include_bytes!("../icons/git-compare.svg"),
            "icons/history.svg" => include_bytes!("../icons/history.svg"),
            "icons/keyboard.svg" => include_bytes!("../icons/keyboard.svg"),
            "icons/layout-template.svg" => include_bytes!("../icons/layout-template.svg"),
            "icons/log-out.svg" => include_bytes!("../icons/log-out.svg"),
            "icons/maximize-2.svg" => include_bytes!("../icons/maximize-2.svg"),
            "icons/menu.svg" => include_bytes!("../icons/menu.svg"),
            "icons/minimize-2.svg" => include_bytes!("../icons/minimize-2.svg"),
            "icons/minus.svg" => include_bytes!("../icons/minus.svg"),
            "icons/monitor.svg" => include_bytes!("../icons/monitor.svg"),
            "icons/moon.svg" => include_bytes!("../icons/moon.svg"),
            "icons/panel-bottom.svg" => include_bytes!("../icons/panel-bottom.svg"),
            "icons/panel-left.svg" => include_bytes!("../icons/panel-left.svg"),
            "icons/pencil.svg" => include_bytes!("../icons/pencil.svg"),
            "icons/plus.svg" => include_bytes!("../icons/plus.svg"),
            "icons/refresh-cw.svg" => include_bytes!("../icons/refresh-cw.svg"),
            "icons/rows-2.svg" => include_bytes!("../icons/rows-2.svg"),
            "icons/save.svg" => include_bytes!("../icons/save.svg"),
            "icons/scissors.svg" => include_bytes!("../icons/scissors.svg"),
            "icons/search.svg" => include_bytes!("../icons/search.svg"),
            "icons/settings.svg" => include_bytes!("../icons/settings.svg"),
            "icons/square-terminal.svg" => include_bytes!("../icons/square-terminal.svg"),
            "icons/circle-x.svg" => include_bytes!("../icons/circle-x.svg"),
            "icons/eye.svg" => include_bytes!("../icons/eye.svg"),
            "icons/image.svg" => include_bytes!("../icons/image.svg"),
            "icons/play.svg" => include_bytes!("../icons/play.svg"),
            "icons/square.svg" => include_bytes!("../icons/square.svg"),
            "icons/triangle-alert.svg" => include_bytes!("../icons/triangle-alert.svg"),
            "icons/sun.svg" => include_bytes!("../icons/sun.svg"),
            "icons/trash-2.svg" => include_bytes!("../icons/trash-2.svg"),
            "icons/undo-2.svg" => include_bytes!("../icons/undo-2.svg"),
            "icons/x.svg" => include_bytes!("../icons/x.svg"),
            LOGO => include_bytes!("../brand/ion-logo.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(if path.trim_end_matches('/') == "icons" {
            vec![
                "icons/arrow-down.svg".into(),
                "icons/arrow-up.svg".into(),
                "icons/check.svg".into(),
                "icons/chevron-down.svg".into(),
                "icons/chevron-right.svg".into(),
                "icons/chevron-up.svg".into(),
                "icons/chevrons-down-up.svg".into(),
                "icons/circle-help.svg".into(),
                "icons/clipboard-paste.svg".into(),
                "icons/columns-2.svg".into(),
                "icons/command.svg".into(),
                "icons/copy.svg".into(),
                "icons/ellipsis.svg".into(),
                "icons/external-link.svg".into(),
                "icons/file.svg".into(),
                "icons/file-code.svg".into(),
                "icons/file-plus.svg".into(),
                "icons/file-text.svg".into(),
                "icons/files.svg".into(),
                "icons/folder.svg".into(),
                "icons/folder-open.svg".into(),
                "icons/folder-plus.svg".into(),
                "icons/folder-search.svg".into(),
                "icons/git-branch.svg".into(),
                "icons/git-commit-horizontal.svg".into(),
                "icons/git-compare.svg".into(),
                "icons/history.svg".into(),
                "icons/keyboard.svg".into(),
                "icons/layout-template.svg".into(),
                "icons/log-out.svg".into(),
                "icons/maximize-2.svg".into(),
                "icons/menu.svg".into(),
                "icons/minimize-2.svg".into(),
                "icons/minus.svg".into(),
                "icons/monitor.svg".into(),
                "icons/moon.svg".into(),
                "icons/panel-bottom.svg".into(),
                "icons/panel-left.svg".into(),
                "icons/pencil.svg".into(),
                "icons/plus.svg".into(),
                "icons/refresh-cw.svg".into(),
                "icons/rows-2.svg".into(),
                "icons/save.svg".into(),
                "icons/scissors.svg".into(),
                "icons/search.svg".into(),
                "icons/settings.svg".into(),
                "icons/square-terminal.svg".into(),
                "icons/sun.svg".into(),
                "icons/trash-2.svg".into(),
                "icons/undo-2.svg".into(),
                "icons/x.svg".into(),
            ]
        } else {
            Vec::new()
        })
    }
}
