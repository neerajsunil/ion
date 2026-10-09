//! Problems: errors and warnings read from build and test output in
//! terminals. When a command's output pauses, it is parsed (rustc, tsc,
//! gcc, ESLint, Python and the like), its paths resolved against the
//! terminal's folder and the project, and the results listed in the
//! Problems tab, underlined in open editors, and offered to agents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use editor::{Diagnostic, DiagnosticSeverity, Editor};
use gpui::{
    AppContext, ClickEvent, Context, Div, Entity, EntityId, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use project::FileSystem;
use terminal::{Severity, TerminalView};
use ui::IconName;

use crate::pane::{Item, ItemKind};
use crate::workspace::Workspace;

/// How much of a command's output is scanned.
const SCANNED_LINES: usize = 4000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    pub path: PathBuf,
    /// Zero-based.
    pub row: usize,
    /// Zero-based, if the tool printed one.
    pub column: Option<usize>,
    pub severity: Severity,
    pub message: SharedString,
}

/// The problems one terminal's last command printed.
pub(crate) struct TerminalProblems {
    /// The line the command was typed on.
    command: Option<String>,
    items: Vec<Problem>,
}

/// Compares paths the way the file system does.
fn path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && !text.starts_with('/') {
        text.to_lowercase()
    } else {
        text
    }
}

/// Finds the files problems name, keeping those that exist.
fn resolve(
    found: Vec<terminal::Problem>,
    cwd: Option<PathBuf>,
    root: Option<PathBuf>,
    filesystem: FileSystem,
) -> Vec<Problem> {
    let remote = filesystem.remote().is_some();
    let mut known: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut problems: Vec<Problem> = Vec::new();
    for problem in found {
        let path = known
            .entry(problem.path.clone())
            .or_insert_with(|| {
                let text = &problem.path;
                let absolute = if remote {
                    text.starts_with('/')
                } else {
                    Path::new(text).is_absolute()
                };
                let candidates: Vec<PathBuf> = if absolute {
                    vec![PathBuf::from(text)]
                } else {
                    let relative = text.replace('\\', "/");
                    let relative = relative.trim_start_matches("./");
                    cwd.iter()
                        .chain(root.iter())
                        .map(|base| project::join_path(base, relative))
                        .collect()
                };
                candidates.into_iter().find(|path| {
                    filesystem.exists(path).unwrap_or(false)
                        && !filesystem.is_dir(path).unwrap_or(true)
                })
            })
            .clone();
        let Some(path) = path else {
            continue;
        };
        let problem = Problem {
            path,
            row: problem.line.saturating_sub(1),
            column: problem.column.checked_sub(1),
            severity: problem.severity,
            message: problem.message.into(),
        };
        if !problems.contains(&problem) {
            problems.push(problem);
        }
    }
    problems
}

impl Workspace {
    /// Every problem, errors first, then by file and line.
    pub(crate) fn all_problems(&self) -> Vec<&Problem> {
        let mut all: Vec<&Problem> = self
            .problems
            .values()
            .flat_map(|problems| &problems.items)
            .collect();
        all.sort_by(|a, b| {
            (path_key(&a.path), a.row, a.column).cmp(&(path_key(&b.path), b.row, b.column))
        });
        all.dedup_by(|a, b| a == b);
        all
    }

    pub(crate) fn problem_counts(&self) -> (usize, usize) {
        let all = self.all_problems();
        let errors = all
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
            .count();
        (errors, all.len() - errors)
    }

    /// Reads the problems out of a terminal's last command, in the
    /// background.
    pub(crate) fn scan_problems(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let id = view.entity_id();
        let (output, command) = {
            let view = view.read(cx);
            (
                view.command_output(SCANNED_LINES),
                view.command_line().map(str::to_owned),
            )
        };
        let cwd =
            self.find_item(id)
                .and_then(|(pane, ix)| match &self.panes[&pane].items[ix].kind {
                    ItemKind::Terminal { cwd, .. } => cwd.clone(),
                    _ => None,
                });
        let root = self.root.clone();
        let filesystem = self.filesystem.clone();
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    resolve(terminal::parse_problems(&output), cwd, root, filesystem)
                })
                .await;
            this.update(cx, |this, cx| {
                this.problem_scans.remove(&id);
                this.set_problems(id, command, found, cx)
            })
            .ok();
        });
        self.problem_scans.insert(id, task);
    }

    /// Stores a terminal's problems. A command with no problems clears
    /// them only if it's the one that reported them (a rebuild that passes),
    /// so an `ls` in between doesn't.
    fn set_problems(
        &mut self,
        terminal: EntityId,
        command: Option<String>,
        items: Vec<Problem>,
        cx: &mut Context<Self>,
    ) {
        match self.problems.get(&terminal) {
            Some(existing) if existing.items == items => return,
            Some(existing) if items.is_empty() && existing.command != command => return,
            None if items.is_empty() => return,
            _ => {}
        }
        if items.is_empty() {
            self.problems.remove(&terminal);
        } else {
            self.problems
                .insert(terminal, TerminalProblems { command, items });
        }
        self.apply_problems(cx);
        cx.notify();
    }

    /// Forgets a closed terminal's problems.
    pub(crate) fn forget_problems(&mut self, terminal: EntityId, cx: &mut Context<Self>) {
        self.problem_scans.remove(&terminal);
        if self.problems.remove(&terminal).is_some() {
            self.apply_problems(cx);
            cx.notify();
        }
    }

    fn clear_problems(&mut self, cx: &mut Context<Self>) {
        self.problems.clear();
        self.apply_problems(cx);
        cx.notify();
    }

    /// The problems in one file, for its editor.
    fn diagnostics_for(&self, path: &Path) -> Vec<Diagnostic> {
        let key = path_key(path);
        self.all_problems()
            .into_iter()
            .filter(|problem| path_key(&problem.path) == key)
            .map(|problem| Diagnostic {
                row: problem.row,
                column: problem.column,
                severity: match problem.severity {
                    Severity::Error => DiagnosticSeverity::Error,
                    Severity::Warning => DiagnosticSeverity::Warning,
                },
                message: problem.message.clone(),
            })
            .collect()
    }

    /// Shows the problems in an editor (not a diff: its rows differ).
    pub(crate) fn apply_problems_to(&self, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let diagnostics = match editor.read(cx).path() {
            Some(path) => self.diagnostics_for(path),
            None => Vec::new(),
        };
        editor.update(cx, |editor, cx| editor.set_diagnostics(diagnostics, cx));
    }

    fn apply_problems(&self, cx: &mut Context<Self>) {
        self.refresh_problems_view(cx);
        let editors: Vec<Entity<Editor>> = self
            .items()
            .filter_map(|(_, item)| match &item.kind {
                ItemKind::Editor { editor, diff: None } => Some(editor.clone()),
                _ => None,
            })
            .collect();
        for editor in editors {
            self.apply_problems_to(&editor, cx);
        }
    }

    /// The Problems tab, wherever it was moved to.
    fn problems_tab(&self) -> Option<(crate::pane::PaneId, Entity<ProblemsView>)> {
        self.items().find_map(|(pane, item)| match &item.kind {
            ItemKind::Problems(view) => Some((pane, view.clone())),
            _ => None,
        })
    }

    /// Hands the Problems tab the current list.
    fn refresh_problems_view(&self, cx: &mut Context<Self>) {
        if let Some((_, view)) = self.problems_tab() {
            let problems = self.all_problems().into_iter().cloned().collect();
            let root = self.root.clone();
            view.update(cx, |view, cx| view.set_problems(problems, root, cx));
        }
    }

    /// A Problems tab, kept up to date with the list.
    pub(crate) fn problems_item(&self, window: &mut Window, cx: &mut Context<Self>) -> Item {
        let view = cx.new(ProblemsView::new);
        let problems = self.all_problems().into_iter().cloned().collect();
        let root = self.root.clone();
        view.update(cx, |view, cx| view.set_problems(problems, root, cx));
        let subscription =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                ProblemsEvent::Open { path, point } => {
                    this.open_file_at(path.clone(), Some((*point, *point)), window, cx)
                }
                ProblemsEvent::SendToAgent => this.send_problems_to_agent(window, cx),
                ProblemsEvent::Clear => this.clear_problems(cx),
            });
        Item {
            kind: ItemKind::Problems(view),
            _subscriptions: vec![subscription],
        }
    }

    /// Shows the Problems tab (opening it in the dock, like a terminal), or
    /// hides the dock if the tab is already showing there.
    pub(crate) fn show_problems(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((pane, view)) = self.problems_tab() {
            let Some(ix) = self.panes[&pane].position(view.entity_id()) else {
                return;
            };
            let showing = self.panes[&pane].active == ix;
            if showing && self.dock.contains(pane) && self.dock_visible {
                self.dock_visible = false;
                if self.zoomed.is_some_and(|pane| self.dock.contains(pane)) {
                    self.zoomed = None;
                }
                if self.dock.contains(self.active_pane) {
                    self.active_pane = self.file_pane();
                    self.focus_active(window, cx);
                }
                self.layout_changed(cx);
            } else {
                self.activate_item(pane, ix, window, cx);
                self.layout_changed(cx);
            }
            return;
        }
        let pane = if self.dock.contains(self.last_dock_pane) {
            self.last_dock_pane
        } else {
            self.dock.first_pane()
        };
        let item = self.problems_item(window, cx);
        self.insert_item(pane, item, window, cx);
    }

    /// How a problem's file is named for people and agents.
    fn problem_path(&self, path: &Path) -> String {
        self.root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Types the problems into the agent's terminal for it to fix.
    pub(crate) fn send_problems_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lines: Vec<String> = self
            .all_problems()
            .into_iter()
            .map(|problem| {
                let severity = match problem.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                };
                let position = match problem.column {
                    Some(column) => format!("{}:{}", problem.row + 1, column + 1),
                    None => (problem.row + 1).to_string(),
                };
                format!(
                    "{}:{position}: {severity}: {}",
                    self.problem_path(&problem.path),
                    problem.message
                )
            })
            .collect();
        if lines.is_empty() {
            return;
        }
        let Some((pane, ix)) = self.agent_terminal(cx) else {
            self.status = Some("Start an agent or a terminal first".into());
            cx.notify();
            return;
        };
        let ItemKind::Terminal { view, .. } = &self.panes[&pane].items[ix].kind else {
            return;
        };
        let text = format!("Fix these problems:\n{}\n", lines.join("\n"));
        view.clone()
            .update(cx, |view, cx| view.insert_text(&text, cx));
        self.activate_item(pane, ix, window, cx);
    }

    /// `getDiagnostics` for `/ide`: problems per file, for one file or all.
    pub(crate) fn ide_problems(&self, path: Option<&Path>) -> serde_json::Value {
        let key = path.map(path_key);
        let mut files: Vec<(PathBuf, Vec<serde_json::Value>)> = Vec::new();
        for problem in self.all_problems() {
            if key
                .as_ref()
                .is_some_and(|key| *key != path_key(&problem.path))
            {
                continue;
            }
            let column = problem.column.unwrap_or(0);
            let diagnostic = serde_json::json!({
                "message": problem.message.as_ref(),
                "severity": match problem.severity {
                    Severity::Error => "Error",
                    Severity::Warning => "Warning",
                },
                "range": {
                    "start": { "line": problem.row, "character": column },
                    "end": { "line": problem.row, "character": column },
                },
                "source": "ion",
            });
            match files.last_mut() {
                Some((last, diagnostics)) if *last == problem.path => diagnostics.push(diagnostic),
                _ => files.push((problem.path.clone(), vec![diagnostic])),
            }
        }
        serde_json::Value::Array(
            files
                .into_iter()
                .map(|(path, diagnostics)| {
                    serde_json::json!({
                        "uri": crate::ide::file_uri(&path),
                        "diagnostics": diagnostics,
                    })
                })
                .collect(),
        )
    }

    /// Error and warning counts for the status bar; opens the list.
    pub(crate) fn render_problem_counts(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.root.as_ref()?;
        let (errors, warnings) = self.problem_counts();
        let count = |icon, count: usize, color| {
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .child(ui::icon_sized(
                    icon,
                    px(13.),
                    if count > 0 {
                        color
                    } else {
                        theme::text_muted()
                    },
                ))
                .child(count.to_string())
        };
        Some(
            div()
                .id("problem-counts")
                .h(px(20.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|item| item.bg(theme::hover_bg()).text_color(theme::text()))
                .child(count(IconName::CircleX, errors, theme::git_deleted()))
                .child(count(
                    IconName::TriangleAlert,
                    warnings,
                    theme::git_modified(),
                ))
                .tooltip(ui::tooltip(
                    "Problems",
                    Some(Box::new(crate::workspace::ShowProblems)),
                ))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.show_problems(window, cx)),
                ),
        )
    }
}

/// What the Problems tab asks the workspace to do.
pub(crate) enum ProblemsEvent {
    Open {
        path: PathBuf,
        point: (usize, usize),
    },
    SendToAgent,
    Clear,
}

/// The Problems tab: every problem, grouped by file. It lives in the dock
/// by default and moves between panes like any tab.
pub(crate) struct ProblemsView {
    focus_handle: FocusHandle,
    /// Sorted by file, then line.
    problems: Vec<Problem>,
    root: Option<PathBuf>,
}

impl gpui::EventEmitter<ProblemsEvent> for ProblemsView {}

impl Focusable for ProblemsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ProblemsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            problems: Vec::new(),
            root: None,
        }
    }

    pub fn set_problems(
        &mut self,
        problems: Vec<Problem>,
        root: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.problems != problems || self.root != root {
            self.problems = problems;
            self.root = root;
            cx.notify();
        }
    }

    pub fn title(&self) -> SharedString {
        match self.problems.len() {
            0 => "Problems".into(),
            count => format!("Problems ({count})").into(),
        }
    }

    fn display_path(&self, path: &Path) -> String {
        self.root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let errors = self
            .problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
            .count();
        let warnings = self.problems.len() - errors;
        let summary = format!(
            "{errors} error{}, {warnings} warning{}",
            if errors == 1 { "" } else { "s" },
            if warnings == 1 { "" } else { "s" },
        );
        let empty = self.problems.is_empty();
        div()
            .h(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .text_size(theme::ui_font_size_small())
            .text_color(theme::text_faint())
            .child(summary)
            .child(div().flex_1())
            .when(!empty, |bar| {
                bar.child(
                    ui::icon_button("problems-send", IconName::Bot)
                        .tooltip(ui::tooltip("Send to Agent", None))
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                            cx.emit(ProblemsEvent::SendToAgent)
                        })),
                )
                .child(
                    ui::icon_button("problems-clear", IconName::X)
                        .tooltip(ui::tooltip("Clear", None))
                        .on_click(
                            cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ProblemsEvent::Clear)),
                        ),
                )
            })
    }

    fn render_file(&self, path: &Path, count: usize) -> Div {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = Path::new(&self.display_path(path))
            .parent()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default();
        div()
            .mx_1()
            .px_2()
            .pt_1()
            .h(px(24.))
            .flex()
            .items_center()
            .gap_1()
            .text_size(theme::ui_font_size_small())
            .child(ui::icon_sized(
                IconName::FileCode,
                px(13.),
                theme::text_faint(),
            ))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(name),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_faint())
                    .child(dir),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_muted())
                    .child(count.to_string()),
            )
    }

    fn render_problem(
        &self,
        ix: usize,
        problem: &Problem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (icon, color) = match problem.severity {
            Severity::Error => (IconName::CircleX, theme::git_deleted()),
            Severity::Warning => (IconName::TriangleAlert, theme::git_modified()),
        };
        let position = match problem.column {
            Some(column) => format!("[Ln {}, Col {}]", problem.row + 1, column + 1),
            None => format!("[Ln {}]", problem.row + 1),
        };
        let path = problem.path.clone();
        let point = (problem.row, problem.column.unwrap_or(0));
        div()
            .id(("problem", ix))
            .mx_1()
            .pl(px(26.))
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
            .child(
                div()
                    .flex_none()
                    .child(ui::icon_sized(icon, px(13.), color)),
            )
            .child(
                div().min_w_0().truncate().child(
                    problem
                        .message
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme::text_faint())
                    .child(position),
            )
            .tooltip(ui::text_tooltip(problem.message.clone()))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(ProblemsEvent::Open {
                    path: path.clone(),
                    point,
                })
            }))
    }
}

impl Render for ProblemsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.problems.is_empty() {
            div()
                .px_3()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(theme::ui_font_size_small())
                .text_color(theme::text_faint())
                .child("No problems.")
                .child("Errors and warnings from builds and tests run in a terminal show up here.")
                .into_any_element()
        } else {
            let mut list = div()
                .id("problems")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .pb_2();
            let all = &self.problems;
            let mut ix = 0;
            while ix < all.len() {
                let path = &all[ix].path;
                let count = all[ix..]
                    .iter()
                    .take_while(|problem| problem.path == *path)
                    .count();
                list = list.child(self.render_file(path, count));
                for problem in &all[ix..ix + count] {
                    list = list.child(self.render_problem(ix, problem, cx));
                    ix += 1;
                }
            }
            list.into_any_element()
        };
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .child(self.render_toolbar(cx))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_keys_ignore_separators() {
        assert_eq!(
            path_key(Path::new("C:\\a\\b.rs")),
            path_key(Path::new("C:/a/b.rs"))
        );
        assert_ne!(
            path_key(Path::new("/a/B.rs")),
            path_key(Path::new("/a/b.rs"))
        );
    }
}
