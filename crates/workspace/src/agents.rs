//! Agents: the sidebar view of every terminal (agent harnesses first), the
//! files each agent changed, and follow mode: a diff tab showing the file the
//! followed agent is editing, against the last commit, with the cursor on
//! the agent's latest edit.
//!
//! Which agent changed a file is inferred from signals every harness gives
//! (see [`Workspace::change_owner`]); nothing here polls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use editor::Editor;

use gpui::{
    AnyElement, AppContext, ClickEvent, Context, Div, EntityId, FontWeight, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use ui::{IconName, Menu, MenuEntry};

use crate::diff_view::{self, DiffContent, DiffTarget};
use crate::pane::{DiffTab, Item, ItemKind, PaneId};
use crate::pane_view::MenuAction;
use crate::workspace::{MenuKind, Workspace};

/// Output this recent marks the agent that is working when several run.
const RECENT_OUTPUT: Duration = Duration::from_secs(3);
/// Changed files listed under an agent in the view.
const SHOWN_FILES: usize = 5;
/// Unchanged lines shown around each change in the follow tab.
const CONTEXT_LINES: u32 = 3;
/// Larger files aren't remembered, so their edits are found from the diff.
const MAX_REMEMBERED_BYTES: usize = 1 << 20;
const MAX_REMEMBERED_FILES: usize = 64;

/// The agent whose edits the editor is following.
pub(crate) struct Following {
    pub terminal: EntityId,
    /// The follow diff tab, once shown. Closing it stops following.
    pub tab: Option<EntityId>,
    /// The last text seen of each followed file, to find what an edit
    /// changed.
    seen: HashMap<PathBuf, Arc<str>>,
    /// Orders diff jobs, so a slow one can't overwrite a newer one.
    next_job: u64,
    shown_job: u64,
}

/// A followed file, diffed in the background.
struct FollowUpdate {
    text: String,
    content: DiffContent,
    /// The zero-based line of the agent's latest edit.
    edited: Option<u32>,
}

/// What the view shows for one terminal.
struct Row {
    pane: PaneId,
    id: EntityId,
    title: SharedString,
    agent: bool,
    state: Option<SharedString>,
    attention: bool,
    working: bool,
    running: bool,
}

impl Workspace {
    // ---- attribution ---------------------------------------------------------

    /// The agent terminal that most likely made a change on disk: the only
    /// running agent, else the only one reporting progress, else the one
    /// that printed most recently (agents print each edit as they make it).
    fn change_owner(&self, cx: &gpui::App) -> Option<EntityId> {
        let running: Vec<(EntityId, Option<Instant>, bool)> = self
            .items()
            .filter_map(|(_, item)| {
                let view = item.terminal()?.read(cx);
                (view.harness().is_some() && view.is_running())
                    .then(|| (item.id(), view.last_output(), view.progress().is_some()))
            })
            .collect();
        pick_owner(&running, Instant::now())
    }

    /// A file changed on disk, not by Ion. `row` is the first changed row,
    /// when the file was open and Ion saw the old text.
    pub(crate) fn note_external_change(
        &mut self,
        path: PathBuf,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(owner) = self.change_owner(cx) else {
            return;
        };
        let touched = self.touched.entry(owner).or_default();
        touched.retain(|known| *known != path);
        touched.push(path.clone());
        self.note_recent_file(path.clone());
        if self
            .following
            .as_ref()
            .is_some_and(|following| following.terminal == owner)
        {
            self.follow_to(path, row, window, cx);
        }
        cx.notify();
    }

    /// Forgets a closed tab. Closing the agent or the follow tab stops
    /// following.
    pub(crate) fn forget_item(&mut self, id: EntityId, cx: &mut Context<Self>) {
        self.touched.remove(&id);
        self.forget_proposal(id);
        self.forget_problems(id, cx);
        self.task_terminals.retain(|_, terminal| *terminal != id);
        if self
            .following
            .as_ref()
            .is_some_and(|following| following.terminal == id || following.tab == Some(id))
        {
            self.following = None;
            cx.notify();
        }
    }

    // ---- follow mode ---------------------------------------------------------

    pub(crate) fn follow(
        &mut self,
        terminal: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Switching agents keeps the tab.
        let tab = self.following.take().and_then(|following| following.tab);
        self.following = Some(Following {
            terminal,
            tab,
            seen: HashMap::new(),
            next_job: 0,
            shown_job: 0,
        });
        // Catch up with the agent's latest edit.
        if let Some(path) = self.touched.get(&terminal).and_then(|files| files.last()) {
            self.follow_to(path.clone(), None, window, cx);
        }
        cx.notify();
    }

    pub(crate) fn stop_following(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(following) = self.following.take() else {
            return;
        };
        if let Some((pane, ix)) = following.tab.and_then(|tab| self.find_item(tab)) {
            self.remove_item(pane, ix, window, cx);
        }
        cx.notify();
    }

    /// Shows a followed file's diff in the follow tab, without taking focus
    /// from the terminal. `row` is where the edit started, when known.
    fn follow_to(
        &mut self,
        path: PathBuf,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(following) = &mut self.following else {
            return;
        };
        following.next_job += 1;
        let job = following.next_job;
        let previous = following.seen.get(&path).cloned();
        let terminal = following.terminal;
        let agent = self
            .items()
            .find(|(_, item)| item.id() == terminal)
            .map(|(_, item)| item.title(cx).to_string())
            .unwrap_or_default();
        let filesystem = self.filesystem.clone();
        let git = self
            .git
            .as_ref()
            .map(|git| (git.repo.clone(), git.paths.clone()));
        let display = self
            .root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let update = cx.background_spawn({
            let path = path.clone();
            async move {
                let text = filesystem.load_text(&path).ok()?.text;
                let (relative, base) = match &git {
                    Some((repo, paths)) => match paths.relative(&path) {
                        Some(relative) => {
                            let base = repo.head_text(&relative).ok().flatten();
                            (relative, base)
                        }
                        None => (display, None),
                    },
                    None => (display, None),
                };
                // Without a commit to compare with, show what changed since
                // follow mode first saw the file.
                let base = base.or_else(|| previous.as_deref().map(str::to_owned));
                let diff = git::diff_texts(&relative, base.as_deref(), &text, CONTEXT_LINES);
                let edited = match &previous {
                    Some(previous) => git::first_changed_line(previous, &text),
                    None => row.map(|row| row as u32),
                };
                let content = diff_view::follow(agent, &relative, &[diff], &|_| path.clone());
                Some(FollowUpdate {
                    text,
                    content,
                    edited,
                })
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(update) = update.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.show_follow(job, path, update, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn show_follow(
        &mut self,
        job: u64,
        path: PathBuf,
        update: FollowUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(following) = &mut self.following else {
            return;
        };
        if job < following.shown_job {
            return;
        }
        following.shown_job = job;
        if update.text.len() <= MAX_REMEMBERED_BYTES {
            if following.seen.len() >= MAX_REMEMBERED_FILES && !following.seen.contains_key(&path) {
                following.seen.clear();
            }
            following.seen.insert(path, update.text.into());
        }
        let FollowUpdate {
            content, edited, ..
        } = update;
        let row = diff_view::cursor_row(&content.rows, edited);
        let open = following.tab.and_then(|tab| self.find_item(tab));
        if let Some((pane, ix)) = open
            && let Some(item) = self.panes.get_mut(&pane).map(|pane| &mut pane.items[ix])
            && let ItemKind::Editor {
                editor,
                diff: Some(diff),
            } = &mut item.kind
        {
            diff.jumps = content.jumps;
            diff.reverts = content.reverts;
            let revert_rows = diff.revert_rows();
            let editor = editor.clone();
            editor.update(cx, |editor, cx| {
                editor.replace_diff(
                    content.title,
                    &content.text,
                    content.rows,
                    content.language_hint,
                    cx,
                );
                editor.set_revert_rows(revert_rows, cx);
                editor.reveal_row(row, cx);
            });
            self.show_item(pane, ix, false, window, cx);
        } else {
            let diff = DiffTab {
                target: DiffTarget::Follow,
                jumps: content.jumps,
                reverts: content.reverts,
            };
            let editor = cx.new(|cx| {
                let mut editor = Editor::diff_view(
                    content.title,
                    &content.text,
                    content.rows,
                    content.language_hint,
                    cx,
                );
                editor.set_revert_rows(diff.revert_rows(), cx);
                editor.reveal_row(row, cx);
                editor
            });
            let item = self.editor_item(editor, Some(diff), window, cx);
            let id = item.id();
            let pane = self.file_pane();
            self.insert_item_with_focus(pane, item, false, window, cx);
            if let Some(following) = &mut self.following {
                following.tab = Some(id);
            }
        }
        cx.notify();
    }

    // ---- the view ------------------------------------------------------------

    fn rows(&self, cx: &gpui::App) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .items()
            .filter_map(|(pane, item)| {
                let view = item.terminal()?.read(cx);
                let agent = view.harness().is_some();
                let running = view.is_running();
                let attention = item.needs_attention(cx);
                let progress = view.progress();
                let state: Option<SharedString> = if !running {
                    Some("Exited".into())
                } else if attention {
                    Some(item.notification(cx).unwrap_or_else(|| {
                        if agent {
                            "Needs input"
                        } else {
                            "Needs attention"
                        }
                        .into()
                    }))
                } else if let Some(progress) = progress {
                    Some(match progress.percent {
                        Some(percent) => format!("Working · {percent}%").into(),
                        None => "Working".into(),
                    })
                } else if agent {
                    Some("Running".into())
                } else {
                    None
                };
                Some(Row {
                    pane,
                    id: item.id(),
                    title: item.title(cx),
                    agent,
                    state,
                    attention,
                    working: progress.is_some(),
                    running,
                })
            })
            .collect();
        // Agents first; otherwise in tab order.
        rows.sort_by_key(|row| !row.agent);
        rows
    }

    pub(crate) fn render_agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.rows(cx);
        if rows.is_empty() {
            return self.render_agents_empty(cx).into_any_element();
        }
        let followed = self.following.as_ref().map(|following| following.terminal);
        let active = self.active_item().map(Item::id);
        let mut list = div()
            .id("agents")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb_2();
        let shells_start = rows.iter().position(|row| !row.agent);
        for (ix, row) in rows.into_iter().enumerate() {
            if ix == 0 && row.agent {
                list = list.child(section("Agents"));
            }
            if Some(ix) == shells_start {
                list = list.child(section("Shells"));
            }
            let files = if row.agent {
                self.touched.get(&row.id).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            let is_followed = followed == Some(row.id);
            list = list.child(self.render_agent_row(&row, files.len(), is_followed, active, cx));
            for path in files.iter().rev().take(SHOWN_FILES) {
                list = list.child(self.render_touched_file(row.id, path, cx));
            }
        }
        list.into_any_element()
    }

    fn render_agent_row(
        &self,
        row: &Row,
        files: usize,
        followed: bool,
        active: Option<EntityId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (pane, id, agent) = (row.pane, row.id, row.agent);
        let icon = if agent {
            IconName::Bot
        } else {
            IconName::SquareTerminal
        };
        let icon_color = if !row.running {
            theme::text_faint()
        } else if row.attention || row.working {
            theme::accent()
        } else {
            theme::text_muted()
        };
        let detail = match (&row.state, files) {
            (Some(state), 0) => Some(state.clone()),
            (Some(state), 1) => Some(format!("{state} · 1 file").into()),
            (Some(state), files) => Some(format!("{state} · {files} files").into()),
            (None, _) => None,
        };
        div()
            .id(SharedString::from(format!("agent-{id:?}")))
            .mx_1()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|row| row.bg(theme::hover_bg()))
            .when(active == Some(id), |row| row.bg(theme::active_bg()))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(ui::icon_sized(icon, px(16.), icon_color))
                    .when(row.attention, |icon| {
                        icon.child(
                            div()
                                .absolute()
                                .top(px(-2.))
                                .right(px(-2.))
                                .size(px(7.))
                                .rounded_full()
                                .bg(theme::accent()),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_color(if row.running {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .child(row.title.clone()),
                    )
                    .children(detail.map(|detail| {
                        div()
                            .truncate()
                            .text_size(theme::ui_font_size_small())
                            .text_color(if row.attention {
                                theme::accent()
                            } else {
                                theme::text_faint()
                            })
                            .child(detail)
                    })),
            )
            .when(followed, |row| {
                row.child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(px(3.))
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .bg(theme::accent())
                        .text_color(theme::on_accent())
                        .child("FOLLOWING"),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if let Some(ix) = this.panes.get(&pane).and_then(|p| p.position(id)) {
                    this.activate_item(pane, ix, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.show_agent_menu(id, agent, event.position, cx);
                }),
            )
    }

    fn render_touched_file(
        &self,
        terminal: EntityId,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let relative = self
            .root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        let open = path.to_path_buf();
        div()
            .id(SharedString::from(format!(
                "touched-{terminal:?}-{relative}"
            )))
            .mx_1()
            .pl(px(34.))
            .pr_2()
            .h(px(22.))
            .flex()
            .items_center()
            .gap_1()
            .rounded(px(4.))
            .cursor_pointer()
            .text_size(theme::ui_font_size_small())
            .text_color(theme::text_muted())
            .hover(|row| row.bg(theme::hover_bg()).text_color(theme::text()))
            .child(ui::icon_sized(
                IconName::FileCode,
                px(13.),
                theme::text_faint(),
            ))
            .child(div().flex_none().child(name))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_faint())
                    .child(relative),
            )
            .tooltip(ui::text_tooltip(path.to_string_lossy().into_owned()))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_file(open.clone(), window, cx)
            }))
    }

    fn render_agents_empty(&self, cx: &mut Context<Self>) -> Div {
        let mut empty = div()
            .px_4()
            .pt_2()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(theme::ui_font_size_small())
            .text_color(theme::text_faint())
            .child("No terminals are open.");
        let harnesses = self.harnesses();
        let start = |id: SharedString, icon: IconName, label: SharedString| {
            div()
                .id(id)
                .mx(px(-8.))
                .px_2()
                .h(px(26.))
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(4.))
                .cursor_pointer()
                .text_color(theme::text_muted())
                .hover(|row| row.bg(theme::hover_bg()).text_color(theme::text()))
                .child(ui::icon_sized(icon, px(14.), theme::accent()))
                .child(label)
        };
        for harness in harnesses {
            let kind = harness.kind;
            empty = empty.child(
                start(
                    format!("start-{}", kind.command()).into(),
                    IconName::Bot,
                    format!("New {}", kind.name()).into(),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let pane = this.terminal_pane();
                    this.new_agent_in(pane, harness.clone(), window, cx);
                })),
            );
        }
        empty.child(
            start(
                "start-terminal".into(),
                IconName::SquareTerminal,
                "New Terminal".into(),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                let pane = this.terminal_pane();
                this.new_terminal_in(pane, window, cx);
            })),
        )
    }

    fn show_agent_menu(
        &mut self,
        id: EntityId,
        agent: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let mut entries = Vec::new();
        if agent {
            let followed = self
                .following
                .as_ref()
                .is_some_and(|following| following.terminal == id);
            entries.push(if followed {
                self.menu_entry(
                    "Stop Following",
                    IconName::X,
                    MenuAction::StopFollowing,
                    None,
                    cx,
                )
            } else {
                self.menu_entry(
                    "Follow",
                    IconName::Monitor,
                    MenuAction::Follow(id),
                    None,
                    cx,
                )
            });
            entries.push(self.menu_entry(
                "Restart",
                IconName::RefreshCw,
                MenuAction::RestartAgent(id),
                None,
                cx,
            ));
            entries.push(MenuEntry::Separator);
        }
        entries.push(self.menu_entry(
            "Rename…",
            IconName::Pencil,
            MenuAction::RenameTerminal(id),
            None,
            cx,
        ));
        entries.push(self.menu_entry("Close", IconName::X, MenuAction::CloseItem(id), None, cx));
        self.open_menu(MenuKind::Agent, Menu::new(position, entries), cx);
    }

    /// The status bar item while following an agent.
    pub(crate) fn following_label(&self, cx: &gpui::App) -> Option<SharedString> {
        let following = self.following.as_ref()?;
        let (_, item) = self
            .items()
            .find(|(_, item)| item.id() == following.terminal)?;
        Some(format!("Following {}", item.title(cx)).into())
    }
}

/// The rule behind [`Workspace::change_owner`], for running agents given as
/// (id, last output, reports progress).
fn pick_owner<T: Copy>(running: &[(T, Option<Instant>, bool)], now: Instant) -> Option<T> {
    if let [(id, ..)] = running {
        return Some(*id);
    }
    let mut busy = running.iter().filter(|(_, _, progress)| *progress);
    if let (Some((id, ..)), None) = (busy.next(), busy.next()) {
        return Some(*id);
    }
    running
        .iter()
        .filter_map(|(id, output, _)| Some((*id, (*output)?)))
        .filter(|(_, output)| now.saturating_duration_since(*output) < RECENT_OUTPUT)
        .max_by_key(|(_, output)| *output)
        .map(|(id, _)| id)
}

fn section(title: &'static str) -> Div {
    div()
        .px_4()
        .pt_2()
        .pb_1()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_faint())
        .child(title.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_changes_to_the_likely_agent() {
        let now = Instant::now();
        let ago = |secs| Some(now - Duration::from_secs(secs));
        assert_eq!(pick_owner::<u8>(&[], now), None);
        // The only agent, however quiet.
        assert_eq!(pick_owner(&[(1, None, false)], now), Some(1));
        // The only one reporting progress.
        assert_eq!(
            pick_owner(&[(1, ago(0), false), (2, ago(9), true)], now),
            Some(2)
        );
        // Otherwise the one that printed last, if recently.
        let quiet = [(1, ago(1), true), (2, ago(0), true), (3, None, false)];
        assert_eq!(pick_owner(&quiet, now), Some(2));
        assert_eq!(
            pick_owner(&[(1, ago(10), false), (2, ago(20), false)], now),
            None
        );
    }
}
