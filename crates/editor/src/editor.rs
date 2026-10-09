use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AppContext, ClipboardItem, Context, CursorStyle, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, point, prelude::FluentBuilder, px,
};
use settings::WordWrap;
use text::{Buffer, Selection};

use crate::actions::*;
use crate::element::EditorElement;
use crate::git_gutter::GitDiffState;
use crate::highlighting::SyntaxState;
use crate::layout::EditorLayout;
use crate::wrap::WrapMap;

/// Searches stop after this many matches.
const MAX_SEARCH_MATCHES: usize = 10_000;

pub enum EditorEvent {
    /// The text changed (typing, paste, undo, reload...).
    Edited,
    SaveFailed(String),
    /// A row of a diff view was double-clicked.
    DiffRowActivated(usize),
    /// The Revert button of a diff view's hunk header row was clicked.
    RevertDiffHunk(usize),
}

/// What a row of a diff view shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRowKind {
    FileHeader,
    HunkHeader,
    /// Commit details and other notes.
    Meta,
    Context,
    Added,
    Removed,
    /// Space opposite a line the other side of a side-by-side diff has alone.
    Filler,
}

/// Styling and line numbers for one row of a diff view.
#[derive(Clone, Copy, Debug)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

struct SearchState {
    query: String,
    case_sensitive: bool,
    /// Char ranges of all matches, in order.
    matches: Vec<Range<usize>>,
}

/// A view that edits one text buffer, optionally backed by a file. In
/// single-line mode it serves as a text input (finder, find bar, prompts).
pub struct Editor {
    pub(crate) focus_handle: FocusHandle,
    pub(crate) buffer: Buffer,
    path: Option<PathBuf>,
    filesystem: project::FileSystem,
    pub(crate) secret: bool,
    has_bom: bool,
    pub(crate) single_line: bool,
    /// A multi-line text box (no gutter or line highlight).
    pub(crate) input: bool,
    pub(crate) read_only: bool,
    pub(crate) placeholder: SharedString,
    /// Tab title for buffers without a file (diff views).
    title: Option<SharedString>,
    /// File name used to pick the syntax language when there's no path.
    pub(crate) language_hint: Option<PathBuf>,
    /// Row styles when this editor shows a diff.
    pub(crate) diff_rows: Option<Arc<[DiffRow]>>,
    /// Changes against the last commit, for gutter markers.
    pub(crate) git_diff: Option<GitDiffState>,
    /// Diff view rows (hunk headers) that get a Revert button, sorted.
    pub(crate) revert_rows: Vec<usize>,
    /// A diff view shown side by side: the two sides.
    pub(crate) split: Option<crate::split_diff::SplitDiff>,
    /// This is one side of a side-by-side diff (one line number column).
    pub(crate) diff_side: bool,
    /// The other side, which scrolls along.
    pub(crate) scroll_partner: Option<gpui::WeakEntity<Editor>>,
    /// The row under the mouse, for hunk buttons.
    pub(crate) hover_row: Option<usize>,
    /// The mouse is over the gutter: fold chevrons show.
    pub(crate) gutter_hovered: bool,
    /// Problems reported for this file.
    pub(crate) diagnostics: crate::diagnostics::Diagnostics,
    /// Soft wrapping, rebuilt each frame the text or width changed.
    pub(crate) wrap: WrapMap,
    /// Alt+Z: wrapping turned on or off for this editor, over the setting.
    wrap_override: Option<bool>,
    /// Up/Down through wrapped rows: the cursor offset they left and the
    /// column within the row they aim for.
    wrap_goal: Option<(usize, usize)>,
    /// How far the content is scrolled. Clamped to valid values on each frame.
    pub(crate) scroll: Point<Pixels>,
    /// Scroll the cursor into view on the next frame.
    pub(crate) autoscroll: bool,
    /// IME composition range (char offsets).
    pub(crate) marked_range: Option<Range<usize>>,
    is_selecting: bool,
    pub(crate) layout: Option<EditorLayout>,
    pub(crate) syntax: Option<SyntaxState>,
    search: Option<SearchState>,
    /// Hash of the file as Ion last loaded or saved it; see
    /// [`Editor::matches_disk`].
    disk_hash: Option<u64>,
    /// Compresses the text of a tab that stays hidden (see `hidden.rs`).
    pub(crate) pack_task: Option<Task<()>>,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Editor {
    pub fn new(text: &str, path: Option<PathBuf>, has_bom: bool, cx: &mut Context<Self>) -> Self {
        let mut editor = Self {
            focus_handle: cx.focus_handle(),
            buffer: Buffer::new(text),
            path,
            filesystem: project::FileSystem::Local,
            secret: false,
            has_bom,
            single_line: false,
            input: false,
            read_only: false,
            placeholder: SharedString::default(),
            title: None,
            language_hint: None,
            diff_rows: None,
            git_diff: None,
            revert_rows: Vec::new(),
            split: None,
            diff_side: false,
            scroll_partner: None,
            hover_row: None,
            gutter_hovered: false,
            diagnostics: Default::default(),
            wrap: WrapMap::default(),
            wrap_override: None,
            wrap_goal: None,
            scroll: point(px(0.), px(0.)),
            autoscroll: false,
            marked_range: None,
            is_selecting: false,
            layout: None,
            syntax: None,
            search: None,
            disk_hash: None,
            pack_task: None,
        };
        editor.init_syntax();
        editor
    }

    /// A multi-line text box, like a commit message field. Enter adds a line.
    pub fn multi_line_input(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        let mut editor = Self::new("", None, false, cx);
        editor.input = true;
        editor.placeholder = placeholder.into();
        editor
    }

    /// A one-line text input. Enter, Up/Down and Escape bubble up to the
    /// parent view as the usual editor actions.
    pub fn single_line(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        let mut editor = Self::new("", None, false, cx);
        editor.single_line = true;
        // Tab and Shift+Tab move between fields.
        editor.focus_handle = cx.focus_handle().tab_stop(true);
        editor.placeholder = placeholder.into();
        editor
    }

    pub fn password(cx: &mut Context<Self>) -> Self {
        let mut editor = Self::single_line("Password or key passphrase", cx);
        editor.secret = true;
        editor
    }

    pub fn set_filesystem(&mut self, filesystem: project::FileSystem) {
        self.filesystem = filesystem;
    }

    pub fn filesystem(&self) -> &project::FileSystem {
        &self.filesystem
    }

    /// A read-only view of a diff. `language_hint` is a file name whose
    /// language highlights the code.
    pub fn diff_view(
        title: impl Into<SharedString>,
        text: &str,
        rows: Vec<DiffRow>,
        language_hint: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self::new(text, None, false, cx);
        editor.read_only = true;
        editor.title = Some(title.into());
        editor.diff_rows = Some(rows.into());
        editor.language_hint = language_hint;
        editor.init_syntax();
        editor
    }

    /// Replaces a diff view's contents, keeping the scroll position.
    /// Shows a different diff in this view, e.g. another file's.
    pub fn replace_diff(
        &mut self,
        title: impl Into<SharedString>,
        text: &str,
        rows: Vec<DiffRow>,
        language_hint: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.title = Some(title.into());
        let relanguage = self.language_hint != language_hint;
        self.language_hint = language_hint;
        self.set_diff_content(text, rows, cx);
        if relanguage {
            self.syntax = None;
            self.init_syntax();
        }
    }

    pub fn set_diff_content(&mut self, text: &str, rows: Vec<DiffRow>, cx: &mut Context<Self>) {
        self.diff_rows = Some(rows.into());
        if self.buffer.text() != text {
            let scroll = self.scroll;
            let cursor = self.buffer.cursor().min(text.chars().count());
            let len = self.buffer.len();
            self.buffer.replace(Some(0..len), text);
            self.buffer.place_cursor(cursor, false);
            self.buffer.mark_saved();
            self.text_changed(cx);
            self.scroll = scroll;
            self.clamp_scroll();
        }
        self.refresh_split(cx);
        cx.notify();
    }

    /// Changes on every edit (and undo), for views that cache the text.
    pub fn version(&self) -> u64 {
        self.buffer.version()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The selection's start and end as zero-based (row, column), in order.
    pub fn selection_points(&self) -> ((usize, usize), (usize, usize)) {
        let range = self.buffer.selection.range();
        (
            self.buffer.row_col(range.start),
            self.buffer.row_col(range.end),
        )
    }

    /// Zero-based row of the cursor.
    pub fn cursor_row(&self) -> usize {
        self.buffer.row_col(self.buffer.cursor()).0
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Gives an untitled buffer a file (Save As), or renames the backing file.
    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let language_changed =
            syntax::detect(&path) != self.path.as_deref().and_then(syntax::detect);
        self.path = Some(path);
        if language_changed {
            self.syntax = None;
            self.init_syntax();
        }
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        if let Some(title) = &self.title {
            return title.clone();
        }
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned().into())
            .unwrap_or_else(|| "untitled".into())
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    /// The text for background work: cloning a rope is O(1).
    pub fn rope(&self) -> text::Rope {
        self.buffer.rope().clone()
    }

    /// The faint text shown while the input is empty.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    /// Replaces all text and selects it (for inputs).
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let len = self.buffer.len();
        self.update_buffer(cx, |buffer| {
            buffer.replace(Some(0..len), text);
            buffer.select_all();
        });
    }

    /// Ctrl+/: comments the selected lines out, or back in.
    fn toggle_comment(&mut self, cx: &mut Context<Self>) {
        let tokens = self
            .path()
            .or(self.language_hint.as_deref())
            .and_then(syntax::detect)
            .and_then(syntax::LanguageId::comment_tokens);
        if let Some((open, close)) = tokens {
            self.update_buffer(cx, |b| {
                b.for_each_selection(|b| b.toggle_comment(open, close))
            });
        }
    }

    /// Shift+Tab: one level less indentation on the selected lines.
    fn outdent(&mut self, cx: &mut Context<Self>) {
        let unit = settings::get(cx).indent_unit();
        self.update_buffer(cx, |b| {
            b.for_each_selection(|b| b.indent_lines(&unit, true))
        });
    }

    pub fn selected_text(&self) -> String {
        self.buffer.selected_text()
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    pub fn set_disk_hash(&mut self, hash: u64) {
        self.disk_hash = Some(hash);
    }

    /// Whether file contents with this hash are what Ion itself last loaded
    /// or saved (so a watcher event for them isn't an outside change).
    pub fn matches_disk(&self, hash: u64) -> bool {
        self.disk_hash == Some(hash)
    }

    /// One-based line and column of the cursor.
    pub fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        let (row, col) = self.buffer.row_col(self.buffer.cursor());
        (row + 1, col + 1)
    }

    pub fn line_ending(&self) -> &'static str {
        if self.buffer.line_ending() == "\r\n" {
            "CRLF"
        } else {
            "LF"
        }
    }

    /// Selects a range given as zero-based (line, column) positions and
    /// scrolls it into view.
    pub fn select_range(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        cx: &mut Context<Self>,
    ) {
        let anchor = self.buffer.offset(start.0, start.1);
        let head = self.buffer.offset(end.0, end.1);
        self.buffer.set_selection(Selection { anchor, head });
        self.buffer.reveal_cursors();
        self.autoscroll = true;
        self.sync_split_cursor(cx);
        cx.notify();
    }

    /// Writes the buffer to its file on a background thread. Resolves to
    /// whether the save succeeded.
    pub fn save(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let Some(path) = self.path.clone() else {
            return Task::ready(false);
        };
        let settings = settings::get(cx);
        let (trim, newline) = (
            settings.trim_trailing_whitespace,
            settings.insert_final_newline,
        );
        if !self.read_only && (trim || newline) && self.buffer.tidy_for_save(trim, newline) {
            self.text_changed(cx);
        }
        // Cloning a rope is O(1), so the UI thread never waits on disk I/O.
        let rope = self.buffer.rope().clone();
        let version = self.buffer.version();
        let has_bom = self.has_bom;
        let filesystem = self.filesystem.clone();
        let write = cx.background_spawn(async move {
            filesystem
                .save_text(&path, has_bom, rope.chunks())
                .map(|()| project::content_hash(rope.chunks()))
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |editor, cx| {
                let saved = match result {
                    Ok(hash) => {
                        editor.buffer.mark_saved_at(version);
                        editor.disk_hash = Some(hash);
                        true
                    }
                    Err(err) => {
                        cx.emit(EditorEvent::SaveFailed(err.to_string()));
                        false
                    }
                };
                cx.notify();
                saved
            })
            .unwrap_or(false)
        })
    }

    /// Takes the file's new contents from disk (e.g. after an agent edited it).
    /// Only the changed middle part is replaced, as one undoable step, so the
    /// cursor stays put and Ctrl+Z restores the previous version.
    /// Returns the first row that changed, if any did.
    pub fn reload_from_disk(
        &mut self,
        new_text: &str,
        hash: u64,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.disk_hash = Some(hash);
        self.replace_changed(new_text, true, cx)
    }

    /// Replaces the whole text as one undoable step, like an edit (the
    /// buffer becomes dirty). Returns the first row that changed.
    pub fn apply_text(&mut self, new_text: &str, cx: &mut Context<Self>) -> Option<usize> {
        self.replace_changed(new_text, false, cx)
    }

    /// Replaces only the part of the text that differs from `new_text`, so
    /// the cursor and scroll position stay put.
    fn replace_changed(
        &mut self,
        new_text: &str,
        saved: bool,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let old_text = self.buffer.text();
        if old_text == new_text {
            if saved {
                self.buffer.mark_saved();
            }
            cx.notify();
            return None;
        }
        let prefix = common_prefix_chars(&old_text, new_text);
        let suffix = common_suffix_chars(&old_text, new_text, prefix);
        let old_len = old_text.chars().count();
        let middle: String = new_text
            .chars()
            .skip(prefix)
            .take(new_text.chars().count() - prefix - suffix)
            .collect();
        let (row, col) = self.buffer.row_col(self.buffer.cursor());
        let scroll = self.scroll;
        self.update_buffer(cx, |buffer| {
            buffer.replace(Some(prefix..old_len - suffix), &middle);
            let offset = buffer.offset(row, col);
            buffer.place_cursor(offset, false);
            if saved {
                buffer.mark_saved();
            }
        });
        self.scroll = scroll;
        self.autoscroll = false;
        Some(self.buffer.row_col(prefix).0)
    }

    /// Puts the cursor at the start of `row` and scrolls it into view.
    pub fn reveal_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let row = row.min(self.line_count().saturating_sub(1));
        self.select_range((row, 0), (row, 0), cx);
    }

    /// Applies a buffer operation, then scrolls the cursor into view and redraws.
    fn update_buffer(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Buffer)) {
        let version = self.buffer.version();
        f(&mut self.buffer);
        self.buffer.reveal_cursors();
        self.marked_range = None;
        self.autoscroll = true;
        if self.buffer.version() != version {
            self.text_changed(cx);
        }
        cx.notify();
    }

    /// Bookkeeping after any text change.
    pub(crate) fn text_changed(&mut self, cx: &mut Context<Self>) {
        self.buffer_changed(cx);
        self.schedule_git_diff(cx);
        self.refresh_search();
        cx.emit(EditorEvent::Edited);
    }

    fn page_rows(&self) -> isize {
        self.layout
            .as_ref()
            .map_or(30, |layout| layout.visible_rows().saturating_sub(1).max(1)) as isize
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.secret {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(self.clipboard_text()));
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.secret {
            return;
        }
        if self.read_only {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(self.clipboard_text()));
        self.update_buffer(cx, |buffer| {
            buffer.for_each_selection(|buffer| {
                if buffer.selection.is_empty() {
                    buffer.select_line_at(buffer.cursor());
                }
                buffer.backspace();
            })
        });
    }

    /// The selection, or the whole current line when nothing is selected.
    /// Several cursors copy a line each.
    fn clipboard_text(&self) -> String {
        if self.buffer.has_extra_cursors() {
            let texts: Vec<String> = self
                .buffer
                .selections()
                .iter()
                .map(|selection| self.buffer.text_in(selection.range()))
                .collect();
            return texts.join(self.buffer.line_ending());
        }
        if self.buffer.selection.is_empty() && !self.single_line {
            let (row, _) = self.buffer.row_col(self.buffer.cursor());
            let start = self.buffer.line_start(row);
            let end = if row + 1 < self.buffer.line_count() {
                self.buffer.line_start(row + 1)
            } else {
                self.buffer.len()
            };
            self.buffer.text_in(start..end)
        } else {
            self.buffer.selected_text()
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = if self.single_line {
                text.replace(['\r', '\n'], " ")
            } else {
                text
            };
            self.update_buffer(cx, |buffer| {
                // As many lines as cursors: a line each.
                let lines: Vec<&str> = text.lines().collect();
                if buffer.has_extra_cursors() && lines.len() == buffer.selections().len() {
                    buffer.edit_each(|buffer, ix| buffer.paste(lines[ix]));
                } else {
                    buffer.for_each_selection(|buffer| buffer.paste(&text));
                }
            });
        }
    }

    fn save_action(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        if self.path.is_none() {
            // No file yet: let the workspace ask where to save it.
            cx.propagate();
            return;
        }
        self.save(cx).detach();
    }

    // ---- search --------------------------------------------------------------

    /// Highlights every match of `query` and returns how many there are.
    pub fn set_search(
        &mut self,
        query: &str,
        case_sensitive: bool,
        cx: &mut Context<Self>,
    ) -> usize {
        self.search = (!query.is_empty()).then(|| SearchState {
            query: query.to_owned(),
            case_sensitive,
            matches: Vec::new(),
        });
        self.refresh_search();
        cx.notify();
        self.search
            .as_ref()
            .map_or(0, |search| search.matches.len())
    }

    pub fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        cx.notify();
    }

    fn refresh_search(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        search.matches.clear();
        let Ok(regex) = regex::RegexBuilder::new(&regex::escape(&search.query))
            .case_insensitive(!search.case_sensitive)
            .build()
        else {
            return;
        };
        let rope = self.buffer.rope();
        let text = rope.to_string();
        search.matches.extend(
            regex
                .find_iter(&text)
                .take(MAX_SEARCH_MATCHES)
                .map(|m| rope.byte_to_char(m.start())..rope.byte_to_char(m.end())),
        );
    }

    /// Current match (one-based) and total, for "3 of 12".
    pub fn search_position(&self) -> Option<(usize, usize)> {
        let search = self.search.as_ref()?;
        let selection = self.buffer.selection.range();
        let current = search
            .matches
            .iter()
            .position(|m| *m == selection)
            .map_or(0, |ix| ix + 1);
        Some((current, search.matches.len()))
    }

    /// Selects the next (or previous) match after the cursor, wrapping around.
    pub fn select_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(search) = &self.search else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let selection = self.buffer.selection.range();
        let ix = if forward {
            search
                .matches
                .iter()
                .position(|m| {
                    m.start > selection.start
                        || (m.start == selection.start && selection.is_empty())
                })
                .unwrap_or(0)
        } else {
            search
                .matches
                .iter()
                .rposition(|m| m.start < selection.start)
                .unwrap_or(search.matches.len() - 1)
        };
        let range = search.matches[ix].clone();
        self.buffer.set_selection(Selection {
            anchor: range.start,
            head: range.end,
        });
        self.autoscroll = true;
        cx.notify();
    }

    /// Selects the first match at or after the cursor (for find-as-you-type).
    pub fn select_nearest_match(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &self.search else {
            return;
        };
        let start = self.buffer.selection.range().start;
        let Some(range) = search
            .matches
            .iter()
            .find(|m| m.start >= start)
            .or(search.matches.first())
            .cloned()
        else {
            return;
        };
        self.buffer.set_selection(Selection {
            anchor: range.start,
            head: range.end,
        });
        self.autoscroll = true;
        cx.notify();
    }

    /// Replaces the selected match and moves to the next one.
    pub fn replace_match(&mut self, replacement: &str, cx: &mut Context<Self>) {
        let Some(search) = &self.search else {
            return;
        };
        let selection = self.buffer.selection.range();
        if search.matches.contains(&selection) {
            self.update_buffer(cx, |buffer| buffer.replace(Some(selection), replacement));
        }
        self.select_match(true, cx);
    }

    /// Replaces every match as one undoable step. Returns how many were replaced.
    pub fn replace_all(&mut self, replacement: &str, cx: &mut Context<Self>) -> usize {
        let Some(search) = &self.search else {
            return 0;
        };
        let count = search.matches.len();
        if count == 0 {
            return 0;
        }
        let first = search.matches[0].start;
        let mut text = String::with_capacity(self.buffer.rope().len_bytes());
        let mut last = 0;
        for range in &search.matches {
            text.push_str(&self.buffer.text_in(last..range.start));
            text.push_str(replacement);
            last = range.end;
        }
        text.push_str(&self.buffer.text_in(last..self.buffer.len()));
        let len = self.buffer.len();
        self.update_buffer(cx, |buffer| {
            buffer.replace(Some(0..len), &text);
            buffer.place_cursor(first, false);
        });
        count
    }

    /// Match ranges intersecting `chars`, for drawing.
    pub(crate) fn search_matches_in(&self, chars: Range<usize>) -> &[Range<usize>] {
        let Some(search) = &self.search else {
            return &[];
        };
        let start = search.matches.partition_point(|m| m.end <= chars.start);
        let end = search.matches.partition_point(|m| m.start < chars.end);
        &search.matches[start..end.max(start)]
    }

    // ---- mouse -------------------------------------------------------------

    /// Maps a window position to a buffer offset using the last frame's layout.
    pub(crate) fn offset_for_position(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let display_row = if self.single_line {
            0
        } else {
            let y = position.y - layout.bounds.top() + self.scroll.y;
            let last_row = self.wrap.row_count(self.buffer.line_count()) - 1;
            ((y / layout.line_height).floor().max(0.) as usize).min(last_row)
        };
        let x = position.x - layout.text_left + self.scroll.x;
        let (row, col) = match layout.display_line(display_row) {
            Some(line) => (line.row, line.col_for_x(x)),
            // Dragging past the visible lines: snap to the line start or end.
            None => {
                let (row, start, end) = self.wrap.segment(display_row);
                let above = display_row < layout.lines.first().map_or(0, |l| l.display_row);
                let col = match end {
                    _ if above => start,
                    Some(end) => end.saturating_sub(1),
                    None => self.buffer.line_len(row),
                };
                (row, col)
            }
        };
        Some(self.buffer.offset(row, col))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        if let Some(row) = self.fold_toggle_at(event.position) {
            self.toggle_fold_at(row, cx);
            return;
        }
        let Some(offset) = self.offset_for_position(event.position) else {
            return;
        };
        self.marked_range = None;
        match event.click_count {
            2 => {
                self.buffer.select_word_at(offset);
                if self.diff_rows.is_some() {
                    let row = self.buffer.row_col(offset).0;
                    cx.emit(EditorEvent::DiffRowActivated(row));
                }
            }
            3.. => self.buffer.select_line_at(offset),
            _ if event.modifiers.alt && !self.single_line => self.buffer.toggle_cursor(offset),
            _ => {
                self.buffer.place_cursor(offset, event.modifiers.shift);
                self.is_selecting = true;
            }
        }
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_selecting || event.pressed_button != Some(MouseButton::Left) {
            self.update_hover_row(event.position, cx);
            self.update_gutter_hover(event.position, cx);
            return;
        }
        if let Some(offset) = self.offset_for_position(event.position) {
            self.buffer.place_cursor(offset, true);
            self.autoscroll = true;
            cx.notify();
        }
    }

    /// Tracks the row under the mouse when hunks have buttons to show.
    fn update_hover_row(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.git_diff.is_none() && self.revert_rows.is_empty() {
            return;
        }
        let row = self
            .layout
            .as_ref()
            .filter(|layout| layout.bounds.contains(&position))
            .and_then(|layout| {
                let y = position.y - layout.bounds.top() + self.scroll.y;
                let display_row = (y / layout.line_height).floor().max(0.) as usize;
                let rows = self.wrap.row_count(self.buffer.line_count());
                (display_row < rows).then(|| self.wrap.segment(display_row).0)
            });
        if row != self.hover_row {
            self.hover_row = row;
            cx.notify();
        }
    }

    // ---- word wrap ---------------------------------------------------------

    /// Whether long lines wrap: Alt+Z's choice, else the setting. Inputs
    /// and diffs keep one row per line (a commit message box wraps).
    pub(crate) fn wraps(&self, cx: &gpui::App) -> bool {
        if self.single_line || self.secret || self.diff_rows.is_some() {
            return false;
        }
        if let Some(wrap) = self.wrap_override {
            return wrap;
        }
        if self.input {
            return true;
        }
        match settings::get(cx).word_wrap {
            WordWrap::Off => false,
            WordWrap::On => true,
            WordWrap::Prose => self.is_prose(),
        }
    }

    /// Markdown and plain text, which read better wrapped.
    fn is_prose(&self) -> bool {
        let name = self.path.as_deref().or(self.language_hint.as_deref());
        let extension = name
            .and_then(|name| name.extension())
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        matches!(
            extension.as_deref(),
            Some("md" | "markdown" | "mdx" | "txt" | "text" | "rst" | "adoc" | "org")
        )
    }

    /// Whether this editor shows a Markdown file.
    pub fn is_markdown(&self) -> bool {
        let extension = self
            .path
            .as_deref()
            .and_then(|path| path.extension())
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        matches!(extension.as_deref(), Some("md" | "markdown" | "mdx"))
    }

    fn toggle_word_wrap(&mut self, _: &ToggleWordWrap, _: &mut Window, cx: &mut Context<Self>) {
        self.wrap_override = Some(!self.wraps(cx));
        self.autoscroll = true;
        cx.notify();
    }

    /// Up/Down: through wrapped rows when wrapping, else buffer lines.
    fn move_rows(&mut self, delta: isize, select: bool, cx: &mut Context<Self>) {
        if !self.wrap.is_active() || self.buffer.has_extra_cursors() {
            self.wrap_goal = None;
            return self.update_buffer(cx, |b| {
                b.for_each_selection(|b| b.move_vertical(delta, select))
            });
        }
        let cursor = self.buffer.cursor();
        let (row, col) = self.buffer.row_col(cursor);
        let display_row = self.wrap.display_row(row, col);
        let (_, start, _) = self.wrap.segment(display_row);
        let goal = match self.wrap_goal {
            Some((offset, goal)) if offset == cursor => goal,
            _ => col - start,
        };
        let rows = self.wrap.row_count(self.buffer.line_count());
        let target = display_row as isize + delta;
        let offset = if target < 0 {
            0
        } else if target as usize >= rows {
            self.buffer.len()
        } else {
            let (row, start, end) = self.wrap.segment(target as usize);
            let last = match end {
                // The last column of a continued row shows on the next one.
                Some(end) => end.saturating_sub(1),
                None => self.buffer.line_len(row),
            };
            self.buffer.offset(row, (start + goal).min(last))
        };
        self.update_buffer(cx, |b| b.move_to(offset, select));
        self.wrap_goal = Some((self.buffer.cursor(), goal));
    }

    /// Diff view rows (hunk headers) that get a Revert button.
    pub fn set_revert_rows(&mut self, rows: Vec<usize>, cx: &mut Context<Self>) {
        if self.revert_rows != rows {
            self.revert_rows = rows;
            self.refresh_split(cx);
            cx.notify();
        }
    }

    /// Ctrl+Alt+Z: reverts the change at the cursor. In a diff view, asks
    /// the workspace to revert the hunk the cursor is in.
    fn revert_hunk_action(&mut self, _: &RevertHunk, _: &mut Window, cx: &mut Context<Self>) {
        let row = self.cursor_row();
        if self.diff_rows.is_some() {
            let header = self.revert_rows.partition_point(|&header| header <= row);
            match header.checked_sub(1).map(|ix| self.revert_rows[ix]) {
                Some(header) => cx.emit(EditorEvent::RevertDiffHunk(header)),
                None => cx.propagate(),
            }
        } else if !self.revert_hunk_at_row(row, cx) {
            cx.propagate();
        }
    }

    /// Revert buttons over the gutter's change (under the mouse, else at the
    /// cursor) and over a diff view's visible hunk headers.
    fn render_hunk_buttons(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        let (first, last) = match (layout.lines.first(), layout.lines.last()) {
            (Some(first), Some(last)) => (first.row, last.row),
            _ => return Vec::new(),
        };
        let line_height = layout.line_height;
        let button = |id: usize, row: usize, cx: &mut Context<Self>| {
            let top = line_height * self.wrap.row_start(row) as f32 - self.scroll.y;
            div()
                .id(("revert-hunk", id))
                .absolute()
                .top(top)
                .right(px(14.))
                .h(line_height)
                .flex()
                .items_center()
                .px(px(6.))
                .gap_1()
                .rounded(px(5.))
                .bg(theme::elevated_bg())
                .border_1()
                .border_color(theme::border())
                .text_size(theme::ui_font_size_small())
                .text_color(theme::text_muted())
                .cursor_pointer()
                .hover(|button| button.bg(theme::hover_bg()).text_color(theme::text()))
                .tooltip(ui::tooltip(
                    "Revert this change",
                    Some(Box::new(RevertHunk)),
                ))
                .child(ui::icon_sized(
                    ui::IconName::Undo2,
                    px(12.),
                    theme::text_muted(),
                ))
                .child("Revert")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if this.diff_rows.is_some() {
                            cx.emit(EditorEvent::RevertDiffHunk(row));
                        } else {
                            this.revert_hunk_at_row(row, cx);
                        }
                    }),
                )
                .into_any_element()
        };
        if self.diff_rows.is_some() {
            let start = self.revert_rows.partition_point(|&row| row < first);
            let end = self.revert_rows.partition_point(|&row| row <= last);
            return self.revert_rows[start..end]
                .iter()
                .map(|&row| button(row, row, cx))
                .collect();
        }
        if self.read_only || self.input || self.single_line || !settings::get(cx).git_gutter {
            return Vec::new();
        }
        let hunk = self
            .hover_row
            .and_then(|row| self.hunk_at_row(row))
            .or_else(|| self.hunk_at_row(self.cursor_row()));
        match hunk.map(|hunk| hunk.new.start as usize) {
            Some(row) if (first..=last).contains(&row) => vec![button(0, row, cx)],
            _ => Vec::new(),
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.single_line {
            return;
        }
        let line_height = theme::editor_line_height();
        let delta = event.delta.pixel_delta(line_height);
        // Shift + wheel scrolls horizontally on mice without a horizontal wheel.
        let delta = if event.modifiers.shift && delta.x == px(0.) {
            point(delta.y, delta.x)
        } else {
            delta
        };
        self.scroll -= delta;
        self.autoscroll = false;
        self.clamp_scroll();
        // Keep the other side of a side-by-side diff in step this frame
        // rather than one late, so its rows and buttons don't trail.
        if let Some(partner) = self.scroll_partner.as_ref().and_then(|p| p.upgrade()) {
            let y = self.scroll.y;
            partner.update(cx, |partner, cx| {
                if partner.scroll.y != y {
                    partner.scroll.y = y;
                    partner.autoscroll = false;
                    partner.clamp_scroll();
                    cx.notify();
                }
            });
        }
        cx.notify();
        cx.stop_propagation();
    }

    /// Clamps `scroll` as prepaint will. Revert buttons and hit tests read it
    /// before the next prepaint, so an overscrolled value makes them jump.
    pub(crate) fn clamp_scroll(&mut self) {
        let line_height = theme::editor_line_height();
        let rows = self.wrap.row_count(self.buffer.line_count());
        let max_y = line_height * rows.saturating_sub(1) as f32;
        self.scroll.y = self.scroll.y.clamp(px(0.), max_y);
        self.scroll.x = self.scroll.x.max(px(0.));
    }
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Common suffix length in chars, not overlapping a prefix of `prefix` chars.
fn common_suffix_chars(a: &str, b: &str, prefix: usize) -> usize {
    let max = a.chars().count().min(b.chars().count()) - prefix;
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take(max)
        .take_while(|(x, y)| x == y)
        .count()
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.wake();
        if let Some(split) = &self.split {
            return div()
                .id("editor")
                .key_context("Editor")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(theme::bg())
                .on_action(cx.listener(Self::toggle_diff_layout))
                .child(self.render_split(split))
                .into_any_element();
        }
        self.ensure_syntax_tree(cx);
        macro_rules! on {
            ($action:ty, |$b:ident| $body:expr) => {
                cx.listener(|this, _: &$action, _, cx| {
                    this.update_buffer(cx, |b| b.for_each_selection(|$b| $body))
                })
            };
        }
        // Text-changing actions do nothing in read-only views.
        macro_rules! edit {
            ($action:ty, |$b:ident| $body:expr) => {
                cx.listener(|this, _: &$action, _, cx| {
                    if !this.read_only {
                        this.update_buffer(cx, |b| b.for_each_selection(|$b| $body))
                    }
                })
            };
        }
        // In an input these keys belong to the surrounding view (e.g. Enter
        // confirms, Up/Down move through a result list).
        macro_rules! multiline {
            ($action:ty, |$this:ident, $cx:ident| $body:expr) => {
                cx.listener(|$this, _: &$action, _, $cx| {
                    if $this.single_line {
                        $cx.propagate();
                    } else {
                        $body
                    }
                })
            };
        }

        let hunk_buttons = self.render_hunk_buttons(cx);
        div()
            .id("editor")
            .key_context(if self.buffer.has_extra_cursors() {
                "Editor multicursor"
            } else {
                "Editor"
            })
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            // Inputs take their parent's background.
            .when(!self.single_line && !self.input, |editor| {
                editor.bg(theme::bg())
            })
            .cursor(CursorStyle::IBeam)
            .on_action(on!(MoveLeft, |b| b.move_left(false)))
            .on_action(on!(MoveRight, |b| b.move_right(false)))
            .on_action(multiline!(MoveUp, |this, cx| this.move_rows(-1, false, cx)))
            .on_action(multiline!(MoveDown, |this, cx| this.move_rows(1, false, cx)))
            .on_action(on!(SelectLeft, |b| b.move_left(true)))
            .on_action(on!(SelectRight, |b| b.move_right(true)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.move_rows(-1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.move_rows(1, true, cx)))
            .on_action(cx.listener(Self::toggle_word_wrap))
            .on_action(on!(MoveWordLeft, |b| b.move_word_left(false)))
            .on_action(on!(MoveWordRight, |b| b.move_word_right(false)))
            .on_action(on!(SelectWordLeft, |b| b.move_word_left(true)))
            .on_action(on!(SelectWordRight, |b| b.move_word_right(true)))
            .on_action(on!(MoveLineStart, |b| b.move_line_start(false)))
            .on_action(on!(MoveLineEnd, |b| b.move_line_end(false)))
            .on_action(on!(SelectLineStart, |b| b.move_line_start(true)))
            .on_action(on!(SelectLineEnd, |b| b.move_line_end(true)))
            .on_action(on!(MoveDocStart, |b| b.move_doc_start(false)))
            .on_action(on!(MoveDocEnd, |b| b.move_doc_end(false)))
            .on_action(on!(SelectDocStart, |b| b.move_doc_start(true)))
            .on_action(on!(SelectDocEnd, |b| b.move_doc_end(true)))
            .on_action(edit!(Backspace, |b| if !b.backspace_pair() {
                b.backspace()
            }))
            .on_action(edit!(Delete, |b| b.delete()))
            .on_action(edit!(DeleteWordLeft, |b| b.delete_word_left()))
            .on_action(edit!(DeleteWordRight, |b| b.delete_word_right()))
            .on_action(multiline!(Newline, |this, cx| if !this.read_only {
                this.update_buffer(cx, |b| b.for_each_selection(|b| b.newline()))
            }))
            .on_action(multiline!(ToggleComment, |this, cx| if !this.read_only {
                this.toggle_comment(cx)
            }))
            .on_action(multiline!(DuplicateLine, |this, cx| if !this.read_only {
                this.update_buffer(cx, |b| b.for_each_selection(|b| b.duplicate_lines()))
            }))
            .on_action(multiline!(MoveLineUp, |this, cx| if !this.read_only {
                this.update_buffer(cx, |b| {
                    b.clear_extra_cursors();
                    b.move_lines(true)
                })
            }))
            .on_action(multiline!(MoveLineDown, |this, cx| if !this.read_only {
                this.update_buffer(cx, |b| {
                    b.clear_extra_cursors();
                    b.move_lines(false)
                })
            }))
            .on_action(multiline!(DeleteLine, |this, cx| if !this.read_only {
                this.update_buffer(cx, |b| b.for_each_selection(|b| b.delete_lines()))
            }))
            .on_action(multiline!(Backtab, |this, cx| if !this.read_only {
                this.outdent(cx)
            }))
            .on_action(multiline!(Tab, |this, cx| if !this.read_only {
                let spaces = settings::get(cx).indent_unit();
                this.update_buffer(cx, |b| {
                    b.for_each_selection(|b| {
                        if b.selection_is_multiline() {
                            b.indent_lines(&spaces, false)
                        } else {
                            b.insert(&spaces)
                        }
                    })
                })
            }))
            .on_action(
                cx.listener(|this, _: &SelectAll, _, cx| {
                    this.update_buffer(cx, |b| b.select_all())
                }),
            )
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                if !this.read_only {
                    this.update_buffer(cx, |b| {
                        b.undo();
                    })
                }
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                if !this.read_only {
                    this.update_buffer(cx, |b| {
                        b.redo();
                    })
                }
            }))
            .on_action(multiline!(AddNextOccurrence, |this, cx| {
                this.update_buffer(cx, |b| {
                    b.add_next_occurrence();
                })
            }))
            .on_action(multiline!(SelectAllOccurrences, |this, cx| {
                this.update_buffer(cx, |b| b.select_all_occurrences())
            }))
            .on_action(cx.listener(|this, _: &ClearCursors, _, cx| {
                if this.buffer.clear_extra_cursors() {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(multiline!(PageUp, |this, cx| {
                let rows = this.page_rows();
                this.move_rows(-rows, false, cx)
            }))
            .on_action(multiline!(PageDown, |this, cx| {
                let rows = this.page_rows();
                this.move_rows(rows, false, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| {
                let rows = this.page_rows();
                this.move_rows(-rows, true, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| {
                let rows = this.page_rows();
                this.move_rows(rows, true, cx)
            }))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::save_action))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_action(cx.listener(Self::revert_hunk_action))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.hover_row.take().is_some() {
                    cx.notify();
                }
                if !hovered && this.gutter_hovered {
                    this.gutter_hovered = false;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(Self::toggle_diff_layout))
            .on_action(cx.listener(Self::fold))
            .on_action(cx.listener(Self::unfold))
            .on_action(cx.listener(Self::fold_all))
            .on_action(cx.listener(Self::unfold_all))
            .child(EditorElement::new(cx.entity()))
            .children(hunk_buttons)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_finds_changed_middle() {
        let (a, b) = ("hello world", "hello brave world");
        let prefix = common_prefix_chars(a, b);
        let suffix = common_suffix_chars(a, b, prefix);
        assert_eq!((prefix, suffix), (6, 5));
        // Prefix and suffix never overlap, even for repeated text.
        let (a, b) = ("aaa", "aaaa");
        let prefix = common_prefix_chars(a, b);
        assert_eq!((prefix, common_suffix_chars(a, b, prefix)), (3, 0));
    }
}
