//! Panes and the split layout they live in.
//!
//! The editor area and the terminal dock are each a tree of panes. A pane
//! holds tabs; any tab (file, diff or terminal) can move to any pane.

use std::path::PathBuf;

use editor::Editor;
use gpui::{
    App, Context, Entity, EntityId, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px,
};
use terminal::TerminalView;

use crate::diff_view::DiffTarget;

pub(crate) type PaneId = u64;

/// Which part of the window a pane tree fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Region {
    /// The editor area.
    Center,
    /// The terminal dock below it.
    Dock,
}

/// Direction of a split: `Row` places panes side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Axis {
    Row,
    Column,
}

/// Where to put a new pane relative to an existing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    pub fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Row,
            Side::Up | Side::Down => Axis::Column,
        }
    }

    fn before(self) -> bool {
        matches!(self, Side::Left | Side::Up)
    }
}

/// What a diff tab shows, so it can refresh and jump to lines.
pub(crate) struct DiffTab {
    pub target: DiffTarget,
    /// File and one-based line to open for each row.
    pub jumps: Vec<Option<(PathBuf, u32)>>,
    /// Hunks with a Revert button.
    pub reverts: Vec<crate::diff_view::HunkRevert>,
}

impl DiffTab {
    /// Header rows of the hunks that can be reverted, for the editor.
    pub fn revert_rows(&self) -> Vec<usize> {
        self.reverts.iter().map(|revert| revert.row).collect()
    }
}

pub(crate) enum ItemKind {
    Editor {
        editor: Entity<Editor>,
        diff: Option<DiffTab>,
    },
    Terminal {
        view: Entity<TerminalView>,
        /// A name the user gave the tab, shown instead of the shell's title.
        name: Option<SharedString>,
        /// The folder the shell started in, to restore it next time.
        cwd: Option<PathBuf>,
        /// The shell picked from the menu; `None` is the default shell.
        shell: Option<String>,
        /// The agent harness it runs instead of a shell.
        harness: Option<terminal::HarnessKind>,
    },
    Settings(Entity<crate::settings_view::SettingsView>),
    Preview(Entity<crate::markdown_preview::MarkdownPreview>),
    Image(Entity<crate::image_view::ImageView>),
}

/// One tab.
pub(crate) struct Item {
    pub kind: ItemKind,
    pub _subscriptions: Vec<Subscription>,
}

impl Item {
    pub fn id(&self) -> EntityId {
        match &self.kind {
            ItemKind::Editor { editor, .. } => editor.entity_id(),
            ItemKind::Terminal { view, .. } => view.entity_id(),
            ItemKind::Settings(view) => view.entity_id(),
            ItemKind::Preview(view) => view.entity_id(),
            ItemKind::Image(view) => view.entity_id(),
        }
    }

    /// The icon shown on the tab.
    pub fn icon(&self, cx: &App) -> ui::IconName {
        match &self.kind {
            ItemKind::Editor { diff: Some(_), .. } => ui::IconName::GitCompare,
            ItemKind::Editor { editor, .. } => match editor.read(cx).path() {
                Some(path) if syntax::detect(path).is_some_and(|lang| lang.is_code()) => {
                    ui::IconName::FileCode
                }
                Some(_) => ui::IconName::FileText,
                None => ui::IconName::File,
            },
            ItemKind::Terminal {
                harness: Some(_), ..
            } => ui::IconName::Bot,
            ItemKind::Terminal { .. } => ui::IconName::SquareTerminal,
            ItemKind::Settings(_) => ui::IconName::Settings,
            ItemKind::Preview(_) => ui::IconName::Eye,
            ItemKind::Image(_) => ui::IconName::Image,
        }
    }

    pub fn editor(&self) -> Option<&Entity<Editor>> {
        match &self.kind {
            ItemKind::Editor { editor, .. } => Some(editor),
            _ => None,
        }
    }

    pub fn diff(&self) -> Option<&DiffTab> {
        match &self.kind {
            ItemKind::Editor { diff, .. } => diff.as_ref(),
            _ => None,
        }
    }

    pub fn terminal(&self) -> Option<&Entity<TerminalView>> {
        match &self.kind {
            ItemKind::Terminal { view, .. } => Some(view),
            _ => None,
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.kind {
            ItemKind::Editor { editor, .. } => editor.focus_handle(cx),
            ItemKind::Terminal { view, .. } => view.focus_handle(cx),
            ItemKind::Settings(view) => view.focus_handle(cx),
            ItemKind::Preview(view) => view.focus_handle(cx),
            ItemKind::Image(view) => view.focus_handle(cx),
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        match &self.kind {
            ItemKind::Editor { editor, .. } => editor.read(cx).title(),
            ItemKind::Terminal { view, name, .. } => {
                name.clone().unwrap_or_else(|| view.read(cx).title())
            }
            ItemKind::Settings(_) => "Settings".into(),
            ItemKind::Preview(view) => view.read(cx).title(),
            ItemKind::Image(view) => view.read(cx).title(),
        }
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        self.editor()
            .is_some_and(|editor| editor.read(cx).is_dirty())
    }

    pub fn needs_attention(&self, cx: &App) -> bool {
        self.terminal()
            .is_some_and(|view| view.read(cx).needs_attention())
    }

    /// What a terminal's notification said, while it needs attention.
    pub fn notification(&self, cx: &App) -> Option<SharedString> {
        self.terminal()
            .and_then(|view| view.read(cx).notification())
    }

    /// Progress a terminal program reported, e.g. an agent at work.
    pub fn progress(&self, cx: &App) -> Option<terminal::Progress> {
        self.terminal().and_then(|view| view.read(cx).progress())
    }

    pub fn view(&self) -> gpui::AnyView {
        match &self.kind {
            ItemKind::Editor { editor, .. } => editor.clone().into(),
            ItemKind::Terminal { view, .. } => view.clone().into(),
            ItemKind::Settings(view) => view.clone().into(),
            ItemKind::Preview(view) => view.clone().into(),
            ItemKind::Image(view) => view.clone().into(),
        }
    }
}

#[derive(Default)]
pub(crate) struct Pane {
    pub items: Vec<Item>,
    pub active: usize,
}

impl Pane {
    pub fn active_item(&self) -> Option<&Item> {
        self.items.get(self.active)
    }

    pub fn position(&self, id: EntityId) -> Option<usize> {
        self.items.iter().position(|item| item.id() == id)
    }
}

/// A layout tree: panes, or splits of smaller trees.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaneNode {
    Pane(PaneId),
    Split {
        axis: Axis,
        children: Vec<PaneNode>,
        /// Relative sizes of the children.
        flexes: Vec<f32>,
    },
}

impl PaneNode {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            PaneNode::Pane(id) => out.push(*id),
            PaneNode::Split { children, .. } => {
                for child in children {
                    child.collect(out);
                }
            }
        }
    }

    pub fn first_pane(&self) -> PaneId {
        match self {
            PaneNode::Pane(id) => *id,
            PaneNode::Split { children, .. } => children[0].first_pane(),
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            PaneNode::Pane(pane) => *pane == id,
            PaneNode::Split { children, .. } => children.iter().any(|child| child.contains(id)),
        }
    }

    /// Puts `new` beside `target`. Returns false if `target` isn't here.
    pub fn split(&mut self, target: PaneId, new: PaneId, side: Side) -> bool {
        let axis = side.axis();
        match self {
            PaneNode::Pane(id) if *id == target => {
                let children = if side.before() {
                    vec![PaneNode::Pane(new), PaneNode::Pane(target)]
                } else {
                    vec![PaneNode::Pane(target), PaneNode::Pane(new)]
                };
                *self = PaneNode::Split {
                    axis,
                    children,
                    flexes: vec![1., 1.],
                };
                true
            }
            PaneNode::Pane(_) => false,
            PaneNode::Split {
                axis: split_axis,
                children,
                flexes,
            } => {
                // Splitting along the same axis adds a sibling instead of
                // nesting, sharing the target's space.
                if *split_axis == axis
                    && let Some(ix) = children
                        .iter()
                        .position(|child| *child == PaneNode::Pane(target))
                {
                    let half = flexes[ix] / 2.;
                    flexes[ix] = half;
                    let at = if side.before() { ix } else { ix + 1 };
                    children.insert(at, PaneNode::Pane(new));
                    flexes.insert(at, half);
                    return true;
                }
                children
                    .iter_mut()
                    .any(|child| child.split(target, new, side))
            }
        }
    }

    /// Removes a pane, collapsing splits left with one child. The root pane
    /// itself can't be removed.
    pub fn remove(&mut self, id: PaneId) -> bool {
        let PaneNode::Split {
            axis,
            children,
            flexes,
        } = self
        else {
            return false;
        };
        let removed = if let Some(ix) = children
            .iter()
            .position(|child| *child == PaneNode::Pane(id))
        {
            children.remove(ix);
            flexes.remove(ix);
            true
        } else {
            children.iter_mut().any(|child| child.remove(id))
        };
        if !removed {
            return false;
        }
        // Merge a child split with the same direction into this one.
        let mut ix = 0;
        while ix < children.len() {
            if let PaneNode::Split {
                axis: child_axis, ..
            } = &children[ix]
                && *child_axis == *axis
            {
                let PaneNode::Split {
                    children: grandchildren,
                    flexes: grand_flexes,
                    ..
                } = children.remove(ix)
                else {
                    unreachable!()
                };
                let share = flexes.remove(ix);
                let total: f32 = grand_flexes.iter().sum();
                let count = grandchildren.len();
                for (offset, (child, flex)) in
                    grandchildren.into_iter().zip(grand_flexes).enumerate()
                {
                    children.insert(ix + offset, child);
                    flexes.insert(ix + offset, share * flex / total);
                }
                ix += count;
            } else {
                ix += 1;
            }
        }
        if children.len() == 1 {
            *self = children.remove(0);
        }
        true
    }

    /// The node at a path of child indices.
    pub fn at_mut(&mut self, path: &[usize]) -> Option<&mut PaneNode> {
        match path.split_first() {
            None => Some(self),
            Some((ix, rest)) => match self {
                PaneNode::Split { children, .. } => children.get_mut(*ix)?.at_mut(rest),
                PaneNode::Pane(_) => None,
            },
        }
    }
}

/// The payload when dragging a tab.
#[derive(Clone)]
pub(crate) struct DraggedTab {
    pub item: EntityId,
    pub title: SharedString,
}

/// What follows the mouse while dragging a tab.
pub(crate) struct DragPreview {
    title: SharedString,
}

impl DragPreview {
    pub fn new(title: SharedString) -> Self {
        Self { title }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(theme::elevated_bg())
            .border_1()
            .border_color(theme::accent())
            .shadow_lg()
            .font_family(theme::ui_font())
            .text_size(theme::ui_font_size())
            .text_color(theme::text())
            .opacity(0.9)
            .max_w(px(240.))
            .child(self.title.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(children: Vec<PaneNode>, flexes: Vec<f32>) -> PaneNode {
        PaneNode::Split {
            axis: Axis::Row,
            children,
            flexes,
        }
    }

    #[test]
    fn splitting_nests_or_adds_siblings() {
        let mut tree = PaneNode::Pane(1);
        assert!(tree.split(1, 2, Side::Right));
        assert_eq!(
            tree,
            row(vec![PaneNode::Pane(1), PaneNode::Pane(2)], vec![1., 1.])
        );
        // Same direction: a sibling sharing pane 1's space.
        assert!(tree.split(1, 3, Side::Left));
        assert_eq!(
            tree,
            row(
                vec![PaneNode::Pane(3), PaneNode::Pane(1), PaneNode::Pane(2)],
                vec![0.5, 0.5, 1.]
            )
        );
        // Other direction: nests.
        assert!(tree.split(2, 4, Side::Down));
        assert_eq!(tree.panes(), [3, 1, 2, 4]);
        assert!(!tree.split(9, 5, Side::Down));
    }

    #[test]
    fn removing_collapses_and_merges() {
        let mut tree = PaneNode::Pane(1);
        tree.split(1, 2, Side::Right);
        tree.split(2, 3, Side::Down);
        // Removing 2 leaves a column with only 3, which collapses into the row.
        assert!(tree.remove(2));
        assert_eq!(
            tree,
            row(vec![PaneNode::Pane(1), PaneNode::Pane(3)], vec![1., 1.])
        );
        assert!(tree.remove(3));
        assert_eq!(tree, PaneNode::Pane(1));
        // The last pane stays.
        assert!(!tree.remove(1));
    }

    #[test]
    fn removal_merges_same_direction_splits() {
        // Row[1, Column[2, Row[3, 4]]]: removing 2 leaves Row[3, 4] inside
        // the outer row, which merges into it.
        let mut tree = PaneNode::Pane(1);
        tree.split(1, 2, Side::Right);
        tree.split(2, 3, Side::Down);
        tree.split(3, 4, Side::Right);
        assert!(tree.remove(2));
        assert_eq!(tree.panes(), [1, 3, 4]);
        let PaneNode::Split {
            children, flexes, ..
        } = &tree
        else {
            panic!("expected a split");
        };
        assert_eq!(children.len(), 3);
        assert_eq!(flexes, &vec![1., 0.5, 0.5]);
    }
}
