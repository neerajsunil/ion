//! Claude Code's IDE integration (`/ide`): the agent sees which file is open
//! and what's selected, opens files, and proposes edits as diff tabs the
//! user accepts or rejects.
//!
//! One server per Ion process (in the `bridge` crate) advertises every open
//! folder. Tool calls go to the window whose folder holds the file, else
//! the active window. Terminals get the server's port in their environment,
//! so `claude` started in Ion connects on its own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bridge::{IdeEvent, IdeServer, ToolCall};
use editor::Editor;
use futures::StreamExt;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, EntityId, Global, InteractiveElement,
    IntoElement, ParentElement, StatefulInteractiveElement, Styled, Task, Window, WindowHandle,
    div, px,
};
use serde_json::{Value, json};

use crate::diff_view::{self, DiffTarget};
use crate::pane::{DiffTab, Item};
use crate::workspace::Workspace;

/// How long the selection rests before agents hear about it.
const SELECTION_DELAY: Duration = Duration::from_millis(150);
/// Lines of context around each change in a proposed edit.
const PROPOSAL_CONTEXT: u32 = 3;

struct IdeBridge {
    server: IdeServer,
    connections: usize,
    /// The selection last sent, for `getLatestSelection`.
    latest: Option<Value>,
    _events: Task<()>,
}

impl Global for IdeBridge {}

/// An edit an agent proposed (`openDiff`), shown as a diff tab until the
/// user accepts or rejects it.
pub(crate) struct Proposal {
    pub id: u64,
    pub item: EntityId,
    pub tab_name: String,
    /// The file as shown to the user, e.g. `src/main.rs`.
    pub display: String,
    pub contents: String,
    /// The agent's waiting `openDiff` call; `None` once answered.
    call: Option<ToolCall>,
}

/// Starts the server. Without it (no free port, say) Ion works as before.
pub fn init(cx: &mut App) {
    let Ok((server, mut events)) = IdeServer::start("Ion") else {
        return;
    };
    terminal::set_extra_env(server.terminal_env());
    let task = cx.spawn(async move |cx| {
        while let Some(event) = events.next().await {
            if cx.update(|cx| handle_event(event, cx)).is_err() {
                break;
            }
        }
    });
    cx.set_global(IdeBridge {
        server,
        connections: 0,
        latest: None,
        _events: task,
    });
    // Dropping the server removes the lock file.
    cx.on_app_quit(|cx| {
        if cx.has_global::<IdeBridge>() {
            cx.remove_global::<IdeBridge>();
        }
        async {}
    })
    .detach();
}

/// Whether an agent is connected.
pub(crate) fn connected(cx: &App) -> bool {
    cx.try_global::<IdeBridge>()
        .is_some_and(|bridge| bridge.connections > 0)
}

fn workspaces(cx: &App) -> Vec<WindowHandle<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<Workspace>())
        .collect()
}

/// Rewrites the lock file with every local window's folder. Call after a
/// folder opens or a window closes (deferred, so no workspace is borrowed).
pub fn refresh_folders(cx: &mut App) {
    cx.defer(|cx| {
        if !cx.has_global::<IdeBridge>() {
            return;
        }
        let folders: Vec<PathBuf> = workspaces(cx)
            .into_iter()
            .filter_map(|handle| {
                let workspace = handle.read(cx).ok()?;
                workspace.filesystem.remote().is_none().then_some(())?;
                workspace.root.clone()
            })
            .collect();
        cx.global::<IdeBridge>()
            .server
            .set_workspace_folders(&folders)
            .ok();
    });
}

fn handle_event(event: IdeEvent, cx: &mut App) {
    match event {
        IdeEvent::Connections(count) => {
            cx.global_mut::<IdeBridge>().connections = count;
            for handle in workspaces(cx) {
                handle.update(cx, |_, _, cx| cx.notify()).ok();
            }
        }
        IdeEvent::Call(call) => match target_workspace(&call, cx) {
            Some(handle) => {
                // If the window is gone, the call drops and answers with an error.
                handle
                    .update(cx, |workspace, window, cx| {
                        workspace.handle_ide_call(call, window, cx)
                    })
                    .ok();
            }
            None => call.reply_error("No Ion window is open"),
        },
    }
}

/// The window a call is for: the one whose folder holds the file it names,
/// else the active one, else any.
fn target_workspace(call: &ToolCall, cx: &App) -> Option<WindowHandle<Workspace>> {
    let handles = workspaces(cx);
    let path = ["filePath", "new_file_path", "old_file_path"]
        .iter()
        .find_map(|name| call.string(name))
        .map(ide_path);
    if let Some(path) = path {
        let owner = handles.iter().find(|handle| {
            handle
                .read(cx)
                .ok()
                .and_then(|workspace| workspace.root.as_deref())
                .is_some_and(|root| path.starts_with(root))
        });
        if let Some(owner) = owner {
            return Some(*owner);
        }
    }
    let active = cx
        .active_window()
        .and_then(|window| window.downcast::<Workspace>());
    active.or_else(|| handles.first().copied())
}

/// A path from an agent, with the platform's separators.
fn ide_path(path: &str) -> PathBuf {
    let path = path.strip_prefix("file://").map_or(path, uri_to_path_text);
    if cfg!(windows) {
        PathBuf::from(path.replace('/', "\\"))
    } else {
        PathBuf::from(path)
    }
}

/// `/C:/x%20y` (from a `file://` URI) to `C:/x y`.
fn uri_to_path_text(rest: &str) -> &str {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    // "/C:/..." on Windows drops the leading slash.
    if bytes.len() > 2 && bytes[0] == b'/' && bytes[2] == b':' {
        &rest[1..]
    } else {
        rest
    }
}

pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'%'
            && let Some(hex) = text.get(ix + 1..ix + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            ix += 3;
            continue;
        }
        out.push(bytes[ix]);
        ix += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Path from a `file://` URI, decoding `%20` and the like.
pub(crate) fn uri_path(uri: &str) -> PathBuf {
    ide_path(&percent_decode(uri))
}

/// `file:///C:/Users/me/x%20y.rs` for a path.
pub(crate) fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for ch in text.chars() {
        match ch {
            ' ' => uri.push_str("%20"),
            '#' => uri.push_str("%23"),
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3F"),
            ch => uri.push(ch),
        }
    }
    uri
}

/// Zero-based (row, column) of a byte offset.
fn point_at(text: &str, byte: usize) -> (usize, usize) {
    let before = &text[..byte];
    let row = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |ix| ix + 1);
    (row, before[line_start..].chars().count())
}

/// The range `openFile` asks to select: from the first `start` to the next
/// `end` after it (or the end of `start`), optionally to the end of its line.
fn find_range(
    text: &str,
    start: Option<&str>,
    end: Option<&str>,
    to_line_end: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let start_text = start?;
    let from = text.find(start_text)?;
    let mut to = match end {
        Some(end) => text[from..]
            .find(end)
            .map_or(from + start_text.len(), |ix| from + ix + end.len()),
        None => from + start_text.len(),
    };
    if to_line_end {
        to = text[to..]
            .find(['\r', '\n'])
            .map_or(text.len(), |ix| to + ix);
    }
    Some((point_at(text, from), point_at(text, to)))
}

impl Workspace {
    fn display_path(&self, path: &Path) -> String {
        self.root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub(crate) fn handle_ide_call(
        &mut self,
        call: ToolCall,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = call.name.clone();
        match name.as_str() {
            "openFile" => self.ide_open_file(call, window, cx),
            "openDiff" => self.ide_open_diff(call, window, cx),
            "getCurrentSelection" => {
                let selection = self.active_file_editor(cx).and_then(|editor| {
                    let mut params = selection_params(&editor, cx)?;
                    params["success"] = json!(true);
                    Some(params)
                });
                call.reply_json(selection.unwrap_or_else(
                    || json!({ "success": false, "message": "No active editor found" }),
                ));
            }
            "getLatestSelection" => {
                let latest = cx.global::<IdeBridge>().latest.clone().map(|mut params| {
                    params["success"] = json!(true);
                    params
                });
                call.reply_json(latest.unwrap_or_else(
                    || json!({ "success": false, "message": "No selection available" }),
                ));
            }
            "getOpenEditors" => {
                let active = self.active_file_editor(cx).map(|e| e.entity_id());
                let tabs: Vec<Value> = self
                    .items()
                    .filter_map(|(_, item)| {
                        let editor = item.editor()?.read(cx);
                        let path = editor.path()?;
                        Some(json!({
                            "uri": file_uri(path),
                            "isActive": Some(item.id()) == active,
                            "label": editor.title().to_string(),
                            "languageId": language_id(editor),
                            "isDirty": editor.is_dirty(),
                        }))
                    })
                    .collect();
                call.reply_json(json!({ "tabs": tabs }));
            }
            "getWorkspaceFolders" => {
                let folders: Vec<Value> = self
                    .root
                    .iter()
                    .map(|root| {
                        json!({
                            "name": root.file_name().map(|n| n.to_string_lossy().into_owned()),
                            "uri": file_uri(root),
                            "path": root.to_string_lossy(),
                        })
                    })
                    .collect();
                call.reply_json(json!({
                    "success": true,
                    "folders": folders,
                    "rootPath": self.root.as_ref().map(|root| root.to_string_lossy()),
                }));
            }
            "getDiagnostics" => {
                let uri = call.string("uri").map(uri_path);
                let diagnostics = self.ide_diagnostics(uri.as_deref(), cx);
                call.reply_json(diagnostics);
            }
            "checkDocumentDirty" => {
                let Some(path) = call.string("filePath").map(ide_path) else {
                    return call.reply_error("filePath is required");
                };
                let reply = match self.file_editor(&path, cx) {
                    Some(editor) => json!({
                        "success": true,
                        "filePath": path.to_string_lossy(),
                        "isDirty": editor.read(cx).is_dirty(),
                        "isUntitled": false,
                    }),
                    None => json!({
                        "success": false,
                        "message": format!("Document not open: {}", path.display()),
                    }),
                };
                call.reply_json(reply);
            }
            "saveDocument" => self.ide_save(call, cx),
            "close_tab" => {
                let name = call.string("tab_name").unwrap_or_default().to_owned();
                if let Some(id) = self
                    .proposals
                    .iter()
                    .find(|proposal| proposal.tab_name == name)
                    .map(|proposal| proposal.id)
                {
                    self.resolve_proposal(id, false, window, cx);
                }
                call.reply_text(&["TAB_CLOSED"]);
            }
            "closeAllDiffTabs" => {
                let ids: Vec<u64> = self.proposals.iter().map(|proposal| proposal.id).collect();
                for id in &ids {
                    self.resolve_proposal(*id, false, window, cx);
                }
                call.reply_text(&[&format!("CLOSED_{}_DIFF_TABS", ids.len())]);
            }
            name => call.reply_error(&format!("Ion doesn't support the {name} tool")),
        }
    }

    /// The active tab's editor, if it edits a file (not a diff).
    fn active_file_editor(&self, cx: &App) -> Option<Entity<Editor>> {
        self.active_editor()
            .filter(|editor| editor.read(cx).path().is_some())
    }

    fn file_editor(&self, path: &Path, cx: &App) -> Option<Entity<Editor>> {
        let (pane, ix) = self.find_file(path, cx)?;
        self.panes[&pane].items[ix].editor().cloned()
    }

    fn ide_open_file(&mut self, call: ToolCall, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = call.string("filePath").map(ide_path) else {
            return call.reply_error("filePath is required");
        };
        let start = call.string("startText").map(str::to_owned);
        let end = call.string("endText").map(str::to_owned);
        let to_line_end = call.bool("selectToEndOfLine", false);
        let filesystem = self.filesystem.clone();
        let find = cx.background_spawn({
            let path = path.clone();
            async move {
                let text = filesystem.load_text(&path).ok()?.text;
                let range = find_range(&text, start.as_deref(), end.as_deref(), to_line_end);
                Some((range, text.lines().count()))
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some((range, line_count)) = find.await else {
                return call.reply_error(&format!("File not found: {}", path.display()));
            };
            this.update_in(cx, |this, window, cx| {
                // Keep the keyboard where it is: the user is talking to the agent.
                this.open_file_with(path.clone(), range, false, window, cx);
                if call.bool("makeFrontmost", true) {
                    call.reply_text(&[&format!("Opened file: {}", path.display())]);
                } else {
                    let language = syntax::detect(&path).map(|id| id.name().to_lowercase());
                    call.reply_json(json!({
                        "success": true,
                        "filePath": path.to_string_lossy(),
                        "languageId": language.unwrap_or_else(|| "plaintext".into()),
                        "lineCount": line_count,
                    }));
                }
            })
            .ok();
        })
        .detach();
    }

    fn ide_save(&mut self, call: ToolCall, cx: &mut Context<Self>) {
        let Some(path) = call.string("filePath").map(ide_path) else {
            return call.reply_error("filePath is required");
        };
        let Some(editor) = self.file_editor(&path, cx) else {
            return call.reply_json(json!({
                "success": false,
                "message": format!("Document not open: {}", path.display()),
            }));
        };
        let save = editor.update(cx, |editor, cx| editor.save(cx));
        cx.spawn(async move |_, _| {
            let saved = save.await;
            call.reply_json(json!({
                "success": saved,
                "filePath": path.to_string_lossy(),
                "saved": saved,
                "message": if saved { "Document saved successfully" } else { "Save failed" },
            }));
        })
        .detach();
    }

    /// Problems for one file, or every file, in the shape `getDiagnostics`
    /// answers with.
    fn ide_diagnostics(&self, path: Option<&Path>, _cx: &App) -> Value {
        self.ide_problems(path)
    }

    // ---- proposed edits ------------------------------------------------------

    fn ide_open_diff(&mut self, call: ToolCall, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = call
            .string("new_file_path")
            .or(call.string("old_file_path"))
            .map(ide_path)
        else {
            return call.reply_error("new_file_path is required");
        };
        let old_path = call.string("old_file_path").map(ide_path);
        let tab_name = call.string("tab_name").unwrap_or_default().to_owned();
        let contents = call
            .arguments
            .get("new_file_contents")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // A new proposal for the same tab replaces the old one.
        if let Some(id) = self
            .proposals
            .iter()
            .find(|proposal| proposal.tab_name == tab_name)
            .map(|proposal| proposal.id)
        {
            self.resolve_proposal(id, false, window, cx);
        }
        let display = self.display_path(&path);
        let filesystem = self.filesystem.clone();
        let load = cx.background_spawn(async move {
            let old = filesystem
                .load_text(old_path.as_deref().unwrap_or(&path))
                .ok()
                .map(|loaded| loaded.text);
            let diff = git::diff_texts(&display, old.as_deref(), &contents, PROPOSAL_CONTEXT);
            (path, display, contents, diff)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (path, display, contents, diff) = load.await;
            this.update_in(cx, |this, window, cx| {
                let content = diff_view::proposal(&display, &[diff], &|_| path.clone());
                this.next_proposal += 1;
                let id = this.next_proposal;
                let row = diff_view::cursor_row(&content.rows, None);
                let tab = DiffTab {
                    target: DiffTarget::Proposal(id),
                    jumps: content.jumps,
                    reverts: Vec::new(),
                };
                let editor = cx.new(|cx| {
                    let mut editor = Editor::diff_view(
                        content.title,
                        &content.text,
                        content.rows,
                        content.language_hint,
                        cx,
                    );
                    editor.reveal_row(row, cx);
                    editor
                });
                let item = this.editor_item(editor, Some(tab), window, cx);
                let item_id = item.id();
                let pane = this.file_pane();
                this.insert_item_with_focus(pane, item, false, window, cx);
                this.proposals.push(Proposal {
                    id,
                    item: item_id,
                    tab_name,
                    display,
                    contents,
                    call: Some(call),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Answers the agent and closes the proposal's tab. Claude Code writes
    /// the file itself after an accept.
    pub(crate) fn resolve_proposal(
        &mut self,
        id: u64,
        accept: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.proposals.iter().position(|proposal| proposal.id == id) else {
            return;
        };
        let mut proposal = self.proposals.remove(ix);
        if let Some(call) = proposal.call.take() {
            if accept {
                call.reply_text(&["FILE_SAVED", &proposal.contents]);
            } else {
                call.reply_text(&["DIFF_REJECTED", &proposal.tab_name]);
            }
        }
        if let Some((pane, ix)) = self.find_item(proposal.item) {
            self.remove_item(pane, ix, window, cx);
        }
        cx.notify();
    }

    /// A proposal's tab closed: that rejects it.
    pub(crate) fn forget_proposal(&mut self, item: EntityId) {
        if let Some(ix) = self.proposals.iter().position(|p| p.item == item) {
            let mut proposal = self.proposals.remove(ix);
            if let Some(call) = proposal.call.take() {
                call.reply_text(&["DIFF_REJECTED", &proposal.tab_name]);
            }
        }
    }

    /// The Accept / Reject bar above a proposal's diff.
    pub(crate) fn render_proposal_bar(&self, item: &Item, cx: &mut Context<Self>) -> Option<Div> {
        let DiffTarget::Proposal(id) = item.diff()?.target else {
            return None;
        };
        let proposal = self.proposals.iter().find(|proposal| proposal.id == id)?;
        Some(
            div()
                .flex_none()
                .h(px(36.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .bg(theme::panel_bg())
                .border_b_1()
                .border_color(theme::border())
                .child(ui::icon_sized(ui::IconName::Bot, px(14.), theme::accent()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_muted())
                        .child(format!("Claude Code wants to change {}", proposal.display)),
                )
                .child(
                    ui::secondary_button(("proposal-reject", id as usize), "Reject").on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.resolve_proposal(id, false, window, cx)
                        }),
                    ),
                )
                .child(
                    ui::primary_button(("proposal-accept", id as usize), "Accept").on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.resolve_proposal(id, true, window, cx)
                        }),
                    ),
                ),
        )
    }

    // ---- selection -----------------------------------------------------------

    /// Tells connected agents about the active selection once it rests.
    pub(crate) fn schedule_ide_selection(&mut self, cx: &mut Context<Self>) {
        if !connected(cx) {
            return;
        }
        // Replacing the task cancels the previous wait.
        self.ide_selection_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SELECTION_DELAY).await;
            this.update(cx, |this, cx| this.send_ide_selection(cx)).ok();
        }));
    }

    fn send_ide_selection(&mut self, cx: &mut Context<Self>) {
        self.ide_selection_task = None;
        let Some(params) = self
            .active_file_editor(cx)
            .and_then(|editor| selection_params(&editor, cx))
        else {
            return;
        };
        let bridge = cx.global_mut::<IdeBridge>();
        if bridge.latest.as_ref() != Some(&params) {
            bridge.server.notify("selection_changed", params.clone());
            bridge.latest = Some(params);
        }
    }

    /// Sends a file reference straight into a connected agent's prompt.
    /// Returns false when no agent is connected.
    pub(crate) fn ide_mention(&self, path: &Path, lines: Option<(usize, usize)>, cx: &App) -> bool {
        if !connected(cx) {
            return false;
        }
        let mut params = json!({ "filePath": path.to_string_lossy() });
        if let Some((start, end)) = lines {
            params["lineStart"] = json!(start);
            params["lineEnd"] = json!(end);
        }
        cx.global::<IdeBridge>()
            .server
            .notify("at_mentioned", params);
        true
    }

    /// "Claude Code" in the status bar while an agent is connected.
    pub(crate) fn render_ide_status(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        connected(cx).then(|| {
            div()
                .id("ide-status")
                .h(px(20.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(4.))
                .whitespace_nowrap()
                .text_color(theme::accent())
                .child(div().size(px(6.)).rounded_full().bg(theme::git_added()))
                .child("Claude Code")
                .tooltip(ui::text_tooltip(
                    "Claude Code is connected: it sees your open file and selection,\n\
                     and its edits open here as diffs to accept or reject.",
                ))
        })
    }
}

/// `selection_changed` params for an editor's selection.
fn selection_params(editor: &Entity<Editor>, cx: &App) -> Option<Value> {
    let editor = editor.read(cx);
    let path = editor.path()?;
    let ((start_line, start_col), (end_line, end_col)) = editor.selection_points();
    let empty = (start_line, start_col) == (end_line, end_col);
    Some(json!({
        "text": editor.selected_text(),
        "filePath": path.to_string_lossy(),
        "fileUrl": file_uri(path),
        "selection": {
            "start": { "line": start_line, "character": start_col },
            "end": { "line": end_line, "character": end_col },
            "isEmpty": empty,
        },
    }))
}

fn language_id(editor: &Editor) -> String {
    editor
        .language_name()
        .map_or_else(|| "plaintext".into(), str::to_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_open_file_ranges() {
        let text = "fn a() {}\nfn main() {\n    run();\n}\n";
        assert_eq!(
            find_range(text, Some("fn main"), None, false),
            Some(((1, 0), (1, 7)))
        );
        assert_eq!(
            find_range(text, Some("fn main"), Some("}"), false),
            Some(((1, 0), (3, 1)))
        );
        assert_eq!(
            find_range(text, Some("run"), None, true),
            Some(((2, 4), (2, 10)))
        );
        assert_eq!(find_range(text, Some("missing"), None, false), None);
        assert_eq!(find_range(text, None, None, false), None);
    }

    #[test]
    fn converts_paths_and_uris() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\Users\me\my file.rs")
        } else {
            PathBuf::from("/home/me/my file.rs")
        };
        let uri = file_uri(&path);
        assert!(uri.starts_with("file:///"));
        assert!(uri.ends_with("my%20file.rs"));
        assert_eq!(uri_path(&uri), path);
    }
}
